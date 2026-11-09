//! Production wiring for memory extraction and idle consolidation.
//!
//! @nyan: `query/stop_hooks.rs`, `services/compact/auto_compact.rs` and
//! `commands/compact/compact.rs` call only the functions in this file, so the
//! gating, single-flight and cursor rules live in exactly one place.

use super::{
    ExtractFailure, ExtractOutcome, ExtractSkipReason, ExtractStatus, ExtractionProgress,
    extraction_is_due,
};
use crate::constants::query_source::QuerySource;
use crate::tool::ToolUseContext;
use crate::types::message::Message;
use crate::utils::forked_agent::CacheSafeParams;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// One memory-writing agent (extraction or consolidation) at a time per
/// process. Both edit `MEMORY.md`, and consolidation re-validates the index
/// against its pre-run copy, so a concurrent extraction could otherwise have
/// its index line rolled back. Turn-end extraction uses `try_lock` (skip when
/// busy); the forced pre-compact path waits, bounded by its own timeout.
static MEMORY_AGENT_GATE: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// `None` when another memory agent is running.
pub(crate) fn try_memory_agent_gate() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    MEMORY_AGENT_GATE.try_lock().ok()
}

/// Wait for the gate, giving up after `timeout`.
pub(crate) async fn memory_agent_gate_within(
    timeout: Duration,
) -> Option<tokio::sync::MutexGuard<'static, ()>> {
    tokio::time::timeout(timeout, MEMORY_AGENT_GATE.lock())
        .await
        .ok()
}

/// True while another memory agent holds the gate (used by the idle trigger).
pub(crate) fn memory_agent_busy() -> bool {
    try_memory_agent_gate().is_none()
}

static PROGRESS: LazyLock<Mutex<ExtractionProgress>> =
    LazyLock::new(|| Mutex::new(ExtractionProgress::default()));
static CURSOR_LOADED: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

fn progress() -> std::sync::MutexGuard<'static, ExtractionProgress> {
    PROGRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Main-thread, auto-memory-on, non-bare, `nyan.memory.extract` gate shared by
/// every entry point.
pub fn extraction_enabled(query_source: &QuerySource) -> bool {
    matches!(query_source, QuerySource::Prompt)
        && crate::memdir::paths::is_auto_memory_enabled()
        && !crate::utils::env_utils::is_bare_mode()
        && memory_settings().extract()
}

fn memory_settings() -> crate::utils::settings::nyan::MemorySettings {
    crate::utils::settings::get_initial_settings()
        .nyan
        .as_ref()
        .map(|nyan| nyan.memory())
        .unwrap_or_default()
}

/// Load the persisted cursor once per session id (covers resume).
fn ensure_cursor_loaded() {
    let session = crate::bootstrap::state::get_session_id();
    let mut loaded = CURSOR_LOADED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if loaded.as_deref() == Some(session.as_str()) {
        return;
    }
    *loaded = Some(session);
    *progress() = ExtractionProgress {
        cursor: crate::utils::session_storage::read_memory_extract_cursor(),
        ..Default::default()
    };
}

/// Append the cursor entry, then advance the in-memory state. Called only for
/// `ExtractStatus::Success`.
fn commit_success(outcome: &ExtractOutcome, token_count: u64) {
    if let Some(uuid) = outcome.cursor.message_uuid.as_deref() {
        let at = outcome
            .cursor
            .at_ms
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        if let Err(error) = crate::utils::session_storage::record_memory_extract_cursor(uuid, at) {
            // A cursor that cannot be persisted must not advance in memory
            // either, or a resume would re-extract a different range.
            tracing::warn!(
                target: "nyan::memory",
                error = %error,
                "memory_extract_cursor_persist_failed"
            );
            return;
        }
    }
    let mut state = progress();
    state.turns_since_extract = 0;
    state.tokens_at_last_extract = token_count;
    state.cursor = outcome.cursor.clone();
}

