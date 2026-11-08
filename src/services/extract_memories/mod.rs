//! Background memory extraction service.
//!
//! @nyan: production entry points live in [`driver`] — the stop hook calls
//! [`driver::extract_if_due`] at turn end, and both compaction paths call
//! [`driver::extract_before_compact_for_summary`] before a summary replaces
//! history. The functions here run one fork and report a typed outcome; they
//! never persist the cursor themselves.

pub mod driver;

use crate::constants::query_source::QuerySource;
use crate::tool::{CanUseToolCallback, ToolUseContext};
use crate::types::message::{Message, UserContent, UserMessage};
use crate::types::permissions::{PermissionDecision, PermissionDecisionReason};
use crate::utils::forked_agent::{CacheSafeParams, ForkedAgentParams, SubagentContextOverrides};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const CURSOR_METADATA_TYPE: &str = "memory-extract-cursor";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractCursor {
    pub message_uuid: Option<String>,
    pub at_ms: Option<i64>,
}

impl ExtractCursor {
    pub fn marker(&self) -> Option<serde_json::Value> {
        self.message_uuid.as_ref().map(|uuid| {
            serde_json::json!({
                "type": CURSOR_METADATA_TYPE,
                "uuid": uuid,
                "at": self.at_ms.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
            })
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractionProgress {
    pub turns_since_extract: u32,
    pub tokens_at_last_extract: u64,
    pub cursor: ExtractCursor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtractSkipReason {
    Disabled,
    NotDue,
    /// Another extraction holds the process-wide gate.
    Busy,
    NoCacheSafeParams,
    NoNewMessages,
    MemoryGuardBlocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtractFailure {
    Agent(String),
    TimedOut,
    Aborted,
    MemoryGuardBlocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtractStatus {
    Success,
    Skipped(ExtractSkipReason),
    Failed(ExtractFailure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractOutcome {
    pub status: ExtractStatus,
    pub cursor: ExtractCursor,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl ExtractOutcome {
    fn skipped(reason: ExtractSkipReason, cursor: ExtractCursor) -> Self {
        Self {
            status: ExtractStatus::Skipped(reason),
            cursor,
            input_tokens: 0,
            output_tokens: 0,
        }
    }
}

/// Cursor metadata is parsed separately from conversation messages because it
/// is a session JSONL metadata entry, not a model-visible `Message`.
pub fn cursor_from_metadata(entries: &[serde_json::Value]) -> ExtractCursor {
    entries
        .iter()
        .rev()
        .find(|entry| {
            entry.get("type").and_then(serde_json::Value::as_str) == Some(CURSOR_METADATA_TYPE)
        })
        .map(|entry| ExtractCursor {
            message_uuid: entry
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            at_ms: entry.get("at").and_then(serde_json::Value::as_i64),
        })
        .unwrap_or_default()
}

pub fn messages_after_cursor(messages: &[Message], cursor: &ExtractCursor) -> Vec<Message> {
    // M2.2: no cursor = from the start of what is visible.
    let Some(uuid) = cursor.message_uuid.as_deref() else {
        return messages.to_vec();
    };
    let Some(index) = messages.iter().position(|message| message.uuid() == uuid) else {
        // M2.2 resume: a cursor that was summarized away sits at the compact
        // boundary, so only the boundary and later are unextracted.
        if let Some(boundary) = crate::utils::messages::find_last_compact_boundary_index(messages) {
            return messages.iter().skip(boundary).cloned().collect();
        }
        return messages.to_vec();
    };
    messages.iter().skip(index + 1).cloned().collect()
}

fn extraction_prompt(memory_index: &str, memory_dir: &Path) -> String {
    format!(
        "[MEMORY EXTRACTION]\nExtract only durable facts that cannot be derived from the current repository or git history. Use only the four memory types user, feedback, project, and reference. Prefer updating an existing topic over creating a duplicate. `type: user` files are candidates and must not be added to MEMORY.md automatically. Never save prompts, transcripts, temporary task state, secrets, or credentials.\n\nThe complete cache-safe conversation history is already available to you. Use the cursor and index below to avoid duplicating existing memories.\n\nCurrent MEMORY.md index:\n{memory_index}\n\nWrite candidate/topic files only under the auto-memory directory `{dir}`. Always pass absolute paths inside it to every tool call (Glob/Grep need an explicit `path`). Add `source: extract` to the frontmatter of every file you create. Add an index entry only for low-risk, non-user memories you are confident about. You cannot delete files. Report what you wrote and which files remain candidates.",
        dir = memory_dir.display(),
    )
}

fn deny(tool: &str, reason: &str, tool_use_id: &str) -> PermissionDecision {
    PermissionDecision::Deny {
        message: format!(
            "{tool} is not allowed for memory extraction outside the memory directory"
        ),
        decision_reason: PermissionDecisionReason::Other {
            reason: reason.to_string(),
        },
        tool_use_id: Some(tool_use_id.to_string()),
    }
}

fn path_is_in_memory(path: &str, context: &ToolUseContext, memory_dir: &Path) -> bool {
    crate::utils::path::expand_path(path, Some(&context.effective_cwd()))
        .map(|path| path.starts_with(memory_dir))
        .unwrap_or(false)
}

fn tool_path_allowed(
    tool_name: &str,
    input: &serde_json::Value,
    context: &ToolUseContext,
    memory_dir: &Path,
) -> bool {
    let paths = if tool_name == "Read" {
        input
            .get("files")
            .and_then(serde_json::Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(|file| file.get("file_path").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
            })
            .filter(|paths| !paths.is_empty())
            .unwrap_or_else(|| {
                input
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .into_iter()
                    .collect()
            })
    } else {
        input
            .get("path")
            .or_else(|| input.get("file_path"))
            .and_then(serde_json::Value::as_str)
            .into_iter()
            .collect()
    };
    !paths.is_empty()
        && paths
            .into_iter()
            .all(|path| path_is_in_memory(path, context, memory_dir))
}

/// @nyan: the single tool allowlist shared by extraction and consolidation.
/// Read/Glob/Grep/Write/Edit only, every path inside auto-memory; no Bash and
/// no delete-capable tool exist in this set.
pub(crate) fn memory_tool_callback(
    context: &ToolUseContext,
    memory_dir: &Path,
) -> CanUseToolCallback {
    let context = context.clone();
    let memory_dir = memory_dir.to_path_buf();
    CanUseToolCallback::new(
        move |tool, input, _context, _assistant, tool_use_id, _force| {
            let allowed_name = matches!(
                tool.name.as_str(),
                "Read" | "Glob" | "Grep" | "Write" | "Edit"
            );
            if !allowed_name {
                return deny(
                    &tool.name,
                    "memory agents have no shell or unrelated tool access",
                    tool_use_id,
                );
            }
            if !tool_path_allowed(&tool.name, input, &context, &memory_dir) {
                return deny(
                    &tool.name,
                    "memory agent paths must stay inside auto-memory",
                    tool_use_id,
                );
            }
            PermissionDecision::Allow {
                updated_input: None,
                user_modified: None,
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: "memory extraction directory allowlist".to_string(),
                }),
                tool_use_id: Some(tool_use_id.to_string()),
                accept_feedback: None,
                content_blocks: Vec::new(),
            }
        },
    )
}

/// Input side of a fork's usage: cache reads/creations are input tokens too
/// and dominate a cache-sharing fork's bill.
fn input_total(usage: &crate::services::api::claude::NonNullableUsage) -> u64 {
    usage
        .input_tokens
        .saturating_add(usage.cache_creation_input_tokens)
        .saturating_add(usage.cache_read_input_tokens)
}

/// Deterministic turn/token throttle used by the stop-hook integration.
pub fn extraction_is_due(
    progress: &ExtractionProgress,
    message_count: usize,
    token_count: u64,
    every_turns: u32,
    every_tokens: u32,
) -> bool {
    message_count > 0
        && (progress.turns_since_extract >= every_turns.max(1)
            || token_count.saturating_sub(progress.tokens_at_last_extract)
                >= u64::from(every_tokens))
}

/// Run one extraction fork. The coordinator must append `outcome.cursor.marker()`
/// to the session metadata only after this returns `ExtractStatus::Success`.
pub async fn run_extract_memories(
    context: ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    messages: Vec<Message>,
    cursor: ExtractCursor,
    memory_index: String,
    max_turns: u32,
    timeout: Duration,
) -> ExtractOutcome {
    run_extract_memories_with_deps(
        context,
        cache_safe_params,
        messages,
        cursor,
        memory_index,
        max_turns,
        timeout,
        crate::query::deps::production_deps(),
    )
    .await
}

pub(crate) async fn run_extract_memories_with_deps<D: crate::query::deps::QueryDeps>(
    context: ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    messages: Vec<Message>,
    cursor: ExtractCursor,
    memory_index: String,
    max_turns: u32,
    timeout: Duration,
    deps: D,
) -> ExtractOutcome {
    let Some(cache_safe_params) = cache_safe_params else {
        return ExtractOutcome::skipped(ExtractSkipReason::NoCacheSafeParams, cursor);
    };
    let new_messages = messages_after_cursor(&messages, &cursor);
    if new_messages.is_empty() {
        return ExtractOutcome::skipped(ExtractSkipReason::NoNewMessages, cursor);
    }
    let memory_dir = crate::memdir::paths::get_auto_mem_path();
    if !crate::memdir::write_guard::check_memory_injection(
        &crate::memdir::paths::get_auto_mem_entrypoint(),
        &memory_index,
    ) {
        return ExtractOutcome {
            status: ExtractStatus::Failed(ExtractFailure::MemoryGuardBlocked),
            cursor,
            input_tokens: 0,
            output_tokens: 0,
        };
    }

    let prompt_message = Message::User(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::Text(extraction_prompt(
            &memory_index,
            &memory_dir,
        ))],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    });

    // @nyan: the fork gets its own child controller so a timeout can stop it
    // without aborting the main session; a main-session abort still cascades.
    let fork_abort = crate::tool::AbortController::child_of(context.abort_controller.clone());
    let callback = memory_tool_callback(&context, &memory_dir);
    let params = ForkedAgentParams {
        prompt_messages: vec![prompt_message],
        cache_safe_params: (*cache_safe_params).clone(),
        can_use_tool: callback,
        query_source: QuerySource::ExtractMemories,
        fork_label: "extract-memories".to_string(),
        overrides: Some(SubagentContextOverrides {
            abort_controller: Some(fork_abort.clone()),
            ..Default::default()
        }),
        max_output_tokens: None,
        max_turns: Some(max_turns.max(1)),
        on_message: None,
        on_progress: None,
        skip_cache_write: true,
        skip_transcript: true,
    };

    let index_before = std::fs::read_to_string(crate::memdir::paths::get_auto_mem_entrypoint())
        .unwrap_or_default();
    let result = match tokio::time::timeout(
        timeout,
        crate::utils::forked_agent::run_forked_agent_with_deps(params, deps),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let status = if crate::utils::errors::is_abort_error(&error) {
                ExtractFailure::Aborted
            } else {
                ExtractFailure::Agent(error.to_string())
            };
            return ExtractOutcome {
                status: ExtractStatus::Failed(status),
                cursor,
                input_tokens: 0,
                output_tokens: 0,
            };
        }
        Err(_) => {
            // Dropping the timed-out future does not stop the fork's query
            // thread; abort it explicitly.
            fork_abort.abort();
            let _ = crate::memdir::memdir::enforce_agent_index_policy(&index_before);
            return ExtractOutcome {
                status: ExtractStatus::Failed(ExtractFailure::TimedOut),
                cursor,
                input_tokens: 0,
                output_tokens: 0,
            };
        }
    };
    let usage = result.total_usage;
    // `run_forked_agent` returns Ok on an aborted query; abort must not look
    // like success or the cursor would advance over unextracted messages.
    if fork_abort.is_aborted() {
        let _ = crate::memdir::memdir::enforce_agent_index_policy(&index_before);
        return ExtractOutcome {
            status: ExtractStatus::Failed(ExtractFailure::Aborted),
            cursor,
            input_tokens: input_total(&usage),
            output_tokens: usage.output_tokens,
        };
    }
    if let Err(error) = crate::memdir::memdir::enforce_agent_index_policy(&index_before) {
        return ExtractOutcome {
            status: ExtractStatus::Failed(ExtractFailure::Agent(error)),
            cursor,
            input_tokens: input_total(&usage),
            output_tokens: usage.output_tokens,
        };
    }
    if !result.api_errors.is_empty() {
        return ExtractOutcome {
            status: ExtractStatus::Failed(ExtractFailure::Agent(
                "extraction fork returned API errors".to_string(),
            )),
            cursor,
            input_tokens: input_total(&usage),
            output_tokens: usage.output_tokens,
        };
    }
    let next_cursor = new_messages
        .last()
        .map(|message| ExtractCursor {
            message_uuid: Some(message.uuid().to_string()),
            at_ms: Some(chrono::Utc::now().timestamp_millis()),
        })
        .unwrap_or(cursor);
    ExtractOutcome {
        status: ExtractStatus::Success,
        cursor: next_cursor,
        input_tokens: input_total(&usage),
        output_tokens: usage.output_tokens,
    }
}

/// Public seam for the compaction owner. The timeout is deliberately bounded;
/// compaction proceeds after a failed/expired extraction and the cursor stays.
pub async fn extract_before_compact(
    context: ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    messages: Vec<Message>,
    cursor: ExtractCursor,
    memory_index: String,
    max_turns: u32,
    timeout_ms: u64,
) -> ExtractOutcome {
    run_extract_memories(
        context,
        cache_safe_params,
        messages,
        cursor,
        memory_index,
        max_turns,
        Duration::from_millis(timeout_ms),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_requires_turn_or_token_threshold() {
        let progress = ExtractionProgress {
            turns_since_extract: 7,
            tokens_at_last_extract: 100,
            ..Default::default()
        };
        // No message yet: never due, whatever the counters say.
        assert!(!extraction_is_due(&progress, 0, 30_000, 8, 30_000));
        // 7 turns and a 29_900-token delta are both below their thresholds.
        assert!(!extraction_is_due(&progress, 1, 30_000, 8, 30_000));
        // Turns threshold reached.
        assert!(extraction_is_due(
            &ExtractionProgress {
                turns_since_extract: 8,
                ..progress.clone()
            },
            1,
            100,
            8,
            30_000
        ));
        // Token threshold reached (delta exactly 30_000).
        assert!(extraction_is_due(&progress, 1, 30_100, 8, 30_000));
    }

    #[test]
    fn cursor_slices_after_last_message_and_preserves_no_cursor_behavior() {
        let messages = vec![
            Message::User(crate::utils::messages::create_user_message(
                "one".to_string(),
            )),
            Message::User(crate::utils::messages::create_user_message(
                "two".to_string(),
            )),
        ];
        let cursor = ExtractCursor {
            message_uuid: Some(messages[0].uuid().to_string()),
            at_ms: None,
        };
        assert_eq!(messages_after_cursor(&messages, &cursor).len(), 1);
        assert_eq!(
            messages_after_cursor(&messages, &ExtractCursor::default()).len(),
            2
        );
        // A cursor whose message was summarized away stops at the boundary.
        let mut compacted = vec![Message::System(
            crate::types::message::SystemMessage::compact_boundary(None),
        )];
        compacted.extend(messages.clone());
        let stale = ExtractCursor {
            message_uuid: Some("summarized-away".to_string()),
            at_ms: None,
        };
        assert_eq!(
            messages_after_cursor(&compacted, &stale).len(),
            3,
            "boundary itself + the two visible messages"
        );
    }

    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// One scripted model turn: optional index edit (simulating what a Write
    /// tool would have done), then a final text answer with usage. Every
    /// request is recorded.
    #[derive(Clone)]
    struct ScriptedDeps {
        before_answer: Arc<dyn Fn() + Send + Sync>,
        abort_on_call: Option<crate::tool::AbortController>,
        requests: Arc<std::sync::Mutex<Vec<crate::query::deps::CallModelRequest>>>,
    }

    impl crate::query::deps::QueryDeps for ScriptedDeps {
        fn call_model(
            &self,
            request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            self.requests.lock().unwrap().push(request);
            let before_answer = self.before_answer.clone();
            let abort = self.abort_on_call.clone();
            Box::pin(async move {
                (before_answer)();
                if let Some(abort) = abort {
                    abort.abort();
                }
                let (tx, rx) = tokio::sync::mpsc::channel(8);
                tx.send(crate::services::api::claude::QueryModelStreamItem::Stream(
                    crate::types::message::StreamEvent::ApiEvent {
                        event: serde_json::json!({
                            "type": "message_delta",
                            "usage": {"input_tokens": 5, "output_tokens": 3}
                        }),
                        ttft_ms: Some(1),
                    },
                ))
                .await
                .unwrap();
                tx.send(
                    crate::services::api::claude::QueryModelStreamItem::Assistant(
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::Text(
                                "done".to_string(),
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
        let root = std::env::temp_dir().join(format!("nyan-extract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let env = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &root);
        Fixture {
            root,
            _lock: lock,
            _env: env,
        }
    }

    fn cache_params(context: &ToolUseContext) -> Option<Arc<CacheSafeParams>> {
        Some(Arc::new(CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(Vec::new()),
        }))
    }

    fn history() -> Vec<Message> {
        vec![
            Message::User(crate::utils::messages::create_user_message(
                "one".to_string(),
            )),
            Message::User(crate::utils::messages::create_user_message(
                "two".to_string(),
            )),
        ]
    }

    async fn run(
        context: ToolUseContext,
        messages: Vec<Message>,
        deps: ScriptedDeps,
    ) -> ExtractOutcome {
        run_extract_memories_with_deps(
            context.clone(),
            cache_params(&context),
            messages,
            ExtractCursor::default(),
            String::new(),
            2,
            Duration::from_secs(20),
            deps,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fork_replays_the_session_prefix_on_the_session_selection() {
        let _fixture = fixture();
        let mut context = ToolUseContext::default()
            .with_main_loop_model("session-model")
            .with_effort_value(Some(crate::utils::effort::Effort::High));
        context.main_loop_profile = Some("session-profile".to_string());
        let messages = history();
        let cache = CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(messages.clone()),
        };
        let deps = ScriptedDeps {
            before_answer: Arc::new(|| {}),
            abort_on_call: None,
            requests: Default::default(),
        };
        let outcome = run_extract_memories_with_deps(
            context,
            Some(Arc::new(cache)),
            messages.clone(),
            ExtractCursor::default(),
            String::new(),
            2,
            Duration::from_secs(20),
            deps.clone(),
        )
        .await;
        assert_eq!(outcome.status, ExtractStatus::Success);

        let requests = deps.requests.lock().unwrap();
        let [request] = requests.as_slice() else {
            panic!("expected one request, got {}", requests.len());
        };
        assert_eq!(request.query_source, QuerySource::ExtractMemories);
        assert_eq!(request.options.model, "session-model");
        assert_eq!(request.options.profile.as_deref(), Some("session-profile"));
        assert_eq!(
            request.options.effort_value,
            Some(crate::utils::effort::Effort::High)
        );
        assert_eq!(request.system_prompt, vec!["system".to_string()]);
        // The session history first, unchanged; the extraction prompt last.
        assert_eq!(request.messages.len(), messages.len() + 1);
        assert_eq!(request.messages[..messages.len()], messages[..]);
        let Some(Message::User(prompt)) = request.messages.last() else {
            panic!("the extraction prompt is the last message");
        };
        assert!(
            crate::utils::messages::get_user_message_text(prompt)
                .is_some_and(|text| text.starts_with("[MEMORY EXTRACTION]"))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn successful_fork_advances_cursor_to_last_message_and_reports_usage() {
        let _fixture = fixture();
        let context = ToolUseContext::default().with_main_loop_model("m");
        let messages = history();
        let last = messages.last().unwrap().uuid().to_string();
        let outcome = run(
            context,
            messages,
            ScriptedDeps {
                before_answer: Arc::new(|| {}),
                abort_on_call: None,
                requests: Default::default(),
            },
        )
        .await;
        assert_eq!(outcome.status, ExtractStatus::Success);
        assert_eq!(outcome.cursor.message_uuid.as_deref(), Some(last.as_str()));
        assert_eq!(outcome.input_tokens, 5);
        assert_eq!(outcome.output_tokens, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn aborted_fork_fails_and_keeps_the_cursor() {
        let _fixture = fixture();
        let context = ToolUseContext::default().with_main_loop_model("m");
        // Aborting the session controller cascades into the fork's child.
        let session_abort = context.abort_controller.clone();
        let outcome = run(
            context,
            history(),
            ScriptedDeps {
                before_answer: Arc::new(|| {}),
                abort_on_call: Some(session_abort),
                requests: Default::default(),
            },
        )
        .await;
        assert_eq!(
            outcome.status,
            ExtractStatus::Failed(ExtractFailure::Aborted)
        );
        assert_eq!(outcome.cursor, ExtractCursor::default());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn indexing_a_user_memory_before_its_file_exists_is_rolled_back() {
        let fixture = fixture();
        let index = crate::memdir::paths::get_auto_mem_entrypoint();
        std::fs::write(&index, "- [kept](kept.md) — kept\n").unwrap();
        let root = fixture.root.clone();
        let context = ToolUseContext::default().with_main_loop_model("m");
        let outcome = run(
            context,
            history(),
            ScriptedDeps {
                // Index first (target missing -> the write-time check cannot
                // see its type), file second: the order that bypasses it.
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
                abort_on_call: None,
                requests: Default::default(),
            },
        )
        .await;
        assert!(matches!(
            outcome.status,
            ExtractStatus::Failed(ExtractFailure::Agent(_))
        ));
        assert_eq!(outcome.cursor, ExtractCursor::default());
        assert_eq!(
            std::fs::read_to_string(&index).unwrap(),
            "- [kept](kept.md) — kept\n"
        );
        assert!(
            fixture.root.join("me.md").exists(),
            "file stays a candidate"
        );
    }

    #[test]
    fn tool_allowlist_denies_shell_delete_and_outside_paths() {
        let _fixture = fixture();
        let memory_dir = crate::memdir::paths::get_auto_mem_path();
        let context = ToolUseContext::default();
        let callback = memory_tool_callback(&context, &memory_dir);
        let assistant = crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let decide = |tool: crate::types::tools::Tool, input: serde_json::Value| {
            callback
                .decide(&tool, &input, &context, &assistant, "toolu", None)
                .unwrap()
                .unwrap()
        };
        let inside = memory_dir.join("a.md").display().to_string();
        assert!(matches!(
            decide(
                crate::tools::file_write_tool::file_write_tool_schema(),
                serde_json::json!({"file_path": inside, "content": "x"})
            ),
            PermissionDecision::Allow { .. }
        ));
        assert!(matches!(
            decide(
                crate::tools::file_write_tool::file_write_tool_schema(),
                serde_json::json!({"file_path": "/etc/passwd", "content": "x"})
            ),
            PermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            decide(
                crate::tools::file_read_tool::file_read_tool_schema(),
                serde_json::json!({"files": [
                    {"file_path": inside},
                    {"file_path": "/etc/passwd"}
                ]})
            ),
            PermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            decide(
                crate::tools::bash_tool::bash_tool_schema(),
                serde_json::json!({"command": format!("rm {}", memory_dir.join("a.md").display())})
            ),
            PermissionDecision::Deny { .. }
        ));
    }
}