fn log_outcome(label: &str, outcome: &ExtractOutcome) {
    match &outcome.status {
        ExtractStatus::Success => tracing::info!(
            target: "nyan::memory",
            input_tokens = outcome.input_tokens,
            output_tokens = outcome.output_tokens,
            "{label}: memory_extract_ok"
        ),
        ExtractStatus::Skipped(reason) => {
            tracing::debug!(target: "nyan::memory", ?reason, "{label}: memory_extract_skipped")
        }
        ExtractStatus::Failed(failure) => {
            tracing::warn!(target: "nyan::memory", ?failure, "{label}: memory_extract_failed")
        }
    }
}

fn read_index() -> String {
    std::fs::read_to_string(crate::memdir::paths::get_auto_mem_entrypoint()).unwrap_or_default()
}

/// @nyan (C4): `memory.extract_now` / `memory.extract_skip` shadow comparison.
/// Detached, so the throttle below stays the only decision. Cloning the history
/// only happens when at least one switch is on.
fn spawn_turn_shadow(messages: &[Message], due: bool, turns_since: u32, token_delta: u64) {
    use crate::services::memory_decision as shadow;
    let extract_now = shadow::shadow_enabled_now(shadow::DecisionPoint::MemoryExtractNow);
    let extract_skip = shadow::shadow_enabled_now(shadow::DecisionPoint::MemoryExtractSkip);
    if !extract_now && !extract_skip {
        return;
    }
    let messages = messages.to_vec();
    shadow::spawn_detached(async move {
        if extract_now {
            shadow::evaluate_extract_now(&messages, due, turns_since, token_delta).await;
        }
        // The deterministic rule never skips, so a skip comparison is only
        // meaningful on a turn the rule says is due.
        if extract_skip && due {
            shadow::evaluate_extract_skip(&messages, turns_since, token_delta).await;
        }
    });
}

/// @nyan (C4): `memory.type` / `memory.duplicate` shadow comparison around one
/// memory-agent run. Returns `None` (no directory walk, no allocation) when both
/// switches are off. When on, the directory walk runs on the blocking pool so
/// the async worker never does file IO; it still finishes before the memory
/// agent starts, so the snapshot is a true pre-run view.
async fn snapshot_before_memory_run()
-> Option<Vec<crate::services::memory_decision::MemoryFileSnapshot>> {
    if !crate::services::memory_decision::types_or_duplicates_enabled() {
        return None;
    }
    // A failed blocking task only drops this shadow comparison.
    tokio::task::spawn_blocking(crate::services::memory_decision::snapshot_memory_files)
        .await
        .ok()
}

fn spawn_memory_file_shadow(
    before: Option<Vec<crate::services::memory_decision::MemoryFileSnapshot>>,
) {
    use crate::services::memory_decision as shadow;
    let Some(before) = before else {
        return;
    };
    shadow::spawn_detached(async move {
        let Ok(after) = tokio::task::spawn_blocking(shadow::snapshot_memory_files).await else {
            return;
        };
        let changed = shadow::changed_since(&before, &after);
        if changed.is_empty() {
            return;
        }
        shadow::evaluate_types(&changed, 30).await;
        shadow::evaluate_duplicates(&before, &changed, 30, 30).await;
    });
}

/// Turn-end entry. Counts the turn, applies the turn/token throttle and, when
/// due, runs one extraction. Safe to call from a detached task.
pub async fn extract_if_due(
    context: ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    messages: Vec<Message>,
    query_source: &QuerySource,
) -> ExtractStatus {
    extract_if_due_with_deps(
        context,
        cache_safe_params,
        messages,
        query_source,
        crate::query::deps::production_deps(),
    )
    .await
}

pub(crate) async fn extract_if_due_with_deps<D: crate::query::deps::QueryDeps>(
    context: ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    messages: Vec<Message>,
    query_source: &QuerySource,
    deps: D,
) -> ExtractStatus {
    if !extraction_enabled(query_source) {
        return ExtractStatus::Skipped(ExtractSkipReason::Disabled);
    }
    ensure_cursor_loaded();
    let settings = memory_settings();
    let token_count = crate::utils::tokens::token_count_with_estimation(&messages).max(0) as u64;
    let (due, cursor, turns_since, token_delta) = {
        let mut state = progress();
        state.turns_since_extract = state.turns_since_extract.saturating_add(1);
        let turns_since = state.turns_since_extract;
        let token_delta = token_count.saturating_sub(state.tokens_at_last_extract);
        (
            extraction_is_due(
                &state,
                messages.len(),
                token_count,
                settings.extract_every_turns(),
                settings.extract_every_tokens(),
            ),
            state.cursor.clone(),
            turns_since,
            token_delta,
        )
    };
    // @nyan (C4): shadow comparison; never alters `due` and never blocks.
    spawn_turn_shadow(&messages, due, turns_since, token_delta);
    if !due {
        return ExtractStatus::Skipped(ExtractSkipReason::NotDue);
    }
    let Some(_gate) = try_memory_agent_gate() else {
        return ExtractStatus::Skipped(ExtractSkipReason::Busy);
    };
    // Boxed so the snapshot adds one pointer to this future, not its state.
    let shadow_before = Box::pin(snapshot_before_memory_run()).await;
    let outcome = super::run_extract_memories_with_deps(
        context,
        cache_safe_params,
        messages,
        cursor,
        read_index(),
        settings.extract_max_turns(),
        // Background extraction is bounded by the same ceiling as the forced
        // path so a wedged fork cannot hold the gate forever.
        Duration::from_millis(settings.extract_before_compact_timeout_ms()),
        deps,
    )
    .await;
    spawn_memory_file_shadow(shadow_before);
    log_outcome("turn_end", &outcome);
    if outcome.status == ExtractStatus::Success {
        commit_success(&outcome, token_count);
    }
    outcome.status
}

/// Forced extraction before a summary replaces history (M2.1). Ignores the
/// throttle, honours `extractBeforeCompact` and its timeout, never fails the
/// compaction: every non-success status is returned for the caller to log.
pub async fn extract_before_compact(
    context: &ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    messages: &[Message],
) -> ExtractStatus {
    extract_before_compact_with_deps(
        context,
        cache_safe_params,
        messages,
        crate::query::deps::production_deps(),
    )
    .await
}

pub(crate) async fn extract_before_compact_with_deps<D: crate::query::deps::QueryDeps>(
    context: &ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    messages: &[Message],
    deps: D,
) -> ExtractStatus {
    if !crate::memdir::paths::is_auto_memory_enabled() || crate::utils::env_utils::is_bare_mode() {
        return ExtractStatus::Skipped(ExtractSkipReason::Disabled);
    }
    let settings = memory_settings();
    if !settings.extract() || !settings.extract_before_compact() {
        return ExtractStatus::Skipped(ExtractSkipReason::Disabled);
    }
    // Sub-agents and forks (summary, session-memory, ...) have no cursor of
    // their own; only the main session's history is extracted.
    if context.agent_id.is_some() {
        return ExtractStatus::Skipped(ExtractSkipReason::Disabled);
    }
    ensure_cursor_loaded();
    let timeout = Duration::from_millis(settings.extract_before_compact_timeout_ms());
    let token_count = crate::utils::tokens::token_count_with_estimation(messages).max(0) as u64;
    let cursor = progress().cursor.clone();
    // Wait for an in-flight memory agent, but the WHOLE forced step (wait +
    // run) stays under the one configured ceiling.
    let started = std::time::Instant::now();
    let Some(_gate) = memory_agent_gate_within(timeout).await else {
        tracing::warn!(
            target: "nyan::memory",
            "extract_skipped_before_compact: another memory agent held the gate past the timeout"
        );
        return ExtractStatus::Failed(ExtractFailure::TimedOut);
    };
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining < Duration::from_millis(250) {
        tracing::warn!(
            target: "nyan::memory",
            "extract_skipped_before_compact: no time left after waiting for the memory agent gate"
        );
        return ExtractStatus::Failed(ExtractFailure::TimedOut);
    }
    // The in-flight run may have just advanced the cursor.
    let cursor = {
        let fresh = progress().cursor.clone();
        if fresh.message_uuid.is_some() {
            fresh
        } else {
            cursor
        }
    };
    // Boxed so the snapshot adds one pointer to this future, not its state.
    let shadow_before = Box::pin(snapshot_before_memory_run()).await;
    let outcome = super::run_extract_memories_with_deps(
        context.clone(),
        cache_safe_params,
        messages.to_vec(),
        cursor,
        read_index(),
        settings.extract_max_turns(),
        remaining,
        deps,
    )
    .await;
    spawn_memory_file_shadow(shadow_before);
    log_outcome("before_compact", &outcome);
    match &outcome.status {
        ExtractStatus::Success => commit_success(&outcome, token_count),
        ExtractStatus::Failed(_) => tracing::warn!(
            target: "nyan::memory",
            "extract_skipped_before_compact: extraction failed; cursor unchanged, compaction proceeds"
        ),
        ExtractStatus::Skipped(_) => {}
    }
    outcome.status
}

/// Pre-summary hook for both compaction entries (`auto_compact_if_needed` and
/// `/compact`): runs the forced extraction over the exact history the summary
/// is about to replace, sharing its cache prefix. Any non-success is logged by
/// the driver and swallowed here — it must never block or fail compaction.
///
/// `fork_context_messages` is the history the summary will see; the system
/// prompt and contexts are the compaction's own, i.e. the main thread's.
pub async fn extract_before_compact_for_summary(
    context: &ToolUseContext,
    cache: &crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
) {
    // Cheap exits first so the common disabled case allocates nothing. The
    // settings gate repeats inside `extract_before_compact`, but checking it
    // here avoids cloning the whole history on every compaction in sessions
    // that turned extraction off.
    let settings = memory_settings();
    if context.agent_id.is_some()
        || !settings.extract()
        || !settings.extract_before_compact()
        || !crate::memdir::paths::is_auto_memory_enabled()
        || crate::utils::env_utils::is_bare_mode()
    {
        return;
    }
    // Compaction tests run the real `auto_compact_if_needed` / `/compact` path
    // with production deps and a non-aborted controller; without this gate they
    // would hit the network from a unit test. The harness opts in explicitly.
    #[cfg(test)]
    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_TEST_MEMORY_EXTRACT").as_deref(),
    ) {
        return;
    }
    let messages = cache.fork_context_messages.clone();
    let cache_safe_params = Arc::new(CacheSafeParams {
        system_prompt: cache.system_prompt.clone(),
        user_context: cache.user_context.clone(),
        system_context: cache.system_context.clone(),
        tool_use_context: context.clone(),
        fork_context_messages: Arc::new(messages.clone()),
    });
    let _ = extract_before_compact(context, Some(cache_safe_params), &messages).await;
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    *progress() = ExtractionProgress::default();
    *CURSOR_LOADED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[cfg(test)]
pub(crate) fn progress_for_test() -> ExtractionProgress {
    progress().clone()
}

/// Hold the shared gate the way a running extraction would.
#[cfg(test)]
pub(crate) fn test_gate_guard() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    try_memory_agent_gate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::extract_memories::ExtractCursor;
    use crate::utils::settings::types::SettingsJson;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Scripted fork: optionally fails with an API error, otherwise answers.
    #[derive(Clone)]
    struct ScriptedDeps {
        api_error: bool,
    }

    impl crate::query::deps::QueryDeps for ScriptedDeps {
        fn call_model(
            &self,
            _request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            let api_error = self.api_error;
            Box::pin(async move {
                let (tx, rx) = tokio::sync::mpsc::channel(8);
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
                            content: vec![crate::types::message::AssistantContent::Text(
                                "extracted".to_string(),
                            )],
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
        session_root: std::path::PathBuf,
        original_session: String,
        original_project_dir: Option<std::path::PathBuf>,
        _env: Vec<EnvVarGuard>,
        _lock: crate::utils::test_env::TestEnvGuard<'static>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            crate::bootstrap::state::switch_session(
                &self.original_session,
                self.original_project_dir.clone(),
            );
            let _ = std::fs::remove_dir_all(&self.root);
            let _ = std::fs::remove_dir_all(&self.session_root);
        }
    }

    fn fixture(every_turns: u32, extract: bool) -> Fixture {
        let lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("nyan-driver-{}", uuid::Uuid::new_v4()));
        let session_root =
            std::env::temp_dir().join(format!("nyan-driver-session-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&session_root).unwrap();
        let env = vec![
            EnvVarGuard::set("NYAN_MEMORY_PATH_OVERRIDE", &root),
            EnvVarGuard::unset("NYAN_DISABLE_AUTO_MEMORY"),
            EnvVarGuard::unset("NYAN_SIMPLE"),
            EnvVarGuard::unset("NYAN_SKIP_PROMPT_HISTORY"),
            EnvVarGuard::set("NYAN_WRITE_ENABLED", "1"),
        ];
        crate::bootstrap::state::set_session_persistence_disabled(false);
        let fixture = Fixture {
            root,
            session_root,
            original_session: crate::bootstrap::state::get_session_id(),
            original_project_dir: crate::bootstrap::state::get_session_project_dir(),
            _env: env,
            _lock: lock,
        };
        crate::bootstrap::state::switch_session(
            "driver-active",
            Some(fixture.session_root.clone()),
        );
        std::fs::write(fixture.session_root.join("driver-active.jsonl"), "").unwrap();
        // Adopt the session file so cursor entries land on disk (not in the
        // pre-materialization pending queue).
        crate::utils::session_storage::adopt_resumed_session_file().unwrap();
        let mut settings = SettingsJson {
            auto_memory_enabled: Some(true),
            ..Default::default()
        };
        let mut memory = crate::utils::settings::nyan::MemorySettings::default();
        memory.extract = Some(extract);
        memory.extract_every_turns = Some(every_turns);
        // Large token threshold so turns alone decide.
        memory.extract_every_tokens = Some(u32::MAX);
        memory.extract_before_compact_timeout_ms = Some(5_000);
        settings.nyan = Some(Box::new(crate::utils::settings::nyan::NyanSettings {
            memory: Some(memory),
            ..Default::default()
        }));
        crate::memdir::paths::seed_settings(settings, None);
        reset_for_test();
        fixture
    }

    fn messages() -> Vec<Message> {
        vec![
            Message::User(crate::utils::messages::create_user_message(
                "u1".to_string(),
            )),
            Message::User(crate::utils::messages::create_user_message(
                "u2".to_string(),
            )),
        ]
    }

    fn cache(context: &ToolUseContext) -> Option<Arc<CacheSafeParams>> {
        Some(Arc::new(CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(Vec::new()),
        }))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn turn_throttle_skips_until_due_then_advances_cursor_on_success() {
        let _fixture = fixture(2, true);
        let context = ToolUseContext::default().with_main_loop_model("m");
        // Same history every turn: uuids are freshly generated per call.
        let history = messages();
        let last_uuid = history.last().unwrap().uuid().to_string();
        let status = extract_if_due_with_deps(
            context.clone(),
            cache(&context),
            history.clone(),
            &QuerySource::Prompt,
            ScriptedDeps { api_error: false },
        )
        .await;
        assert_eq!(status, ExtractStatus::Skipped(ExtractSkipReason::NotDue));
        assert_eq!(progress_for_test().turns_since_extract, 1);

        let status = extract_if_due_with_deps(
            context.clone(),
            cache(&context),
            history.clone(),
            &QuerySource::Prompt,
            ScriptedDeps { api_error: false },
        )
        .await;
        assert_eq!(status, ExtractStatus::Success);
        let state = progress_for_test();
        assert_eq!(state.turns_since_extract, 0);
        assert_eq!(
            state.cursor.message_uuid.as_deref(),
            Some(last_uuid.as_str()),
            "cursor lands on the last message of the extracted range"
        );
        // The transcript append is queued; flush before reading it back.
        crate::utils::session_storage::flush_session_storage()
            .await
            .unwrap();
        assert_eq!(
            crate::utils::session_storage::read_memory_extract_cursor()
                .message_uuid
                .as_deref(),
            Some(last_uuid.as_str()),
            "the successful cursor is persisted for resume"
        );

        // Success reset the counter, so the very next turn is not due again.
        let status = extract_if_due_with_deps(
            context,
            None,
            history,
            &QuerySource::Prompt,
            ScriptedDeps { api_error: false },
        )
        .await;
        assert_eq!(status, ExtractStatus::Skipped(ExtractSkipReason::NotDue));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_extraction_keeps_the_cursor_and_retries_next_turn() {
        let _fixture = fixture(1, true);
        let context = ToolUseContext::default().with_main_loop_model("m");
        let status = extract_if_due_with_deps(
            context.clone(),
            cache(&context),
            messages(),
            &QuerySource::Prompt,
            ScriptedDeps { api_error: true },
        )
        .await;
        assert!(matches!(status, ExtractStatus::Failed(_)));
        let state = progress_for_test();
        assert_eq!(
            state.cursor,
            ExtractCursor::default(),
            "cursor must not move"
        );
        assert_eq!(state.turns_since_extract, 1);

        // Still due (the counter never reset), so the next turn retries.
        let status = extract_if_due_with_deps(
            context,
            cache(&ToolUseContext::default()),
            messages(),
            &QuerySource::Prompt,
            ScriptedDeps { api_error: false },
        )
        .await;
        assert_eq!(status, ExtractStatus::Success);
        assert!(progress_for_test().cursor.message_uuid.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gating_requires_the_main_thread_and_the_extract_setting() {
        let _fixture = fixture(1, true);
        let context = ToolUseContext::default().with_main_loop_model("m");
        let status = extract_if_due_with_deps(
            context.clone(),
            cache(&context),
            messages(),
            &QuerySource::Sdk,
            ScriptedDeps { api_error: false },
        )
        .await;
        assert_eq!(status, ExtractStatus::Skipped(ExtractSkipReason::Disabled));

        // Forced pre-compact extraction from a subagent is likewise a skip.
        assert_eq!(
            extract_before_compact_with_deps(
                &context.with_agent_id(Some("agent-1".into())),
                None,
                &messages(),
                ScriptedDeps { api_error: false },
            )
            .await,
            ExtractStatus::Skipped(ExtractSkipReason::Disabled)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forced_extraction_ignores_the_throttle_but_honours_the_setting() {
        let _fixture = fixture(99, true);
        let context = ToolUseContext::default().with_main_loop_model("m");
        let status = extract_before_compact_with_deps(
            &context,
            cache(&context),
            &messages(),
            ScriptedDeps { api_error: false },
        )
        .await;
        assert_eq!(
            status,
            ExtractStatus::Success,
            "the forced path does not wait for the turn throttle"
        );
        assert!(progress_for_test().cursor.message_uuid.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extract_setting_off_disables_every_entry_point() {
        let _fixture = fixture(1, false);
        let context = ToolUseContext::default().with_main_loop_model("m");
        assert!(!extraction_enabled(&QuerySource::Prompt));
        assert_eq!(
            extract_if_due_with_deps(
                context.clone(),
                cache(&context),
                messages(),
                &QuerySource::Prompt,
                ScriptedDeps { api_error: false },
            )
            .await,
            ExtractStatus::Skipped(ExtractSkipReason::Disabled)
        );
        assert_eq!(
            extract_before_compact_with_deps(
                &context,
                cache(&context),
                &messages(),
                ScriptedDeps { api_error: false },
            )
            .await,
            ExtractStatus::Skipped(ExtractSkipReason::Disabled)
        );
        assert_eq!(progress_for_test().turns_since_extract, 0);
    }
}
