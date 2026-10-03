//! Query loop seam.
//! Maps to official `query.ts`: after `processUserInput(...)` returns
//! `shouldQuery=true`, REPL builds query context and consumes `query(...)` as an
//! async generator. This Rust port keeps the same file boundary and naming:
//! it asks a [`QueryDeps`](crate::query::deps::QueryDeps) implementation for
//! source-level query events and maps those events into `RenderableMessage` rows at this
//! seam. The production path now enters model streaming through
//! [`QueryDeps::call_model`](crate::query::deps::QueryDeps::call_model),
//! mirroring the official `deps.callModel` dependency seam; accepted typed
//! events persist through the official-shaped sessionStorage `Project` boundary.

pub mod config;
pub mod deps;
mod event_channel;
pub use event_channel::QueryEventSender;
pub(crate) use event_channel::QueryGeneratorGuard;
use event_channel::QueryPullControl;
pub mod stop_hooks;
pub mod token_budget;
pub mod transitions;

use crate::constants::query_source::QuerySource;
use crate::tool::ToolPermissionContext;
use crate::types::message::Message;
use crate::types::message::{
    Attachment, GroupedToolUseMessage, RenderableMessage, RenderableMessageKind, SystemMessage,
    SystemMessageLevel, TombstoneMessage, ToolUseProgressMessage, ToolUseStatus, UserContent,
};
use crate::types::permissions::{
    PermissionPromptChoice, PermissionPromptResponse, PermissionRequest,
};
use crate::utils::classifier_approvals::ClassifierChecking;
use crate::utils::process_env::JsTruthy;
use std::{sync::Arc, time::Duration};

/// Maps to CC `query.ts` `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT`.
const MAX_OUTPUT_TOKENS_RECOVERY_LIMIT: u32 = 3;

/// Maps to CC `query.ts` max-output recovery user message.
const MAX_OUTPUT_TOKENS_RECOVERY_MESSAGE: &str = "Output token limit hit. Resume directly — no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.";

/// Parameters passed to the query seam.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryParams {
    /// Stable ID for the submitted prompt/command.
    pub turn_id: String,
    /// User-visible input or demo command name that seeded this query.
    pub input: String,
    /// Messages produced by `process_user_input` before query begins.
    /// This remains available for UI projection and for typed-history fallback
    /// when restoring legacy sessions that lack typed model history.
    pub messages: Vec<RenderableMessage>,
    /// Maps to: CC `query.ts` `messages` / `messagesForQuery` typed history.
    /// REPL owns this across turns and passes it into the actor so query core is
    /// not forced to reconstruct model context from `RenderableMessage` rows.
    pub model_messages: Vec<Message>,
    /// Maps to: CC `query.ts` `QueryParams.systemPrompt`; REPL snapshots the
    /// effective prompt once before starting the query actor.
    pub system_prompt: crate::services::api::claude::SystemPrompt,
    /// Maps to: CC `query.ts` `QueryParams.userContext`.
    pub user_context: std::collections::BTreeMap<String, String>,
    /// Maps to: CC `query.ts` `QueryParams.systemContext`.
    pub system_context: std::collections::BTreeMap<String, String>,
    /// Official-style query source discriminator.
    pub query_source: QuerySource,
    /// Maps to: CC `snapshotOutputTokensForTurn(parsedBudget ?? ...)` and
    /// `getCurrentTurnTokenBudget()` for the token-budget continuation gate.
    pub token_budget: Option<i64>,
    /// Maps to: CC `query.ts` `QueryParams.taskBudget` (API-side task_budget,
    /// distinct from the local token-budget auto-continue feature).
    pub task_budget: Option<crate::services::api::claude::TaskBudget>,
    /// Maps to: CC `query.ts` `QueryParams.maxTurns`. Interactive REPL leaves
    /// this `None`; SDK/headless callers may bound recursive tool continuations.
    pub max_turns: Option<u32>,
    /// Maps to: CC `QueryParams.toolUseContext` — the canonical seeded context
    /// carried into the query loop. Permission / mcp / resume / app_store /
    /// content-replacement all live on this seed (Batch 3f).
    pub tool_use_context: crate::tool::ToolUseContext,
}

impl QueryParams {
    /// Ensure the seeded ToolUseContext is ready for the actor.
    /// Fills tools from permission context when empty, and default effort.
    pub fn ensure_tool_use_context_ready(&mut self) {
        if self.tool_use_context.tools.is_empty() {
            self.tool_use_context.tools = self.default_tool_pool();
        }
        if self.tool_use_context.effort_value.is_none() {
            self.tool_use_context.effort_value = crate::utils::effort::get_initial_effort_setting();
        }
    }

    /// Attach the live AppStore and hydrate permission/mcp into the seed.
    /// Maps to: REPL `getToolUseContext` before `query(...)`.
    pub fn attach_app_store(&mut self, store: crate::state::store::AppStore) {
        self.tool_use_context = self.tool_use_context.clone().with_app_store(store);
        if self.tool_use_context.tools.is_empty() {
            self.tool_use_context.tools = self.default_tool_pool();
        }
    }

    /// The pool a seed that carries no tools of its own falls back to.
    ///
    /// Maps to: CC `query.ts:660` `tools: toolUseContext.options.tools`, whose
    /// only producer is `useMergedTools` (`hooks/useMergedTools.ts:30`) →
    /// `assembleToolPool` (`tools.ts:346-368`). CC has no fallback at all — the
    /// pool is always assembled before `query(...)` — so when Rust needs one it
    /// has to produce the same VALUE: built-ins **plus deny-filtered MCP tools**.
    /// `get_tools` alone (`tools.ts:349`) is only the built-in half and silently
    /// drops every MCP tool.
    fn default_tool_pool(&self) -> Vec<crate::types::tools::Tool> {
        crate::tools::assemble_tool_pool(
            &self.tool_use_context.tool_permission_context,
            &self.tool_use_context.mcp_state.tools,
        )
    }

    /// Seed QueryParams from a fully prepared ToolUseContext (CC style).
    pub fn with_tool_use_context_seed(mut self, seed: crate::tool::ToolUseContext) -> Self {
        self.tool_use_context = seed;
        self
    }

    /// Build the loop-local ToolUseContext for this query turn.
    /// Maps to: CC carrying `QueryParams.toolUseContext` into the query loop
    /// (Cometix clones the seed each iteration so messages/tracking stay current).
    ///
    /// Options (`mainLoopModel`, tools, thinking, …) must already live on the
    /// seed — callers write them before [`crate::query::spawn_query`].
    pub fn tool_use_context(
        &self,
        messages: Vec<Message>,
        query_tracking: Option<crate::tool::QueryChainTracking>,
        content_replacement_state: Option<
            crate::utils::tool_result_storage::ContentReplacementState,
        >,
    ) -> crate::tool::ToolUseContext {
        let mut context = self.tool_use_context.clone();
        context.messages = messages;
        context.query_tracking = query_tracking;
        if content_replacement_state.is_some() {
            context.content_replacement_state = content_replacement_state;
        }
        if context.agent_id.is_none() {
            context.agent_id = crate::utils::teammate::get_agent_id();
        }
        context
    }
}

/// A source-level query event scheduled by query deps before UI mapping.
/// This keeps mock deps aligned with official `query.ts`: deps emit assistant
/// text/tool_use/tool_result-like events; the query seam owns conversion to the
/// `RenderableMessage` shape consumed by REPL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQueryEvent {
    pub event: QuerySourceEvent,
    pub delay: Duration,
    /// Display-only classifier checking state that accompanies this event.
    /// It mirrors official classifierApprovals state without starting a
    /// classifier or changing permissions.
    pub classifier_checking: Option<ClassifierChecking>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuerySourceEvent {
    pub uuid: String,
    pub kind: QuerySourceEventKind,
}

/// Live stop-hook progress used by the REPL loading spinner suffix.
/// Maps to CC `REPL.tsx` `stopHookSpinnerSuffix`, sourced from
/// `ProgressMessage<HookProgress>` and resolved hook attachments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopHookProgressEvent {
    Started {
        tool_use_id: String,
        hook_event: String,
        command: String,
        status_message: Option<String>,
        total: usize,
    },
    Completed {
        tool_use_id: String,
        hook_event: String,
    },
    Finished {
        tool_use_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuerySourceEventKind {
    // C3c-3: every variant without a producer is gone — the whole User
    // family, the System family, the Attachment family, `CompactSummary`, and
    // `CollapsedReadSearch` had zero construction sites anywhere in the tree
    // (their producers moved to REPL/process_user_input in batches B/C1, which
    // construct rows and model messages directly). Rewriting dead arms into
    // the new `into_message` seam would be untested conversion code — the
    // `declare 占位` runtime mine; the same reasoning already deleted
    // `UserPlanApproval`/`UserImage`, and `AttachmentMaxTurnsReached` in D3
    // (`emit_max_turns_reached` now sends the whole typed `Message::Attachment`
    // directly). The live vocabulary is the assistant family produced by
    // `assistant_content_to_source_event_kind_*`.
    AssistantText {
        text: String,
        /// Maps to: CC `AssistantMessage.isApiErrorMessage` — set only by
        /// `create_assistant_api_error_message`.
        is_api_error_message: bool,
    },
    AssistantThinking {
        text: String,
        expanded: bool,
    },
    AssistantRedactedThinking,
    AssistantToolUse {
        tool_use_id: Option<String>,
        tool_name: String,
        /// Flattened `ToolUseBlock::input` (CC `ToolUseBlockParam.input`).
        input: Option<serde_json::Value>,
        description: String,
        status: ToolUseStatus,
        progress_messages: Vec<ToolUseProgressMessage>,
    },
    /// Source-level seam for official `tool.isTransparentWrapper()` rows.
    /// Fixture deps can provide already-known progress output/status without
    /// starting a tool registry or live progress stream.
    AssistantTransparentToolUseProgress {
        tool_use_id: Option<String>,
        tool_name: String,
        progress_output: Option<String>,
        progress_status: Option<String>,
    },
    AssistantAdvisor {
        tool_use_id: crate::types::ids::ToolUseId,
        content: crate::types::message::AdvisorResult,
    },

    /// Source summary for grouped tool-use renderer coverage. Carries the
    /// grouped rows whole (CC `types/message.ts:140-144`); all display state
    /// is derived by `GroupedToolUseContent` at render time.
    GroupedToolUse {
        tool_name: String,
        messages: Vec<RenderableMessage>,
        results: Vec<RenderableMessage>,
    },
}

/// Handle for the Rust query actor.
/// Maps to: CC `query.ts` async generator consumer handle in REPL.
#[derive(Clone)]
pub struct QueryHandle {
    pub id: String,
    pub events: Arc<async_channel::Receiver<QueryEvent>>,
    pub commands: Arc<async_channel::Sender<QueryCommand>>,
    /// Cross-thread cancellation handle shared with the actor's
    /// `ToolUseContext.abort_controller` (↔ CC Tool.ts:180). The REPL calls
    /// `abort()` alongside `QueryCommand::Abort` so a tool running inside
    /// `run_tools(...)` — during which the actor cannot poll commands — is
    /// killed cooperatively by the execution loop's abort polling.
    pub abort_controller: crate::tool::AbortController,
    /// A2 pull transport: the consumer explicitly advances after each event.
    pub(crate) resume: Option<Arc<QueryPullControl>>,
}

/// Events yielded by the query actor.
/// Maps to: CC `query.ts` `async function* query(...)` yield stream.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryEvent {
    StreamRequestStart,
    /// `StreamEvent` values yielded during API streaming. REPL currently
    /// ignores these, but keeping them on the actor stream preserves the
    /// official async-generator event boundary and TTFT metadata for future
    /// status/profiling UI.
    Stream(crate::types::message::StreamEvent),
    /// A whole model `Message` yielded by the query seam.
    /// Maps to: CC `query.ts` yielding `Message` values (types/message.ts:120-146).
    /// The interactive REPL pushes it into the single history as a
    /// `HistoryEntry::Message`: both the render rows
    /// (`normalize_messages` projection) and the API history read the same
    /// entry, exactly like CC's one `useState<Message[]>` array.
    Message(Message),
    /// Render-only transcript row that bypasses model history
    /// (`HistoryEntry::Row` at the REPL). CC has no such event: its single
    /// yielded Message serves both purposes. Batch-D3 audited residue — the
    /// "rich fields cannot ride the model structs" class is GONE (attachments,
    /// max-turns, and fallback warnings now travel as [`Self::Message`]); what
    /// remains is each a dual-carrier flow whose halves are not the same value:
    /// - tool-execution `MessageUpdate.message` rows: the row half carries the
    ///   raw `toolUseResult` that only the executing tool can attach; the
    ///   model half (`tool_result`) is deferred for API ordering;
    /// - stop-hook summary/blocking rows: render half (system summary) and
    ///   model half (blocking user messages) are different values by design.
    ///
    /// Compact/snip/microcompact boundaries left this list (task #9): they now
    /// travel as whole [`Self::Message`] yields (CC query.ts:406-407, :528-535,
    /// :884, :1147-1149) and the compact boundary trips the REPL's history
    /// reset (REPL.tsx:3443-3463).
    ///
    /// Streamed per-block assistants also left this list: they converged on
    /// CC's exact shape — a whole single-block `AssistantMessage` with a fresh
    /// uuid per `content_block_stop` travels as [`Self::Message`]
    /// (services/api/claude.ts:2192-2211, appended at REPL.tsx:3496), final
    /// `usage`/`stop_reason` arrive as [`Self::AssistantDelta`], and
    /// streaming-fallback orphans travel as [`Self::Tombstone`] carrying the
    /// whole message (query.ts:713-718).
    Row(RenderableMessage),
    /// Rust transport for CC's post-yield shared-reference mutation of the
    /// last streamed per-block assistant (services/api/claude.ts:2229-2248
    /// "IMPORTANT: Use direct property mutation, not object replacement"):
    /// `message_delta` arrives after the last `content_block_stop`, and CC
    /// writes the final `usage`/`stop_reason` onto the already-yielded message
    /// object; the transcript write queue then serializes it lazily
    /// (sessionStorage.ts:567 `FLUSH_INTERVAL_MS = 100`,
    /// `enqueueWrite`→`drainWriteQueue` stringify at drain time), so the
    /// JSONL captures the mutated values. Rust has no shared reference across
    /// the actor channel, so the mutation is an event keyed by the message
    /// uuid: the REPL applies it to the matching `HistoryEntry::Message` in
    /// place, and the session layer applies it to the deferred assistant
    /// record before its single write.
    AssistantDelta {
        uuid: String,
        stop_reason: Option<crate::types::message::StopReason>,
        usage: Option<crate::types::message::TokenUsage>,
    },
    /// Clears the REPL-owned streaming preview without mutating transcript
    /// history. Maps to CC `utils/messages.ts` clearing `streamingText` when
    /// a stream fallback/tombstone boundary invalidates partial deltas before a
    /// formal assistant message is yielded.
    ClearStreamingPreview,
    /// Maps to: CC `query.ts` yielding `{ type: 'tombstone', message }` for
    /// partial assistant rows orphaned by streaming-to-non-streaming fallback.
    Tombstone(TombstoneMessage),
    /// History-only half of the dual carrier (`HistoryEntry::ModelOnly` at
    /// the REPL): consumed like [`Self::Message`] minus the row projection.
    /// Batch-D3 audited residue, paired one-to-one with the [`Self::Row`]
    /// flows listed there (deferred tool results, stop-hook blocking
    /// messages). The post-compact history replay left this list (task #9),
    /// and the streamed-assistant flow left it with the per-block
    /// [`Self::Message`] + [`Self::AssistantDelta`] convergence — the only
    /// streamed remnant is the abort/API-error partial built from
    /// never-completed block buffers, which stays history-only because its
    /// render half is the REPL-owned streaming preview (CC has no such
    /// history entry at all; claude.ts drops un-stopped buffers). Collapses
    /// per-flow as each blocker lands, not as a carrier change.
    ModelMessage(Message),
    /// Typed API-error metadata retained alongside the renderable error row.
    /// Forked consumers use this instead of reparsing display text.
    ApiError(crate::types::message::SystemApiErrorMessage),
    /// Rust actor seam for CC `ToolUseContext.contentReplacementState` mutation
    /// inside `applyToolResultBudget(...)`; REPL stores this for the next
    /// top-level query call.
    ContentReplacementStateUpdate(crate::utils::tool_result_storage::ContentReplacementState),
    PermissionRequest(PermissionRequest),
    /// Live tool execution progress crossing the actor boundary.
    /// Maps to: CC `runToolUse` `onToolProgress` forwarding
    /// (toolExecution.ts:1216-1221) surfaced to the REPL for the in-flight
    /// tool-use row.
    ToolProgress(crate::types::tools::ToolProgress),
    /// Maps to: CC `REPL.tsx:1255` `useState<StreamingToolUse[]>`, fed by the
    /// stream handler as `tool_use` blocks arrive and cleared at the turn
    /// boundary (`REPL.tsx:2118,3910`).
    ///
    /// Only the id set is consumed downstream (`Messages.tsx:690` reduces the
    /// array to `new Set(streamingToolUses.map(_ => _.contentBlock.id))`), so
    /// this carries ids rather than the whole `StreamingToolUse` record.
    ///
    /// The window it covers is stated at `MessageRow.tsx:85-88`: a tool_use
    /// block reaches the transcript BEFORE its id enters
    /// `inProgressToolUseIDs`, and without this set a collapsed read group
    /// briefly finalizes mid-stream.
    /// The turn-boundary clear has no event: CC's REPL calls
    /// `setStreamingToolUses([])` on itself (`REPL.tsx:2118,3910`), so Cometix
    /// clears the set where it handles `Terminal`, next to `setStreamingText(null)`.
    StreamingToolUseStarted {
        tool_use_id: String,
    },
    /// Transport for [`crate::tool::SetInProgressToolUseIds`] crossing the actor
    /// boundary, the same role [`Self::ToolProgress`] plays for
    /// [`crate::tool::ToolProgressSink`]. The CC-mapped declaration is the
    /// `ToolUseContext` field in `tool.rs`; this variant only carries one
    /// functional update of the REPL-owned set (`REPL.tsx:1897`) to its owner.
    SetInProgressToolUse {
        tool_use_id: String,
        in_progress: bool,
    },
    /// Live Stop/SubagentStop hook progress used to derive the spinner suffix
    /// while the actor is still executing turn-end hooks.
    StopHookProgress(StopHookProgressEvent),
    /// Maps to CC `ToolUseContext.setToolJSX` mounting
    /// `utils/swarm/It2SetupPrompt.tsx` from `spawnMultiAgent.ts` and awaiting
    /// its `onDone` response while the prompt input is hidden.
    It2SetupPromptRequest(crate::utils::swarm::it2_setup_prompt::It2SetupPromptRequest),
    /// Maps to CC `query.ts` yielding a pending `ToolUseSummaryMessage` from
    /// the previous completed tool batch after the next model stream resolves.
    ToolUseSummary(crate::types::message::ToolUseSummaryMessage),
    /// Maps to CC `query.ts` consumption of `runTools(...)` `MessageUpdate.newContext`
    /// crossing the Rust actor boundary. Official REPL/App state is mutated
    /// in-process by permission/tool handlers; Cometix sends the updated
    /// `ToolPermissionContext` back to REPL so future top-level turns inherit
    /// hook grants, plan-mode transitions, and `AlwaysAllow` session rules.
    PermissionContextUpdate(ToolPermissionContext),
    /// Full out-of-band tool context for non-interactive multi-turn owners
    /// (cwd/read cache/structured output). Interactive REPL may ignore it.
    ToolContextUpdate(std::sync::Arc<crate::tool::ToolUseContext>),
    /// Validated `StructuredOutput` payload for print/SDK result projection.
    StructuredOutput(serde_json::Value),
    Terminal(transitions::Terminal),
}

/// Commands sent back into the query actor when it pauses on tool permission.
/// Maps to: CC interactive permission handler continuation callbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryCommand {
    PermissionDecision {
        tool_use_id: String,
        choice: PermissionPromptChoice,
    },
    /// Maps to CC `ToolUseConfirm.onAllow(updatedInput, ...)` continuation
    /// payload. The legacy `PermissionDecision` variant is retained for tests
    /// and non-interactive choices that do not update tool input.
    PermissionResponse {
        tool_use_id: String,
        response: PermissionPromptResponse,
    },
    Abort,
}

fn create_query_abort_controller(
    seed: &crate::tool::ToolUseContext,
) -> crate::tool::AbortController {
    crate::tool::AbortController::child_of(seed.abort_controller.clone())
}

pub fn spawn_query<D: crate::query::deps::QueryDeps>(params: QueryParams, deps: D) -> QueryHandle {
    spawn_query_with_transport(params, deps, None)
}

/// A2 transport for a for-await consumer that must inspect each yield before
/// allowing the producer to advance. The caller closes/resumes `QueryHandle.resume`.
pub(crate) fn spawn_query_generator<D: crate::query::deps::QueryDeps>(
    params: QueryParams,
    deps: D,
) -> QueryHandle {
    spawn_query_with_transport(params, deps, Some(Arc::new(QueryPullControl::new())))
}

fn spawn_query_with_transport<D>(
    mut params: QueryParams,
    deps: D,
    resume: Option<Arc<QueryPullControl>>,
) -> QueryHandle
where
    D: crate::query::deps::QueryDeps,
{
    // Maps to: CC `query({ toolUseContext })` — options already live on the
    // seeded ToolUseContext.
    params.ensure_tool_use_context_ready();
    let id = params.turn_id.clone();
    let (event_tx, event_rx) = async_channel::unbounded();
    let event_tx = match &resume {
        Some(resume) => QueryEventSender::pull(event_tx, resume.clone()),
        None => QueryEventSender::from(event_tx),
    };
    let actor_resume = resume.clone();
    let (command_tx, command_rx) = async_channel::unbounded();
    // Preserve the caller-owned context cancellation chain while retaining a
    // query-local handle. This mirrors CC query/fork contexts: aborting the
    // seed propagates into the actor, while aborting this child remains local.
    let abort_controller = create_query_abort_controller(&params.tool_use_context);
    let actor_abort = abort_controller.clone();
    // Maps to CC `utils/teammateContext.ts` AsyncLocalStorage semantics: an
    // ALS store entered by `runWithTeammateContext` propagates automatically
    // into every async continuation, including the query the teammate's
    // `runAgent` starts. Rust's task-local does not cross this thread/runtime
    // boundary, so capture the caller's teammate context here and re-enter the
    // scope around the actor future. `None` (non-teammate callers) adds
    // nothing.
    let teammate_context = crate::utils::teammate_context::get_teammate_context();

    // This runtime is PRIVATE to one query and dies with it. CC has no
    // counterpart — `query()` there is an async generator on the single process
    // event loop — so nothing that must outlive the turn may be spawned onto
    // it. Detached work goes through `utils::process_runtime` instead; the
    // teardown is logged because "which runtime did this task land on" is the
    // failure mode that silently strands background agents.
    let runtime_trace_id = id.clone();
    std::thread::spawn(move || {
        let terminal = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => {
                let actor = run_query_actor_with_abort(
                    params,
                    deps,
                    event_tx.clone(),
                    command_rx,
                    actor_abort,
                );
                // Closing the pull receiver corresponds to AsyncGenerator.return:
                // drop the suspended producer, rather than letting it execute past yield.
                let actor = async move {
                    if let Some(resume) = actor_resume {
                        tokio::select! {
                            biased;
                            _ = resume.closed() => transitions::Terminal::new("aborted_streaming"),
                            terminal = actor => terminal,
                        }
                    } else {
                        actor.await
                    }
                };
                let terminal = match teammate_context {
                    Some(context) => runtime.block_on(
                        crate::utils::teammate_context::run_with_teammate_context(context, actor),
                    ),
                    None => runtime.block_on(actor),
                };
                // Bounded teardown, derived from the A2 per-query-runtime
                // choice (PORTING.md § "Node-async → tokio" tiers): CC's
                // generator return is instantaneous, so ANY wait here is
                // Rust-only. A plain `drop(runtime)` waits UNBOUNDED on
                // in-flight `spawn_blocking` work — one wedged blocking task
                // and Terminal below never sends, i.e. "UI stuck loading
                // forever". `shutdown_timeout` cancels the runtime's async
                // tasks immediately (same as drop) and caps the blocking-pool
                // wait at a short grace, after which the blocking thread is
                // detached to finish in the background. (Native-Rust
                // precedent: grok-build's run_and_shutdown, same rationale.)
                runtime.shutdown_timeout(std::time::Duration::from_secs(2));
                crate::utils::debug::log_for_debugging(&format!(
                    "query[{runtime_trace_id}]: actor resolved, per-query runtime shut down"
                ));
                terminal
            }
            Err(error) => {
                let mut terminal = transitions::Terminal::new("model_error");
                terminal.exception = Some(error.to_string());
                terminal
            }
        };
        let _ = event_tx.try_send(QueryEvent::Terminal(terminal));
    });

    QueryHandle {
        id,
        events: Arc::new(event_rx),
        commands: Arc::new(command_tx),
        abort_controller,
        resume,
    }
}

async fn run_query_actor<D>(
    params: QueryParams,
    deps: D,
    event_tx: impl Into<QueryEventSender>,
    command_rx: async_channel::Receiver<QueryCommand>,
) -> transitions::Terminal
where
    D: crate::query::deps::QueryDeps,
{
    run_query_actor_with_abort(
        params,
        deps,
        event_tx.into(),
        command_rx,
        crate::tool::AbortController::default(),
    )
    .await
}

async fn run_query_actor_with_abort<D>(
    params: QueryParams,
    deps: D,
    event_tx: impl Into<QueryEventSender>,
    command_rx: async_channel::Receiver<QueryCommand>,
    abort_controller: crate::tool::AbortController,
) -> transitions::Terminal
where
    D: crate::query::deps::QueryDeps,
{
    let event_tx = event_tx.into();
    // Maps to CC REPL `diagnosticTracker.handleQueryStart(freshClients)`:
    // reset per user query while connected-client lookup remains lazy in the
    // Rust diagnostic service. Subagent actors do not own the singleton.
    // A2: a direct query() generator does not enter REPL.handleQueryStart.
    // In particular, an agent hook must retain its parent's diagnostic baseline.
    if !event_tx.is_pull() && !params.query_source.is_agent() {
        crate::services::diagnostic_tracking::reset();
    }

    // Keep the very large query-loop state machine off the actor thread's
    // fixed stack. The production actor and direct test runtimes both use a
    // current-thread executor; boxing here prevents nested poll frames from
    // exhausting their default stack as compact/tool state grows.
    let query_loop = Box::pin(query_loop(
        params.clone(),
        deps,
        event_tx.clone(),
        command_rx,
        abort_controller.clone(),
    ));
    match query_loop.await {
        Ok(terminal) => terminal,
        Err(error) => {
            let error_text = error.to_string();
            if event_tx.is_pull() {
                let mut terminal = transitions::Terminal::new("model_error");
                terminal.exception = Some(error_text);
                return terminal;
            }
            if abort_controller.is_aborted() || crate::utils::errors::is_abort_error(&error) {
                return transitions::Terminal::new("aborted_streaming");
            }
            let system_error = crate::types::message::SystemApiErrorMessage {
                content: error_text.clone(),
                api_error: error_text.clone(),
                error: error_text.clone(),
                error_details: Some(error_text.clone()),
            };
            schedule_stop_failure_hooks_for_api_error(system_error.clone(), None);
            let _ = event_tx.send(QueryEvent::ApiError(system_error)).await;
            let _ = send_assistant_api_error_message(&event_tx, &params.turn_id, &error_text).await;
            let mut terminal = transitions::Terminal::new("model_error");
            terminal.exception = Some(error_text);
            terminal
        }
    }
}

/// Maps to CC query.ts:955-1002 catch body. Rust's stream-open and stream-item
/// error carriers enter this same catch; these are normal generator returns,
/// unlike errors outside the model sampling block that escape to the caller.
async fn yield_query_model_error(
    event_tx: &QueryEventSender,
    params: &QueryParams,
    error: &anyhow::Error,
    assistant_messages: &[crate::types::message::AssistantMessage],
) -> transitions::Terminal {
    let error_text = error.to_string();
    for missing in crate::services::tools::tool_execution::yield_missing_tool_result_blocks(
        assistant_messages,
        &error_text,
    ) {
        if event_tx
            .send(QueryEvent::Message(Message::User(missing.tool_result)))
            .await
            .is_err()
        {
            return transitions::Terminal::new("aborted_streaming");
        }
    }
    let system_error = crate::types::message::SystemApiErrorMessage {
        content: error_text.clone(),
        api_error: error_text.clone(),
        error: error_text.clone(),
        error_details: Some(error_text.clone()),
    };
    let _ = event_tx.send(QueryEvent::ApiError(system_error)).await;
    let _ = send_assistant_api_error_message(event_tx, &params.turn_id, &error_text).await;
    transitions::Terminal::new("model_error")
}

fn run_query_once_blocking<D>(
    params: QueryParams,
    deps: D,
) -> anyhow::Result<Vec<PendingQueryEvent>>
where
    D: crate::query::deps::QueryDeps,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(run_query_once(params, deps))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("query worker thread panicked"))?
}

async fn maybe_make_file_history_snapshot(params: &QueryParams) {
    // Maps to CC `handlePromptSubmit` / REPL `fileHistoryMakeSnapshot(...)`.
    // The source fire-and-forgets this before starting the query; awaiting at
    // the query boundary guarantees the initial snapshot exists before a fast
    // Write tool result can attempt to attach its pre-edit backup.
    if matches!(params.query_source, QuerySource::Prompt)
        && crate::utils::env_utils::is_cometix_write_enabled()
        && crate::utils::file_history::file_history_enabled()
    {
        if let Some(store) = params.tool_use_context.app_store.store.as_ref() {
            crate::utils::file_history::file_history_make_snapshot(store, &params.turn_id).await;
        }
    }
}

async fn run_query_once<D>(params: QueryParams, deps: D) -> anyhow::Result<Vec<PendingQueryEvent>>
where
    D: crate::query::deps::QueryDeps,
{
    maybe_make_file_history_snapshot(&params).await;
    let initial_messages = if params.model_messages.is_empty() {
        legacy_model_messages_from_params(&params)
    } else {
        params.model_messages.clone()
    };
    let initial_messages =
        crate::utils::messages::get_messages_after_compact_boundary(&initial_messages);
    // Maps to: CC `query.ts:252-269`: all three context parts are required
    // immutable inputs. An empty collection is a resolved value, not a request
    // to re-read the main session's prompt or context.
    let system_context = params.system_context.clone();
    let system_prompt = params.system_prompt.clone();
    let user_context = params.user_context.clone();
    let request = build_call_model_request(
        prepare_call_model_messages(initial_messages, &user_context, false),
        params.tool_use_context.tool_permission_context.clone(),
        &params.query_source,
        &system_prompt,
        &system_context,
        &params.tool_use_context.mcp_state,
        &params.tool_use_context,
    );
    let mut stream = crate::query::deps::QueryDeps::call_model(&deps, request).await?;
    let mut items = Vec::new();
    while let Some(item) = stream.recv().await {
        items.push(item);
    }
    Ok(query_model_stream_items_to_pending_events(
        &params.turn_id,
        std::mem::take(&mut items),
    ))
}

/// Maps to: CC `query.ts` `async function* queryLoop(...)`.
async fn query_loop<D>(
    mut params: QueryParams,
    deps: D,
    event_tx: QueryEventSender,
    command_rx: async_channel::Receiver<QueryCommand>,
    abort_controller: crate::tool::AbortController,
) -> anyhow::Result<transitions::Terminal>
where
    D: crate::query::deps::QueryDeps,
{
    maybe_make_file_history_snapshot(&params).await;

    let mut model_messages = if params.model_messages.is_empty() {
        // Legacy-parameter shim: the REPL always passes accumulated typed
        // history in `QueryParams.model_messages` (C3c-3), so this only
        // serves rows-only parameter sets (fixture tests, pre-typed callers).
        legacy_model_messages_from_params(&params)
    } else {
        params.model_messages.clone()
    };
    // Maps to CC `processAtMentionedFiles(...)`: explicit @file/@dir
    // attachments are resolved before the initial model request, not after the
    // first tool batch. Read state updates flow back into the loop context.
    if matches!(params.query_source, QuerySource::Prompt) {
        // Prompt slash commands (including inline skills) run attachment
        // extraction over their expanded prompt body, not the visible `/name`
        // command. Ordinary prompts continue to use the submitted input.
        let attachment_input = model_messages
            .iter()
            .rev()
            .find_map(|message| match message {
                // The old `Prompt { is_meta: true }` read — the expanded
                // slash-command prompt body travels as a `MetaText` block.
                Message::User(message) if params.input.trim_start().starts_with('/') => {
                    // CC processSlashCommand.tsx:1223-1226 reads prompt text
                    // even when pasted images precede it in the meta envelope.
                    let text = message
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            UserContent::MetaText(text) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    (!text.is_empty()).then(|| text.join(" "))
                }
                _ => None,
            })
            .unwrap_or_else(|| params.input.clone());
        for attachment in crate::utils::attachments::process_user_input_attachments(
            &attachment_input,
            &mut params.tool_use_context,
        )
        .await
        {
            // Batch D3: one whole `Message` per attachment (CC query.ts
            // yields the attachment message itself); the REPL's normalize
            // projection derives the row from the typed model value.
            if event_tx
                .send(QueryEvent::Message(attachment.model_message.clone()))
                .await
                .is_err()
            {
                return Ok(transitions::Terminal::new("aborted_streaming"));
            }
            model_messages.push(attachment.model_message);
        }
        // CC waits for user-input attachments first, then collects the shared
        // attachment phase. This is where changed files, nested memory,
        // dynamic skills, and the skill listing enter the *initial* request.
        let initial_thread_attachments = crate::utils::attachments::get_attachments(
            crate::utils::attachments::GetAttachmentMessagesParams {
                tool_use_context: &mut params.tool_use_context,
                messages: &model_messages,
                query_source: &params.query_source,
                // CC turn-0 callers pass `[]`: "queuedCommands - handled by
                // query.ts for mid-turn attachments" (processUserInput.ts:508,
                // processSlashCommand.tsx:1230).
                queued_commands: Vec::new(),
            },
        )
        .await;
        for attachment in initial_thread_attachments {
            // Batch D3: one whole `Message` (CC query.ts:1588 `yield
            // attachment`); the row is the REPL-side normalize projection.
            if event_tx
                .send(QueryEvent::Message(attachment.model_message.clone()))
                .await
                .is_err()
            {
                return Ok(transitions::Terminal::new("aborted_streaming"));
            }
            model_messages.push(attachment.model_message);
        }
        // CC mutates one shared ToolUseContext in place. The Rust actor always
        // publishes its current context after this phase so no Read field is
        // diffed or reconstructed merely to decide whether to carry identity.
        if event_tx
            .send(QueryEvent::ToolContextUpdate(std::sync::Arc::new(
                params.tool_use_context.clone(),
            )))
            .await
            .is_err()
        {
            return Ok(transitions::Terminal::new("aborted_streaming"));
        }
    }
    let mut permission_context = params
        .tool_use_context
        .get_app_state()
        .map(|state| (*state.tool_permission_context).clone())
        .unwrap_or_else(|| params.tool_use_context.tool_permission_context.clone());
    let mut budget_tracker = token_budget::create_budget_tracker();
    let mut turn_output_tokens = 0i64;
    let mut max_output_tokens_recovery_count = 0u32;
    // Maps to CC `query.ts` `State.hasAttemptedReactiveCompact`: prevents
    // prompt-too-long/media recovery spirals after a reactive compact retry.
    let mut has_attempted_reactive_compact = false;
    // Maps to CC `query.ts` `State.turnCount`: starts at 1 and increments only
    // when a tool-result continuation would recurse into the next model turn.
    let mut turn_count = 1u32;
    // Maps to CC `query.ts` loop-local `taskBudgetRemaining`: once a compact
    // summarizes away prior history, subsequent API calls send the server the
    // remaining task-budget tokens based on the pre-compact final context.
    let mut task_budget_remaining: Option<u64> = None;
    // Maps to CC `ToolUseContext.contentReplacementState`: `None` mirrors the
    // official feature-gated `undefined` state and skips aggregate tool-result
    // budgeting; `Some` keeps decisions stable across this query loop so prompt
    // cache prefixes remain byte-identical on later continuations.
    let mut content_replacement_state = params.tool_use_context.content_replacement_state.clone();
    let mut auto_compact_tracking: Option<
        crate::services::compact::auto_compact::AutoCompactTrackingState,
    > = None;
    // Maps to CC `query.ts` `const config = buildQueryConfig()` once at
    // query entry. The snapshot is threaded into stop-hook context now and can
    // drive future gate-specific branches without re-reading mutable env state.
    let query_config = config::build_query_config();
    let mut stop_hook_active: Option<bool> = None;
    // ForkedAgent passes CC `maxOutputTokensOverride` through its isolated
    // ToolUseContext. Main REPL contexts leave this unset.
    let mut max_output_tokens_override: Option<u32> =
        params.tool_use_context.max_output_tokens_override;
    let mut current_model_override: Option<String> = None;
    let mut model_fallback_retry_messages: Option<Vec<Message>> = None;
    let query_chain_id = deps.uuid();
    let mut next_query_depth = 0u32;
    let mut model_fallback_retry_tracking: Option<
        crate::services::api::claude::QueryChainTracking,
    > = None;
    // Maps to CC `query.ts` `pendingToolUseSummary`: the previous tool batch
    // starts a non-critical Haiku summary task; the next loop iteration yields
    // it after model streaming so the main request is not delayed.
    let mut pending_tool_use_summary: Option<
        tokio::task::JoinHandle<Option<crate::types::message::ToolUseSummaryMessage>>,
    > = None;
    // Maps to: CC `query.ts:252-269`: caller-resolved context is invariant
    // across query iterations, including deliberately empty Agent/fork maps.
    // QueryConfig remains runtime state and never enters the model prompt.
    let system_prompt = params.system_prompt.clone();
    let user_context = params.user_context.clone();
    let system_context = params.system_context.clone();
    let mut resume_restore_stores = params.tool_use_context.resume_restore_stores.clone();

    'query_loop: loop {
        let retrying_model_fallback = model_fallback_retry_messages.is_some();
        // Maps to CC `query.ts` `queryTracking`: every non-fallback outer
        // query iteration gets one chain id and a monotonically increasing
        // depth. Model fallback retries reuse the current iteration tracking
        // because upstream retries inside the API loop, not as a new turn.
        let query_tracking = if retrying_model_fallback {
            model_fallback_retry_tracking.take().unwrap_or_else(|| {
                crate::services::api::claude::QueryChainTracking {
                    chain_id: query_chain_id.clone(),
                    depth: next_query_depth.saturating_sub(1),
                }
            })
        } else {
            let tracking = crate::services::api::claude::QueryChainTracking {
                chain_id: query_chain_id.clone(),
                depth: next_query_depth,
            };
            next_query_depth = next_query_depth.saturating_add(1);
            tracking
        };
        // Maps to CC `query.ts`: yield `{ type: 'stream_request_start' }`
        // before compact/context preparation so the REPL can show loading while
        // all pre-call work hides under the model request spinner. Model
        // fallback retries stay inside the official API loop, so they reuse the
        // existing request-start event instead of emitting a second spinner
        // edge.
        if !retrying_model_fallback && event_tx.send(QueryEvent::StreamRequestStart).await.is_err()
        {
            return Ok(transitions::Terminal::new("aborted_streaming"));
        }
        let mut content_replacement_state_changed = false;
        let mut messages_for_query = if let Some(retry_messages) =
            model_fallback_retry_messages.take()
        {
            retry_messages
        } else {
            let previous_content_replacement_state = content_replacement_state.clone();
            let messages_after_compact_boundary =
                crate::utils::messages::get_messages_after_compact_boundary(&model_messages);
            let messages = if let Some(state) = content_replacement_state.as_mut() {
                // Maps to CC `query.ts:379-393`: derive `skipToolNames` from
                // the live Tool definitions whose result budget is Infinity.
                let skip_tool_names = crate::tools::get_tools(&permission_context)
                    .into_iter()
                    .filter(|tool| {
                        crate::services::tools::tool_execution::find_tool_call(&tool.name)
                            .is_some_and(|call| {
                                call.max_result_size_chars()
                                    == crate::tool::UNBOUNDED_MAX_RESULT_SIZE_CHARS
                            })
                    })
                    .map(|tool| tool.name)
                    .collect::<std::collections::HashSet<_>>();
                let budget_result = crate::utils::tool_result_storage::apply_tool_result_budget_with_state_and_skip_tool_names_result(
                    messages_after_compact_boundary,
                    state,
                    &skip_tool_names,
                );
                // Maps to CC `query.ts` optional `recordContentReplacement(...)`
                // callback. Query owns the official repl/agent persistence gate;
                // accepted records enter the sessionStorage `Project` queue at
                // this control-flow point.
                if should_persist_content_replacements(&params.query_source)
                    && !budget_result.newly_replaced.is_empty()
                {
                    let _ = crate::utils::session_storage::record_content_replacement(
                        &budget_result.newly_replaced,
                        None,
                    );
                }
                budget_result.messages
            } else {
                messages_after_compact_boundary
            };
            content_replacement_state_changed =
                content_replacement_state != previous_content_replacement_state;
            messages
        };
        let mut pending_cache_edits: Option<
            crate::services::compact::micro_compact::PendingCacheEdits,
        > = None;

        if content_replacement_state_changed {
            if let Some(state) = content_replacement_state.clone() {
                if event_tx
                    .send(QueryEvent::ContentReplacementStateUpdate(state))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }
        }

        if !retrying_model_fallback {
            // Maps to: CC `query.ts` `applyToolResultBudget(...)` before
            // microcompact. Session writes remain disabled, so oversized aggregate
            // tool_result batches are replaced in-memory with the official cleared
            // content marker instead of persisted previews.

            // Maps to: CC `query.ts` `snipCompactIfNeeded(...)` before
            // microcompact. In this source snapshot both `snipCompact.ts` and
            // `tools/SnipTool/prompt.ts` are generated empty stubs, so Rust keeps a
            // safe no-op at the official control-flow point and only threads the
            // official `tokensFreed` value into autocompact.
            let snip_result =
                crate::services::compact::snip_compact::snip_compact_if_needed(messages_for_query);
            // CC query.ts:406-407 `yield snipResult.boundaryMessage` — a whole
            // Message. Snip boundaries do not trip the REPL's compact reset
            // (isCompactBoundaryMessage checks subtype 'compact_boundary'
            // only, utils/messages.ts:4608-4612), so they append like any
            // other yielded message (REPL.tsx:3496).
            for boundary_message in snip_result.boundary_messages {
                if event_tx
                    .send(QueryEvent::Message(boundary_message))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }
            let snip_tokens_freed = snip_result.tokens_freed;
            messages_for_query = snip_result.messages;

            // Maps to: CC `query.ts` `deps.microcompact(messagesForQuery,
            // toolUseContext, querySource)` before `deps.autocompact(...)`.
            // Microcompact remains owned by `services/compact/*`; the production
            // path preserves the official disabled default and supports the
            // time-based content-clearing path when explicitly enabled.
            let microcompact_context = loop_local_tool_use_context(
                &params,
                &permission_context,
                messages_for_query.clone(),
                Some(query_tracking.clone()),
                &resume_restore_stores,
                content_replacement_state.clone(),
                &abort_controller,
            );
            let microcompact_result = deps.microcompact(
                messages_for_query,
                &microcompact_context,
                &params.query_source,
            );
            // CC yields eager microcompact boundaries as whole Messages (the
            // deferred cached-MC boundary is query.ts:884). Not a compact
            // reset trigger — subtype is 'microcompact_boundary'.
            for boundary_message in microcompact_result.boundary_messages {
                if event_tx
                    .send(QueryEvent::Message(boundary_message))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }
            pending_cache_edits = microcompact_result
                .compaction_info
                .and_then(|info| info.pending_cache_edits);
            messages_for_query = microcompact_result.messages;

            // Maps to: CC `query.ts` `contextCollapse.applyCollapsesIfNeeded(...)`
            // after microcompact and before autocompact. The current service is a
            // store-backed no-op seam until context collapse operations are ported.
            let collapse_context = loop_local_tool_use_context(
                &params,
                &permission_context,
                messages_for_query.clone(),
                Some(query_tracking.clone()),
                &resume_restore_stores,
                content_replacement_state.clone(),
                &abort_controller,
            );
            let collapse_result = crate::services::context_collapse::apply_collapses_if_needed(
                messages_for_query,
                &collapse_context,
                &params.query_source,
            );
            messages_for_query = collapse_result.messages;

            // Maps to: CC `query.ts` `deps.autocompact(...)` and
            // `buildPostCompactMessages(compactionResult)` before the model call.
            let autocompact_context = loop_local_tool_use_context(
                &params,
                &permission_context,
                messages_for_query.clone(),
                Some(query_tracking.clone()),
                &resume_restore_stores,
                content_replacement_state.clone(),
                &abort_controller,
            );
            let autocompact_cache_safe_params =
                crate::services::compact::auto_compact::AutoCompactCacheSafeParams {
                    system_prompt: system_prompt.clone(),
                    user_context: user_context.clone(),
                    system_context: system_context.clone(),
                    fork_context_messages: messages_for_query.clone(),
                };
            let pre_autocompact_final_context_tokens = params.task_budget.as_ref().map(|_| {
                crate::utils::tokens::final_context_tokens_from_last_response(&messages_for_query)
            });
            let mut autocompact_result = deps
                .autocompact(
                    messages_for_query,
                    &autocompact_context,
                    autocompact_cache_safe_params,
                    &params.query_source,
                    auto_compact_tracking.clone(),
                    snip_tokens_freed,
                )
                .await;
            if autocompact_result.rebuilt_read_file_state.take().is_some() {
                // `compactConversation` cleared and rebuilt this authoritative
                // handle in place. The result Vec is an actor/durable snapshot,
                // never completion authority for cache or trigger replay.
                params.tool_use_context.loaded_nested_memory_paths.clear();
                let updated_context = params.tool_use_context.clone();
                if event_tx
                    .send(QueryEvent::ToolContextUpdate(std::sync::Arc::new(
                        updated_context,
                    )))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }
            let autocompact_compacted = autocompact_result.compacted;
            let autocompact_consecutive_failures = autocompact_result.consecutive_failures;
            messages_for_query = autocompact_result.messages;
            if autocompact_compacted {
                if let (Some(task_budget), Some(pre_compact_context)) = (
                    params.task_budget.as_ref(),
                    pre_autocompact_final_context_tokens,
                ) {
                    // Maps to CC `query.ts` task_budget carryover on compact:
                    // after summary replacement the server no longer sees the
                    // pre-compact final window, so subsequent requests pass a
                    // `remaining` value decremented by that window.
                    let base_remaining = task_budget_remaining.unwrap_or(task_budget.total);
                    task_budget_remaining =
                        Some(base_remaining.saturating_sub(pre_compact_context));
                }
                // Maps to CC `tracking = { compacted: true, turnId: deps.uuid(),
                // turnCounter: 0, consecutiveFailures: 0 }` after a successful
                // autocompact. The current model request uses the post-compact
                // messages and future iterations thread this tracking state into
                // `deps.autocompact(...)`.
                auto_compact_tracking = Some(
                    crate::services::compact::auto_compact::AutoCompactTrackingState {
                        compacted: true,
                        turn_counter: 0,
                        turn_id: deps.uuid(),
                        consecutive_failures: Some(autocompact_consecutive_failures.unwrap_or(0)),
                    },
                );
                // CC query.ts:528-535: `for (const message of
                // postCompactMessages) { yield message }` — every member of
                // the post-compact block (boundary first) is yielded as a
                // whole Message. The REPL's compact-boundary branch resets its
                // single history to `[boundary]` (REPL.tsx:3458-3459) and the
                // following members append (REPL.tsx:3496), so render rows and
                // API history converge on the post-compact block with no
                // separate boundary Row or ModelOnly replay.
                for compacted_message in messages_for_query.iter().cloned() {
                    if event_tx
                        .send(QueryEvent::Message(compacted_message))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                }
                // CC comments this as continuing the current query call with
                // post-compact messages, not as a recursive loop retry.
                let _compact_continue = transitions::Continue::new();
            } else if let Some(consecutive_failures) = autocompact_consecutive_failures {
                // Maps to CC autocompact failure propagation for the circuit
                // breaker: preserve existing compact tracking but update the
                // consecutive failure count for the next loop iteration.
                let mut tracking = auto_compact_tracking.unwrap_or(
                    crate::services::compact::auto_compact::AutoCompactTrackingState {
                        compacted: false,
                        turn_counter: 0,
                        turn_id: String::new(),
                        consecutive_failures: None,
                    },
                );
                tracking.consecutive_failures = Some(consecutive_failures);
                auto_compact_tracking = Some(tracking);
            }

            // Maps to CC `query.ts` hard blocking-limit preempt after
            // autocompact and before `deps.callModel(...)`. When the prepared
            // context still exceeds the manual-compact safety margin, return a
            // terminal API-error row instead of making a doomed model request.
            if should_preempt_context_blocking_limit(&params.query_source, autocompact_compacted) {
                let model_for_token_warning =
                    crate::utils::model::model::get_main_loop_model();
                let token_usage =
                    crate::utils::tokens::token_count_with_estimation(&messages_for_query)
                        .saturating_sub(snip_tokens_freed);
                let warning_state =
                    crate::services::compact::auto_compact::calculate_token_warning_state(
                        token_usage,
                        &model_for_token_warning,
                    );
                if warning_state.is_at_blocking_limit {
                    if send_assistant_api_error_message(
                        &event_tx,
                        &params.turn_id,
                        crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE,
                    )
                    .await
                    .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    return Ok(transitions::Terminal::new("blocking_limit"));
                }
            }
        }

        // CC continue sites assign `messages = [...messagesForQuery, ...]`.
        // The current request uses the prepared context; continuation appends
        // assistant/tool_result messages below before the next loop iteration.
        // Prepend user context immediately before `deps.callModel(...)`. The
        // current REPL path has no user-context provider yet, so this is
        // normally a no-op in production. The loop also owns official
        // `FallbackTriggeredError` handling: switch to the fallback model,
        // strip model-bound thinking signatures for ant-internal retries, and
        // rerun the entire request without leaking partial tool results.
        let mut fallback_retry_used = current_model_override.is_some();
        let mut stream = loop {
            let messages_for_model =
                prepare_call_model_messages(messages_for_query.clone(), &user_context, false);
            let cached_mc_new_user_message_index = pending_cache_edits.as_ref().and_then(|_| {
                messages_for_model
                    .iter()
                    .rposition(|message| matches!(message, Message::User(_)))
            });
            let cached_mc_pinned_edits =
                crate::services::compact::micro_compact::get_pinned_cache_edits();
            let mut request = build_call_model_request(
                messages_for_model,
                permission_context.clone(),
                &params.query_source,
                &system_prompt,
                &system_context,
                &params.tool_use_context.mcp_state,
                &params.tool_use_context,
            );
            // Maps to CC `callModel({ signal: toolUseContext.abortController.signal })`.
            // Must be the shared QueryHandle controller so Esc cancels HTTP/retry.
            request.options.abort_signal = Some(abort_controller.signal());
            request.options.query_tracking = Some(query_tracking.clone());
            request.options.max_output_tokens_override = max_output_tokens_override;
            if let Some(task_budget) = params.task_budget.as_ref() {
                request.options.task_budget = Some(crate::services::api::claude::TaskBudget {
                    total: task_budget.total,
                    remaining: task_budget_remaining.or(task_budget.remaining),
                });
            }
            if let Some(pending_cache_edits) = pending_cache_edits.as_ref() {
                request.options.cached_mc_enabled = true;
                request.options.cached_mc_pinned_edits = cached_mc_pinned_edits.clone();
                request.options.cached_mc_new_cache_edits = Some(
                    crate::services::api::claude::CachedMcEditsBlock::delete_refs(
                        pending_cache_edits.deleted_tool_ids.clone(),
                    ),
                );
                if let Some(user_message_index) = cached_mc_new_user_message_index {
                    crate::services::compact::micro_compact::pin_cache_edits(
                        user_message_index,
                        crate::services::compact::micro_compact::CacheEditsBlock::delete_refs(
                            pending_cache_edits.deleted_tool_ids.clone(),
                        ),
                    );
                    let _ = crate::services::compact::micro_compact::consume_pending_cache_edits();
                }
            } else if !cached_mc_pinned_edits.is_empty() {
                request.options.cached_mc_enabled = true;
                request.options.cached_mc_pinned_edits = cached_mc_pinned_edits.clone();
            }
            if let Some(model) = current_model_override.clone() {
                request.options.model = model;
                request.options.fallback_model = None;
            }
            match deps.call_model(request).await {
                Ok(stream) => break stream,
                Err(error) => {
                    // Maps to CC `APIUserAbortError` before/during stream open —
                    // Esc must end the turn, not surface a model_error retry path.
                    if abort_controller.is_aborted() || crate::utils::errors::is_abort_error(&error)
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    if !fallback_retry_used {
                        if let Some(fallback) = model_fallback_from_error(&error) {
                            fallback_retry_used = true;
                            messages_for_query =
                                strip_signature_blocks_for_model_fallback_retry(messages_for_query);
                            current_model_override = Some(fallback.fallback_model.clone());
                            if event_tx
                                .send(QueryEvent::Message(model_fallback_warning_message(
                                    &params.turn_id,
                                    &fallback.original_model,
                                    &fallback.fallback_model,
                                )))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_streaming"));
                            }
                            continue;
                        }
                    }
                    return Ok(yield_query_model_error(&event_tx, &params, &error, &[]).await);
                }
            }
        };
        let mut assistant_content = Vec::new();
        let mut assistant_messages = Vec::new();
        // Maps to CC assistant content block update semantics. The SDK emits
        // text/thinking deltas, while REPL consumes a transient streaming
        // preview; only completed text/thinking blocks are materialized as
        // `RenderableMessage` rows. Keep a stable UUID for that completed row so fallback
        // tombstones and partial-recovery paths can still target streamed rows.
        let mut streaming_text_message_uuid: Option<String> = None;
        let mut streaming_text_buffer = String::new();
        let mut streaming_thinking_message_uuid: Option<String> = None;
        let mut streaming_thinking_buffer = String::new();
        let mut emitted_assistant_content = false;
        // Maps to: CC `claude.ts:1761` `newMessages: AssistantMessage[]` as
        // seen by `query.ts:716-717` — the per-block assistants this attempt
        // already yielded (QueryEvent::Message). Streaming-fallback and
        // model-fallback tombstone exactly these (whole messages), and the
        // abort/API-error flushes skip them (their entries are already in the
        // one history).
        let mut streamed_assistant_messages = Vec::<Message>::new();
        let mut withheld_api_error: Option<crate::types::message::SystemApiErrorMessage> = None;
        let mut withheld_prompt_too_long_error: Option<
            crate::types::message::SystemApiErrorMessage,
        > = None;
        let mut withheld_media_size_error: Option<crate::types::message::SystemApiErrorMessage> =
            None;
        let mut streaming_tool_executor = if query_config.gates.streaming_tool_execution {
            let mut streaming_context = loop_local_tool_use_context(
                &params,
                &permission_context,
                messages_for_query.clone(),
                Some(query_tracking.clone()),
                &resume_restore_stores,
                content_replacement_state.clone(),
                &abort_controller,
            );
            install_tool_progress_sink(&mut streaming_context, &event_tx);
            Some(
                crate::services::tools::streaming_tool_executor::StreamingToolExecutor::new(
                    streaming_context.tools.clone(),
                    streaming_context,
                ),
            )
        } else {
            None
        };
        let mut streaming_tool_wakeup_rx = streaming_tool_executor
            .as_ref()
            .map(|executor| executor.wakeup_receiver());
        let mut streaming_executor_tool_use_ids = std::collections::HashSet::<String>::new();
        let mut streaming_deferred_updates =
            Vec::<crate::services::tools::tool_orchestration::MessageUpdate>::new();
        let mut streaming_deferred_model_messages = Vec::<crate::types::message::Message>::new();
        let mut tool_results = Vec::<crate::types::message::UserMessage>::new();
        // Maps to CC `query.ts` `toolResults: (UserMessage | AttachmentMessage)[]`.
        // Tool execution returns user tool_result messages; typed attachment
        // producers are collected separately at the official post-tool pass so
        // their ordering and model normalization remain lossless.
        let mut tool_result_attachments = Vec::<crate::types::message::Message>::new();
        let mut processed_tool_use_ids = std::collections::HashSet::<String>::new();
        let mut streaming_pending_permission_decisions =
            std::collections::HashMap::<String, PermissionPromptResponse>::new();
        let mut streaming_permission_requests_sent = std::collections::HashSet::<String>::new();

        loop {
            let item = tokio::select! {
                            command = command_rx.recv() => match command {
                                Ok(QueryCommand::Abort) | Err(_) => {
                                    abort_controller.abort();
                                    // The scan list covers EVERY streamed tool_use — the
                                    // interrupt tool_results below must pair each one,
                                    // including blocks whose assistant already entered
                                    // history as a whole `QueryEvent::Message`.
                                    let mut interrupted_assistants = assistant_messages.clone();
                                    if let Some(partial_assistant) =
                                        partial_assistant_message_from_streamed_content(
                                            assistant_content.clone(),
                                            &streaming_text_buffer,
                                            &streaming_thinking_buffer,
                                        )
                                    {
                                        interrupted_assistants.push(partial_assistant);
                                    }
                                    if !interrupted_assistants.is_empty() {
                                        // History flush: streamed per-block assistants are
                                        // already in the one history (CC REPL.tsx:3496
                                        // appended them at yield) — skip those. The
                                        // un-block-stopped text/thinking buffers die here
                                        // too (CC claude.ts throws APIUserAbortError
                                        // before any buffer yield); the REPL's cancel
                                        // path promotes its own streamingText copy
                                        // (REPL.tsx:2839-2844), so flushing the buffers
                                        // would double the partial text. Never-streamed
                                        // accumulated blocks (legacy injected seam) still
                                        // flush so history keeps their tool_use halves.
                                        for interrupted_assistant in &assistant_messages {
                                            if assistant_message_already_streamed(
                                                interrupted_assistant,
                                                &streamed_assistant_messages,
                                            ) {
                                                continue;
                                            }
                                            if event_tx
                                                .send(QueryEvent::ModelMessage(Message::Assistant(
                                                    interrupted_assistant.clone(),
                                                )))
                                                .await
                                                .is_err()
                                            {
                                                return Ok(transitions::Terminal::new("aborted_streaming"));
                                            }
                                        }
                                        if let Some(history_partial) =
                                            partial_assistant_message_from_streamed_content(
                                                unstreamed_assistant_content(
                                                    &assistant_content,
                                                    &streamed_assistant_messages,
                                                ),
                                                "",
                                                "",
                                            )
                                        {
                                            if event_tx
                                                .send(QueryEvent::ModelMessage(Message::Assistant(
                                                    history_partial,
                                                )))
                                                .await
                                                .is_err()
                                            {
                                                return Ok(transitions::Terminal::new("aborted_streaming"));
                                            }
                                        }
                                        if let Some(executor) = streaming_tool_executor.as_mut() {
                                            // Maps to CC `query.ts` abort branch consuming
                                            // `streamingToolExecutor.getRemainingResults()`
                                            // so streamed tool_use blocks keep matching
                                            // tool_result blocks even when interrupted.
                                            for user_message in &tool_results {
                                                if event_tx
                                                    .send(QueryEvent::ModelMessage(Message::User(
                                                        user_message.clone(),
                                                    )))
                                                    .await
                                                    .is_err()
                                                {
                                                    return Ok(transitions::Terminal::new("aborted_streaming"));
                                                }
                                            }
                                            for update in executor.get_remaining_results() {
                                                permission_context = update.new_context.tool_permission_context.clone();
                                                if emit_structured_output_update(&event_tx, &update.new_context)
                                                    .await
                                                    .is_err()
                                                {
                                                    return Ok(transitions::Terminal::new("aborted_streaming"));
                                                }
                                                if event_tx
                                                    .send(QueryEvent::PermissionContextUpdate(
                                                        permission_context.clone(),
                                                    ))
                                                    .await
                                                    .is_err()
                                                {
                                                    return Ok(transitions::Terminal::new("aborted_streaming"));
                                                }
                                                if let Some(progress) = update.progress {
                                                    if event_tx.send(QueryEvent::ToolProgress(progress)).await.is_err() {
                                                        return Ok(transitions::Terminal::new("aborted_streaming"));
                                                    }
                                                }
                                                if let Some(message) = update.message {
                                                    if emit_tool_result_renderable_message(&event_tx, message)
                                                        .await
                                                        .is_err()
                                                    {
                                                        return Ok(transitions::Terminal::new("aborted_streaming"));
                                                    }
                                                }
                                                if let Some(user_message) = update.tool_result {
                                                    if let Some(tool_use_id) = model_tool_result_tool_use_id(&user_message) {
                                                        processed_tool_use_ids.insert(tool_use_id);
                                                    }
                                                    if event_tx
                                                        .send(QueryEvent::ModelMessage(Message::User(
                                                            user_message,
                                                        )))
                                                        .await
                                                        .is_err()
                                                    {
                                                        return Ok(transitions::Terminal::new("aborted_streaming"));
                                                    }
                                                }
                                                if let Some(model_message) = update.model_message {
                                                    if event_tx
                                                        .send(QueryEvent::ModelMessage(model_message))
                                                        .await
                                                        .is_err()
                                                    {
                                                        return Ok(transitions::Terminal::new("aborted_streaming"));
                                                    }
                                                }
                                            }
                                        }
                                        for missing in crate::services::tools::tool_execution::yield_missing_tool_result_blocks(
                                            &interrupted_assistants,
                                            "Interrupted by user",
                                        ) {
                                            let Some(tool_use_id) = model_tool_result_tool_use_id(&missing.tool_result) else {
                                                continue;
                                            };
                                            if !processed_tool_use_ids.insert(tool_use_id) {
                                                continue;
                                            }
                                            if event_tx
                                                .send(QueryEvent::Row(missing.message))
                                                .await
                                                .is_err()
                                            {
                                                return Ok(transitions::Terminal::new("aborted_streaming"));
                                            }
                                            if event_tx
                                                .send(QueryEvent::ModelMessage(Message::User(
                                                    missing.tool_result,
                                                )))
                                                .await
                                                .is_err()
                                            {
                                                return Ok(transitions::Terminal::new("aborted_streaming"));
                                            }
                                        }
                                    }
                                    // Same marker the `is_aborted()` checkpoint emits (CC
                                    // `query.ts:1044-1050`). This branch is Rust-only: the
                                    // actor is woken by the explicit `QueryCommand::Abort`
                                    // and returns without reaching that checkpoint, so
                                    // without this a plain Esc during streaming — the most
                                    // common interrupt of all — would produce no marker.
                                    if emit_user_interruption_message(&event_tx, &abort_controller, false)
                                        .await
                                        .is_err()
                                    {
                                        return Ok(transitions::Terminal::new("aborted_streaming"));
                                    }
                                    return Ok(transitions::Terminal::new("aborted_streaming"));
                                }
                                Ok(QueryCommand::PermissionDecision { tool_use_id, choice }) => {
                                    let response = PermissionPromptResponse::new(choice);
                                    // Maps to CC `StreamingToolExecutor` permission promise
                                    // wakeup: if the prompt was surfaced while the model
                                    // stream is still open, resume that executor-owned tool
                                    // immediately instead of waiting for final stream drain.
                                    if streaming_tool_executor.is_some() {
                                        let deferred_index = streaming_deferred_updates.iter().position(|update| {
                                            update.permission_request.as_ref().map(|request| request.tool_use_id.as_str())
                                                == Some(tool_use_id.as_str())
                                        });
                                        if let Some(index) = deferred_index {
                                            let update = streaming_deferred_updates.remove(index);
                                            if let Some(permission_request) = update.permission_request.clone() {
            // CC keeps one live ToolUseContext. A deferred
                                                // permission update carries the worker's old
                                                // snapshot, so resume from the executor's
                                                // authoritative context to retain sibling Read
                                                // state and prior permission/tool effects.
                                                let mut current_tool_use_context = streaming_tool_executor
                                                    .as_ref()
                                                    .map(|executor| executor.get_updated_context())
                                                    .unwrap_or_else(|| update.new_context.clone());
                                                current_tool_use_context.update_permission_context(permission_context.clone());
                                                current_tool_use_context.messages = messages_for_query.clone();
                                                install_tool_progress_sink(&mut current_tool_use_context, &event_tx);

                                                if let Some(executor) = streaming_tool_executor.as_mut() {
                                                    executor.sync_tool_use_context(current_tool_use_context.clone());
                                                    let continuation_updates = executor
                                                        .continue_after_permission(
                                                            permission_request,
                                                            response.clone(),
                                                            current_tool_use_context,
                                                        )
                                                        .await;
                                                    permission_context = executor
                                                        .get_updated_context()
                                                        .tool_permission_context
                                                        .clone();
                                                    if event_tx
                                                        .send(QueryEvent::PermissionContextUpdate(
                                                            permission_context.clone(),
                                                        ))
                                                        .await
                                                        .is_err()
                                                    {
                                                        return Ok(transitions::Terminal::new(
                                                            "aborted_streaming",
                                                        ));
                                                    }
                                                    for continuation_update in continuation_updates {
                                                        if let Some(terminal) = process_streaming_completed_tool_update(
                                                            continuation_update,
                                                            &event_tx,
                                                            &mut permission_context,
                                                            &mut tool_results,
                                                            &mut processed_tool_use_ids,
                                                            &mut streaming_deferred_model_messages,
                                                        )
                                                        .await
                                                        {
                                                            return Ok(terminal);
                                                        }
                                                    }
                                                    continue;
                                                }
                                            }
                                            streaming_deferred_updates.insert(index, update);
                                        }
                                    }
                                    streaming_pending_permission_decisions.insert(tool_use_id, response);
                                    continue;
                                }
                                Ok(QueryCommand::PermissionResponse { tool_use_id, response }) => {
                                    if streaming_tool_executor.is_some() {
                                        let deferred_index = streaming_deferred_updates.iter().position(|update| {
                                            update.permission_request.as_ref().map(|request| request.tool_use_id.as_str())
                                                == Some(tool_use_id.as_str())
                                        });
                                        if let Some(index) = deferred_index {
                                            let update = streaming_deferred_updates.remove(index);
                                            if let Some(permission_request) = update.permission_request.clone() {
            let mut current_tool_use_context = streaming_tool_executor
                                                    .as_ref()
                                                    .map(|executor| executor.get_updated_context())
                                                    .unwrap_or_else(|| update.new_context.clone());
                                                current_tool_use_context.update_permission_context(permission_context.clone());
                                                current_tool_use_context.messages = messages_for_query.clone();
                                                install_tool_progress_sink(&mut current_tool_use_context, &event_tx);

                                                if let Some(executor) = streaming_tool_executor.as_mut() {
                                                    executor.sync_tool_use_context(current_tool_use_context.clone());
                                                    let continuation_updates = executor
                                                        .continue_after_permission(
                                                            permission_request,
                                                            response.clone(),
                                                            current_tool_use_context,
                                                        )
                                                        .await;
                                                    permission_context = executor
                                                        .get_updated_context()
                                                        .tool_permission_context
                                                        .clone();
                                                    if event_tx
                                                        .send(QueryEvent::PermissionContextUpdate(
                                                            permission_context.clone(),
                                                        ))
                                                        .await
                                                        .is_err()
                                                    {
                                                        return Ok(transitions::Terminal::new(
                                                            "aborted_streaming",
                                                        ));
                                                    }
                                                    for continuation_update in continuation_updates {
                                                        if let Some(terminal) = process_streaming_completed_tool_update(
                                                            continuation_update,
                                                            &event_tx,
                                                            &mut permission_context,
                                                            &mut tool_results,
                                                            &mut processed_tool_use_ids,
                                                            &mut streaming_deferred_model_messages,
                                                        )
                                                        .await
                                                        {
                                                            return Ok(terminal);
                                                        }
                                                    }
                                                    continue;
                                                }
                                            }
                                            streaming_deferred_updates.insert(index, update);
                                        }
                                    }
                                    streaming_pending_permission_decisions.insert(tool_use_id, response);
                                    continue;
                                }
                            },
                            _ = async {
                                if let Some(rx) = &streaming_tool_wakeup_rx {
                                    let _ = rx.recv().await;
                                } else {
                                    std::future::pending::<()>().await;
                                }
                            } => {
                                // Maps to CC `StreamingToolExecutor` promise wakeups:
                                // worker progress/completion should wake the query loop
                                // even when the Anthropic stream is temporarily idle.
                                if let Some(executor) = streaming_tool_executor.as_mut() {
                                    if let Some(terminal) = drain_streaming_executor_updates(
                                        executor,
                                        &abort_controller,
                                        &event_tx,
                                        &mut permission_context,
                                        &mut streaming_permission_requests_sent,
                                        &mut streaming_deferred_updates,
                                        &mut tool_results,
                                        &mut processed_tool_use_ids,
                                        &mut streaming_deferred_model_messages,
                                    )
                                    .await
                                    {
                                        return Ok(terminal);
                                    }
                                }
                                continue;
                            },
                            item = stream.recv() => match item {
                                Some(item) => item,
                                None => break,
                            },
                        };
            match item {
                crate::services::api::claude::QueryModelStreamItem::Content(item) => match item {
                    crate::services::api::claude::ClaudeStreamItem::Text(text) => {
                        if text.is_empty() {
                            continue;
                        }
                        // Maps to CC `utils/messages.ts` text_delta handling:
                        // deltas update REPL-local `streamingText`, not the
                        // formal transcript. Keep the buffer for partial-error
                        // recovery, but do not emit `QueryEvent::Message` here.
                        if streaming_text_message_uuid.is_none() {
                            streaming_text_message_uuid = Some(deps.uuid());
                        }
                        streaming_text_buffer.push_str(&text);
                        continue;
                    }
                    crate::services::api::claude::ClaudeStreamItem::Thinking { text, .. } => {
                        if text.is_empty() {
                            continue;
                        }
                        // Maps to CC `utils/messages.ts` thinking_delta handling:
                        // thinking deltas update metrics/spinner state only;
                        // the renderable thinking block arrives when the content
                        // block is complete (with signature).
                        if streaming_thinking_message_uuid.is_none() {
                            streaming_thinking_message_uuid = Some(deps.uuid());
                        }
                        streaming_thinking_buffer.push_str(&text);
                        continue;
                    }
                    other => {
                        let Some(content) = claude_stream_item_to_assistant_content(other) else {
                            continue;
                        };
                        if let Some(kind) = assistant_content_to_source_event_kind_with_status(
                            content.clone(),
                            ToolUseStatus::Queued,
                        ) {
                            let fallback = deps.uuid();
                            let uuid = assistant_content_uuid(&content, fallback);
                            let source = QuerySourceEvent { uuid, kind };
                            // CC shape (claude.ts:2192-2211 → query.ts:823-824):
                            // every completed streamed block is a whole
                            // single-block assistant `Message` appended to the
                            // one history (REPL.tsx:3496). No Row/ModelMessage
                            // dual carrier remains for streamed assistants.
                            match source.into_message() {
                                SeamMessage::Message(message) => {
                                    streamed_assistant_messages.push(message.clone());
                                    if event_tx
                                        .send(QueryEvent::Message(message))
                                        .await
                                        .is_err()
                                    {
                                        return Ok(transitions::Terminal::new(
                                            "aborted_streaming",
                                        ));
                                    }
                                    emitted_assistant_content = true;
                                }
                                SeamMessage::Row(renderable_message) => {
                                    if event_tx
                                        .send(QueryEvent::Row(renderable_message))
                                        .await
                                        .is_err()
                                    {
                                        return Ok(transitions::Terminal::new(
                                            "aborted_streaming",
                                        ));
                                    }
                                    emitted_assistant_content = true;
                                }
                            }
                        }
                        let maybe_streaming_tool_use = match &content {
                            crate::types::message::AssistantContent::ToolUse(tool_use) => {
                                Some(tool_use.clone())
                            }
                            _ => None,
                        };
                        assistant_content.push(content);
                        if let (Some(executor), Some(tool_use)) =
                            (streaming_tool_executor.as_mut(), maybe_streaming_tool_use)
                        {
                            if streaming_executor_tool_use_ids.insert(tool_use.id.0.clone()) {
                                // Maps to CC `REPL.tsx:3910` `setStreamingToolUses`:
                                // the block is in the transcript now, but its id
                                // does not reach `inProgressToolUseIDs` until the
                                // tool actually starts.
                                let _ = event_tx
                                    .send(QueryEvent::StreamingToolUseStarted {
                                        tool_use_id: tool_use.id.0.clone(),
                                    })
                                    .await;
                                // Maps to CC `query.ts` adding streamed
                                // tool_use blocks to `StreamingToolExecutor`
                                // during the model stream.
                                executor.add_tool(
                                    tool_use,
                                    partial_streaming_assistant_message(assistant_content.clone()),
                                );
                            }
                        }
                    }
                },
                crate::services::api::claude::QueryModelStreamItem::CompletedContent(item) => {
                    let Some(content) = claude_stream_item_to_assistant_content(item) else {
                        continue;
                    };
                    if let Some(kind) = assistant_content_to_source_event_kind_with_status(
                        content.clone(),
                        ToolUseStatus::Queued,
                    ) {
                        let existing_id = match &content {
                            crate::types::message::AssistantContent::Text(_) => {
                                streaming_text_message_uuid.take()
                            }
                            crate::types::message::AssistantContent::Thinking { .. } => {
                                streaming_thinking_message_uuid.take()
                            }
                            _ => None,
                        };
                        let fallback_id = existing_id.unwrap_or_else(|| deps.uuid());
                        let uuid = assistant_content_uuid(&content, fallback_id);
                        let source = QuerySourceEvent { uuid, kind };
                        // Whole single-block message per completed block (CC
                        // claude.ts:2192-2211; see the incremental site above).
                        match source.into_message() {
                            SeamMessage::Message(message) => {
                                streamed_assistant_messages.push(message.clone());
                                if event_tx
                                    .send(QueryEvent::Message(message))
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_streaming"));
                                }
                                emitted_assistant_content = true;
                            }
                            SeamMessage::Row(renderable_message) => {
                                if event_tx
                                    .send(QueryEvent::Row(renderable_message))
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_streaming"));
                                }
                                emitted_assistant_content = true;
                            }
                        }
                    }
                    match &content {
                        crate::types::message::AssistantContent::Text(_) => {
                            streaming_text_buffer.clear();
                        }
                        crate::types::message::AssistantContent::Thinking { .. } => {
                            streaming_thinking_buffer.clear();
                        }
                        _ => {}
                    }
                    assistant_content.push(content);
                    continue;
                }
                crate::services::api::claude::QueryModelStreamItem::StreamingFallback => {
                    // Maps to CC `query.ts` `options.onStreamingFallback()`:
                    // tombstone/discard partial streamed assistant state before
                    // accepting the non-streaming fallback assistant. This
                    // removes invalid-signature partial thinking/tool-use rows
                    // from the UI, then forces the fallback final assistant to
                    // be projected to the transcript.
                    if event_tx.send(QueryEvent::ClearStreamingPreview).await.is_err() {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    // CC query.ts:713-718: yield a tombstone per already-yielded
                    // assistant message (whole values) so the REPL removes them
                    // from the one history and the transcript.
                    if tombstone_streamed_assistant_messages(
                        &event_tx,
                        &streamed_assistant_messages,
                    )
                    .await
                    .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    streamed_assistant_messages.clear();
                    assistant_content.clear();
                    assistant_messages.clear();
                    tool_results.clear();
                    processed_tool_use_ids.clear();
                    streaming_executor_tool_use_ids.clear();
                    streaming_deferred_updates.clear();
                    streaming_deferred_model_messages.clear();
                    streaming_pending_permission_decisions.clear();
                    streaming_permission_requests_sent.clear();
                    streaming_text_message_uuid = None;
                    streaming_text_buffer.clear();
                    streaming_thinking_message_uuid = None;
                    streaming_thinking_buffer.clear();
                    emitted_assistant_content = false;
                    withheld_api_error = None;
                    withheld_prompt_too_long_error = None;
                    withheld_media_size_error = None;

                    if let Some(executor) = streaming_tool_executor.as_mut() {
                        executor.discard();
                        let mut streaming_context = loop_local_tool_use_context(
                            &params,
                            &permission_context,
                            messages_for_query.clone(),
                            Some(query_tracking.clone()),
                            &resume_restore_stores,
                            content_replacement_state.clone(),
                            &abort_controller,
                        );
                        install_tool_progress_sink(&mut streaming_context, &event_tx);
                        let replacement = crate::services::tools::streaming_tool_executor::StreamingToolExecutor::new(
                            streaming_context.tools.clone(),
                            streaming_context,
                        );
                        streaming_tool_wakeup_rx = Some(replacement.wakeup_receiver());
                        streaming_tool_executor = Some(replacement);
                    }
                    continue;
                }
                crate::services::api::claude::QueryModelStreamItem::ModelFallback {
                    original_model,
                    fallback_model,
                } => {
                    // Maps to CC `query.ts` catch of `FallbackTriggeredError`
                    // thrown by a streaming-to-non-streaming fallback: discard
                    // partial stream state, switch to the fallback model, strip
                    // model-bound thinking signatures, and retry this turn.
                    if fallback_retry_used {
                        let error = anyhow::Error::new(
                            crate::services::api::with_retry::FallbackTriggeredError {
                                original_model,
                                fallback_model,
                            },
                        );
                        return Ok(yield_query_model_error(&event_tx, &params, &error, &assistant_messages).await);
                    }
                    fallback_retry_used = true;
                    if event_tx.send(QueryEvent::ClearStreamingPreview).await.is_err() {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    // Same tombstone contract as the streaming fallback above
                    // (CC query.ts:713-718): the model-switch retry re-streams
                    // the whole turn, so already-appended per-block assistants
                    // must leave the history and transcript.
                    if tombstone_streamed_assistant_messages(
                        &event_tx,
                        &streamed_assistant_messages,
                    )
                    .await
                    .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    streamed_assistant_messages.clear();
                    if let Some(executor) = streaming_tool_executor.as_mut() {
                        executor.discard();
                    }
                    messages_for_query =
                        strip_signature_blocks_for_model_fallback_retry(messages_for_query);
                    model_fallback_retry_messages = Some(messages_for_query.clone());
                    model_fallback_retry_tracking = Some(query_tracking.clone());
                    current_model_override = Some(fallback_model.clone());
                    if event_tx
                        .send(QueryEvent::Message(model_fallback_warning_message(
                            &params.turn_id,
                            &original_model,
                            &fallback_model,
                        )))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    continue 'query_loop;
                }
                crate::services::api::claude::QueryModelStreamItem::Assistant(message) => {
                    // Maps to CC claude.ts:2192-2211 → query.ts:823-827: every
                    // `content_block_stop` yields a whole fresh-uuid
                    // single-block AssistantMessage that the REPL appends to
                    // the one history (REPL.tsx:3496). The dedup bookkeeping
                    // below covers the legacy/injected seam where per-block
                    // Content/CompletedContent items preceded this envelope:
                    // those blocks already traveled as their own messages, so
                    // the envelope stays actor-internal.
                    let envelope_uuid = Some(message.uuid.clone());
                    let mut unrendered_blocks = Vec::new();
                    let mut model_content_index = 0usize;
                    for content in &message.content {
                        if matches!(
                            content,
                            crate::types::message::AssistantContent::MessageIdentity(_)
                        ) {
                            continue;
                        }
                        let already_rendered = assistant_content
                            .iter()
                            .position(|pending| pending == content)
                            .map(|position| {
                                assistant_content.remove(position);
                            })
                            .is_some();
                        if !already_rendered {
                            unrendered_blocks.push((model_content_index, content.clone()));
                        }
                        match content {
                            crate::types::message::AssistantContent::Text(_) => {
                                streaming_text_message_uuid = None;
                                streaming_text_buffer.clear();
                            }
                            crate::types::message::AssistantContent::Thinking { .. } => {
                                streaming_thinking_message_uuid = None;
                                streaming_thinking_buffer.clear();
                            }
                            _ => {}
                        }
                        model_content_index += 1;
                    }
                    if !unrendered_blocks.is_empty() {
                        if unrendered_blocks.len() == model_content_index {
                            // Production shape: no block of this envelope was
                            // pre-emitted, so the envelope itself is CC's
                            // yielded value — model/usage/identity ride it into
                            // the history and the transcript record.
                            let whole = Message::Assistant(message.clone());
                            streamed_assistant_messages.push(whole.clone());
                            if event_tx.send(QueryEvent::Message(whole)).await.is_err() {
                                return Ok(transitions::Terminal::new("aborted_streaming"));
                            }
                            emitted_assistant_content = true;
                        } else {
                            // Mixed legacy seam: some blocks already traveled;
                            // emit only the remainder as single-block messages
                            // so nothing renders twice.
                            for (index, content) in unrendered_blocks {
                                let Some(kind) =
                                    assistant_content_to_source_event_kind_with_status(
                                        content.clone(),
                                        ToolUseStatus::Queued,
                                    )
                                else {
                                    continue;
                                };
                                let fallback = envelope_uuid
                                    .as_deref()
                                    .map(|uuid| {
                                        if index == 0 {
                                            uuid.to_string()
                                        } else {
                                            format!("{uuid}:{index}")
                                        }
                                    })
                                    .unwrap_or_else(|| deps.uuid());
                                let source = QuerySourceEvent {
                                    uuid: assistant_content_uuid(&content, fallback),
                                    kind,
                                };
                                match source.into_message() {
                                    SeamMessage::Message(single) => {
                                        streamed_assistant_messages.push(single.clone());
                                        if event_tx
                                            .send(QueryEvent::Message(single))
                                            .await
                                            .is_err()
                                        {
                                            return Ok(transitions::Terminal::new(
                                                "aborted_streaming",
                                            ));
                                        }
                                        emitted_assistant_content = true;
                                    }
                                    SeamMessage::Row(renderable_message) => {
                                        if event_tx
                                            .send(QueryEvent::Row(renderable_message))
                                            .await
                                            .is_err()
                                        {
                                            return Ok(transitions::Terminal::new(
                                                "aborted_streaming",
                                            ));
                                        }
                                        emitted_assistant_content = true;
                                    }
                                }
                            }
                        }
                    }

                    if let Some(executor) = streaming_tool_executor.as_mut() {
                        for tool_use in assistant_tool_use_blocks(&message) {
                            if streaming_executor_tool_use_ids.insert(tool_use.id.0.clone()) {
                                executor.add_tool(tool_use, message.clone());
                            }
                        }
                    }
                    assistant_messages.push(message);
                }
                crate::services::api::claude::QueryModelStreamItem::AssistantDelta {
                    stop_reason,
                    usage,
                } => {
                    // CC claude.ts:2229-2248: `message_delta` arrives after the
                    // last `content_block_stop` and mutates the last yielded
                    // assistant through its shared reference. Rust patches the
                    // retained typed value AND forwards the mutation across the
                    // channel keyed by that message's uuid, so every consumer's
                    // already-appended copy (REPL history entry, headless
                    // history, deferred session record) converges on the final
                    // usage/stop_reason.
                    if let Some(last) = assistant_messages.last_mut() {
                        last.stop_reason = stop_reason.clone();
                        last.usage = usage.clone();
                        let last_uuid = last.uuid.clone();
                        // Envelope (MessageBase) uuid, NOT the identity-block
                        // accessor `AssistantMessage::uuid()`.
                        if let Some(Message::Assistant(streamed)) = streamed_assistant_messages
                            .iter_mut()
                            .rev()
                            .find(|streamed| matches!(
                                streamed,
                                Message::Assistant(assistant) if assistant.uuid == last_uuid
                            ))
                        {
                            streamed.stop_reason = stop_reason.clone();
                            streamed.usage = usage.clone();
                        }
                        if event_tx
                            .send(QueryEvent::AssistantDelta {
                                uuid: last_uuid,
                                stop_reason,
                                usage,
                            })
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_streaming"));
                        }
                    }
                }
                crate::services::api::claude::QueryModelStreamItem::SystemApiError(
                    system_message,
                ) => {
                    // CC `query.ts:659,824`: the retry heartbeat passes
                    // through the yield seam unchanged — no withhold branch
                    // matches a system message — and lands in REPL messages
                    // (`REPL.tsx:3496` `setMessages(old => [...old, msg])`).
                    // Non-terminal: `withRetry` is still sleeping toward the
                    // next attempt, so the loop keeps consuming. The API
                    // projection drops system messages (`claude.rs`
                    // `Message::System(_) => None`), matching CC's
                    // `messagesForAPI` filter.
                    if event_tx
                        .send(QueryEvent::Message(Message::System(system_message)))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                }
                crate::services::api::claude::QueryModelStreamItem::SystemError(error)
                    if crate::services::context_collapse::is_withheld_prompt_too_long(
                        &error,
                        &params.query_source,
                    ) || crate::services::compact::reactive_compact::is_withheld_prompt_too_long(&error) =>
                {
                    withheld_prompt_too_long_error = Some(error);
                    break;
                }
                crate::services::api::claude::QueryModelStreamItem::SystemError(error)
                    if crate::services::compact::reactive_compact::is_withheld_media_size_error(&error) =>
                {
                    withheld_media_size_error = Some(error);
                    break;
                }
                crate::services::api::claude::QueryModelStreamItem::SystemError(error)
                    if system_api_error_is_max_output_tokens(&error) =>
                {
                    withheld_api_error = Some(error);
                    break;
                }
                crate::services::api::claude::QueryModelStreamItem::SystemError(_error)
                    if abort_controller.is_aborted() =>
                {
                    // Maps to CC `APIUserAbortError` mid-stream: do not treat as
                    // model_error / stop-failure — interruption UI is owned by
                    // query.ts / REPL (`[Request interrupted by user]`).
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
                crate::services::api::claude::QueryModelStreamItem::SystemError(error) => {
                    let last_assistant_message = if !streaming_text_buffer.trim().is_empty() {
                        Some(streaming_text_buffer.as_str())
                    } else {
                        None
                    };

                    // Scan list: every streamed tool_use (whole-Message
                    // emissions included) so the error tool_results below pair
                    // each one.
                    let mut partial_assistants = assistant_messages.clone();
                    if let Some(partial_assistant) =
                        partial_assistant_message_from_streamed_content(
                            assistant_content.clone(),
                            &streaming_text_buffer,
                            &streaming_thinking_buffer,
                        )
                    {
                        partial_assistants.push(partial_assistant);
                    }
                    if !partial_assistants.is_empty() {
                        // History flush: streamed per-block assistants already
                        // entered the one history as whole
                        // `QueryEvent::Message` values; only never-yielded
                        // content (accumulated blocks and the un-stopped
                        // buffers) still needs the history-only flush.
                        for partial_assistant in &assistant_messages {
                            if assistant_message_already_streamed(
                                partial_assistant,
                                &streamed_assistant_messages,
                            ) {
                                continue;
                            }
                            if event_tx
                                .send(QueryEvent::ModelMessage(Message::Assistant(
                                    partial_assistant.clone(),
                                )))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_streaming"));
                            }
                        }
                        if let Some(history_partial) =
                            partial_assistant_message_from_streamed_content(
                                unstreamed_assistant_content(
                                    &assistant_content,
                                    &streamed_assistant_messages,
                                ),
                                &streaming_text_buffer,
                                &streaming_thinking_buffer,
                            )
                        {
                            if event_tx
                                .send(QueryEvent::ModelMessage(Message::Assistant(
                                    history_partial,
                                )))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_streaming"));
                            }
                        }
                        if streaming_tool_executor.is_some() {
                            abort_controller.abort();
                        }
                        for user_message in &tool_results {
                            if event_tx
                                .send(QueryEvent::ModelMessage(Message::User(
                                    user_message.clone(),
                                )))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_streaming"));
                            }
                        }
                        for missing in
                            crate::services::tools::tool_execution::yield_missing_tool_result_blocks(
                                &partial_assistants,
                                &error.content,
                            )
                        {
                            let Some(tool_use_id) =
                                model_tool_result_tool_use_id(&missing.tool_result)
                            else {
                                continue;
                            };
                            if !processed_tool_use_ids.insert(tool_use_id) {
                                continue;
                            }
                            if event_tx
                                .send(QueryEvent::Row(missing.message))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_streaming"));
                            }
                            if event_tx
                                .send(QueryEvent::ModelMessage(Message::User(missing.tool_result)))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_streaming"));
                            }
                        }
                    }

                    schedule_stop_failure_hooks_for_api_error(
                        error.clone(),
                        last_assistant_message.map(str::to_string),
                    );
                    let terminal_reason = terminal_reason_for_system_api_error(&error);
                    let _ = event_tx.send(QueryEvent::ApiError(error.clone())).await;
                    let _ =
                        send_assistant_api_error_message(&event_tx, &params.turn_id, &error.content)
                            .await;
                    return Ok(transitions::Terminal::new(terminal_reason));
                }
                crate::services::api::claude::QueryModelStreamItem::Stream(event) => {
                    if event_tx.send(QueryEvent::Stream(event)).await.is_err() {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                }
            }

            if let Some(executor) = streaming_tool_executor.as_mut() {
                if let Some(terminal) = drain_streaming_executor_updates(
                    executor,
                    &abort_controller,
                    &event_tx,
                    &mut permission_context,
                    &mut streaming_permission_requests_sent,
                    &mut streaming_deferred_updates,
                    &mut tool_results,
                    &mut processed_tool_use_ids,
                    &mut streaming_deferred_model_messages,
                )
                .await
                {
                    return Ok(terminal);
                }
            }
        }

        if !streaming_thinking_buffer.is_empty() {
            // Recovery path when content_block_stop never arrived; match CC
            // stream-start init (`signature: ''`).
            assistant_content.push(crate::types::message::AssistantContent::Thinking {
                text: streaming_thinking_buffer.clone(),
                signature: String::new(),
            });
        }
        if !streaming_text_buffer.is_empty() {
            assistant_content.push(crate::types::message::AssistantContent::Text(
                streaming_text_buffer.clone(),
            ));
        }
        if !assistant_content.is_empty() {
            let stop_reason = if assistant_content.iter().any(|content| {
                matches!(content, crate::types::message::AssistantContent::ToolUse(_))
            }) {
                crate::types::message::StopReason::ToolUse
            } else {
                crate::types::message::StopReason::EndTurn
            };
            assistant_messages.push(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: assistant_content.clone(),
                model: None,
                stop_reason: Some(stop_reason),
                usage: None,
            });
        }

        let suppress_empty_withheld_recoverable_error_assistant = assistant_messages.is_empty()
            && (withheld_prompt_too_long_error.is_some() || withheld_media_size_error.is_some());
        let mut tool_use_blocks = Vec::new();
        if !suppress_empty_withheld_recoverable_error_assistant {
            // Injected deps may provide only a synthetic recovered assistant
            // (the post-loop buffer recovery above). Production per-block
            // assistants were already yielded whole during the stream (CC
            // claude.ts:2210), so this emits only when nothing streamed: the
            // whole assistant enters the one history exactly like a streamed
            // yield would (CC's non-streaming fallback yields the whole
            // message the same way, claude.ts:2571-2594).
            if !emitted_assistant_content {
                for assistant_message in &assistant_messages {
                    let whole = Message::Assistant(assistant_message.clone());
                    streamed_assistant_messages.push(whole.clone());
                    if event_tx.send(QueryEvent::Message(whole)).await.is_err() {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                }
            }

            tool_use_blocks = assistant_messages
                .iter()
                .flat_map(assistant_tool_use_blocks)
                .collect();
            // The pre-A1 status re-send (`send_tool_use_status_event`) died
            // with the dual carrier: tool-use state renders from
            // `inProgressToolUseIDs` + lookups (CC REPL.tsx:1897,
            // AssistantToolUseMessage.tsx:120-121), never from row re-yields.
            if let Some(executor) = streaming_tool_executor.as_mut() {
                for tool_use in &tool_use_blocks {
                    if streaming_executor_tool_use_ids.insert(tool_use.id.0.clone()) {
                        if let Some(owner) =
                            assistant_message_for_tool_use(&assistant_messages, &tool_use.id.0)
                        {
                            executor.add_tool(tool_use.clone(), owner.clone());
                        }
                    }
                }
            }
            // Streaming preapproved tool results can be displayed before the model
            // finishes, but typed model history must preserve API order:
            // all assistant blocks before user(tool_result).
            for user_message in &tool_results {
                if event_tx
                    .send(QueryEvent::ModelMessage(Message::User(
                        user_message.clone(),
                    )))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }
            for model_message in streaming_deferred_model_messages.drain(..) {
                if event_tx
                    .send(QueryEvent::ModelMessage(model_message))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }

            if let (Some(pending_cache_edits), Some(last_assistant)) =
                (pending_cache_edits.as_ref(), assistant_messages.last())
            {
                let cumulative_deleted = last_assistant
                    .usage
                    .as_ref()
                    .map(|usage| usage.cache_deleted_input_tokens)
                    .unwrap_or(0);
                let deleted_tokens = cumulative_deleted
                    .saturating_sub(pending_cache_edits.baseline_cache_deleted_tokens);
                if deleted_tokens > 0
                    && event_tx
                        .send(QueryEvent::Message(microcompact_boundary_message(
                            deps.uuid(),
                            pending_cache_edits,
                            deleted_tokens,
                        )))
                        .await
                        .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }
            crate::services::compact::micro_compact::mark_tools_sent_to_api_state();
        }

        schedule_post_sampling_hooks(
            &messages_for_query,
            &assistant_messages,
            &system_prompt,
            &user_context,
            &system_context,
            permission_context.clone(),
            params.query_source.clone(),
            Some(query_tracking.clone()),
            &resume_restore_stores,
            &params.tool_use_context.mcp_state,
        )
        .await;

        if let Some(summary_task) = pending_tool_use_summary.take() {
            // Maps to CC `query.ts` yielding the previous iteration's
            // `pendingToolUseSummary` after model streaming and before the
            // no-tool-use terminal/continuation branch.
            if let Ok(Some(summary)) = summary_task.await {
                if event_tx
                    .send(QueryEvent::ToolUseSummary(summary))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }
        }

        // Maps to CC `query.ts:1015` — abort after the model stream finishes
        // (Esc may have raced with stream end / Abort still queued). Must stop
        // before stop-hooks / tool follow-up continue the agent turn.
        if abort_controller.is_aborted() {
            if let Some(executor) = streaming_tool_executor.as_mut() {
                for update in executor.get_remaining_results() {
                    permission_context = update.new_context.tool_permission_context.clone();
                    if emit_structured_output_update(&event_tx, &update.new_context)
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    if event_tx
                        .send(QueryEvent::PermissionContextUpdate(
                            permission_context.clone(),
                        ))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    if let Some(message) = update.message {
                        if emit_tool_result_renderable_message(&event_tx, message)
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_streaming"));
                        }
                    }
                    if let Some(user_message) = update.tool_result {
                        if let Some(tool_use_id) = model_tool_result_tool_use_id(&user_message) {
                            processed_tool_use_ids.insert(tool_use_id);
                        }
                        if event_tx
                            .send(QueryEvent::ModelMessage(Message::User(user_message)))
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_streaming"));
                        }
                    }
                }
            } else if !assistant_messages.is_empty() {
                // Maps to: CC `query.ts:1025-1028`. These synthetic
                // `Interrupted by user` results are NOT a PostToolUseFailure
                // site, and deliberately so: `runPostToolUseFailureHooks` is
                // reached only from the `catch` INSIDE
                // `checkPermissionsAndCallTool` (`toolExecution.ts:1589-1711`),
                // which wraps `tool.call` alone. A tool_use standing here never
                // entered that try — `runToolUse` was not called for it at all
                // — so CC fires no tool event for it, exactly as this branch
                // does not. The port's counterpart of CC's catch is
                // `tool_execution.rs#post_tool_hook_event`.
                for missing in
                    crate::services::tools::tool_execution::yield_missing_tool_result_blocks(
                        &assistant_messages,
                        "Interrupted by user",
                    )
                {
                    let Some(tool_use_id) = model_tool_result_tool_use_id(&missing.tool_result)
                    else {
                        continue;
                    };
                    if !processed_tool_use_ids.insert(tool_use_id) {
                        continue;
                    }
                    if event_tx
                        .send(QueryEvent::Row(missing.message))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    if event_tx
                        .send(QueryEvent::ModelMessage(Message::User(missing.tool_result)))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                }
            }
            // CC `query.ts:1044-1050`. The submit-interrupt guard matters: when
            // a queued user message caused the abort, that message follows
            // immediately and supplies the context this marker would.
            if emit_user_interruption_message(&event_tx, &abort_controller, false)
                .await
                .is_err()
            {
                return Ok(transitions::Terminal::new("aborted_streaming"));
            }
            return Ok(transitions::Terminal::new("aborted_streaming"));
        }

        if tool_use_blocks.is_empty()
            && assistant_messages
                .iter()
                .all(|message| assistant_tool_use_blocks(message).is_empty())
        {
            if withheld_prompt_too_long_error.is_some() || withheld_media_size_error.is_some() {
                let is_withheld_media = withheld_media_size_error.is_some();
                let error = withheld_media_size_error
                    .clone()
                    .or_else(|| withheld_prompt_too_long_error.clone())
                    .expect("checked recoverable withheld API error");
                // Maps to CC `query.ts` withheld prompt-too-long/media recovery:
                // context-collapse gets the first chance only for prompt-too-long
                // overflow, then reactive compact gets the common recovery slot.
                // If neither can recover, the API error surfaces before stop
                // hooks to avoid error→hook retry spirals.
                if !is_withheld_media {
                    let collapse_recovery =
                        crate::services::context_collapse::recover_from_overflow(
                            messages_for_query.clone(),
                            &params.query_source,
                        );
                    if collapse_recovery.committed > 0 {
                        model_messages = collapse_recovery.messages;
                        max_output_tokens_override = None;
                        stop_hook_active = None;
                        if transitions::Continue::with_reason("collapse_drain_retry")
                            .should_continue_query_loop()
                        {
                            continue 'query_loop;
                        }
                    }
                }
                let reactive_compact =
                    crate::services::compact::reactive_compact::try_reactive_compact(
                        crate::services::compact::reactive_compact::ReactiveCompactParams {
                            has_attempted: has_attempted_reactive_compact,
                            query_source: params.query_source.clone(),
                            aborted: abort_controller.is_aborted(),
                            messages: messages_for_query.clone(),
                            cache_safe_params:
                                crate::services::compact::auto_compact::AutoCompactCacheSafeParams {
                                    system_prompt: system_prompt.clone(),
                                    user_context: user_context.clone(),
                                    system_context: system_context.clone(),
                                    fork_context_messages: messages_for_query.clone(),
                                },
                        },
                    );
                if let Some(compacted) = reactive_compact.compacted {
                    // CC query.ts:1147-1149: `const postCompactMessages =
                    // buildPostCompactMessages(compacted); for (const msg of
                    // postCompactMessages) { yield msg }` — whole Messages;
                    // the boundary member trips the REPL reset exactly like
                    // the proactive autocompact flood above.
                    for compacted_message in compacted.messages.iter().cloned() {
                        if event_tx
                            .send(QueryEvent::Message(compacted_message))
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_streaming"));
                        }
                    }
                    model_messages = compacted.messages;
                    auto_compact_tracking = None;
                    has_attempted_reactive_compact = true;
                    max_output_tokens_override = None;
                    stop_hook_active = None;
                    if transitions::Continue::with_reason("reactive_compact_retry")
                        .should_continue_query_loop()
                    {
                        continue 'query_loop;
                    }
                }
                schedule_stop_failure_hooks_for_api_error(
                    error.clone(),
                    last_assistant_text(&assistant_messages),
                );
                let _ = event_tx.send(QueryEvent::ApiError(error.clone())).await;
                if send_assistant_api_error_message(&event_tx, &params.turn_id, &error.content)
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
                return Ok(transitions::Terminal::new(
                    terminal_reason_for_system_api_error(&error),
                ));
            }

            // Maps to CC `query.ts` `isWithheldMaxOutputTokens(...)` recovery
            // branch before stop hooks. A `max_tokens` response or withheld
            // `apiError === 'max_output_tokens'` is not final work: ask the
            // model to resume directly, preserving the assistant output in
            // typed history and continuing the same query loop.
            let saw_max_output_tokens = assistant_messages
                .last()
                .and_then(|message| message.stop_reason.as_ref())
                == Some(&crate::types::message::StopReason::MaxTokens)
                || is_withheld_max_output_tokens(&withheld_api_error);
            if saw_max_output_tokens {
                if should_escalate_max_output_tokens(max_output_tokens_override) {
                    // Maps to CC `query.ts` `tengu_otk_slot_v1` branch: retry
                    // the same request once at 64k without adding a recovery
                    // meta-message to the conversation.
                    model_messages = messages_for_query.clone();
                    max_output_tokens_override = Some(crate::utils::context::ESCALATED_MAX_TOKENS);
                    stop_hook_active = None;
                    if transitions::Continue::with_reason("max_output_tokens_escalate")
                        .should_continue_query_loop()
                    {
                        continue 'query_loop;
                    }
                }
                if max_output_tokens_recovery_count < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT {
                    model_messages = messages_for_query.clone();
                    model_messages.extend(
                        assistant_messages
                            .iter()
                            .cloned()
                            .map(crate::types::message::Message::Assistant),
                    );
                    let recovery_message = crate::types::message::UserMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        // CC `createUserMessage({ content, isMeta: true })`
                        // (query.ts:1224-1229); the Rust carrier for the
                        // envelope's `isMeta` is the Meta* block family, and
                        // `MetaText` serializes to the same Anthropic text
                        // block as `Text`.
                        content: vec![crate::types::message::UserContent::MetaText(
                            MAX_OUTPUT_TOKENS_RECOVERY_MESSAGE.to_string(),
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
                    };
                    // CC `query.ts:1223-1250` builds this message and puts it
                    // ONLY in the next `State.messages` — there is no `yield`,
                    // so it never reaches the REPL array, never renders, and
                    // never enters the transcript. It lives for the remainder
                    // of this one query call and dies with it. `model_messages`
                    // is that `State.messages`, so the push below is the whole
                    // of CC's behaviour; emitting an event too would leak it
                    // into REPL history (and from there into the JSONL via
                    // `useLogMessages`) across turns.
                    model_messages.push(crate::types::message::Message::User(recovery_message));
                    max_output_tokens_recovery_count =
                        max_output_tokens_recovery_count.saturating_add(1);
                    max_output_tokens_override = None;
                    stop_hook_active = None;
                    if transitions::Continue::with_reason("max_output_tokens_recovery")
                        .should_continue_query_loop()
                    {
                        continue 'query_loop;
                    }
                }

                if let Some(error) = withheld_api_error.clone() {
                    schedule_stop_failure_hooks_for_api_error(
                        error.clone(),
                        last_assistant_text(&assistant_messages),
                    );
                    let _ = event_tx.send(QueryEvent::ApiError(error.clone())).await;
                    if send_assistant_api_error_message(&event_tx, &params.turn_id, &error.content)
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    return Ok(transitions::Terminal::new("completed"));
                }
            }
            max_output_tokens_recovery_count = 0;

            let tool_use_context = loop_local_tool_use_context(
                &params,
                &permission_context,
                messages_for_query.clone(),
                Some(query_tracking.clone()),
                &resume_restore_stores,
                content_replacement_state.clone(),
                &abort_controller,
            );
            let stop_hook_result = stop_hooks::handle_stop_hooks(stop_hooks::StopHookParams {
                messages_for_query: messages_for_query.clone(),
                assistant_messages: assistant_messages.clone(),
                system_prompt: system_prompt.clone(),
                user_context: user_context.clone(),
                system_context: system_context.clone(),
                tool_use_context: tool_use_context.clone(),
                query_source: params.query_source.clone(),
                stop_hook_active,
                event_tx: Some(event_tx.clone()),
            })
            .await;

            for hook_message in stop_hook_result.messages {
                if event_tx.send(QueryEvent::Row(hook_message)).await.is_err() {
                    return Ok(transitions::Terminal::new("aborted_streaming"));
                }
            }

            if stop_hook_result.prevent_continuation {
                return Ok(transitions::Terminal::new("stop_hook_prevented"));
            }

            let stop_hook_blocking_model_messages = stop_hook_result.blocking_model_messages;
            let saw_stop_hook_blocking_error = !stop_hook_blocking_model_messages.is_empty();

            if saw_stop_hook_blocking_error {
                model_messages = messages_for_query.clone();
                model_messages.extend(
                    assistant_messages
                        .iter()
                        .cloned()
                        .map(crate::types::message::Message::Assistant),
                );
                for blocking_model_message in stop_hook_blocking_model_messages {
                    // CC yields this exact message into the transcript
                    // (stopHooks.ts:263) — it is `isMeta`, so the render list
                    // drops it at `shouldShowUserMessage` while the model half
                    // and the transcript both keep it. One message, not a
                    // model-only leg plus a visible row.
                    if event_tx
                        .send(QueryEvent::Message(blocking_model_message.clone()))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_streaming"));
                    }
                    model_messages.push(blocking_model_message);
                }
                max_output_tokens_override = None;
                stop_hook_active = Some(true);
                if transitions::Continue::with_reason("stop_hook_blocking")
                    .should_continue_query_loop()
                {
                    continue 'query_loop;
                }
            }

            turn_output_tokens += assistant_output_tokens(&assistant_messages);
            match token_budget::check_token_budget(
                &mut budget_tracker,
                None,
                params.token_budget,
                turn_output_tokens,
            ) {
                token_budget::TokenBudgetDecision::Continue(decision) => {
                    model_messages = messages_for_query.clone();
                    model_messages.extend(
                        assistant_messages
                            .iter()
                            .cloned()
                            .map(crate::types::message::Message::Assistant),
                    );
                    let nudge_user_message = crate::types::message::UserMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        // CC `createUserMessage({ content: decision.nudgeMessage,
                        // isMeta: true })` (query.ts:1324-1327).
                        content: vec![crate::types::message::UserContent::MetaText(
                            decision.nudge_message,
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
                    };
                    // CC `query.ts:1309-1340` — same shape as the
                    // max_output_tokens recovery message above: the nudge goes
                    // into the next `State.messages` and is never yielded.
                    model_messages.push(crate::types::message::Message::User(nudge_user_message));
                    max_output_tokens_override = None;
                    stop_hook_active = None;
                    if transitions::Continue::with_reason("token_budget_continuation")
                        .should_continue_query_loop()
                    {
                        continue 'query_loop;
                    }
                }
                token_budget::TokenBudgetDecision::Stop(_decision) => {
                    return Ok(transitions::Terminal::new("completed"));
                }
            }
        }

        // CC's StreamingToolExecutor owns one live ToolUseContext. If a
        // permission was answered before stream drain, its executor context
        // already contains the resulting Read/file/permission effects; do not
        // replace it with a loop-local reconstruction from the older actor
        // snapshots.
        let mut tool_use_context = streaming_tool_executor
            .as_ref()
            .filter(|_| query_config.gates.streaming_tool_execution)
            .map(|executor| executor.get_updated_context())
            .unwrap_or_else(|| {
                loop_local_tool_use_context(
                    &params,
                    &permission_context,
                    messages_for_query.clone(),
                    Some(query_tracking.clone()),
                    &resume_restore_stores,
                    content_replacement_state.clone(),
                    &abort_controller,
                )
            });
        // Still needed for the executor branch above, which returns a context
        // the executor built from its own snapshot; the `loop_local_...` branch
        // now sets this itself. Shared with `QueryHandle.abort_controller` so
        // the REPL can kill an in-flight tool while this loop is blocked in
        // `run_tools(...)`.
        tool_use_context.abort_controller = abort_controller.clone();
        if params.query_source.is_agent() {
            tool_use_context.agent_id = Some(params.turn_id.clone());
        }

        let mut should_prevent_continuation = false;
        while processed_tool_use_ids.len() < tool_use_blocks.len() {
            let remaining_tool_use_blocks = tool_use_blocks
                .iter()
                .filter(|tool_use| !processed_tool_use_ids.contains(&tool_use.id.0))
                .cloned()
                .collect::<Vec<_>>();
            if remaining_tool_use_blocks.is_empty() {
                break;
            }
            let processed_before = processed_tool_use_ids.len();
            let mut queued_permissions = Vec::new();
            tool_use_context.update_permission_context(permission_context.clone());
            tool_use_context.messages = messages_for_query.clone();
            install_tool_progress_sink(&mut tool_use_context, &event_tx);
            let using_streaming_tool_execution = query_config.gates.streaming_tool_execution;
            let updates = if using_streaming_tool_execution {
                // Maps to CC `query.ts` choosing
                // `streamingToolExecutor.getRemainingResults()` instead of
                // `runTools(...)` once `tengu_streaming_tool_execution2` is
                // enabled. The executor was populated as tool_use blocks
                // arrived during model streaming; sync actor-owned permission
                // state before draining queued work after any pause/resume.
                let mut updates = Vec::new();
                updates.append(&mut streaming_deferred_updates);
                if !updates.iter().any(|update| update.blocked_on_permission) {
                    if let Some(executor) = streaming_tool_executor.as_mut() {
                        executor.sync_tool_use_context(tool_use_context.clone());
                        let remaining_updates = executor.get_remaining_results();
                        tool_use_context = executor.get_updated_context();
                        updates.extend(remaining_updates);
                    }
                }
                updates
            } else {
                crate::services::tools::tool_orchestration::run_tools(
                    &remaining_tool_use_blocks,
                    &assistant_messages,
                    &tool_use_context,
                    &mut queued_permissions,
                )
            };

            for update in updates {
                // Do not reset `permission_context` from precomputed
                // non-streaming MessageUpdate values here. In the Rust actor
                // the interactive decision happens after `run_tools(...)`
                // yields its permission request, so the authoritative context
                // changes come from `check_permissions_and_call_tool(...)`
                // below. StreamingToolExecutor updates already include real
                // tool execution effects for preapproved tools, so mirror them.
                // Streaming updates have executed and own a fresh context.
                // Non-streaming `run_tools(...)` updates are precomputed gate
                // snapshots; adopting a later snapshot here would erase live
                // Read/Write/Edit effects produced while handling an earlier
                // request in this same batch.
                adopt_tool_update_context(
                    &mut tool_use_context,
                    &update,
                    using_streaming_tool_execution,
                );
                if emit_structured_output_update(&event_tx, &tool_use_context)
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_tools"));
                }
                if using_streaming_tool_execution {
                    permission_context = tool_use_context.tool_permission_context.clone();
                    if event_tx
                        .send(QueryEvent::PermissionContextUpdate(
                            permission_context.clone(),
                        ))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                }
                if let Some(progress) = update.progress {
                    if event_tx
                        .send(QueryEvent::ToolProgress(progress))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                }
                if let Some(message) = update.message {
                    if emit_tool_result_renderable_message(&event_tx, message)
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                }
                if let Some(user_message) = update.tool_result {
                    if let Some(tool_use_id) = model_tool_result_tool_use_id(&user_message) {
                        processed_tool_use_ids.insert(tool_use_id);
                    }
                    if event_tx
                        .send(QueryEvent::ModelMessage(Message::User(
                            user_message.clone(),
                        )))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                    tool_results.push(user_message);
                }
                if let Some(model_message) = update.model_message {
                    if matches!(
                        &model_message,
                        Message::Attachment(attachment)
                            if attachment.attachment_type() == "hook_stopped_continuation"
                    ) {
                        should_prevent_continuation = true;
                    }
                    if event_tx
                        .send(QueryEvent::ModelMessage(model_message.clone()))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                    append_tool_update_to_tool_results(model_message, &mut tool_results);
                }

                let Some(mut permission_request) = update.permission_request else {
                    continue;
                };

                let mut current_tool_use_context = tool_use_context.clone();
                current_tool_use_context.update_permission_context(permission_context.clone());
                current_tool_use_context.messages = messages_for_query.clone();
                let mut pre_tool_prevent_continuation = false;
                let mut pre_tool_stop_reason = None;
                let (forced_choice, should_ask) = if using_streaming_tool_execution {
                    // StreamingToolExecutor runs the pre-tool and permission-request
                    // hook seam before yielding an interactive permission update.
                    // Query only pauses/resumes the actor here; rerunning hooks
                    // would duplicate CC `toolExecution.ts` side effects.
                    (
                        update.forced_choice,
                        update.blocked_on_permission && update.forced_choice.is_none(),
                    )
                } else {
                    let pre_tool_prepare = crate::services::tools::tool_execution::prepare_permission_request_before_prompt(
                        &permission_request,
                        &current_tool_use_context,
                    )
                    .await;
                    pre_tool_prevent_continuation = pre_tool_prepare.prevent_continuation;
                    pre_tool_stop_reason = pre_tool_prepare.stop_reason.clone();
                    for hook_message in pre_tool_prepare.hook_messages {
                        if event_tx.send(QueryEvent::Row(hook_message)).await.is_err() {
                            return Ok(transitions::Terminal::new("aborted_tools"));
                        }
                    }
                    for model_message in pre_tool_prepare.hook_model_messages {
                        if event_tx
                            .send(QueryEvent::ModelMessage(model_message.clone()))
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_tools"));
                        }
                        append_tool_update_to_tool_results(model_message, &mut tool_results);
                    }
                    permission_request = pre_tool_prepare.request;
                    if !pre_tool_prepare.permission_updates.is_empty() {
                        permission_context =
                            crate::utils::permissions::permission_update::apply_permission_updates(
                                &permission_context,
                                &pre_tool_prepare.permission_updates,
                            );
                        current_tool_use_context
                            .update_permission_context(permission_context.clone());
                        if event_tx
                            .send(QueryEvent::PermissionContextUpdate(
                                permission_context.clone(),
                            ))
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_tools"));
                        }
                    }
                    let hook_choice =
                        crate::services::tools::tool_execution::merge_forced_permission_choices(
                            pre_tool_prepare.forced_choice,
                            update.forced_choice,
                        );
                    let assistant_for_tool = assistant_messages.iter().rev().find(|assistant| {
                        assistant_tool_use_blocks(assistant)
                            .iter()
                            .any(|tool_use| tool_use.id.0 == permission_request.tool_use_id)
                    });
                    let permission_error_identity = (
                        permission_request.tool_use_id.clone(),
                        permission_request.tool_name.clone(),
                    );
                    let required = crate::services::tools::tool_execution::apply_required_can_use_tool_after_hooks(
                        permission_request,
                        &current_tool_use_context,
                        assistant_for_tool,
                        hook_choice,
                        pre_tool_prepare.hook_supplied_updated_input,
                    )
                    .await;
                    let required = match required {
                        Ok(required) => required,
                        Err(error) => {
                            let failed = crate::services::tools::tool_execution::permission_check_error_result(
                                &permission_error_identity.0, &permission_error_identity.1, assistant_for_tool, &error,
                            );
                            if let Some(message) = failed.message {
                                if emit_tool_result_renderable_message(&event_tx, message)
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_tools"));
                                }
                            }
                            if let Some(user_message) = failed.tool_result {
                                processed_tool_use_ids.insert(permission_error_identity.0);
                                if event_tx
                                    .send(QueryEvent::ModelMessage(Message::User(
                                        user_message.clone(),
                                    )))
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_tools"));
                                }
                                tool_results.push(user_message);
                            }
                            continue;
                        }
                    };
                    permission_request = required.request;
                    if !required.permission_updates.is_empty() {
                        permission_context =
                            crate::utils::permissions::permission_update::apply_permission_updates(
                                &permission_context,
                                &required.permission_updates,
                            );
                        current_tool_use_context
                            .update_permission_context(permission_context.clone());
                        if event_tx
                            .send(QueryEvent::PermissionContextUpdate(
                                permission_context.clone(),
                            ))
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_tools"));
                        }
                    }
                    let mut forced_choice = required.forced_choice;
                    let mut should_ask = required.force_ask
                        || (update.blocked_on_permission
                            && forced_choice.is_none()
                            && crate::services::tools::tool_execution::should_ask_permission_request(
                                &permission_request,
                                &current_tool_use_context,
                            ));
                    if should_ask {
                        let prompt_prepare = crate::services::tools::tool_execution::prepare_permission_prompt_hooks(
                            &permission_request,
                            &current_tool_use_context,
                        )
                        .await;
                        for hook_message in prompt_prepare.hook_messages {
                            if event_tx.send(QueryEvent::Row(hook_message)).await.is_err() {
                                return Ok(transitions::Terminal::new("aborted_tools"));
                            }
                        }
                        for model_message in prompt_prepare.hook_model_messages {
                            if event_tx
                                .send(QueryEvent::ModelMessage(model_message.clone()))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_tools"));
                            }
                            append_tool_update_to_tool_results(model_message, &mut tool_results);
                        }
                        permission_request = prompt_prepare.request;
                        if !prompt_prepare.permission_updates.is_empty() {
                            permission_context = crate::utils::permissions::permission_update::apply_permission_updates(
                                &permission_context,
                                &prompt_prepare.permission_updates,
                            );
                            current_tool_use_context
                                .update_permission_context(permission_context.clone());
                            if event_tx
                                .send(QueryEvent::PermissionContextUpdate(
                                    permission_context.clone(),
                                ))
                                .await
                                .is_err()
                            {
                                return Ok(transitions::Terminal::new("aborted_tools"));
                            }
                        }
                        // CC PermissionContext.ts:319-335 and
                        // interactiveHandler.ts:417-429 resolve this Ask with
                        // the PermissionRequest decision itself. Only PreToolUse
                        // approvals take the earlier rule-check path.
                        // The producer's Glob/Grep reroute guard stays partial.
                        forced_choice = prompt_prepare.forced_choice;
                        should_ask = forced_choice.is_none() || prompt_prepare.force_ask;
                    }
                    (forced_choice, should_ask)
                };

                let prompt_already_sent = using_streaming_tool_execution
                    && streaming_permission_requests_sent.contains(&permission_request.tool_use_id);
                let channel_permission_relay = if should_ask && !prompt_already_sent {
                    crate::hooks::tool_permission::handlers::interactive_handler::start_channel_permission_relay(
                        &current_tool_use_context,
                        &permission_request,
                    )
                    .await
                } else {
                    None
                };

                if should_ask
                    && !prompt_already_sent
                    && event_tx
                        .send(QueryEvent::PermissionRequest(permission_request.clone()))
                        .await
                        .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_tools"));
                }

                let response = if should_ask {
                    if let Some(response) = if using_streaming_tool_execution {
                        streaming_pending_permission_decisions
                            .remove(&permission_request.tool_use_id)
                    } else {
                        None
                    } {
                        response
                    } else {
                        match wait_for_permission_decision(
                            &command_rx,
                            &permission_request.tool_use_id,
                            channel_permission_relay.as_ref(),
                        )
                        .await
                        {
                            PermissionWaitResult::Response(response) => response,
                            PermissionWaitResult::Abort => {
                                // Same ruling as the streaming-abort branch
                                // above: interrupting a tool that is still
                                // waiting for its permission answer fires no
                                // PostToolUseFailure, because CC's `catch`
                                // (`toolExecution.ts:1589`) starts after the
                                // permission decision (`:921`) at the `try` on
                                // `:1206`.
                                // CC never leaves a tool in the REPL's
                                // in-progress set after an interrupt: the
                                // aborted permission resolves the tool call,
                                // and `markToolUseAsComplete` follows every
                                // `runToolUse` (`toolOrchestration.ts:148,173`,
                                // `StreamingToolExecutor.ts:525`). This branch
                                // returns before that bookkeeping, so it
                                // completes the waiting tool and any still
                                // marked in progress here.
                                let mut still_in_progress: Vec<String> = tool_use_context
                                    .in_progress_tool_use_ids
                                    .iter()
                                    .chain(current_tool_use_context.in_progress_tool_use_ids.iter())
                                    .cloned()
                                    .chain(std::iter::once(permission_request.tool_use_id.clone()))
                                    .collect();
                                still_in_progress.sort_unstable();
                                still_in_progress.dedup();
                                for tool_use_id in &still_in_progress {
                                    tool_use_context.mark_complete(tool_use_id);
                                }
                                let unresolved_assistant_messages =
                                    assistant_messages_with_unresolved_tool_uses(
                                        &assistant_messages,
                                        &processed_tool_use_ids,
                                    );
                                for missing in crate::services::tools::tool_execution::yield_missing_tool_result_blocks(
                                    &unresolved_assistant_messages,
                                    "Interrupted by user",
                                ) {
                                    if event_tx
                                        .send(QueryEvent::Row(missing.message))
                                        .await
                                        .is_err()
                                    {
                                        return Ok(transitions::Terminal::new("aborted_tools"));
                                    }
                                    if event_tx
                                        .send(QueryEvent::ModelMessage(Message::User(
                                            missing.tool_result,
                                        )))
                                        .await
                                        .is_err()
                                    {
                                        return Ok(transitions::Terminal::new("aborted_tools"));
                                    }
                                }
                                // Maps to CC `query.ts` aborted-tools maxTurns
                                // branch: `nextTurnCountOnAbort = turnCount + 1`
                                // is checked before returning `{ reason:
                                // 'aborted_tools' }`.
                                let next_turn_count_on_abort = turn_count.saturating_add(1);
                                if let Some(max_turns) = params.max_turns {
                                    if next_turn_count_on_abort > max_turns {
                                        if let Err(terminal) = emit_max_turns_reached(
                                            &event_tx,
                                            &params.turn_id,
                                            max_turns,
                                            next_turn_count_on_abort,
                                            "aborted_tools",
                                        )
                                        .await
                                        {
                                            return Ok(terminal);
                                        }
                                    }
                                }
                                return Ok(transitions::Terminal::new("aborted_tools"));
                            }
                        }
                    }
                } else {
                    // No dialog ran, so the permission SYSTEM's decision is what
                    // resolves this tool use. CC reads that decision's own
                    // `message` at `toolExecution.ts:1023`; carry it across so a
                    // classifier/rule deny reaches the model with its real
                    // reason instead of the generic REJECT_MESSAGE.
                    PermissionPromptResponse::new(
                        forced_choice.unwrap_or(PermissionPromptChoice::AllowOnce),
                    )
                    .with_decision_message(permission_request.message.clone())
                };
                // Preserve the model/hook-final request here. The execution
                // owner applies `response.updated_input` after comparing it to
                // this request to derive FileEditOutput.userModified. Applying
                // it in the actor first makes every SDK/user amendment compare
                // equal to itself and incorrectly records `userModified=false`.

                if using_streaming_tool_execution {
                    if let Some(executor) = streaming_tool_executor.as_mut() {
                        current_tool_use_context
                            .update_permission_context(permission_context.clone());
                        current_tool_use_context.messages = messages_for_query.clone();
                        install_tool_progress_sink(&mut current_tool_use_context, &event_tx);
                        executor.sync_tool_use_context(current_tool_use_context.clone());
                        let continuation_updates = executor
                            .continue_after_permission(
                                permission_request.clone(),
                                response.clone(),
                                current_tool_use_context,
                            )
                            .await;
                        tool_use_context = executor.get_updated_context();
                        permission_context = tool_use_context.tool_permission_context.clone();
                        if event_tx
                            .send(QueryEvent::PermissionContextUpdate(
                                permission_context.clone(),
                            ))
                            .await
                            .is_err()
                        {
                            return Ok(transitions::Terminal::new("aborted_tools"));
                        }
                        for continuation_update in continuation_updates {
                            if let Some(progress) = continuation_update.progress {
                                if event_tx
                                    .send(QueryEvent::ToolProgress(progress))
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_tools"));
                                }
                            }
                            if let Some(message) = continuation_update.message {
                                if emit_tool_result_renderable_message(&event_tx, message)
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_tools"));
                                }
                            }
                            if let Some(user_message) = continuation_update.tool_result {
                                if let Some(tool_use_id) =
                                    model_tool_result_tool_use_id(&user_message)
                                {
                                    processed_tool_use_ids.insert(tool_use_id);
                                }
                                if event_tx
                                    .send(QueryEvent::ModelMessage(Message::User(
                                        user_message.clone(),
                                    )))
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_tools"));
                                }
                                tool_results.push(user_message);
                            }
                            if let Some(model_message) = continuation_update.model_message {
                                if matches!(
                                    &model_message,
                                    Message::Attachment(attachment)
                                        if attachment.attachment_type()
                                            == "hook_stopped_continuation"
                                ) {
                                    should_prevent_continuation = true;
                                }
                                if event_tx
                                    .send(QueryEvent::ModelMessage(model_message.clone()))
                                    .await
                                    .is_err()
                                {
                                    return Ok(transitions::Terminal::new("aborted_tools"));
                                }
                                append_tool_update_to_tool_results(
                                    model_message,
                                    &mut tool_results,
                                );
                            }
                        }
                        continue;
                    }
                }

                let parent_assistant_message = assistant_message_for_tool_use(
                    &assistant_messages,
                    &permission_request.tool_use_id,
                );
                let result = crate::services::tools::tool_execution::streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
                    &permission_request,
                    &response,
                    pre_tool_prevent_continuation,
                    pre_tool_stop_reason.as_deref(),
                    &current_tool_use_context,
                    parent_assistant_message,
                )
                .await;
                tool_use_context = result.new_context;
                tool_use_context.mark_complete(&permission_request.tool_use_id);
                if emit_structured_output_update(&event_tx, &tool_use_context)
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_tools"));
                }
                permission_context = tool_use_context.tool_permission_context.clone();
                if event_tx
                    .send(QueryEvent::PermissionContextUpdate(
                        permission_context.clone(),
                    ))
                    .await
                    .is_err()
                {
                    return Ok(transitions::Terminal::new("aborted_tools"));
                }

                // Preserve CC toolExecution.ts message ordering: pre-tool hook
                // messages before the primary tool_result; ToolResult.newMessages
                // and PostToolUse hook messages after it.
                for hook_message in result.pre_tool_messages {
                    if event_tx.send(QueryEvent::Row(hook_message)).await.is_err() {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                }
                for model_message in result.pre_tool_model_messages {
                    if event_tx
                        .send(QueryEvent::ModelMessage(model_message.clone()))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                    append_tool_update_to_tool_results(model_message, &mut tool_results);
                }

                if let Some(tool_result) = result.message {
                    if emit_tool_result_renderable_message(&event_tx, tool_result)
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                }

                // The model-visible primary tool_result precedes supplemental
                // Read media/newMessages. The renderable row was emitted just
                // above because CC renders it at the same primary-result
                // boundary; only the dual-projection transport is split here.
                if let Some(user_message) = result.tool_result {
                    if let Some(tool_use_id) = model_tool_result_tool_use_id(&user_message) {
                        processed_tool_use_ids.insert(tool_use_id);
                    }
                    if event_tx
                        .send(QueryEvent::ModelMessage(Message::User(
                            user_message.clone(),
                        )))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                    tool_results.push(user_message);
                }

                // CC appends successful PostToolUse results immediately after
                // the primary result, then appends ToolResult.newMessages.
                for trailing_message in result.post_tool_messages {
                    if event_tx
                        .send(QueryEvent::Row(trailing_message))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                }
                for model_message in result.post_tool_model_messages {
                    if matches!(
                        &model_message,
                        Message::Attachment(attachment)
                            if attachment.attachment_type() == "hook_stopped_continuation"
                    ) {
                        should_prevent_continuation = true;
                    }
                    if event_tx
                        .send(QueryEvent::ModelMessage(model_message.clone()))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                    append_tool_update_to_tool_results(model_message, &mut tool_results);
                }

                for new_message in result.new_messages {
                    if event_tx
                        .send(QueryEvent::ModelMessage(new_message.clone()))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                    // CC `toolExecution.ts:1566-1569` pushes `newMessages` onto
                    // the SAME `resultingMessages` stream as the tool result,
                    // so `query.ts:1391-1400` projects them identically.
                    // Routing attachments to a separate vec appended after the
                    // whole loop moved them past every later tool's result.
                    append_tool_update_to_tool_results(new_message, &mut tool_results);
                }
                for model_message in result.continuation_messages {
                    if matches!(
                        &model_message,
                        Message::Attachment(attachment)
                            if attachment.attachment_type() == "hook_stopped_continuation"
                    ) {
                        should_prevent_continuation = true;
                    }
                    if event_tx
                        .send(QueryEvent::ModelMessage(model_message.clone()))
                        .await
                        .is_err()
                    {
                        return Ok(transitions::Terminal::new("aborted_tools"));
                    }
                    append_tool_update_to_tool_results(model_message, &mut tool_results);
                }
            }

            if processed_tool_use_ids.len() == processed_before {
                return Ok(transitions::Terminal::new("aborted_tools"));
            }
        }

        params.tool_use_context.loaded_nested_memory_paths =
            tool_use_context.loaded_nested_memory_paths.clone();
        params.tool_use_context.nested_memory_attachment_triggers =
            tool_use_context.nested_memory_attachment_triggers.clone();
        params.tool_use_context.dynamic_skill_dir_triggers =
            tool_use_context.dynamic_skill_dir_triggers.clone();
        resume_restore_stores = tool_use_context.resume_restore_stores.clone();
        params.tool_use_context.resume_restore_stores = resume_restore_stores.clone();
        pending_tool_use_summary = start_pending_tool_use_summary(
            &query_config,
            &tool_use_blocks,
            &assistant_messages,
            &tool_results,
            &tool_use_context,
            &params.query_source,
            deps.uuid(),
        );

        // Maps to CC `query.ts` aborted-tools branch after `runTools(...)`:
        // if Ctrl-C or another owner aborted the shared `ToolUseContext` while
        // the actor was awaiting tool execution, return `aborted_tools` and
        // still surface the maxTurns attachment when the bounded continuation
        // would have exceeded the limit.
        if abort_controller.is_aborted() {
            // CC `query.ts:1499-1504` — `toolUse: true` here: the interrupt
            // landed while tools were running, so the model is told that is
            // why its tool_use blocks have no results. Yielded BEFORE the
            // maxTurns attachment (:1506-1513).
            if emit_user_interruption_message(&event_tx, &abort_controller, true)
                .await
                .is_err()
            {
                return Ok(transitions::Terminal::new("aborted_tools"));
            }
            let next_turn_count_on_abort = turn_count.saturating_add(1);
            if let Some(max_turns) = params.max_turns {
                if next_turn_count_on_abort > max_turns {
                    if let Err(terminal) = emit_max_turns_reached(
                        &event_tx,
                        &params.turn_id,
                        max_turns,
                        next_turn_count_on_abort,
                        "aborted_tools",
                    )
                    .await
                    {
                        return Ok(terminal);
                    }
                }
            }
            return Ok(transitions::Terminal::new("aborted_tools"));
        }

        // Maps to CC query.ts `shouldPreventContinuation`: the canonical
        // hook_stopped_continuation attachment is emitted after the primary
        // tool result, then the query terminates before post-tool attachments
        // or another API request. Publish mutable Read context first so a
        // stopped continuation cannot lose successful tool side effects.
        if should_prevent_continuation {
            if event_tx
                .send(QueryEvent::ToolContextUpdate(std::sync::Arc::new(
                    tool_use_context.clone(),
                )))
                .await
                .is_err()
            {
                return Ok(transitions::Terminal::new("aborted_tools"));
            }
            return Ok(transitions::Terminal::new("hook_stopped"));
        }

        max_output_tokens_recovery_count = 0;

        if let Some(tracking) = auto_compact_tracking.as_mut() {
            if tracking.compacted {
                // Maps to CC `tracking.turnCounter++` after tool execution and
                // before the recursive query-loop state update.
                tracking.turn_counter = tracking.turn_counter.saturating_add(1);
            }
        }

        // Maps to CC `query.ts:1560-1578`: drain the queued-command snapshot
        // for this iteration before the post-tool attachment pass. CC peeks
        // (`getCommandsByMaxPriority(sleepRan ? 'later' : 'next')`), filters,
        // hands the snapshot to getAttachmentMessages, then removes the same
        // consumed set by object identity (:1630-1643). The single predicate
        // dequeue in `drain_queued_commands_snapshot` takes exactly that set
        // in one step — commands outside it (other modes, other agents, slash
        // commands, higher priority buckets) stay queued like CC's
        // un-consumed snapshot remainder.
        let sleep_ran = tool_use_blocks
            .iter()
            .any(|block| block.name == crate::tools::sleep_tool::prompt::SLEEP_TOOL_NAME);
        let queued_commands_snapshot = drain_queued_commands_snapshot(
            sleep_ran,
            &params.query_source,
            tool_use_context.agent_id.as_deref(),
        );

        // Maps to CC `query.ts` post-tool `getAttachmentMessages(...)` pass,
        // including queued notifications, diagnostics, and deferred/agent/MCP
        // deltas before max-turns and recursive continuation assembly.
        let mut attachment_history = messages_for_query.clone();
        attachment_history.extend(
            assistant_messages
                .iter()
                .cloned()
                .map(crate::types::message::Message::Assistant),
        );
        attachment_history.extend(
            tool_results
                .iter()
                .cloned()
                .map(crate::types::message::Message::User),
        );
        attachment_history.extend(tool_result_attachments.iter().cloned());
        for attachment in crate::utils::attachments::get_attachments(
            crate::utils::attachments::GetAttachmentMessagesParams {
                tool_use_context: &mut tool_use_context,
                messages: &attachment_history,
                query_source: &params.query_source,
                queued_commands: queued_commands_snapshot,
            },
        )
        .await
        {
            // Batch D3: one whole `Message` (CC query.ts:1586-1594 `yield
            // attachment; toolResults.push(attachment)`).
            if event_tx
                .send(QueryEvent::Message(attachment.model_message.clone()))
                .await
                .is_err()
            {
                return Ok(transitions::Terminal::new("aborted_tools"));
            }
            tool_result_attachments.push(attachment.model_message);
        }

        // Attachment producers mutate the same live context in CC: nested
        // memory consumes triggers while adding loaded paths/read state, and
        // dynamic-skill collection drains its trigger set. Persist those
        // mutations before the next model turn instead of restoring the
        // pre-attachment snapshot.
        params.tool_use_context.loaded_nested_memory_paths =
            tool_use_context.loaded_nested_memory_paths.clone();
        params.tool_use_context.nested_memory_attachment_triggers =
            tool_use_context.nested_memory_attachment_triggers.clone();
        params.tool_use_context.dynamic_skill_dir_triggers =
            tool_use_context.dynamic_skill_dir_triggers.clone();
        resume_restore_stores = tool_use_context.resume_restore_stores.clone();
        params.tool_use_context.resume_restore_stores = resume_restore_stores.clone();
        if event_tx
            .send(QueryEvent::ToolContextUpdate(std::sync::Arc::new(
                tool_use_context.clone(),
            )))
            .await
            .is_err()
        {
            return Ok(transitions::Terminal::new("aborted_tools"));
        }

        let next_turn_count = turn_count.saturating_add(1);
        if let Some(max_turns) = params.max_turns {
            if next_turn_count > max_turns {
                if let Err(terminal) = emit_max_turns_reached(
                    &event_tx,
                    &params.turn_id,
                    max_turns,
                    next_turn_count,
                    "aborted_streaming",
                )
                .await
                {
                    return Ok(terminal);
                }
                return Ok(transitions::Terminal::new("max_turns"));
            }
        }

        // Maps to CC `messages = [...messagesForQuery, ...assistantMessages,
        // ...toolResults]` before continuing the query loop.
        model_messages = messages_for_query;
        model_messages.extend(
            assistant_messages
                .into_iter()
                .map(crate::types::message::Message::Assistant),
        );
        model_messages.extend(
            tool_results
                .into_iter()
                .map(crate::types::message::Message::User),
        );
        model_messages.extend(tool_result_attachments.into_iter());
        max_output_tokens_override = None;
        stop_hook_active = None;
        turn_count = next_turn_count;
        if transitions::Continue::with_reason("next_turn").should_continue_query_loop() {
            continue 'query_loop;
        }
    }
}

/// Maps to: CC `query.ts:1566-1578` — the mid-turn queued-command snapshot.
/// `sleep_ran` widens the drain from the 'next' bucket to 'later' (Sleep ran
/// this iteration, so lower-priority wake-up work may be consumed); the
/// thread gate mirrors the CC filter — the main thread (`repl_main_thread*`
/// / `sdk`) drains only unaddressed commands, everything else only its own
/// task-notifications. CC peeks, filters, and later removes the consumed set
/// by object identity (:1630-1643); this single predicate dequeue takes
/// exactly that set (INLINE modes ∩ thread filter ∩ non-slash) in one step,
/// leaving the remainder queued like CC's un-consumed snapshot.
fn drain_queued_commands_snapshot(
    sleep_ran: bool,
    query_source: &QuerySource,
    current_agent_id: Option<&str>,
) -> Vec<crate::utils::message_queue_manager::QueuedCommand> {
    use crate::utils::message_queue_manager as queue;
    let max_priority = if sleep_ran {
        queue::QueuePriority::Later
    } else {
        queue::QueuePriority::Next
    };
    // CC `isMainThread = querySource.startsWith('repl_main_thread') ||
    // querySource === 'sdk'` (:1567-1568); `QuerySource::Prompt` is the
    // `repl_main_thread*` family's enum projection.
    let is_main_thread = matches!(query_source, QuerySource::Prompt | QuerySource::Sdk);
    queue::dequeue_all_matching(|command| {
        if command.priority.rank_for_query() > max_priority.rank_for_query() {
            return false;
        }
        // CC :1573 `messageQueueManager.isSlashCommand(cmd)` exclusion — slash
        // commands must run through processSlashCommand after the turn, never
        // reach the model.  This manager predicate honors skipSlashCommands;
        // queueProcessor's private shape predicate intentionally does not.
        if queue::is_slash_command(command) {
            return false;
        }
        // Only INLINE_NOTIFICATION_MODES become attachments and get removed
        // (attachments.ts:1057-1059 + query.ts:1630-1643); other modes stay
        // queued for the post-turn queue processor.
        if !crate::utils::attachments::INLINE_NOTIFICATION_MODES.contains(&command.mode.as_str()) {
            return false;
        }
        if is_main_thread {
            // CC :1574 `cmd.agentId === undefined`.
            command.agent_id.is_none()
        } else {
            // CC :1576-1577: subagents only drain task-notifications
            // addressed to them — never user prompts.
            command.mode == "task-notification" && command.agent_id.as_deref() == current_agent_id
        }
    })
}

fn should_start_tool_use_summary(
    query_config: &config::QueryConfig,
    tool_use_blocks: &[crate::types::message::ToolUseBlock],
    tool_use_context: &crate::tool::ToolUseContext,
    query_source: &QuerySource,
) -> bool {
    // Maps to CC `query.ts` `config.gates.emitToolUseSummaries &&
    // toolUseBlocks.length > 0 && !abortController.signal.aborted &&
    // !toolUseContext.agentId`. Rust represents subagent turns by an agent
    // query source (`QuerySource::is_agent`, CC's `startsWith('agent:')`), so
    // those skip the mobile/SDK summary path.
    query_config.gates.emit_tool_use_summaries
        && !tool_use_blocks.is_empty()
        && !tool_use_context.abort_controller.is_aborted()
        && !query_source.is_agent()
}

#[cfg(not(test))]
fn start_pending_tool_use_summary(
    query_config: &config::QueryConfig,
    tool_use_blocks: &[crate::types::message::ToolUseBlock],
    assistant_messages: &[crate::types::message::AssistantMessage],
    tool_results: &[crate::types::message::UserMessage],
    tool_use_context: &crate::tool::ToolUseContext,
    query_source: &QuerySource,
    uuid: String,
) -> Option<tokio::task::JoinHandle<Option<crate::types::message::ToolUseSummaryMessage>>> {
    if !should_start_tool_use_summary(
        query_config,
        tool_use_blocks,
        tool_use_context,
        query_source,
    ) {
        return None;
    }

    let tool_use_ids = tool_use_blocks
        .iter()
        .map(|block| block.id.0.clone())
        .collect::<Vec<_>>();
    let params = crate::services::tool_use_summary::GenerateToolUseSummaryParams {
        tools: tool_info_for_summary(tool_use_blocks, tool_results),
        // Maps to CC `query.ts` forwarding
        // `toolUseContext.options.isNonInteractiveSession` into
        // `generateToolUseSummary(...)`.
        is_non_interactive_session: tool_use_context.is_non_interactive_session,
        last_assistant_text: last_assistant_text_for_tool_summary(assistant_messages),
    };

    Some(tokio::spawn(async move {
        crate::services::tool_use_summary::generate_tool_use_summary(params)
            .await
            .map(|summary| {
                crate::types::message::ToolUseSummaryMessage::new(summary, tool_use_ids, uuid)
            })
    }))
}

#[cfg(test)]
fn start_pending_tool_use_summary(
    query_config: &config::QueryConfig,
    tool_use_blocks: &[crate::types::message::ToolUseBlock],
    assistant_messages: &[crate::types::message::AssistantMessage],
    tool_results: &[crate::types::message::UserMessage],
    tool_use_context: &crate::tool::ToolUseContext,
    query_source: &QuerySource,
    uuid: String,
) -> Option<tokio::task::JoinHandle<Option<crate::types::message::ToolUseSummaryMessage>>> {
    // Unit tests must never perform incidental model I/O if a developer shell
    // has the official summary gate enabled. Pure helper tests cover the
    // control-flow predicates and prompt assembly; production uses the
    // non-test implementation above.
    let _ = (
        query_config,
        tool_use_blocks,
        assistant_messages,
        tool_results,
        tool_use_context,
        query_source,
        uuid,
    );
    None
}

fn tool_info_for_summary(
    tool_use_blocks: &[crate::types::message::ToolUseBlock],
    tool_results: &[crate::types::message::UserMessage],
) -> Vec<crate::services::tool_use_summary::ToolInfo> {
    // Maps to CC `query.ts` `toolInfoForSummary`: pair each tool_use block with
    // the matching `tool_result` content when present.
    tool_use_blocks
        .iter()
        .map(|block| {
            let output = tool_results.iter().find_map(|message| {
                message.content.iter().find_map(|content| match content {
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id == block.id =>
                    {
                        Some(serde_json::Value::String(result.content.clone()))
                    }
                    _ => None,
                })
            });
            crate::services::tool_use_summary::ToolInfo {
                name: block.name.clone(),
                input: block.input.clone(),
                output,
            }
        })
        .collect()
}

fn last_assistant_text_for_tool_summary(
    assistant_messages: &[crate::types::message::AssistantMessage],
) -> Option<String> {
    // Maps to CC `query.ts` extracting the last text block from the last
    // assistant message for `generateToolUseSummary({ lastAssistantText })`.
    assistant_messages.last().and_then(|message| {
        message
            .content
            .iter()
            .rev()
            .find_map(|content| match content {
                crate::types::message::AssistantContent::Text(text) => Some(text.clone()),
                _ => None,
            })
    })
}

fn microcompact_boundary_message(
    id: String,
    pending_cache_edits: &crate::services::compact::micro_compact::PendingCacheEdits,
    tokens_saved: u64,
) -> Message {
    // Maps to CC `query.ts:884` deferred
    // `yield createMicrocompactBoundaryMessage(...)` after cached microcompact
    // cache edits — a whole system Message. Official `Message.tsx` renders
    // this subtype as null; keeping the yield preserves session/log parity
    // without visible UI noise.
    Message::System(SystemMessage::MicrocompactBoundary {
        base: crate::types::message::SystemBase::with_uuid(id),
        // CC factory metadata shape (utils/messages.ts:4575-4581); the
        // deferred cached-microcompact site knows trigger/savings/ids.
        microcompact_metadata: Some(crate::types::message::MicrocompactMetadata {
            trigger: Some("auto".to_string()),
            pre_tokens: None,
            tokens_saved,
            compacted_tool_ids: pending_cache_edits.deleted_tool_ids.clone(),
            cleared_attachment_uuids: Vec::new(),
        }),
    })
}

/// Maps to CC `query.ts:1044-1050` / `:1499-1504` — the interrupt marker both
/// abort branches yield, behind the same guard.
///
/// The guard is CC's `signal.reason !== 'interrupt'`: a submit-interrupt is
/// followed immediately by the queued user message that caused it, and that
/// message already tells the model what happened. Only the REPL's
/// submit paths call `abort_with_reason("interrupt")` (`repl.rs:3816,4304`);
/// a plain Esc leaves the default reason and does produce the marker.
///
/// This is a whole `QueryEvent::Message`, not a `ModelMessage`: CC yields it,
/// so it renders, enters model history, and is written to the transcript.
async fn emit_user_interruption_message(
    event_tx: &QueryEventSender,
    abort_controller: &crate::tool::AbortController,
    tool_use: bool,
) -> Result<(), ()> {
    if abort_controller.reason().as_deref() == Some("interrupt") {
        return Ok(());
    }
    event_tx
        .send(QueryEvent::Message(Message::User(
            crate::utils::messages::create_user_interruption_message(tool_use),
        )))
        .await
        .map_err(|_| ())
}

/// Maps to CC `query.ts` `createAttachmentMessage({ type:
/// 'max_turns_reached', maxTurns, turnCount })` at both bounded-continuation
/// sites: the normal post-tool continuation guard and the aborted-tools guard.
async fn emit_max_turns_reached(
    event_tx: &QueryEventSender,
    turn_id: &str,
    max_turns: u32,
    turn_count: u32,
    aborted_reason_on_send_failure: &'static str,
) -> Result<(), transitions::Terminal> {
    // Batch D3: one whole `Message` (CC query.ts:1706-1710 `yield
    // createAttachmentMessage({ type: 'max_turns_reached', maxTurns,
    // turnCount })`). The typed value null-renders
    // (`nullRenderingAttachments`), exactly like CC.
    if event_tx
        .send(QueryEvent::Message(Message::Attachment(
            crate::types::message::AttachmentMessage {
                uuid: format!("{turn_id}-max-turns-reached"),
                timestamp: chrono::Utc::now(),
                attachment: Attachment::MaxTurnsReached {
                    max_turns,
                    turn_count,
                },
                // Built in-process, so there is no wire form to preserve.
                wire_payload: None,
            },
        )))
        .await
        .is_err()
    {
        return Err(transitions::Terminal::new(aborted_reason_on_send_failure));
    }
    Ok(())
}

fn system_api_error_is_max_output_tokens(
    error: &crate::types::message::SystemApiErrorMessage,
) -> bool {
    error.api_error == "max_output_tokens" || error.error == "max_output_tokens"
}

fn is_withheld_max_output_tokens(
    error: &Option<crate::types::message::SystemApiErrorMessage>,
) -> bool {
    // Maps to CC `query.ts` `isWithheldMaxOutputTokens(...)`: max-output API
    // errors are withheld during streaming and recovered before stop hooks.
    error
        .as_ref()
        .is_some_and(system_api_error_is_max_output_tokens)
}

fn should_escalate_max_output_tokens(current_override: Option<u32>) -> bool {
    should_escalate_max_output_tokens_with_gate(
        crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::MaxOutputTokensEscalation,
        ),
        current_override,
        crate::utils::process_env::var_os("CLAUDE_CODE_MAX_OUTPUT_TOKENS")
            .as_deref()
            .truthy()
            .is_some(),
    )
}

fn should_escalate_max_output_tokens_with_gate(
    cap_enabled: bool,
    current_override: Option<u32>,
    env_override_present: bool,
) -> bool {
    // Maps to CC `query.ts` `tengu_otk_slot_v1` max-output escalation guard.
    cap_enabled && current_override.is_none() && !env_override_present
}

fn should_persist_content_replacements(query_source: &QuerySource) -> bool {
    // Maps to CC `query.ts` `persistReplacements = querySource.startsWith('agent:') || querySource.startsWith('repl_main_thread')`.
    // Rust's current enum collapses concrete `agent:*` strings to
    // `QuerySource::Agent`, so treat both `agent` and future `agent:*` API
    // strings as persistence-eligible while the write helper remains a no-op.
    let source = query_source.as_api_source();
    source.starts_with("repl_main_thread") || source == "agent" || source.starts_with("agent:")
}

fn should_preempt_context_blocking_limit(
    query_source: &QuerySource,
    autocompact_compacted: bool,
) -> bool {
    let context_collapse_owns_overflow =
        crate::services::context_collapse::is_context_collapse_enabled()
            && crate::services::compact::auto_compact::is_auto_compact_enabled();
    should_preempt_context_blocking_limit_with_owners(
        query_source,
        autocompact_compacted,
        context_collapse_owns_overflow,
    )
}

fn should_preempt_context_blocking_limit_with_owners(
    query_source: &QuerySource,
    autocompact_compacted: bool,
    context_collapse_owns_overflow: bool,
) -> bool {
    // Maps to CC `query.ts` hard blocking-limit guard after autocompact: skip
    // the synthetic prompt-too-long preempt when the current call is itself a
    // compact/session-memory/marble-origami reducer. Those sources must reach
    // the model so they can shrink context instead of deadlocking on the guard.
    // Also skip when context-collapse owns recovery: upstream lets collapse see
    // the real API overflow instead of starving it with a synthetic preempt.
    if autocompact_compacted || context_collapse_owns_overflow {
        return false;
    }
    !matches!(
        query_source,
        QuerySource::Compact | QuerySource::SessionMemory | QuerySource::MarbleOrigami
    )
}

fn strip_signature_blocks_for_model_fallback_retry(
    messages_for_query: Vec<Message>,
) -> Vec<Message> {
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    ) {
        crate::utils::messages::strip_signature_blocks(messages_for_query)
    } else {
        messages_for_query
    }
}

fn model_fallback_from_error(
    error: &anyhow::Error,
) -> Option<&crate::services::api::with_retry::FallbackTriggeredError> {
    // Maps to: CC `query.ts` `innerError instanceof FallbackTriggeredError`
    // inside the `deps.callModel(...)` fallback retry loop. The API layer owns
    // retry detection; query only interprets the typed signal and retries the
    // request with clean model-bound thinking history.
    error.downcast_ref::<crate::services::api::with_retry::FallbackTriggeredError>()
}

fn model_fallback_warning_message(
    turn_id: &str,
    original_model: &str,
    fallback_model: &str,
) -> Message {
    // Maps to CC `query.ts:945-948` `yield createSystemMessage(
    // `Switched to ${fallback} due to high demand for ${original}`,
    // 'warning')` — a whole system `Message` in the one history (batch D3);
    // the API projection drops system members.
    Message::System(
        crate::types::message::SystemMessage::informational_with_uuid(
            format!("{turn_id}-model-fallback"),
            format!("Switched to {fallback_model} due to high demand for {original_model}"),
            SystemMessageLevel::Warning,
        ),
    )
}

fn prepare_call_model_messages(
    messages_for_query: Vec<Message>,
    user_context: &std::collections::BTreeMap<String, String>,
    strip_signature_blocks_for_fallback_retry: bool,
) -> Vec<Message> {
    // Maps to CC `query.ts` `deps.callModel({ messages:
    // prependUserContext(messagesForQuery, userContext), ... })`. The
    // `stripSignatureBlocks(messagesForQuery)` branch is only official on an
    // ant-internal model-fallback retry after `FallbackTriggeredError`; Rust
    // toggles this on the retry path owned by `query_loop(...)`, matching the
    // official placement immediately before `deps.callModel(...)`.
    let messages_for_query = if strip_signature_blocks_for_fallback_retry
        && crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Api,
        ) {
        crate::utils::messages::strip_signature_blocks(messages_for_query)
    } else {
        messages_for_query
    };
    crate::utils::api::prepend_user_context(messages_for_query, user_context)
}

/// The one projection every tool/hook update takes into model history.
///
/// Maps to: CC `query.ts:1391-1400`, which handles ALL of them — tool results,
/// `ToolResult.newMessages`, and hook `additionalContext` alike — with a single
/// line after `yield update.message`:
///
/// ```text
/// toolResults.push(
///   ...normalizeMessagesForAPI([update.message], tools).filter(_ => _.type === 'user'),
/// )
/// ```
///
/// `toolResults` is typed `(UserMessage | AttachmentMessage)[]`
/// (`query.ts:552`), and this is the only route into it during tool execution,
/// so every update lands in ONE sequence in arrival order — a tool's
/// `newMessages` sit next to that tool's result, not after every other tool's.
fn append_tool_update_to_tool_results(
    message: Message,
    tool_results: &mut Vec<crate::types::message::UserMessage>,
) {
    match message {
        Message::User(user) => tool_results.push(user),
        Message::Attachment(attachment) => {
            tool_results.extend(crate::utils::messages::normalize_attachment_for_api(
                &attachment,
                None,
            ));
        }
        // CC's `.filter(_ => _.type === 'user')` drops everything else: an
        // assistant/system message a hook or tool yields is display-only and
        // never reaches the next request. (Today every hook model message is
        // an Attachment — this keeps the contract if that changes.)
        _ => {}
    }
}

fn adopt_tool_update_context(
    current: &mut crate::tool::ToolUseContext,
    update: &crate::services::tools::tool_orchestration::MessageUpdate,
    using_streaming_tool_execution: bool,
) {
    if using_streaming_tool_execution {
        let live = current.clone();
        *current = update.new_context.clone();
        current.retain_file_read_handles_from(&live);
    }
}

fn loop_local_tool_use_context(
    params: &QueryParams,
    permission_context: &ToolPermissionContext,
    messages: Vec<Message>,
    query_tracking: Option<crate::tool::QueryChainTracking>,
    resume_restore_stores: &crate::utils::session_restore::ResumeRestoreStores,
    content_replacement_state: Option<crate::utils::tool_result_storage::ContentReplacementState>,
    abort_controller: &crate::tool::AbortController,
) -> crate::tool::ToolUseContext {
    // Clone the seeded ToolUseContext (CC QueryParams.toolUseContext), then
    // overlay loop-local permission / resume / content-replacement so
    // mid-turn mutations stay visible without rebuilding from flat shims.
    // Options already live on the seed (Batch 3d/3e).
    let mut context = params.tool_use_context(messages, query_tracking, content_replacement_state);
    context.tool_permission_context = permission_context.clone();
    // CC carries ONE `toolUseContext.abortController` through the whole query
    // (model, tools, compact, stop hooks), and that is what every
    // `signal.aborted` check reads. The Rust seed controller is the caller's:
    // `spawn_query` derives the actor's own as a CHILD of it (:427), and
    // aborting a child never marks its parent, so a context left holding the
    // seed can NEVER observe the REPL's Esc. Streaming and tool execution had
    // each patched this after the fact; compact and stop hooks had not, which
    // is what made the stop-hook abort check unreachable.
    context.abort_controller = abort_controller.clone();
    // Query-loop snapshots are persistence/reporting data only. Every loop
    // context keeps the seed's live cache handle, exactly like CC carrying one
    // mutable ToolUseContext object through model/tool/compact phases.
    context.resume_restore_stores = resume_restore_stores.clone();
    context
}

fn query_tool_use_context(
    permission_context: ToolPermissionContext,
    messages: Vec<Message>,
    query_tracking: Option<crate::tool::QueryChainTracking>,
    resume_restore_stores: &crate::utils::session_restore::ResumeRestoreStores,
    mcp_state: &crate::state::app_state_store::McpState,
    content_replacement_state: Option<crate::utils::tool_result_storage::ContentReplacementState>,
    app_store: &crate::tool::AppStoreRef,
) -> crate::tool::ToolUseContext {
    // Legacy/test builder. Production prefers seeded `QueryParams::tool_use_context`.
    let mut context = crate::tool::ToolUseContext::with_permission_context(permission_context)
        .with_non_interactive_session(crate::bootstrap::state::get_is_non_interactive_session())
        .with_main_loop_model(
            crate::utils::model::model::get_main_loop_model(),
        )
        .with_query_tracking(query_tracking)
        .with_resume_restore_stores(resume_restore_stores.clone())
        .with_mcp_state(mcp_state.clone())
        .with_content_replacement_state(content_replacement_state)
        .with_effort_value(crate::utils::effort::get_initial_effort_setting())
        .with_messages(messages);
    if app_store.store.is_some() {
        let isolate_writes = !app_store.writable || app_store.avoid_permission_prompts_overlay;
        if isolate_writes {
            context = context.with_app_store_handles(app_store.clone());
        } else if let Some(store) = app_store.store.clone() {
            context = context.with_app_store(store);
            context.app_store.writable = app_store.writable;
            context.app_store.avoid_permission_prompts_overlay =
                app_store.avoid_permission_prompts_overlay;
            if app_store.tasks_store.is_some() {
                context.app_store.tasks_store = app_store.tasks_store.clone();
            }
        }
    }
    if context.agent_id.is_none() {
        context.agent_id = crate::utils::teammate::get_agent_id();
    }
    context
}

fn build_call_model_request(
    messages: Vec<Message>,
    permission_context: ToolPermissionContext,
    query_source: &QuerySource,
    system_prompt: &[String],
    system_context: &std::collections::BTreeMap<String, String>,
    mcp_state: &crate::state::app_state_store::McpState,
    tool_use_context: &crate::tool::ToolUseContext,
) -> crate::query::deps::CallModelRequest {
    // Maps to CC `query.ts` building the `deps.callModel({ messages,
    // systemPrompt, thinkingConfig, tools, options })` request object at the
    // API-loop boundary. Options come from the seeded ToolUseContext (CC
    // `toolUseContext.options` / getAppState snapshots).
    // Maps to CC `query.ts`:
    // `currentModel = getRuntimeMainLoopModel({ mainLoopModel:
    //   toolUseContext.options.mainLoopModel, ... })`.
    // REPL seeds `mainLoopModel` via `useMainLoopModel()` (already resolved).
    let configured_main_loop_model = crate::utils::model::model::get_main_loop_model();
    let exceeds_200k_tokens = permission_context.mode
        == crate::types::permissions::PermissionMode::Plan
        && crate::utils::tokens::does_most_recent_assistant_message_exceed_200k(&messages);
    let main_loop_model = tool_use_context
        .main_loop_model
        .clone()
        .unwrap_or(configured_main_loop_model);
    let model = crate::utils::model::model::get_runtime_main_loop_model(
        permission_context.mode,
        main_loop_model,
        exceeds_200k_tokens,
    );
    let mut options =
        crate::services::api::claude::Options::new(model, query_source.as_api_source().to_string());
    options.query_source = query_source.as_api_source().to_string();
    // Maps to CC `services/api/claude.ts:697` `agentId?: AgentId // Only set
    // for subagents`, sourced the way `query.ts:588-590` sources the dump key:
    // `createDumpPromptsFetch(toolUseContext.agentId ?? config.sessionId)`.
    // Without this the port's per-agent dump file (`runAgent.ts:364`'s
    // "[Subagent X] API calls: …", `AgentTool/UI.tsx:423`) collapsed onto the
    // session id, which also made `clearDumpState(agentId)` — CC's lifecycle
    // `finally` — address an entry that never existed.
    options.agent_id = tool_use_context
        .agent_id
        .clone()
        .map(crate::types::ids::AgentId);
    options.skip_cache_write = Some(tool_use_context.skip_cache_write);
    options.is_non_interactive_session = tool_use_context.is_non_interactive_session;
    // Maps to CC `query.ts:685-686` `hasAppendSystemPrompt:
    // !!toolUseContext.options.appendSystemPrompt` — selects the Agent SDK
    // "Claude Code preset" prefix in `getCLISyspromptPrefix` for headless
    // sessions that append to the default prompt. `!!` treats an empty
    // string as absent.
    options.has_append_system_prompt = tool_use_context
        .append_system_prompt
        .as_deref()
        .is_some_and(|prompt| !prompt.is_empty());
    // Maps to CC `query.ts` `fallbackModel: toolUseContext.options.fallbackModel`.
    options.fallback_model = tool_use_context.fallback_model.clone();
    options.effort_value = tool_use_context
        .effort_value
        .clone()
        .or_else(crate::utils::effort::get_initial_effort_setting);
    options.fast_mode = tool_use_context
        .fast_mode
        .map(|enabled| enabled && crate::utils::fast_mode::is_fast_mode_enabled());
    // Maps to CC `query.ts` → `services/api/claude.ts` advisor option. Read
    // the live AppState slot so `/advisor` takes effect on the next turn;
    // headless contexts fall back to the merged settings snapshot.
    options.advisor_model = tool_use_context
        .get_app_state()
        .and_then(|state| state.advisor_model.clone())
        .or_else(|| crate::utils::advisor::get_initial_advisor_setting());
    // Maps to CC `query.ts:666-669` `getToolPermissionContext: async () =>
    // appState.toolPermissionContext` — resolved here as a snapshot of the
    // same live projection this request already builds its tool pool from;
    // no await sits between this snapshot and its serialization-time read
    // (see `Options::tool_permission_context`).
    options.tool_permission_context = Some(permission_context.clone());
    // Maps to CC `query.ts:682-684`:
    //   `agents: toolUseContext.options.agentDefinitions.activeAgents`
    //   `allowedAgentTypes: toolUseContext.options.agentDefinitions.allowedAgentTypes`.
    options.agents = tool_use_context.agent_definitions.active_agents.clone();
    options.allowed_agent_types = tool_use_context
        .agent_definitions
        .allowed_agent_types
        .clone();
    // Maps to CC `query.ts:689` `mcpTools: appState.mcp.tools` — the RAW,
    // un-deny-filtered `appState.mcp.tools`. CC declares the option
    // (`services/api/claude.ts:694`) and never reads it: every request-side tool
    // list in `claude.ts:1120-1236` derives from the `tools` PARAMETER alone.
    // The write is kept for Options shape parity; nothing may read it back.
    // Anything that chains this onto `tools` re-opens the `mcp__server` deny
    // bypass `assemble_tool_pool` closes (`tools.ts:352`) and duplicates every
    // MCP tool definition in the payload.
    options.mcp_tools = mcp_state.tools.clone();
    options.has_pending_mcp_servers = Some(crate::services::mcp::client::has_pending_mcp_servers(
        mcp_state,
    ));
    // Maps to CC `query.ts:660` `tools: toolUseContext.options.tools`, i.e. the
    // pool `useMergedTools` assembled (`hooks/useMergedTools.ts:30` →
    // `assembleToolPool`): built-ins plus DENY-FILTERED MCP tools. CC has no
    // fallback branch; every production Rust path seeds the pool before the
    // actor starts (`QueryParams::ensure_tool_use_context_ready`), so the empty
    // branch is only reachable from the `#[cfg(test)]` `query(...)` entrypoint
    // and direct unit calls. It must still yield the same complete pool —
    // `get_tools` is the built-in half only and would drop every MCP tool.
    let tools = if tool_use_context.tools.is_empty() {
        crate::tools::assemble_tool_pool(&permission_context, &mcp_state.tools)
    } else {
        tool_use_context.tools.clone()
    };
    // Maps to CC query.ts:449-451,659-661: even [] is caller-resolved.
    let system_prompt = crate::utils::api::append_system_context(system_prompt, system_context);
    // REPL always seeds thinking_config from ReplProps; fallback matches
    // main.tsx default construction (shouldEnableThinkingByDefault + env budget).
    let thinking_config = tool_use_context.thinking_config.clone().unwrap_or_else(|| {
        let settings = crate::utils::settings::get_initial_settings();
        crate::utils::thinking::production_thinking_config_from_env_and_settings(&settings)
    });

    crate::query::deps::CallModelRequest {
        messages,
        system_prompt,
        thinking_config,
        tools,
        options,
        permission_context,
        query_source: query_source.clone(),
    }
}

fn install_tool_progress_sink(
    context: &mut crate::tool::ToolUseContext,
    event_tx: &QueryEventSender,
) {
    // Forward live tool progress across the actor boundary.
    // Maps to: CC `runToolUse` `onToolProgress` closure
    // (toolExecution.ts:1216-1221); unbounded channel, so try_send
    // never blocks orchestration worker threads.
    let progress_tx = event_tx.clone();
    context.tool_progress_sink = crate::tool::ToolProgressSink(Some(std::sync::Arc::new(
        move |progress: crate::types::tools::ToolProgress| {
            let _ = progress_tx.try_send(QueryEvent::ToolProgress(progress));
        },
    )));

    // Maps to: CC `Tool.ts:227` `setInProgressToolUseIDs`, whose official
    // implementation is the REPL's `setState` (`REPL.tsx:1897`). The REPL
    // hands in its own setter (`apply_repl_query_turn_context`), which keeps
    // working after this query is replaced, as CC's does. Only a caller that
    // brought none gets the fallback that forwards over this query's event
    // channel.
    if !event_tx.is_pull() && context.set_in_progress_tool_use_ids.0.is_none() {
        let in_progress_tx = event_tx.clone();
        context.set_in_progress_tool_use_ids = crate::tool::SetInProgressToolUseIds(Some(
            std::sync::Arc::new(move |tool_use_id: &str, in_progress: bool| {
                let _ = in_progress_tx.try_send(QueryEvent::SetInProgressToolUse {
                    tool_use_id: tool_use_id.to_string(),
                    in_progress,
                });
            }),
        ));
    }

    // Maps to CC `ToolUseContext.setToolJSX` for the Agent swarm iTerm2 setup
    // prompt: the tool worker sends a UI request over the query event stream
    // and awaits the `It2SetupPrompt.onDone` result from REPL.
    let prompt_tx = event_tx.clone();
    context.it2_setup_prompt_sink =
        crate::tool::It2SetupPromptSink(Some(std::sync::Arc::new(move |tmux_available: bool| {
            let prompt_tx = prompt_tx.clone();
            Box::pin(async move {
                let (response_tx, response_rx) = async_channel::bounded(1);
                let request = crate::utils::swarm::it2_setup_prompt::It2SetupPromptRequest {
                    tmux_available,
                    responder: response_tx,
                };
                if prompt_tx
                    .send(QueryEvent::It2SetupPromptRequest(request))
                    .await
                    .is_err()
                {
                    return crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::Cancelled;
                }
                response_rx.recv().await.unwrap_or(
                    crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::Cancelled,
                )
            })
        })));
}

async fn emit_structured_output_update(
    event_tx: &QueryEventSender,
    context: &crate::tool::ToolUseContext,
) -> Result<(), async_channel::SendError<()>> {
    event_tx
        .send(QueryEvent::ToolContextUpdate(std::sync::Arc::new(
            context.clone(),
        )))
        .await?;
    if let Some(output) = context.structured_output.clone() {
        event_tx.send(QueryEvent::StructuredOutput(output)).await?;
    }
    Ok(())
}

async fn drain_streaming_executor_updates(
    executor: &mut crate::services::tools::streaming_tool_executor::StreamingToolExecutor,
    abort_controller: &crate::tool::AbortController,
    event_tx: &QueryEventSender,
    permission_context: &mut ToolPermissionContext,
    streaming_permission_requests_sent: &mut std::collections::HashSet<String>,
    streaming_deferred_updates: &mut Vec<crate::services::tools::tool_orchestration::MessageUpdate>,
    tool_results: &mut Vec<crate::types::message::UserMessage>,
    processed_tool_use_ids: &mut std::collections::HashSet<String>,
    streaming_deferred_model_messages: &mut Vec<crate::types::message::Message>,
) -> Option<transitions::Terminal> {
    if abort_controller.is_aborted() {
        return None;
    }
    for update in executor.get_completed_results() {
        // Interactive permission resumes through the Rust query actor after
        // model streaming completes, but the prompt itself can be surfaced as
        // soon as StreamingToolExecutor reaches it. QueryCommand decisions
        // received before the stream ends are stored and applied when draining
        // the deferred update.
        if let Some(permission_request) = update.permission_request.clone() {
            *permission_context = update.new_context.tool_permission_context.clone();
            if event_tx
                .send(QueryEvent::PermissionContextUpdate(
                    permission_context.clone(),
                ))
                .await
                .is_err()
            {
                return Some(transitions::Terminal::new("aborted_streaming"));
            }
            if let Some(message) = update.message.clone() {
                if emit_tool_result_renderable_message(event_tx, message)
                    .await
                    .is_err()
                {
                    return Some(transitions::Terminal::new("aborted_streaming"));
                }
            }
            if streaming_permission_requests_sent.insert(permission_request.tool_use_id.clone())
                && event_tx
                    .send(QueryEvent::PermissionRequest(permission_request))
                    .await
                    .is_err()
            {
                return Some(transitions::Terminal::new("aborted_streaming"));
            }
            streaming_deferred_updates.push(update);
            continue;
        }
        if let Some(terminal) = process_streaming_completed_tool_update(
            update,
            event_tx,
            permission_context,
            tool_results,
            processed_tool_use_ids,
            streaming_deferred_model_messages,
        )
        .await
        {
            return Some(terminal);
        }
    }
    None
}

async fn process_streaming_completed_tool_update(
    update: crate::services::tools::tool_orchestration::MessageUpdate,
    event_tx: &QueryEventSender,
    permission_context: &mut ToolPermissionContext,
    tool_results: &mut Vec<crate::types::message::UserMessage>,
    processed_tool_use_ids: &mut std::collections::HashSet<String>,
    streaming_deferred_model_messages: &mut Vec<crate::types::message::Message>,
) -> Option<transitions::Terminal> {
    // Maps to CC `query.ts` streaming loop draining
    // `streamingToolExecutor.getCompletedResults()` and yielding completed
    // tool_result messages before `getRemainingResults()`.
    *permission_context = update.new_context.tool_permission_context.clone();
    if emit_structured_output_update(event_tx, &update.new_context)
        .await
        .is_err()
    {
        return Some(transitions::Terminal::new("aborted_streaming"));
    }
    if event_tx
        .send(QueryEvent::PermissionContextUpdate(
            permission_context.clone(),
        ))
        .await
        .is_err()
    {
        return Some(transitions::Terminal::new("aborted_streaming"));
    }
    if let Some(progress) = update.progress {
        if event_tx
            .send(QueryEvent::ToolProgress(progress))
            .await
            .is_err()
        {
            return Some(transitions::Terminal::new("aborted_streaming"));
        }
    }
    if let Some(message) = update.message {
        if emit_tool_result_renderable_message(event_tx, message)
            .await
            .is_err()
        {
            return Some(transitions::Terminal::new("aborted_streaming"));
        }
    }
    if let Some(user_message) = update.tool_result {
        if let Some(tool_use_id) = model_tool_result_tool_use_id(&user_message) {
            processed_tool_use_ids.insert(tool_use_id);
        }
        tool_results.push(user_message);
    }
    if let Some(model_message) = update.model_message {
        // Maps to CC `toolExecution.ts` `ToolResult.newMessages`: streamed
        // tool execution may finish before the final assistant message is
        // committed to typed history, so defer non-primary model messages
        // until immediately after `assistantMessages` are yielded. User
        // messages join `tool_results` because the next API turn includes
        // normalized user/tool_result continuation blocks.
        match model_message {
            Message::User(user_message) => tool_results.push(user_message),
            Message::Attachment(attachment) => {
                tool_results.extend(crate::utils::messages::normalize_attachment_for_api(
                    &attachment,
                    None,
                ));
                // Preserve the typed attachment for transcript/event recovery;
                // the normalized User messages above are what the next API
                // request consumes.
                streaming_deferred_model_messages.push(Message::Attachment(attachment));
            }
            other => streaming_deferred_model_messages.push(other),
        }
    }
    None
}

/// Emit a tool-result transcript row. CC appends every result to history and
/// lets the render layer decide visibility (a tool without
/// `renderToolResultMessage`, or a missing/schema-rejected raw, renders null
/// — `UserToolSuccessMessage.tsx:72-103`); the Rust equivalent is the render
/// list's `transcript_tool_result_should_emit_ui` gate, so no pre-push
/// filtering happens here.
async fn emit_tool_result_renderable_message(
    event_tx: &QueryEventSender,
    message: RenderableMessage,
) -> Result<(), async_channel::SendError<()>> {
    // C3c-3 dual carrier: tool-execution `MessageUpdate` still splits row and
    // model tool_result; the row half bypasses history (QueryEvent::Row).
    event_tx.send(QueryEvent::Row(message)).await
}

fn partial_streaming_assistant_message(
    content: Vec<crate::types::message::AssistantContent>,
) -> crate::types::message::AssistantMessage {
    crate::types::message::AssistantMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content,
        model: None,
        stop_reason: Some(crate::types::message::StopReason::ToolUse),
        usage: None,
    }
}

fn partial_assistant_message_from_streamed_content(
    mut assistant_content: Vec<crate::types::message::AssistantContent>,
    streaming_text_buffer: &str,
    streaming_thinking_buffer: &str,
) -> Option<crate::types::message::AssistantMessage> {
    if !streaming_thinking_buffer.is_empty() {
        assistant_content.push(crate::types::message::AssistantContent::Thinking {
            text: streaming_thinking_buffer.to_string(),
            signature: String::new(),
        });
    }
    if !streaming_text_buffer.is_empty() {
        assistant_content.push(crate::types::message::AssistantContent::Text(
            streaming_text_buffer.to_string(),
        ));
    }
    if assistant_content.is_empty() {
        return None;
    }
    let stop_reason = if assistant_content
        .iter()
        .any(|content| matches!(content, crate::types::message::AssistantContent::ToolUse(_)))
    {
        crate::types::message::StopReason::ToolUse
    } else {
        crate::types::message::StopReason::EndTurn
    };
    Some(crate::types::message::AssistantMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: assistant_content,
        model: None,
        stop_reason: Some(stop_reason),
        usage: None,
    })
}

fn assistant_messages_with_unresolved_tool_uses(
    assistant_messages: &[crate::types::message::AssistantMessage],
    resolved_tool_use_ids: &std::collections::HashSet<String>,
) -> Vec<crate::types::message::AssistantMessage> {
    assistant_messages
        .iter()
        .filter_map(|message| {
            let content = message
                .content
                .iter()
                .filter_map(|content| match content {
                    crate::types::message::AssistantContent::ToolUse(tool_use)
                        if !resolved_tool_use_ids.contains(&tool_use.id.0) =>
                    {
                        Some(crate::types::message::AssistantContent::ToolUse(
                            tool_use.clone(),
                        ))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if content.is_empty() {
                None
            } else {
                let mut clone = message.clone();
                clone.content = content;
                Some(clone)
            }
        })
        .collect()
}

/// Maps to: CC `query.ts:713-718` — on streaming fallback every
/// already-yielded per-block assistant is re-yielded as
/// `{ type: 'tombstone', message: msg }` so the REPL removes it from the one
/// history and the transcript ("these partial messages (especially thinking
/// blocks) have invalid signatures").
async fn tombstone_streamed_assistant_messages(
    event_tx: &QueryEventSender,
    messages: &[Message],
) -> Result<(), async_channel::SendError<()>> {
    for message in messages {
        event_tx
            .send(QueryEvent::Tombstone(TombstoneMessage {
                message: message.clone(),
            }))
            .await?;
    }
    Ok(())
}

/// True when this exact block already traveled to the one history inside a
/// streamed per-block `QueryEvent::Message` this attempt. Used by the
/// abort/API-error flushes so nothing enters model history twice.
fn assistant_content_block_already_streamed(
    block: &crate::types::message::AssistantContent,
    streamed: &[Message],
) -> bool {
    streamed.iter().any(|message| match message {
        Message::Assistant(assistant) => assistant.content.iter().any(|existing| existing == block),
        _ => false,
    })
}

/// True when the retained assistant's history half already exists — either
/// the whole envelope was streamed (uuid match, the production shape) or every
/// non-identity block traveled as its own per-block message (legacy injected
/// seam where Content/CompletedContent items preceded the envelope).
fn assistant_message_already_streamed(
    message: &crate::types::message::AssistantMessage,
    streamed: &[Message],
) -> bool {
    if streamed.iter().any(|existing| {
        matches!(existing, Message::Assistant(assistant) if assistant.uuid == message.uuid)
    }) {
        return true;
    }
    let mut blocks = message
        .content
        .iter()
        .filter(|block| {
            !matches!(
                block,
                crate::types::message::AssistantContent::MessageIdentity(_)
            )
        })
        .peekable();
    if blocks.peek().is_none() {
        return false;
    }
    blocks.all(|block| assistant_content_block_already_streamed(block, streamed))
}

/// The accumulated content blocks that never traveled as their own streamed
/// per-block messages — the only content the abort/API-error partial flush may
/// still put into model history.
fn unstreamed_assistant_content(
    assistant_content: &[crate::types::message::AssistantContent],
    streamed: &[Message],
) -> Vec<crate::types::message::AssistantContent> {
    assistant_content
        .iter()
        .filter(|block| !assistant_content_block_already_streamed(block, streamed))
        .cloned()
        .collect()
}

fn assistant_tool_use_blocks(
    message: &crate::types::message::AssistantMessage,
) -> Vec<crate::types::message::ToolUseBlock> {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            crate::types::message::AssistantContent::ToolUse(tool_use) => Some(tool_use.clone()),
            _ => None,
        })
        .collect()
}

fn assistant_content_uuid(
    content: &crate::types::message::AssistantContent,
    fallback: String,
) -> String {
    match content {
        crate::types::message::AssistantContent::ToolUse(tool_use) => tool_use.id.0.clone(),
        _ => fallback,
    }
}

fn assistant_message_for_tool_use<'a>(
    assistant_messages: &'a [crate::types::message::AssistantMessage],
    tool_use_id: &str,
) -> Option<&'a crate::types::message::AssistantMessage> {
    assistant_messages.iter().find(|message| {
        message.content.iter().any(|content| match content {
            crate::types::message::AssistantContent::ToolUse(tool_use) => {
                tool_use.id.0 == tool_use_id
            }
            _ => false,
        })
    })
}

fn tool_use_block_by_id<'a>(
    tool_use_blocks: &'a [crate::types::message::ToolUseBlock],
    tool_use_id: &str,
) -> Option<&'a crate::types::message::ToolUseBlock> {
    tool_use_blocks
        .iter()
        .find(|tool_use| tool_use.id.0 == tool_use_id)
}

fn model_tool_result_tool_use_id(
    user_message: &crate::types::message::UserMessage,
) -> Option<String> {
    user_message
        .content
        .iter()
        .find_map(|content| match content {
            crate::types::message::UserContent::ToolResult(result) => {
                Some(result.tool_use_id.0.clone())
            }
            _ => None,
        })
}

async fn schedule_post_sampling_hooks(
    messages_for_query: &[Message],
    assistant_messages: &[crate::types::message::AssistantMessage],
    system_prompt: &[String],
    user_context: &std::collections::BTreeMap<String, String>,
    system_context: &std::collections::BTreeMap<String, String>,
    permission_context: ToolPermissionContext,
    query_source: QuerySource,
    query_tracking: Option<crate::tool::QueryChainTracking>,
    resume_restore_stores: &crate::utils::session_restore::ResumeRestoreStores,
    mcp_state: &crate::state::app_state_store::McpState,
) {
    // Maps to CC `query.ts` `void executePostSamplingHooks(...)`: execute
    // after model sampling completes and before stop hooks / tool execution.
    if assistant_messages.is_empty() {
        return;
    }
    let mut messages = messages_for_query.to_vec();
    messages.extend(
        assistant_messages
            .iter()
            .cloned()
            .map(crate::types::message::Message::Assistant),
    );
    let system_prompt = crate::utils::api::append_system_context(system_prompt, system_context);
    let tool_use_context = query_tool_use_context(
        permission_context,
        messages_for_query.to_vec(),
        query_tracking,
        resume_restore_stores,
        mcp_state,
        None,
        &crate::tool::AppStoreRef::default(),
    );
    let user_context = user_context.clone();
    let system_context = system_context.clone();

    #[cfg(not(test))]
    {
        tokio::spawn(async move {
            crate::utils::hooks::post_sampling_hooks::execute_post_sampling_hooks(
                messages,
                system_prompt,
                user_context,
                system_context,
                tool_use_context,
                Some(query_source),
            )
            .await;
        });
    }
    #[cfg(test)]
    {
        crate::utils::hooks::post_sampling_hooks::execute_post_sampling_hooks(
            messages,
            system_prompt,
            user_context,
            system_context,
            tool_use_context,
            Some(query_source),
        )
        .await;
    }
}

fn terminal_reason_for_system_api_error(
    error: &crate::types::message::SystemApiErrorMessage,
) -> &'static str {
    // Maps to CC `query.ts` no-tool-use withheld API-error exits after
    // recovery is unavailable: prompt-too-long surfaces as `prompt_too_long`,
    // media-size rejections surface as `image_error`, and other API/runtime
    // errors remain `model_error`.
    let mut raw = error.content.clone();
    raw.push('\n');
    raw.push_str(&error.api_error);
    if let Some(details) = &error.error_details {
        raw.push('\n');
        raw.push_str(details);
    }
    let lower = raw.to_ascii_lowercase();
    if error
        .content
        .starts_with(crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE)
        || lower.contains("prompt is too long")
    {
        return "prompt_too_long";
    }
    if crate::services::api::errors::is_media_size_error(&raw) {
        return "image_error";
    }
    "model_error"
}

fn schedule_stop_failure_hooks_for_api_error(
    error: crate::types::message::SystemApiErrorMessage,
    last_assistant_message: Option<String>,
) {
    // Maps to CC `query.ts` `void executeStopFailureHooks(...)`: fire-and-forget.
    #[cfg(not(test))]
    {
        tokio::spawn(async move {
            execute_stop_failure_hooks_for_api_error(&error, last_assistant_message.as_deref())
                .await;
        });
    }
    #[cfg(test)]
    {
        let _ = (error, last_assistant_message);
    }
}

#[cfg(not(test))]
async fn execute_stop_failure_hooks_for_api_error(
    error: &crate::types::message::SystemApiErrorMessage,
    last_assistant_message: Option<&str>,
) {
    // CC `executeStopFailureHooks` passes `getAppState: toolUseContext?.getAppState`
    // into `executeHooksOutsideREPL` (`utils/hooks.ts:3621-3626`), so session
    // hooks ARE merged — but the key is `getSessionId()`, not the agent id.
    // `:3599-3602` says why in as many words: "executeHooksOutsideREPL hardcodes
    // main sessionId (:2738). Agent frontmatter hooks (registerFrontmatterHooks)
    // key by agentId; gating with agentId here would pass the gate but fail
    // execution. Align gate with execution." Gate mirrors CC `:1516` + `:1541`.
    let config = crate::services::hooks::load_hooks_config_with_session_hooks(
        &crate::bootstrap::state::get_session_id(),
    );
    if config.is_empty() {
        return;
    }
    let error_match = if error.error.trim().is_empty() {
        "unknown"
    } else {
        error.error.as_str()
    };
    let _ = crate::services::hooks::lifecycle::execute_stop_failure_hooks(
        &config,
        error_match,
        error.error_details.as_deref().or(Some(&error.api_error)),
        last_assistant_message,
        Vec::new(),
    )
    .await;
}

fn claude_stream_item_to_assistant_content(
    item: crate::services::api::claude::ClaudeStreamItem,
) -> Option<crate::types::message::AssistantContent> {
    match item {
        crate::services::api::claude::ClaudeStreamItem::Text(text) => {
            Some(crate::types::message::AssistantContent::Text(text))
        }
        crate::services::api::claude::ClaudeStreamItem::Thinking { text, signature } => {
            Some(crate::types::message::AssistantContent::Thinking { text, signature })
        }
        crate::services::api::claude::ClaudeStreamItem::RedactedThinking(data) => {
            Some(crate::types::message::AssistantContent::RedactedThinking { data })
        }
        crate::services::api::claude::ClaudeStreamItem::ToolUse {
            id,
            name,
            input,
            is_server,
        } => {
            let block = crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(id),
                name,
                input,
            };
            if is_server {
                Some(crate::types::message::AssistantContent::ServerToolUse(
                    block,
                ))
            } else {
                Some(crate::types::message::AssistantContent::ToolUse(block))
            }
        }
        crate::services::api::claude::ClaudeStreamItem::WebSearchToolResult {
            tool_use_id,
            content,
        } => Some(
            crate::types::message::AssistantContent::WebSearchToolResult {
                tool_use_id: crate::types::ids::ToolUseId(tool_use_id),
                content,
            },
        ),
    }
}

enum PermissionWaitResult {
    Response(PermissionPromptResponse),
    Abort,
}

async fn wait_for_permission_decision(
    command_rx: &async_channel::Receiver<QueryCommand>,
    expected_tool_use_id: &str,
    channel_permission_relay: Option<
        &crate::hooks::tool_permission::handlers::interactive_handler::ChannelPermissionRelayRegistration,
    >,
) -> PermissionWaitResult {
    let mut channel_rx = channel_permission_relay.map(|relay| &relay.receiver);
    loop {
        let command = if let Some(receiver) = channel_rx {
            enum WaitEvent {
                Command(Result<QueryCommand, async_channel::RecvError>),
                Channel(Result<PermissionPromptResponse, async_channel::RecvError>),
            }
            match crate::utils::race(
                async { WaitEvent::Command(command_rx.recv().await) },
                async { WaitEvent::Channel(receiver.recv().await) },
            )
            .await
            {
                WaitEvent::Channel(Ok(response)) => {
                    return PermissionWaitResult::Response(response);
                }
                WaitEvent::Channel(Err(_)) => {
                    channel_rx = None;
                    continue;
                }
                WaitEvent::Command(command) => command,
            }
        } else {
            command_rx.recv().await
        };
        match command {
            Ok(QueryCommand::PermissionDecision {
                tool_use_id,
                choice,
            }) if tool_use_id == expected_tool_use_id => {
                return PermissionWaitResult::Response(PermissionPromptResponse::new(choice));
            }
            Ok(QueryCommand::PermissionResponse {
                tool_use_id,
                response,
            }) if tool_use_id == expected_tool_use_id => {
                return PermissionWaitResult::Response(response);
            }
            Ok(QueryCommand::Abort) | Err(_) => return PermissionWaitResult::Abort,
            Ok(_) => {}
        }
    }
}

fn last_assistant_text(messages: &[crate::types::message::AssistantMessage]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        let text = message
            .content
            .iter()
            .filter_map(|content| match content {
                crate::types::message::AssistantContent::Text(text) => Some(text.as_str()),
                crate::types::message::AssistantContent::Thinking { text, .. } => {
                    Some(text.as_str())
                }
                crate::types::message::AssistantContent::Advisor { content, .. } => content.text(),
                crate::types::message::AssistantContent::RedactedThinking { .. }
                | crate::types::message::AssistantContent::ToolUse(_)
                | crate::types::message::AssistantContent::ServerToolUse(_)
                | crate::types::message::AssistantContent::WebSearchToolResult { .. }
                | crate::types::message::AssistantContent::MessageIdentity(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    })
}

fn assistant_output_tokens(messages: &[crate::types::message::AssistantMessage]) -> i64 {
    // CC mutates final usage onto the last envelope after message_delta; earlier
    // content-block envelopes retain message_start usage and must not be summed.
    messages
        .iter()
        .rev()
        .find_map(|message| message.usage.as_ref())
        .map(|usage| usage.output_tokens as i64)
        .unwrap_or(0)
}

/// Maps the Rust `queryModelWithStreaming` channel back into source-level query
/// events.
///
/// This belongs in `query.rs` rather than `query/deps.rs` to mirror official
/// `query.ts`: the query loop consumes `deps.callModel(...)`, yields assistant
/// messages, records tool-use blocks, and only then proceeds to tool execution.
pub(crate) fn query_model_stream_items_to_pending_events(
    turn_id: &str,
    items: Vec<crate::services::api::claude::QueryModelStreamItem>,
) -> Vec<PendingQueryEvent> {
    let mut pending = Vec::new();
    let mut index = 0usize;
    // Production streaming emits one typed `Assistant` per
    // content_block_stop. `CompletedContent` remains only for injected legacy
    // deps; suppress its duplicate if a matching assistant follows.
    let mut completed_contents = Vec::<crate::types::message::AssistantContent>::new();

    for item in items {
        match item {
            crate::services::api::claude::QueryModelStreamItem::Assistant(message) => {
                let envelope_uuid = Some(message.uuid.clone());
                let mut envelope_content_index = 0usize;
                for content in message.content {
                    if let Some(position) = completed_contents
                        .iter()
                        .position(|completed| completed == &content)
                    {
                        completed_contents.remove(position);
                        continue;
                    }
                    if let Some(kind) = assistant_content_to_source_event_kind(content.clone()) {
                        let fallback = envelope_uuid
                            .as_deref()
                            .map(|uuid| {
                                if envelope_content_index == 0 {
                                    uuid.to_string()
                                } else {
                                    format!("{uuid}:{envelope_content_index}")
                                }
                            })
                            .unwrap_or_else(|| format!("sdk-{turn_id}-{index}"));
                        pending.push(PendingQueryEvent {
                            event: QuerySourceEvent {
                                uuid: assistant_content_uuid(&content, fallback),
                                kind,
                            },
                            delay: Duration::from_millis(0),
                            classifier_checking: None,
                        });
                        index += 1;
                    }
                    if !matches!(
                        content,
                        crate::types::message::AssistantContent::MessageIdentity(_)
                    ) {
                        envelope_content_index += 1;
                    }
                }
            }
            crate::services::api::claude::QueryModelStreamItem::SystemError(error) => {
                pending.push(sdk_error_event(turn_id, &error.content));
                index += 1;
            }
            crate::services::api::claude::QueryModelStreamItem::CompletedContent(item) => {
                if let Some(content) = claude_stream_item_to_assistant_content(item) {
                    completed_contents.push(content.clone());
                    if let Some(kind) = assistant_content_to_source_event_kind(content.clone()) {
                        pending.push(PendingQueryEvent {
                            event: QuerySourceEvent {
                                uuid: assistant_content_uuid(
                                    &content,
                                    format!("sdk-{turn_id}-{index}"),
                                ),
                                kind,
                            },
                            delay: Duration::from_millis(0),
                            classifier_checking: None,
                        });
                        index += 1;
                    }
                }
            }
            // Retry heartbeats are non-terminal system messages; side-query
            // consumers only extract assistant content (CC side queries drive
            // the same generator and ignore the SystemAPIErrorMessage yields).
            crate::services::api::claude::QueryModelStreamItem::SystemApiError(_)
            | crate::services::api::claude::QueryModelStreamItem::Stream(_)
            | crate::services::api::claude::QueryModelStreamItem::Content(_)
            | crate::services::api::claude::QueryModelStreamItem::AssistantDelta { .. }
            | crate::services::api::claude::QueryModelStreamItem::StreamingFallback
            | crate::services::api::claude::QueryModelStreamItem::ModelFallback { .. } => {}
        }
    }

    pending
}

fn assistant_content_to_source_event_kind(
    content: crate::types::message::AssistantContent,
) -> Option<QuerySourceEventKind> {
    assistant_content_to_source_event_kind_with_status(content, ToolUseStatus::Queued)
}

fn assistant_content_to_source_event_kind_with_status(
    content: crate::types::message::AssistantContent,
    tool_use_status: ToolUseStatus,
) -> Option<QuerySourceEventKind> {
    match content {
        crate::types::message::AssistantContent::Text(text) => {
            Some(QuerySourceEventKind::AssistantText {
                text,
                is_api_error_message: false,
            })
        }
        crate::types::message::AssistantContent::Thinking { text, .. } => {
            Some(QuerySourceEventKind::AssistantThinking {
                text,
                expanded: false,
            })
        }
        crate::types::message::AssistantContent::RedactedThinking { .. } => {
            Some(QuerySourceEventKind::AssistantRedactedThinking)
        }
        crate::types::message::AssistantContent::ToolUse(tool_use) => {
            // Nonvisual tool uses (ToolSearch, TodoWrite, … — empty
            // `userFacingName`) travel like every other block: CC appends the
            // whole message (REPL.tsx:3496) and `AssistantToolUseMessage`
            // returns null at render; the Rust equivalent is the render-list
            // filter (`assistant_row_is_nonvisual_tool_use`). Gating here
            // would also starve the filter's tool_use→name pairing, turning
            // the paired tool_result into an orphan row.
            let description = tool_use_description_for_current_transcript(
                tool_use.name.as_str(),
                &tool_use.input,
            );
            Some(QuerySourceEventKind::AssistantToolUse {
                tool_use_id: Some(tool_use.id.0),
                tool_name: tool_use.name,
                input: Some(tool_use.input),
                description,
                status: tool_use_status,
                progress_messages: Vec::new(),
            })
        }
        crate::types::message::AssistantContent::ServerToolUse(_)
        | crate::types::message::AssistantContent::WebSearchToolResult { .. }
        | crate::types::message::AssistantContent::MessageIdentity(_) => None,
        crate::types::message::AssistantContent::Advisor {
            tool_use_id,
            content,
        } => Some(QuerySourceEventKind::AssistantAdvisor {
            tool_use_id,
            content,
        }),
    }
}

fn tool_use_description_for_current_transcript(
    tool_name: &str,
    input: &serde_json::Value,
) -> String {
    // Official AssistantToolUseMessage receives the full `ToolUseBlockParam` and
    // lets each tool render itself; the row here carries `input` for exactly
    // that. `description` stays alongside it because row collapsing and
    // grouping in `components/messages_list.rs` read it as display text, so it
    // must be the rendered summary rather than a serialized payload.
    crate::components::messages::assistant_tool_use_message::render_tool_use_message(
        tool_name,
        input,
        crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
    )
    .unwrap_or_default()
}

fn tool_use_input_from_transcript_summary(tool_name: &str, description: &str) -> serde_json::Value {
    match tool_name {
        "Bash" | "PowerShell" => serde_json::json!({ "command": description }),
        "Read" => serde_json::json!({ "file_path": description }),
        _ => serde_json::from_str(description)
            .unwrap_or_else(|_| serde_json::json!({ "summary": description })),
    }
}

/// C3c-3: transcript user rows carry their whole model `UserMessage` —
/// typed history extends with those verbatim. This is an identity read, not a
/// projection; the old `renderable_messages_to_model_messages` back-projection
/// (assistant/boundary rehydration + adjacent-block merging, no CC
/// counterpart) retired with the whole-message seam: CC's history IS the
/// message array `processUserInput`/`query` produced, never a reconstruction
/// from rows. The lone guard mirrors CC: a tool_result block without an id
/// cannot be replayed to the API.
pub(crate) fn user_model_messages_from_rows(
    rows: &[RenderableMessage],
) -> Vec<crate::types::message::Message> {
    rows.iter()
        .filter_map(|row| match &row.kind {
            RenderableMessageKind::User { message, .. } => {
                let replayable = !message.content.iter().any(|block| {
                    matches!(
                        block,
                        UserContent::ToolResult(tool_result)
                            if tool_result.tool_use_id.0.is_empty()
                    )
                });
                replayable.then(|| Message::User(message.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Legacy-parameter shim for rows-only `QueryParams` (fixture tests, callers
/// predating REPL-owned typed history): user rows contribute their carried
/// messages; an explicit input seeds the prompt when no user row exists. Dead
/// on the production path — the REPL always passes accumulated typed history.
fn legacy_model_messages_from_params(params: &QueryParams) -> Vec<crate::types::message::Message> {
    let mut messages = user_model_messages_from_rows(&params.messages);
    if messages.is_empty() && !params.input.trim().is_empty() {
        messages.push(Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text(params.input.clone())],
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
    messages
}

/// Rust-only, no CC counterpart — `sdkError`/`sdk_error` does not exist at the
/// source. Governed by the named L1 contract `Query source event carrier`
/// (PORTING.md), which is why the message shape comes from the CC-named factory
/// rather than from a `Kind { .. }` literal here.
///
/// Reproduces CC `query.ts:974,990`: the catch block yields
/// `createAssistantAPIErrorMessage({ content: errorMessage })` — an ASSISTANT
/// row carrying `isApiErrorMessage: true`, not a system row of any kind.
///
/// This used to emit `SystemApiError` with `retry_attempt`/`max_retries`/
/// `retry_in_seconds` hardcoded to 0, and `SystemApiErrorMessage` renders its
/// retry line unconditionally, so every general error came out as
///
///   API error (502): retrying in 527ms (attempt 1/10) · Retrying in 0s (attempt 0/0)
///
/// — the retry facts twice, the second copy zeroed. `SystemApiError` is for
/// genuine retries only: CC calls `createSystemAPIErrorMessage` from exactly two
/// places, both `services/api/withRetry.ts:493,509`.
pub(crate) fn sdk_error_event(turn_id: &str, error: &str) -> PendingQueryEvent {
    // CC `query.ts:974,990` — `yield createAssistantAPIErrorMessage({ content })`.
    let message = crate::utils::messages::create_assistant_api_error_message(
        redact_error_text(error),
        None,
        None,
    );
    let text = message
        .content
        .iter()
        .find_map(|block| match block {
            crate::types::message::AssistantContent::Text(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let is_api_error_message = message.content.iter().any(|block| match block {
        crate::types::message::AssistantContent::MessageIdentity(identity) => {
            identity.is_api_error_message
        }
        _ => false,
    });
    PendingQueryEvent {
        event: QuerySourceEvent {
            uuid: format!("sdk-{turn_id}-error"),
            kind: QuerySourceEventKind::AssistantText {
                text,
                is_api_error_message,
            },
        },
        delay: Duration::from_millis(0),
        classifier_checking: None,
    }
}

/// Maps to: CC `query.ts:974,990` `yield createAssistantAPIErrorMessage({ content })`.
///
/// CC yields ONE value and it lands in `messages` (`REPL.tsx:3496`
/// `setMessages(old => [...old, newMessage])`) — the same array that becomes
/// the next turn's model history. The query loop depends on that:
/// `query.ts:1262` reads `lastMessage?.isApiErrorMessage` to skip stop hooks,
/// "the model never produced a real response — hooks evaluating it create a
/// death spiral: error → hook blocking → retry → error → …".
///
/// C3c-3: this is now the single CC yield — one `QueryEvent::Message`
/// carrying the whole assistant message. Receiving edges treat it as model
/// history and (in the interactive REPL) derive the transcript row via
/// `normalize_messages`, exactly the CC data flow. The uuid keeps the stable
/// `sdk-{turn}-error` identity the transcript row carried before the
/// dual-carrier collapse.
pub(crate) async fn send_assistant_api_error_message(
    event_tx: &QueryEventSender,
    turn_id: &str,
    error: &str,
) -> Result<(), ()> {
    let mut message = crate::utils::messages::create_assistant_api_error_message(
        redact_error_text(error),
        None,
        None,
    );
    message.uuid = format!("sdk-{turn_id}-error");
    event_tx
        .send(QueryEvent::Message(Message::Assistant(message)))
        .await
        .map_err(|_| ())
}

fn redact_error_text(text: &str) -> String {
    let mut sanitized = text.replace('\n', " ");
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        sanitized = sanitized.replace(key, "<redacted>");
        if let Some(secret) = crate::utils::process_env::var(key) {
            let trimmed = secret.trim();
            if !trimmed.is_empty() {
                sanitized = sanitized.replace(trimmed, "<redacted>");
            }
        }
    }
    sanitized
}

/// The query seam's yield value (C3c-3).
///
/// Maps to: CC `query.ts` yielding whole `Message` values
/// (`types/message.ts:120-146`). `Row` covers the payloads that cannot ride
/// the flat model structs yet — see the [`QueryEvent::Row`] doc for the
/// recorded reasons.
#[derive(Clone, Debug, PartialEq)]
pub enum SeamMessage {
    Message(Message),
    Row(RenderableMessage),
}

impl SeamMessage {
    /// Render projection: `normalize_messages` for whole messages (CC
    /// `utils/messages.ts:741-820`, the same projection the REPL applies at
    /// its receiving edge), identity for rows. Used by the legacy `query()`
    /// compatibility seam (mock pending rows stay render-only); the live
    /// streaming path emits whole messages instead.
    pub fn into_rows(self) -> Vec<RenderableMessage> {
        match self {
            SeamMessage::Message(message) => {
                crate::utils::messages::normalize_messages(std::slice::from_ref(&message))
            }
            SeamMessage::Row(row) => vec![row],
        }
    }
}

impl QuerySourceEvent {
    /// C3c-3: the seam yields whole model `Message` values, mirroring CC
    /// `query.ts` (whose generator yields `Message`). The event uuid becomes
    /// the message uuid — row id == message id for single-block messages, so
    /// `normalize_messages` reproduces the previous row shape exactly.
    pub fn into_message(self) -> SeamMessage {
        let timestamp = chrono::Utc::now();
        let assistant_message =
            |uuid: String, content: Vec<crate::types::message::AssistantContent>| {
                SeamMessage::Message(Message::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid,
                        timestamp,
                        content,
                        model: None,
                        stop_reason: None,
                        usage: None,
                    },
                ))
            };
        match self.kind {
            QuerySourceEventKind::AssistantText {
                text,
                is_api_error_message,
            } => {
                // Maps to: CC yielding the AssistantMessage itself;
                // `isApiErrorMessage` is a message-level field, carried here as
                // the `MessageIdentity` sibling block the renderer derives it
                // from (`normalize_messages` rides it on every split row).
                let mut content = vec![crate::types::message::AssistantContent::Text(text)];
                if is_api_error_message {
                    content.push(crate::types::message::AssistantContent::MessageIdentity(
                        crate::types::message::AssistantMessageIdentity {
                            // No uuid: `self.uuid` already rides the envelope on
                            // the AssistantMessage this block is pushed into.
                            is_api_error_message: true,
                            ..Default::default()
                        },
                    ));
                }
                assistant_message(self.uuid, content)
            }
            QuerySourceEventKind::AssistantThinking { text, expanded: _ } => assistant_message(
                self.uuid,
                vec![crate::types::message::AssistantContent::Thinking {
                    text,
                    signature: String::new(),
                }],
            ),
            QuerySourceEventKind::AssistantRedactedThinking => assistant_message(
                self.uuid,
                vec![crate::types::message::AssistantContent::RedactedThinking {
                    data: String::new(),
                }],
            ),
            // `description` and the transparent-progress DTO die at this seam:
            // both are render-time derivations now (render_tool_use_message and
            // the registry's isTransparentWrapper lookup respectively).
            QuerySourceEventKind::AssistantToolUse {
                tool_use_id,
                tool_name,
                input,
                description: _,
                status: _,
                progress_messages: _,
            } => {
                let content = vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId(
                            tool_use_id.unwrap_or_else(|| self.uuid.clone()),
                        ),
                        name: tool_name,
                        input: input.unwrap_or(serde_json::Value::Null),
                    },
                )];
                assistant_message(self.uuid, content)
            }
            QuerySourceEventKind::AssistantTransparentToolUseProgress {
                tool_use_id,
                tool_name,
                progress_output: _,
                progress_status: _,
            } => {
                let content = vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId(
                            tool_use_id.unwrap_or_else(|| self.uuid.clone()),
                        ),
                        name: tool_name,
                        input: serde_json::Value::Null,
                    },
                )];
                assistant_message(self.uuid, content)
            }
            QuerySourceEventKind::AssistantAdvisor {
                tool_use_id,
                content,
            } => assistant_message(
                self.uuid,
                vec![crate::types::message::AssistantContent::Advisor {
                    tool_use_id,
                    content,
                }],
            ),

            QuerySourceEventKind::GroupedToolUse {
                tool_name,
                messages,
                results,
            } => {
                // CC `RenderableMessage`-widening member
                // (types/message.ts:140-146), not a `Message` member — always
                // a row.
                SeamMessage::Row(RenderableMessage {
                    uuid: self.uuid,
                    kind: RenderableMessageKind::GroupedToolUse(GroupedToolUseMessage {
                        tool_name,
                        messages,
                        results,
                    }),
                })
            }
        }
    }
}

/// A mock query event after the query seam maps source data to message rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQueryMessage {
    pub message: RenderableMessage,
    pub delay: Duration,
    pub classifier_checking: Option<ClassifierChecking>,
}

/// Legacy synchronous query entrypoint for tests/compatibility.
/// Maps to: CC `query.ts` `async function* query(...)`; the production REPL
/// path consumes [`spawn_query`] instead. Even this compatibility path now uses
/// the official `QueryDeps.callModel` seam rather than a separate mock-only
/// dependency method.
pub fn query<D>(deps: &D, params: &QueryParams) -> Vec<PendingQueryMessage>
where
    D: crate::query::deps::QueryDeps,
{
    match run_query_once_blocking(params.clone(), deps.clone()) {
        Ok(events) => events,
        Err(error) => vec![sdk_error_event(&params.turn_id, &error.to_string())],
    }
    .into_iter()
    .flat_map(|pending| {
        // C3c-3: the seam yields whole messages; this compatibility path
        // projects them to rows here (`normalize_messages`), matching what
        // the interactive REPL does at its receiving edge.
        pending
            .event
            .into_message()
            .into_rows()
            .into_iter()
            .map(move |message| PendingQueryMessage {
                message,
                delay: pending.delay,
                classifier_checking: pending.classifier_checking.clone(),
            })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Wall-clock ceiling for "the actor must terminate", not a latency
    /// assertion.
    ///
    /// These three sites used a bare 1s. That is a budget for REAL WORK — the
    /// actor drives compaction, tool execution and a mock stream — and `just
    /// test` runs up to 16 test processes at once, so under load the actor
    /// simply had not finished when the timeout fired. The failure then looked
    /// like a logic bug ("actor should finish") while the machine was merely
    /// busy, which is why these two sat in the known-failures list.
    ///
    /// 30s keeps the guard against a genuine hang — the tests it covers finish
    /// in ~0.3s unloaded — without encoding an assumption about how much CPU
    /// the run has to itself.
    const QUERY_ACTOR_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    fn queued(value: &str, mode: &str) -> crate::utils::message_queue_manager::QueuedCommand {
        crate::utils::message_queue_manager::QueuedCommand::new(value, mode)
    }

    /// Maps to: CC `query.ts:1566-1571` — without Sleep in the turn's
    /// tool_use blocks the drain peeks only up to the 'next' bucket, so
    /// 'later'-priority commands stay queued for the post-turn processor.
    /// (External builds never register Sleep, so `sleepRan` is always false
    /// there — 'next' is the only reachable threshold.)
    #[test]
    fn drain_snapshot_leaves_later_bucket_unless_sleep_ran() {
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        let mut later = queued("<task-notification/>", "task-notification");
        later.priority = crate::utils::message_queue_manager::QueuePriority::Later;
        later.skip_slash_commands = true;
        crate::utils::message_queue_manager::enqueue_pending_notification(later);

        let without_sleep = drain_queued_commands_snapshot(false, &QuerySource::Prompt, None);
        assert!(without_sleep.is_empty());
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            1,
            "'later' command must stay queued when Sleep did not run"
        );

        let with_sleep = drain_queued_commands_snapshot(true, &QuerySource::Prompt, None);
        assert_eq!(with_sleep.len(), 1);
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            0
        );
        crate::utils::message_queue_manager::clear_command_queue();
    }

    /// Maps to: CC `query.ts:1572-1577` — the main thread drains only
    /// unaddressed commands (`agentId === undefined`); subagents drain only
    /// task-notifications addressed to them, never prompts. Slash commands
    /// are excluded everywhere (:1573).
    #[test]
    fn drain_snapshot_gates_by_thread_and_excludes_slash_commands() {
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();

        // Plain `enqueue` keeps the constructor's 'next' priority — the
        // priority threshold dimension is covered by the test above;
        // this one exercises the thread gate at a reachable bucket.
        let mut addressed = queued("<task-notification/>", "task-notification");
        addressed.agent_id = Some("agent-a".to_string());
        addressed.skip_slash_commands = true;
        crate::utils::message_queue_manager::enqueue(addressed);
        crate::utils::message_queue_manager::enqueue(queued("/compact", "prompt"));
        crate::utils::message_queue_manager::enqueue(queued("plain follow-up", "prompt"));

        let main_thread = drain_queued_commands_snapshot(false, &QuerySource::Prompt, None);
        assert_eq!(
            main_thread
                .iter()
                .map(|command| command.value.as_str())
                .collect::<Vec<_>>(),
            vec!["plain follow-up"],
            "main thread must not swallow addressed commands or slash commands"
        );

        // The addressed task-notification is only drained by its own agent;
        // a prompt-mode command never reaches a subagent loop.
        let foreign_agent =
            drain_queued_commands_snapshot(false, &QuerySource::Agent, Some("agent-b"));
        assert!(foreign_agent.is_empty());
        let own_agent = drain_queued_commands_snapshot(false, &QuerySource::Agent, Some("agent-a"));
        assert_eq!(own_agent.len(), 1);
        assert_eq!(own_agent[0].agent_id.as_deref(), Some("agent-a"));

        // The slash command is the only survivor — left for
        // processSlashCommand after the turn, exactly like CC.
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            1
        );
        crate::utils::message_queue_manager::clear_command_queue();
    }

    #[test]
    fn drain_snapshot_uses_manager_slash_predicate_for_bridge_text() {
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        let mut bridge = queued("/bridge-text", "task-notification");
        bridge.skip_slash_commands = true;
        crate::utils::message_queue_manager::enqueue(bridge);

        let drained = drain_queued_commands_snapshot(false, &QuerySource::Prompt, None);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].value, "/bridge-text");
        crate::utils::message_queue_manager::clear_command_queue();
    }

    /// Maps to: CC `REPL.tsx:3496` append-all — a nonvisual tool_use block
    /// (ToolSearch, TodoWrite, …) still yields a seam event and lands in
    /// history; hiding is the render-list filter's job. Gating it at the seam
    /// starved the filter's tool_use→name pairing and turned the paired
    /// tool_result into an orphan row, so live and cold rendered different
    /// row counts for the same session.
    #[test]
    fn nonvisual_tool_use_travels_through_the_seam_and_hides_at_the_filter() {
        let tool_use =
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu-tool-search".to_string()),
                name: "ToolSearch".to_string(),
                input: serde_json::json!({"query": "select:Read"}),
            });
        // The seam yields an event for the nonvisual block instead of None.
        let kind = assistant_content_to_source_event_kind(tool_use.clone())
            .expect("nonvisual tool_use must travel like every other block");
        let rows = QuerySourceEvent {
            uuid: "row-tool-search-use".to_string(),
            kind,
        }
        .into_message()
        .into_rows();
        assert_eq!(rows.len(), 1, "one history row for the tool_use block");

        // Its paired success result (nonvisual tool → CC renders null).
        let result_row = RenderableMessage::user_block(
            "row-tool-search-result",
            crate::types::message::UserContent::ToolResult(crate::types::message::ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu-tool-search".to_string()),
                content: "found".to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: Some(serde_json::json!({"tools": []})),
            }),
        );
        let mut history = rows;
        history.push(result_row);

        // Both rows survive in history (append-all) and both disappear at the
        // render-list filter: the tool_use via the nonvisual drop, the result
        // via the emit gate — which can only resolve "ToolSearch" because the
        // tool_use row made it into the same batch.
        let rendered =
            crate::components::messages_list::filter_non_rendering_messages(history.clone(), false);
        assert_eq!(history.len(), 2);
        assert!(
            rendered.is_empty(),
            "both rows hide at render, neither is an orphan: {rendered:?}"
        );
    }

    use crate::types::message::{
        AssistantContent, RenderableMessageKind, ToolResultStatus, ToolUseProgressMessage,
        ToolUseStatus,
    };

    /// A2.3 test extractors: rows carry the real `AssistantMessage`; these read
    /// the row's first non-identity block the way the renderer does.
    fn row_assistant_text(message: &RenderableMessage) -> Option<&str> {
        match &message.kind {
            RenderableMessageKind::Assistant { message } => match message.first_content_block() {
                Some(AssistantContent::Text(text)) => Some(text.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    fn row_assistant_tool_use(
        message: &RenderableMessage,
    ) -> Option<&crate::types::message::ToolUseBlock> {
        match &message.kind {
            RenderableMessageKind::Assistant { message } => match message.first_content_block() {
                Some(AssistantContent::ToolUse(tool_use)) => Some(tool_use),
                _ => None,
            },
            _ => None,
        }
    }

    fn row_is_api_error(message: &RenderableMessage) -> bool {
        matches!(
            &message.kind,
            RenderableMessageKind::Assistant { message, .. }
                if message.content.iter().any(|block| matches!(
                    block,
                    AssistantContent::MessageIdentity(identity)
                        if identity.is_api_error_message
                ))
        )
    }

    /// Test extractors: user rows carry the real `UserMessage`; these read
    /// the row's first block the way the renderer does.
    fn row_user_tool_result(
        message: &RenderableMessage,
    ) -> Option<&crate::types::message::ToolResult> {
        match &message.kind {
            RenderableMessageKind::User { message } => match message.first_content_block() {
                Some(UserContent::ToolResult(tool_result)) => Some(tool_result),
                _ => None,
            },
            _ => None,
        }
    }

    /// C3c-3 test extractors: `QueryEvent::Message` carries a whole model
    /// `Message`; these read the assistant content directly.
    fn model_assistant_text(message: &Message) -> Option<&str> {
        match message {
            Message::Assistant(assistant) => {
                assistant.content.iter().find_map(|block| match block {
                    AssistantContent::Text(text) => Some(text.as_str()),
                    _ => None,
                })
            }
            _ => None,
        }
    }

    fn model_assistant_tool_use(message: &Message) -> Option<&crate::types::message::ToolUseBlock> {
        match message {
            Message::Assistant(assistant) => {
                assistant.content.iter().find_map(|block| match block {
                    AssistantContent::ToolUse(tool_use) => Some(tool_use),
                    _ => None,
                })
            }
            _ => None,
        }
    }

    fn model_is_api_error(message: &Message) -> bool {
        matches!(
            message,
            Message::Assistant(assistant)
                if assistant.content.iter().any(|block| matches!(
                    block,
                    AssistantContent::MessageIdentity(identity)
                        if identity.is_api_error_message
                ))
        )
    }

    #[test]
    fn query_actor_abort_handle_preserves_seed_context_parent_linkage() {
        let seed = crate::tool::ToolUseContext::default();
        let actor_abort = create_query_abort_controller(&seed);

        assert!(!actor_abort.is_aborted());
        seed.abort_controller.abort();
        assert!(
            actor_abort.is_aborted(),
            "fork/query actor must observe cancellation from its invocation context"
        );
    }

    #[test]
    fn actor_snapshot_adoption_never_erases_live_read_handle_state() {
        let mut current = crate::tool::ToolUseContext::default();
        current
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: "/repo/read.txt".to_string(),
                content: Some("old\n".to_string()),
                timestamp_ms: Some(1),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });
        let update = crate::services::tools::tool_orchestration::MessageUpdate {
            message: None,
            model_message: None,
            progress: None,
            tool_result: None,
            new_context: crate::tool::ToolUseContext::default(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        };

        adopt_tool_update_context(&mut current, &update, false);
        assert_eq!(current.read_file_state.len(), 1);
        adopt_tool_update_context(&mut current, &update, true);
        assert_eq!(current.read_file_state.len(), 1);
    }

    /// Maps to: CC `query.ts:1391-1400`
    /// `toolResults.push(...normalizeMessagesForAPI([update.message], tools)
    /// .filter(_ => _.type === 'user'))` — one sequence, in arrival order, and
    /// nothing but user messages survives into the next request.
    #[test]
    fn tool_updates_land_in_one_arrival_ordered_user_only_sequence() {
        let user = |text: &str| {
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(text.to_string())],
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
        };
        let attachment = Message::Attachment(crate::types::message::AttachmentMessage::new(
            serde_json::json!({
                "type": "hook_additional_context",
                "content": ["from the hook"],
                "hookName": "PreToolUse:Bash",
                "toolUseID": "toolu_a",
                "hookEvent": "PreToolUse",
            }),
        ));
        let assistant = Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Text(
                "display-only".to_string(),
            )],
            model: None,
            stop_reason: None,
            usage: None,
        });

        let mut tool_results = Vec::new();
        append_tool_update_to_tool_results(user("tool result A"), &mut tool_results);
        // A tool's `newMessages` attachment must stay next to that tool's
        // result — routing it elsewhere pushed it past every later tool.
        append_tool_update_to_tool_results(attachment, &mut tool_results);
        append_tool_update_to_tool_results(assistant, &mut tool_results);
        append_tool_update_to_tool_results(user("tool result B"), &mut tool_results);

        let texts: Vec<String> = tool_results
            .iter()
            .flat_map(|message| {
                message.content.iter().filter_map(|content| match content {
                    crate::types::message::UserContent::Text(text)
                    | crate::types::message::UserContent::MetaText(text) => Some(text.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            texts.len(),
            3,
            "the assistant message is dropped by CC's user-only filter: {texts:?}"
        );
        assert_eq!(texts[0], "tool result A");
        assert!(
            texts[1].contains("from the hook"),
            "attachment keeps its arrival slot: {texts:?}"
        );
        assert_eq!(texts[2], "tool result B");
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn prepare_call_model_messages_strips_signature_blocks_only_for_fallback_retry() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("NODE_ENV", "test");
        let messages = vec![crate::types::message::Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::AssistantContent::Thinking {
                        text: "signed thinking".to_string(),
                        signature: "sig".to_string(),
                    },
                    crate::types::message::AssistantContent::Text("visible".to_string()),
                    crate::types::message::AssistantContent::RedactedThinking {
                        data: "signature".to_string(),
                    },
                ],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: None,
            },
        )];
        let context = std::collections::BTreeMap::new();

        let normal = prepare_call_model_messages(messages.clone(), &context, false);
        let fallback = prepare_call_model_messages(messages, &context, true);

        assert!(matches!(
            &normal[0],
            crate::types::message::Message::Assistant(message)
                if message.content.len() == 3
        ));
        assert!(matches!(
            &fallback[0],
            crate::types::message::Message::Assistant(message)
                if message.content == vec![crate::types::message::AssistantContent::Text("visible".to_string())]
        ));
        crate::utils::process_env::remove("NODE_ENV");
    }

    #[tokio::test]
    async fn prompt_query_creates_real_app_store_file_history_snapshot_before_model_call() {
        #[derive(Clone)]
        struct EndTurnDeps;
        impl crate::query::deps::QueryDeps for EndTurnDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
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

        struct TestConfigRestore;
        impl Drop for TestConfigRestore {
            fn drop(&mut self) {
                crate::utils::config::set_test_global_config(None);
            }
        }

        struct BootstrapRestore {
            interactive: bool,
            persistence_disabled: bool,
        }
        impl Drop for BootstrapRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_is_interactive(self.interactive);
                crate::bootstrap::state::set_session_persistence_disabled(
                    self.persistence_disabled,
                );
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _non_interactive = EnvVarGuard::unset("CLAUDE_CODE_NON_INTERACTIVE");
        let _cometix_non_interactive = EnvVarGuard::unset("COMETIX_NON_INTERACTIVE");
        let _cometix_non_interactive_session =
            EnvVarGuard::unset("COMETIX_NON_INTERACTIVE_SESSION");
        let _checkpointing = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING");
        let _bootstrap = BootstrapRestore {
            interactive: crate::bootstrap::state::get_is_interactive(),
            persistence_disabled: crate::bootstrap::state::is_session_persistence_disabled(),
        };
        crate::bootstrap::state::set_is_interactive(true);
        crate::bootstrap::state::set_session_persistence_disabled(true);
        let _config = TestConfigRestore;
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut context = crate::tool::ToolUseContext::default();
        context.app_store = crate::tool::AppStoreRef::new(store.clone());
        let params = QueryParams {
            turn_id: "user-file-history-turn".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("user-file-history-turn", "hello")],
            model_messages: Vec::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: context,
            system_prompt: Vec::new(),
            user_context: Default::default(),
            system_context: Default::default(),
        };

        let _ = run_query_once(params, EndTurnDeps).await.unwrap();
        let state = store.get();
        assert_eq!(state.file_history.snapshots.len(), 1);
        assert_eq!(
            state.file_history.snapshots[0].message_id,
            "user-file-history-turn"
        );
    }

    #[tokio::test]
    async fn tool_context_it2_setup_sink_emits_query_event_and_waits_for_response() {
        let (event_tx, event_rx) = async_channel::unbounded();
        let mut context = crate::tool::ToolUseContext::default();
        install_tool_progress_sink(&mut context, &event_tx.clone().into());
        let sink = context
            .it2_setup_prompt_sink
            .0
            .clone()
            .expect("it2 setup sink should be installed");

        let pending = tokio::spawn(async move { sink(true).await });
        let event = event_rx.recv().await.unwrap();
        match event {
            QueryEvent::It2SetupPromptRequest(request) => {
                assert!(request.tmux_available);
                request
                    .respond(crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::UseTmux);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(
            pending.await.unwrap(),
            crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::UseTmux
        );
    }

    #[test]
    fn query_tool_use_context_inherits_dynamic_pane_teammate_agent_id() {
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate::clear_dynamic_team_context();
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "reviewer@alpha".to_string(),
                agent_name: "reviewer".to_string(),
                team_name: "alpha".to_string(),
                color: Some("green".to_string()),
                plan_mode_required: false,
                parent_session_id: Some("parent".to_string()),
            },
        ));

        let context = query_tool_use_context(
            ToolPermissionContext::default(),
            Vec::new(),
            None,
            &crate::utils::session_restore::ResumeRestoreStores::default(),
            &crate::state::app_state_store::McpState::default(),
            None,
            &crate::tool::AppStoreRef::default(),
        );

        assert_eq!(context.agent_id.as_deref(), Some("reviewer@alpha"));
        crate::utils::teammate::clear_dynamic_team_context();
    }

    #[test]
    fn tool_use_context_seed_carries_effort_for_nested_agents() {
        // Maps to: nested agents write effort onto ToolUseContext before
        // spawn_query (formerly QueryRuntimeOptions.effort_value).
        let context = crate::tool::ToolUseContext::default().with_effort_value(Some(
            crate::utils::effort::EffortValue::Named("low".to_string()),
        ));

        assert_eq!(
            context.effort_value,
            Some(crate::utils::effort::EffortValue::Named("low".to_string()))
        );
    }

    #[test]
    fn query_params_seed_carries_permission_context() {
        let mut permission = ToolPermissionContext::default();
        permission.should_avoid_permission_prompts = true;
        let params = QueryParams {
            turn_id: "t1".into(),
            input: "hi".into(),
            messages: Vec::new(),
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: Default::default(),
            system_context: Default::default(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::with_permission_context(permission),
        };
        assert!(
            params
                .tool_use_context
                .tool_permission_context
                .should_avoid_permission_prompts
        );
    }

    #[test]
    fn query_params_seed_carries_content_replacement_and_app_store() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut seed =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default())
                .with_app_store(store);
        seed.agent_id = Some("agent-1".into());
        let replacement = crate::utils::tool_result_storage::ContentReplacementState::new();
        let params = QueryParams {
            turn_id: "t1".into(),
            input: "hi".into(),
            messages: Vec::new(),
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: Default::default(),
            system_context: Default::default(),
            query_source: QuerySource::Agent,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: seed.with_content_replacement_state(Some(replacement.clone())),
        };
        assert_eq!(
            params.tool_use_context.content_replacement_state,
            Some(replacement)
        );
        assert_eq!(params.tool_use_context.agent_id.as_deref(), Some("agent-1"));
        assert!(params.tool_use_context.app_store.store.is_some());
    }

    #[test]
    fn tool_use_summary_predicate_matches_official_gate_conditions() {
        let config = config::QueryConfig {
            session_id: crate::types::ids::SessionId("summary-session".to_string()),
            gates: config::QueryConfigGates {
                streaming_tool_execution: false,
                emit_tool_use_summaries: true,
                is_ant: false,
                fast_mode_enabled: true,
            },
        };
        let block = crate::types::message::ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_summary".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({ "file_path": "Cargo.toml" }),
        };
        let context = crate::tool::ToolUseContext::default();

        assert!(should_start_tool_use_summary(
            &config,
            std::slice::from_ref(&block),
            &context,
            &QuerySource::Prompt,
        ));
        assert!(!should_start_tool_use_summary(
            &config,
            &[],
            &context,
            &QuerySource::Prompt,
        ));
        assert!(!should_start_tool_use_summary(
            &config,
            &[block],
            &context,
            &QuerySource::Agent,
        ));
    }

    #[test]
    fn tool_use_summary_collects_matching_tool_outputs_and_last_text() {
        let block = crate::types::message::ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_summary_output".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({ "file_path": "Cargo.toml" }),
        };
        let user = crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::ToolResult(
                crate::types::message::ToolResult {
                    tool_use_id: block.id.clone(),
                    content: "manifest".to_string(),
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
        };
        let assistant = crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![
                crate::types::message::AssistantContent::Text("first".to_string()),
                crate::types::message::AssistantContent::Text("last".to_string()),
            ],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };

        let info = tool_info_for_summary(std::slice::from_ref(&block), &[user]);
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].name, "Read");
        assert_eq!(info[0].output, Some(serde_json::json!("manifest")));
        assert_eq!(
            last_assistant_text_for_tool_summary(&[assistant]),
            Some("last".to_string())
        );
    }

    #[test]
    fn streaming_completed_tool_update_defers_new_model_messages_until_assistant_history_exists() {
        let (event_tx, event_rx) = async_channel::unbounded();
        let primary_tool_result = crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::ToolResult(
                crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId(
                        "toolu_stream_new_messages".to_string(),
                    ),
                    content: "primary result".to_string(),
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
        };
        let extra_user_message = crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "extra model-visible follow-up".to_string(),
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
        };
        let update = crate::services::tools::tool_orchestration::MessageUpdate {
            message: None,
            model_message: Some(Message::User(extra_user_message.clone())),
            progress: None,
            tool_result: Some(primary_tool_result.clone()),
            new_context: crate::tool::ToolUseContext::default(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        };
        let mut permission_context = ToolPermissionContext::default();
        let mut tool_results = Vec::new();
        let mut processed_tool_use_ids = std::collections::HashSet::new();
        let mut deferred_model_messages = Vec::new();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(process_streaming_completed_tool_update(
                update,
                &event_tx.clone().into(),
                &mut permission_context,
                &mut tool_results,
                &mut processed_tool_use_ids,
                &mut deferred_model_messages,
            ));

        assert_eq!(tool_results, vec![primary_tool_result, extra_user_message]);
        assert!(deferred_model_messages.is_empty());
        assert!(processed_tool_use_ids.contains("toolu_stream_new_messages"));
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::ModelMessage(_)))
        );
    }

    #[test]
    fn blocking_limit_preempt_skips_official_reducer_query_sources() {
        assert!(should_preempt_context_blocking_limit(
            &QuerySource::Prompt,
            false
        ));
        assert!(!should_preempt_context_blocking_limit(
            &QuerySource::Prompt,
            true
        ));
        assert!(!should_preempt_context_blocking_limit(
            &QuerySource::Compact,
            false
        ));
        assert!(!should_preempt_context_blocking_limit(
            &QuerySource::SessionMemory,
            false
        ));
        assert!(!should_preempt_context_blocking_limit(
            &QuerySource::MarbleOrigami,
            false
        ));
        assert!(!should_preempt_context_blocking_limit_with_owners(
            &QuerySource::Prompt,
            false,
            true,
        ));
        assert!(should_preempt_context_blocking_limit_with_owners(
            &QuerySource::Prompt,
            false,
            false,
        ));
    }

    #[test]
    fn content_replacement_persistence_gate_matches_official_repl_and_agent_sources() {
        assert!(should_persist_content_replacements(&QuerySource::Prompt));
        assert!(should_persist_content_replacements(&QuerySource::Agent));
        assert!(!should_persist_content_replacements(&QuerySource::Sdk));
        assert!(!should_persist_content_replacements(&QuerySource::Compact));
        assert!(!should_persist_content_replacements(
            &QuerySource::SessionMemory
        ));
    }

    #[test]
    fn max_output_token_escalation_gate_matches_official_conditions() {
        assert!(!should_escalate_max_output_tokens_with_gate(
            false, None, false
        ));
        assert!(!should_escalate_max_output_tokens_with_gate(
            true,
            Some(8_000),
            false
        ));
        assert!(!should_escalate_max_output_tokens_with_gate(
            true, None, true
        ));
        assert!(should_escalate_max_output_tokens_with_gate(
            true, None, false
        ));
    }

    #[test]
    fn query_actor_passes_task_budget_remaining_after_autocompact() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("NODE_ENV", "test");

        #[derive(Clone, Debug)]
        struct TaskBudgetDeps {
            seen_task_budgets: std::sync::Arc<
                std::sync::Mutex<Vec<Option<crate::services::api::claude::TaskBudget>>>,
            >,
        }

        impl crate::query::deps::QueryDeps for TaskBudgetDeps {
            fn autocompact(
                &self,
                _messages_for_query: Vec<crate::types::message::Message>,
                _tool_use_context: &crate::tool::ToolUseContext,
                _cache_safe_params: crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
                _query_source: &QuerySource,
                _tracking: Option<crate::services::compact::auto_compact::AutoCompactTrackingState>,
                _snip_tokens_freed: i64,
            ) -> futures::future::BoxFuture<'static, crate::query::deps::AutocompactResult>
            {
                Box::pin(async {
                    crate::query::deps::AutocompactResult {
                        messages: vec![crate::types::message::Message::User(
                            crate::types::message::UserMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::UserContent::Text(
                                    "compact summary".to_string(),
                                )],
                                is_compact_summary: true,
                                plan_content: None,
                                image_paste_ids: None,
                                is_visible_in_transcript_only: false,
                                mcp_meta: None,
                                source_tool_assistant_uuid: None,
                                permission_mode: None,
                                origin: None,
                                summarize_metadata: None,
                            },
                        )],
                        compacted: true,
                        consecutive_failures: Some(0),
                        rebuilt_read_file_state: None,
                    }
                })
            }

            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_task_budgets
                    .lock()
                    .unwrap()
                    .push(request.options.task_budget.clone());
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: Some("claude".to_string()),
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-task-budget".to_string(),
            input: "compact then continue".to_string(),
            messages: Vec::new(),
            model_messages: vec![
                crate::types::message::Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        "hello".to_string(),
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
                }),
                crate::types::message::Message::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![crate::types::message::AssistantContent::Text(
                            "previous response".to_string(),
                        )],
                        model: Some("claude".to_string()),
                        stop_reason: Some(crate::types::message::StopReason::EndTurn),
                        usage: Some(crate::types::message::TokenUsage {
                            input_tokens: 120,
                            output_tokens: 30,
                            cache_creation_input_tokens: 10_000,
                            cache_read_input_tokens: 20_000,
                            ..Default::default()
                        }),
                    },
                ),
            ],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: Some(crate::services::api::claude::TaskBudget {
                total: 1_000,
                remaining: None,
            }),
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let seen_task_budgets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                TaskBudgetDeps {
                    seen_task_budgets: seen_task_budgets.clone(),
                },
                event_tx,
                command_rx,
            ));

        crate::utils::process_env::remove("NODE_ENV");

        assert_eq!(terminal.reason, "completed");
        assert_eq!(
            *seen_task_budgets.lock().unwrap(),
            vec![Some(crate::services::api::claude::TaskBudget {
                total: 1_000,
                remaining: Some(850),
            })]
        );
    }

    #[test]
    fn query_actor_threads_resume_stores_and_tracking_into_tool_use_context_like_official() {
        #[derive(Clone, Debug)]
        struct CaptureResumeStoresDeps {
            seen_stores: std::sync::Arc<
                std::sync::Mutex<Option<crate::utils::session_restore::ResumeRestoreStores>>,
            >,
            seen_tracking:
                std::sync::Arc<std::sync::Mutex<Option<crate::tool::QueryChainTracking>>>,
        }

        impl crate::query::deps::QueryDeps for CaptureResumeStoresDeps {
            fn microcompact(
                &self,
                messages_for_query: Vec<crate::types::message::Message>,
                tool_use_context: &crate::tool::ToolUseContext,
                _query_source: &QuerySource,
            ) -> crate::query::deps::MicrocompactResult {
                *self.seen_stores.lock().unwrap() =
                    Some(tool_use_context.resume_restore_stores.clone());
                *self.seen_tracking.lock().unwrap() = tool_use_context.query_tracking.clone();
                crate::query::deps::MicrocompactResult {
                    messages: messages_for_query,
                    boundary_messages: Vec::new(),
                    compaction_info: None,
                }
            }

            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let stores = crate::utils::session_restore::ResumeRestoreStores {
            read_file_state: vec![crate::utils::query_helpers::ReadFileStateEntry {
                path: "/tmp/project/src/lib.rs".to_string(),
                content: Some("fn main() {}".to_string()),
                timestamp_ms: Some(1),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            }],
            bash_tools: vec!["git".to_string()],
            ..crate::utils::session_restore::ResumeRestoreStores::default()
        };
        let params = QueryParams {
            turn_id: "turn-resume-stores".to_string(),
            input: "resume context".to_string(),
            messages: Vec::new(),
            model_messages: vec![crate::types::message::Message::User(
                crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        "resume context".to_string(),
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
                },
            )],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default()
                .with_resume_restore_stores(stores.clone()),
        };
        let seen_stores = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen_tracking = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                CaptureResumeStoresDeps {
                    seen_stores: seen_stores.clone(),
                    seen_tracking: seen_tracking.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen_stores
            .lock()
            .unwrap()
            .clone()
            .expect("microcompact should receive query tool-use context");
        assert_eq!(seen.read_file_state, stores.read_file_state);
        assert_eq!(seen.bash_tools, stores.bash_tools);
        let tracking = seen_tracking
            .lock()
            .unwrap()
            .clone()
            .expect("microcompact should receive query-loop tracking");
        assert_eq!(tracking.depth, 0);
        assert!(!tracking.chain_id.is_empty());
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn query_actor_retries_call_model_after_model_fallback_and_strips_signature_blocks() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("NODE_ENV", "test");
        crate::utils::process_env::set("ANTHROPIC_MODEL", "claude-primary-model");

        #[derive(Clone, Debug)]
        struct FallbackDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_models: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for FallbackDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_models
                    .lock()
                    .unwrap()
                    .push(request.options.model.clone());
                self.seen_messages.lock().unwrap().push(request.messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    if call == 0 {
                        return Err(anyhow::Error::new(
                            crate::services::api::with_retry::FallbackTriggeredError {
                                original_model: "claude-primary-model".to_string(),
                                fallback_model: "claude-fallback-model".to_string(),
                            },
                        ));
                    }
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: Some("claude-fallback-model".to_string()),
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-model-fallback".to_string(),
            input: "continue".to_string(),
            messages: Vec::new(),
            model_messages: vec![
                crate::types::message::Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        "hello".to_string(),
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
                }),
                crate::types::message::Message::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![
                            crate::types::message::AssistantContent::Thinking {
                                text: "signed thinking".to_string(),
                                signature: "sig".to_string(),
                            },
                            crate::types::message::AssistantContent::Text("visible".to_string()),
                            crate::types::message::AssistantContent::RedactedThinking {
                                data: "signature".to_string(),
                            },
                        ],
                        model: Some("claude-primary-model".to_string()),
                        stop_reason: Some(crate::types::message::StopReason::EndTurn),
                        usage: None,
                    },
                ),
            ],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_models = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                FallbackDeps {
                    calls: calls.clone(),
                    seen_models: seen_models.clone(),
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        crate::utils::process_env::remove("NODE_ENV");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");

        assert_eq!(terminal.reason, "completed");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            *seen_models.lock().unwrap(),
            vec![
                "claude-primary-model".to_string(),
                "claude-fallback-model".to_string()
            ]
        );
        let seen_messages = seen_messages.lock().unwrap();
        assert!(seen_messages[0].iter().any(|message| matches!(
            message,
            crate::types::message::Message::Assistant(assistant)
                if assistant.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::AssistantContent::Thinking { .. }
                        | crate::types::message::AssistantContent::RedactedThinking { .. }
                ))
        )));
        assert!(seen_messages[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::Assistant(assistant)
                if assistant.content == vec![crate::types::message::AssistantContent::Text("visible".to_string())]
        )));
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(events.into_iter().any(|event| matches!(
            event,
            QueryEvent::Message(crate::types::message::Message::System(
                SystemMessage::Informational { content: text, level, .. },
            )) if level == SystemMessageLevel::Warning
                && text.contains("Switched to claude-fallback-model")
                && text.contains("claude-primary-model")
        )));
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn query_actor_retries_after_streaming_non_streaming_model_fallback_signal() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("NODE_ENV", "test");
        crate::utils::process_env::set("ANTHROPIC_MODEL", "claude-primary-model");

        #[derive(Clone, Debug)]
        struct StreamModelFallbackDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            autocompact_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_models: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for StreamModelFallbackDeps {
            fn autocompact(
                &self,
                messages_for_query: Vec<crate::types::message::Message>,
                _tool_use_context: &crate::tool::ToolUseContext,
                _cache_safe_params: crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
                _query_source: &QuerySource,
                _tracking: Option<crate::services::compact::auto_compact::AutoCompactTrackingState>,
                _snip_tokens_freed: i64,
            ) -> futures::future::BoxFuture<'static, crate::query::deps::AutocompactResult>
            {
                self.autocompact_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    crate::query::deps::AutocompactResult {
                        messages: messages_for_query,
                        compacted: false,
                        consecutive_failures: None,
                        rebuilt_read_file_state: None,
                    }
                })
            }

            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_models
                    .lock()
                    .unwrap()
                    .push(request.options.model.clone());
                self.seen_messages.lock().unwrap().push(request.messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(2);
                    if call == 0 {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::ModelFallback {
                                original_model: "claude-primary-model".to_string(),
                                fallback_model: "claude-fallback-model".to_string(),
                            },
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: Some("claude-fallback-model".to_string()),
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-stream-model-fallback".to_string(),
            input: "continue".to_string(),
            messages: Vec::new(),
            model_messages: vec![
                crate::types::message::Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        "hello".to_string(),
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
                }),
                crate::types::message::Message::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![
                            crate::types::message::AssistantContent::Thinking {
                                text: "signed thinking".to_string(),
                                signature: "sig".to_string(),
                            },
                            crate::types::message::AssistantContent::Text("visible".to_string()),
                            crate::types::message::AssistantContent::RedactedThinking {
                                data: "signature".to_string(),
                            },
                        ],
                        model: Some("claude-primary-model".to_string()),
                        stop_reason: Some(crate::types::message::StopReason::EndTurn),
                        usage: None,
                    },
                ),
            ],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let autocompact_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_models = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                StreamModelFallbackDeps {
                    calls: calls.clone(),
                    autocompact_calls: autocompact_calls.clone(),
                    seen_models: seen_models.clone(),
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        crate::utils::process_env::remove("NODE_ENV");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");

        assert_eq!(terminal.reason, "completed");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            autocompact_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            *seen_models.lock().unwrap(),
            vec![
                "claude-primary-model".to_string(),
                "claude-fallback-model".to_string()
            ]
        );
        let seen_messages = seen_messages.lock().unwrap();
        assert!(seen_messages[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::Assistant(assistant)
                if assistant.content == vec![crate::types::message::AssistantContent::Text("visible".to_string())]
        )));
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, QueryEvent::StreamRequestStart))
                .count(),
            1
        );
        assert!(events.into_iter().any(|event| matches!(
            event,
            QueryEvent::Message(crate::types::message::Message::System(
                SystemMessage::Informational { content: text, level, .. },
            )) if level == SystemMessageLevel::Warning
                && text.contains("Switched to claude-fallback-model")
                && text.contains("claude-primary-model")
        )));
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn query_actor_keeps_fallback_model_for_tool_result_continuation() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("NODE_ENV", "test");
        crate::utils::process_env::set("ANTHROPIC_MODEL", "claude-primary-model");

        #[derive(Clone, Debug)]
        struct FallbackContinuationDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_models: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }

        impl crate::query::deps::QueryDeps for FallbackContinuationDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_models
                    .lock()
                    .unwrap()
                    .push(request.options.model.clone());
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    if call == 0 {
                        return Err(anyhow::Error::new(
                            crate::services::api::with_retry::FallbackTriggeredError {
                                original_model: "claude-primary-model".to_string(),
                                fallback_model: "claude-fallback-model".to_string(),
                            },
                        ));
                    }
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    let assistant = if call == 1 {
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::ToolUse(
                                crate::types::message::ToolUseBlock {
                                    id: crate::types::ids::ToolUseId(
                                        "toolu_unknown_fallback".to_string(),
                                    ),
                                    name: "DefinitelyMissingTool".to_string(),
                                    input: serde_json::json!({}),
                                },
                            )],
                            model: Some("claude-fallback-model".to_string()),
                            stop_reason: Some(crate::types::message::StopReason::ToolUse),
                            usage: None,
                        }
                    } else {
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::Text(
                                "done".to_string(),
                            )],
                            model: Some("claude-fallback-model".to_string()),
                            stop_reason: Some(crate::types::message::StopReason::EndTurn),
                            usage: None,
                        }
                    };
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(assistant),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-fallback-continuation".to_string(),
            input: "continue".to_string(),
            messages: Vec::new(),
            model_messages: vec![crate::types::message::Message::User(
                crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        "hello".to_string(),
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
                },
            )],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_models = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                FallbackContinuationDeps {
                    calls: calls.clone(),
                    seen_models: seen_models.clone(),
                },
                event_tx,
                command_rx,
            ));

        crate::utils::process_env::remove("NODE_ENV");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");

        assert_eq!(terminal.reason, "completed");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(
            *seen_models.lock().unwrap(),
            vec![
                "claude-primary-model".to_string(),
                "claude-fallback-model".to_string(),
                "claude-fallback-model".to_string(),
            ]
        );
    }

    #[test]
    fn query_actor_resets_partial_stream_state_on_streaming_fallback() {
        #[derive(Clone, Debug)]
        struct StreamingFallbackDeps;

        impl crate::query::deps::QueryDeps for StreamingFallbackDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                        crate::services::api::claude::ClaudeStreamItem::Text("partial".to_string()),
                    ))
                    .await
                    .ok();
                    tx.send(crate::services::api::claude::QueryModelStreamItem::StreamingFallback)
                        .await
                        .ok();
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "fallback final".to_string(),
                                )],
                                model: Some("claude-fallback".to_string()),
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-streaming-fallback".to_string(),
            input: "recover".to_string(),
            messages: Vec::new(),
            model_messages: vec![crate::types::message::Message::User(
                crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        "hello".to_string(),
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
                },
            )],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                StreamingFallbackDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .any(|event| matches!(event, QueryEvent::ClearStreamingPreview))
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            QueryEvent::Message(message) if model_assistant_text(message) == Some("partial")
        )));
        // Streamed-assistant convergence: the fallback assistant is ONE whole
        // `Message` (CC claude.ts:2571-2594 yields the non-streaming result
        // whole); no Row/ModelMessage dual carrier remains.
        assert!(events.iter().any(|event| matches!(
            event,
            QueryEvent::Message(message)
                if model_assistant_text(message) == Some("fallback final")
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            QueryEvent::ModelMessage(crate::types::message::Message::Assistant(_))
        )));
    }

    #[test]
    fn query_attachments_matches_official_plain_and_image_prompt_input() {
        #[derive(Clone, Debug)]
        struct CaptureDeps {
            seen: std::sync::Arc<std::sync::Mutex<Vec<crate::types::message::Message>>>,
        }
        impl crate::query::deps::QueryDeps for CaptureDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                *self.seen.lock().unwrap() = request.messages;
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let root = std::env::temp_dir().join(format!(
            "cometix-query-at-mention-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "alpha\nbeta\n").unwrap();
        for with_image_prompt in [false, true] {
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            // CC processSlashCommand.tsx:1212-1226: the complete prompt's text
            // drives attachment extraction even after UI normalization splits its
            // image and multiple text blocks into separate rows.
            let (input, messages, model_messages) = if with_image_prompt {
                let row = RenderableMessage::user_blocks(
                    "meta-init-mention",
                    vec![
                        UserContent::MetaImage {
                            media_type: "image/png".into(),
                            data: "aW1hZ2U=".into(),
                        },
                        UserContent::MetaText("inspect @note.txt#L2".into()),
                        UserContent::MetaText(
                            "keep the reference from the first text block".into(),
                        ),
                    ],
                );
                let RenderableMessageKind::User { message } = row.kind else {
                    unreachable!()
                };
                let model_messages = vec![Message::User(message)];
                let entries = model_messages
                    .iter()
                    .cloned()
                    .map(crate::types::message::HistoryEntry::Message)
                    .collect::<Vec<_>>();
                (
                    "/init".to_string(),
                    crate::utils::messages::project_history_rows(&entries),
                    model_messages,
                )
            } else {
                (
                    "inspect @note.txt#L2".to_string(),
                    vec![RenderableMessage::user(
                        "user-at-mention",
                        "inspect @note.txt#L2",
                    )],
                    Vec::new(),
                )
            };
            let params = QueryParams {
                turn_id: "turn-at-mention".to_string(),
                input,
                messages,
                model_messages,
                system_prompt: Vec::new(),
                user_context: std::collections::BTreeMap::new(),
                system_context: std::collections::BTreeMap::new(),
                query_source: QuerySource::Prompt,
                token_budget: None,
                task_budget: None,
                max_turns: None,
                tool_use_context: crate::tool::ToolUseContext::default()
                    .with_cwd_override(Some(root.clone())),
            };
            let (event_tx, event_rx) = async_channel::unbounded();
            let (_command_tx, command_rx) = async_channel::unbounded();
            let terminal = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_query_actor(
                    params,
                    CaptureDeps { seen: seen.clone() },
                    event_tx,
                    command_rx,
                ));
            assert_eq!(terminal.reason, "completed");
            let seen = seen.lock().unwrap();
            assert!(seen.iter().any(|message| matches!(
                message,
                crate::types::message::Message::Attachment(attachment)
                    if attachment.attachment_type() == "file"
                        && attachment.attachment.to_wire()["content"]["file"]["content"] == "beta"
                        && attachment.attachment.to_wire()["content"]["file"]["startLine"] == 2
            )));
            drop(seen);
            let mut saw_file_attachment_event = false;
            while let Ok(event) = event_rx.try_recv() {
                // Batch D3: the attachment travels as one whole `Message` (CC
                // query.ts `yield attachment`), not a Row+ModelMessage pair.
                if matches!(
                    event,
                    QueryEvent::Message(crate::types::message::Message::Attachment(ref attachment))
                        if attachment.attachment_type() == "file"
                ) {
                    saw_file_attachment_event = true;
                }
            }
            assert!(saw_file_attachment_event);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn query_loop_applies_tool_result_budget_before_call_model() {
        #[derive(Clone, Debug)]
        struct CaptureDeps {
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for CaptureDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-budget".to_string(),
            input: String::new(),
            // Each result remains below the per-tool persistence threshold,
            // while their adjacent user-message group exceeds the aggregate
            // budget and therefore clears the larger result.
            messages: {
                let mut messages = vec![RenderableMessage::user_tool_result(
                    "tool-result-large",
                    "toolu_large",
                    "x".repeat(30_000),
                    false,
                )];
                messages.extend((0..7).map(|index| {
                    RenderableMessage::user_tool_result(
                        format!("tool-result-secondary-{index}"),
                        format!("toolu_secondary_{index}"),
                        "y".repeat(25_000),
                        false,
                    )
                }));
                messages
            },
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default()
                .with_content_replacement_state(Some(
                    crate::utils::tool_result_storage::ContentReplacementState::new(),
                )),
        };
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                CaptureDeps {
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen_messages.lock().unwrap();
        assert!(seen[0].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_large"
                            && result.content
                                == crate::utils::tool_result_storage::TOOL_RESULT_CLEARED_MESSAGE
                ))
        )));
    }

    #[test]
    fn query_loop_uses_injected_content_replacement_state_before_call_model() {
        #[derive(Clone, Debug)]
        struct CaptureDeps {
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for CaptureDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_messages.lock().unwrap().push(request.messages);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let mut replacement_state =
            crate::utils::tool_result_storage::ContentReplacementState::new();
        replacement_state.seen_ids.insert("toolu_old".to_string());
        replacement_state
            .replacements
            .insert("toolu_old".to_string(), "stored preview".to_string());
        let params = QueryParams {
            turn_id: "turn-content-replacement-state".to_string(),
            input: String::new(),
            messages: Vec::new(),
            model_messages: vec![crate::types::message::Message::User(
                crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_old".to_string()),
                            content: "original full output".to_string(),
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
                },
            )],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default()
                .with_content_replacement_state(Some(replacement_state)),
        };
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                CaptureDeps {
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen_messages.lock().unwrap();
        assert!(seen[0].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_old" && result.content == "stored preview"
                ))
        )));
    }

    #[test]
    fn query_loop_builds_official_call_model_request_shape() {
        #[derive(Clone, Debug)]
        struct CaptureRequestDeps {
            seen: std::sync::Arc<std::sync::Mutex<Vec<crate::query::deps::CallModelRequest>>>,
        }

        impl crate::query::deps::QueryDeps for CaptureRequestDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen.lock().unwrap().push(request.clone());
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: Some(request.options.model.clone()),
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-call-model-request".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("user-call-model-request", "hello")],
            model_messages: Vec::new(),
            system_prompt: vec!["caller-provided system prompt".to_string()],
            user_context: std::collections::BTreeMap::from([(
                "callerUserContext".to_string(),
                "visible to model".to_string(),
            )]),
            system_context: std::collections::BTreeMap::from([(
                "callerSystemContext".to_string(),
                "visible to system prompt".to_string(),
            )]),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                CaptureRequestDeps { seen: seen.clone() },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen.lock().unwrap();
        let request = seen.first().expect("callModel request should be captured");
        assert_eq!(request.query_source, QuerySource::Prompt);
        assert_eq!(request.options.query_source, "repl_main_thread");
        assert!(request.tools.iter().any(|tool| tool.name == "Bash"));
        assert!(request.tools.iter().any(|tool| tool.name == "Read"));
        let system_prompt_text = request.system_prompt.join("\n");
        assert!(system_prompt_text.contains("caller-provided system prompt"));
        assert!(system_prompt_text.contains("callerSystemContext: visible to system prompt"));
        // CC `query.ts:252-269,449-451,659-661` never appends QueryConfig fields.
        assert!(!system_prompt_text.contains("session_id:"));
        assert!(
            request
                .messages
                .iter()
                .any(|message| matches!(message, crate::types::message::Message::User(_)))
        );
    }

    #[test]
    fn build_call_model_request_matches_official_simple_prompt_then_caller_system_context() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "1");
        let base_system_prompt = crate::constants::prompts::get_simple_system_prompt_if_enabled();
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        let mut system_context = std::collections::BTreeMap::new();
        system_context.insert("gitStatus".to_string(), "Current branch: main".to_string());

        let request = build_call_model_request(
            Vec::new(),
            ToolPermissionContext::default(),
            &QuerySource::Prompt,
            &base_system_prompt,
            &system_context,
            &crate::state::app_state_store::McpState::default(),
            &crate::tool::ToolUseContext::default(),
        );

        // CC query.ts:450 appends only the supplied systemContext; runtime
        // session/config fields are not a second implicit context producer.
        assert_eq!(request.system_prompt.len(), 2);
        assert!(
            request.system_prompt[0]
                .starts_with("You are Claude Code, Anthropic's official CLI for Claude.")
        );
        assert!(request.system_prompt[0].contains("\n\nCWD: "));
        assert!(request.system_prompt[0].contains("\nDate: "));
        assert!(
            request
                .system_prompt
                .last()
                .is_some_and(|block| block.contains("gitStatus: Current branch: main"))
        );
        assert!(!request.system_prompt.join("\n").contains("session_id:"));
    }

    #[test]
    fn build_call_model_request_threads_non_interactive_session_like_official_options() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::bootstrap::state::set_is_interactive(true);
        crate::utils::process_env::remove("CLAUDE_CODE_NON_INTERACTIVE");
        let interactive_ctx =
            crate::tool::ToolUseContext::default().with_non_interactive_session(
                crate::bootstrap::state::get_is_non_interactive_session(),
            );
        let interactive_request = build_call_model_request(
            Vec::new(),
            ToolPermissionContext::default(),
            &QuerySource::Prompt,
            &[],
            &std::collections::BTreeMap::new(),
            &crate::state::app_state_store::McpState::default(),
            &interactive_ctx,
        );
        assert!(!interactive_request.options.is_non_interactive_session);

        crate::utils::process_env::set("CLAUDE_CODE_NON_INTERACTIVE", "1");
        let non_interactive_ctx =
            crate::tool::ToolUseContext::default().with_non_interactive_session(
                crate::bootstrap::state::get_is_non_interactive_session(),
            );
        let non_interactive_request = build_call_model_request(
            Vec::new(),
            ToolPermissionContext::default(),
            &QuerySource::Prompt,
            &[],
            &std::collections::BTreeMap::new(),
            &crate::state::app_state_store::McpState::default(),
            &non_interactive_ctx,
        );
        assert!(non_interactive_request.options.is_non_interactive_session);

        crate::utils::process_env::remove("CLAUDE_CODE_NON_INTERACTIVE");
        crate::bootstrap::state::set_is_interactive(true);
    }

    fn deny_rule_permission_context(rule: &str) -> ToolPermissionContext {
        use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::LocalSettings,
            vec![PermissionRuleValue::new(rule, None)],
        );
        context
    }

    fn deny_test_mcp_tool(server: &str, name: &str) -> crate::types::tools::Tool {
        crate::types::tools::Tool {
            name: format!("mcp__{server}__{name}"),
            description: format!("{name} on {server}"),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            is_mcp: true,
            mcp_info: Some(crate::types::tools::McpToolInfo {
                server_name: server.to_string(),
                tool_name: name.to_string(),
            }),
            ..Default::default()
        }
    }

    /// Maps to CC `query.ts:660` `tools: toolUseContext.options.tools` — the
    /// pool `assembleToolPool` produced (`hooks/useMergedTools.ts:30`), which is
    /// built-ins PLUS deny-filtered MCP tools (`tools.ts:349-352`).
    ///
    /// CC has no fallback branch at all; the Rust one exists for seeds that
    /// carry no tools (the `#[cfg(test)]` `query(...)` entrypoint). It used to
    /// call `get_tools`, which is only CC's `:349` half — every MCP tool
    /// silently vanished from the request, and the only reason the model ever
    /// saw one on that branch was `claude.rs` chaining the RAW
    /// `options.mcp_tools` back on afterwards (which is also how deny-ruled MCP
    /// tools got back in). Both halves have to hold at once: MCP tools present,
    /// denied ones absent.
    #[test]
    fn build_call_model_request_assembles_the_deny_filtered_pool_when_the_seed_has_no_tools() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mcp_state = crate::state::app_state_store::McpState {
            tools: vec![
                deny_test_mcp_tool("trusted", "read_doc"),
                deny_test_mcp_tool("untrusted", "read_secret"),
            ],
            ..Default::default()
        };
        // `ToolUseContext::default()` pre-fills `tools` with `get_tools(...)`
        // (tool.rs:1291), so the empty-seed branch has to be asked for
        // explicitly.
        let seed = crate::tool::ToolUseContext::default().with_tools(Vec::new());
        assert!(
            seed.tools.is_empty(),
            "this test covers the empty-seed branch"
        );

        let request = build_call_model_request(
            Vec::new(),
            deny_rule_permission_context("mcp__untrusted"),
            &QuerySource::Prompt,
            &["seeded system prompt".to_string()],
            &std::collections::BTreeMap::new(),
            &mcp_state,
            &seed,
        );

        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        assert!(
            names.iter().any(|name| name == "mcp__trusted__read_doc"),
            "the empty-seed branch must still assemble MCP tools, not built-ins \
             only, got {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|name| name.starts_with("mcp__untrusted__")),
            "a mcp__server deny rule must strip the whole server, got {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "Bash"),
            "built-ins must still be there, got {names:?}"
        );
    }

    /// The seeded branch is the production one (REPL / QueryEngine / runAgent
    /// all seed `tools` before `spawn_query`). It must forward the pool
    /// verbatim: no MCP re-union, no re-filtering.
    ///
    /// `options.mcp_tools` still carries the RAW `appState.mcp.tools` because
    /// CC writes exactly that at `query.ts:689` — it is a declared-but-unread
    /// Options field (`services/api/claude.ts:694`), kept for shape parity.
    #[test]
    fn build_call_model_request_forwards_the_seeded_pool_and_keeps_mcp_tools_write_only() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let permission_context = deny_rule_permission_context("mcp__untrusted");
        let mcp_state = crate::state::app_state_store::McpState {
            tools: vec![
                deny_test_mcp_tool("trusted", "read_doc"),
                deny_test_mcp_tool("untrusted", "read_secret"),
            ],
            ..Default::default()
        };
        let pool = crate::tools::assemble_tool_pool(&permission_context, &mcp_state.tools);
        let seed = crate::tool::ToolUseContext::default().with_tools(pool.clone());

        let request = build_call_model_request(
            Vec::new(),
            permission_context,
            &QuerySource::Prompt,
            &["seeded system prompt".to_string()],
            &std::collections::BTreeMap::new(),
            &mcp_state,
            &seed,
        );

        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            pool.iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            "the seeded pool must reach callModel verbatim"
        );
        assert_eq!(
            request
                .options
                .mcp_tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            vec![
                "mcp__trusted__read_doc".to_string(),
                "mcp__untrusted__read_secret".to_string(),
            ],
            "options.mcp_tools mirrors CC query.ts:689 `appState.mcp.tools` RAW \
             — which is exactly why no request builder may read it back"
        );
    }

    #[test]
    fn build_call_model_request_applies_seeded_tool_use_context_options_like_official() {
        let nested_tool = crate::types::tools::Tool {
            name: "NestedRead".to_string(),
            description: "nested".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            strict: Some(true),
            ..Default::default()
        };
        let seeded = crate::tool::ToolUseContext::default()
            .with_main_loop_model("agent-model")
            .with_tools(vec![nested_tool])
            .with_thinking_config(Some(crate::utils::thinking::ThinkingConfig::Disabled))
            .with_non_interactive_session(true)
            .with_cwd_override(Some(std::path::PathBuf::from("/tmp/agent-cwd")))
            .with_critical_system_reminder_experimental(Some("Stay focused".to_string()))
            .with_effort_value(Some(crate::utils::effort::EffortValue::Named(
                "medium".to_string(),
            )))
            .with_fast_mode(Some(false));

        let request = build_call_model_request(
            Vec::new(),
            ToolPermissionContext::default(),
            &QuerySource::Agent,
            &[],
            &std::collections::BTreeMap::new(),
            &crate::state::app_state_store::McpState::default(),
            &seeded,
        );

        assert_eq!(request.options.model, "agent-model");
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "NestedRead");
        assert_eq!(
            request.thinking_config,
            crate::utils::thinking::ThinkingConfig::Disabled
        );
        assert_eq!(
            request.options.effort_value,
            Some(crate::utils::effort::EffortValue::Named(
                "medium".to_string()
            ))
        );
        assert!(request.options.is_non_interactive_session);
        assert_eq!(request.options.fast_mode, Some(false));
    }

    #[test]
    fn build_call_model_request_forwards_explicit_fast_mode_owner_state() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_FAST_MODE");
        let seeded = crate::tool::ToolUseContext::default().with_fast_mode(Some(true));
        let request = build_call_model_request(
            Vec::new(),
            ToolPermissionContext::default(),
            &QuerySource::Prompt,
            &[],
            &std::collections::BTreeMap::new(),
            &crate::state::app_state_store::McpState::default(),
            &seeded,
        );
        assert_eq!(request.options.fast_mode, Some(true));
    }

    #[test]
    fn build_call_model_request_uses_runtime_plan_model_selection_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_MODEL", "opusplan");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "default-opus");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "default-sonnet");

        let mut plan_context = ToolPermissionContext::default();
        plan_context.mode = crate::types::permissions::PermissionMode::Plan;
        let request = build_call_model_request(
            Vec::new(),
            plan_context.clone(),
            &QuerySource::Prompt,
            &[],
            &std::collections::BTreeMap::new(),
            &crate::state::app_state_store::McpState::default(),
            &crate::tool::ToolUseContext::default(),
        );
        assert_eq!(request.options.model, "default-opus");

        let over_200k_messages = vec![crate::types::message::Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "large plan context".to_string(),
                )],
                model: Some("default-sonnet".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: Some(crate::types::message::TokenUsage {
                    input_tokens: 200_001,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    ..Default::default()
                }),
            },
        )];
        let guarded_request = build_call_model_request(
            over_200k_messages,
            plan_context,
            &QuerySource::Prompt,
            &[],
            &std::collections::BTreeMap::new(),
            &crate::state::app_state_store::McpState::default(),
            &crate::tool::ToolUseContext::default(),
        );
        assert_eq!(guarded_request.options.model, "default-sonnet");

        crate::utils::process_env::remove("ANTHROPIC_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
    }

    #[test]
    fn query_actor_pairs_streamed_tool_use_with_error_result_on_model_error() {
        #[derive(Clone, Debug)]
        struct ToolUseThenErrorDeps;

        impl crate::query::deps::QueryDeps for ToolUseThenErrorDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                        crate::services::api::claude::ClaudeStreamItem::ToolUse {
                            id: "toolu_error_pair".to_string(),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "echo interrupted"}),
                            is_server: false,
                        },
                    ))
                    .await
                    .ok();
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::SystemError(
                            crate::types::message::SystemApiErrorMessage {
                                content: "stream failed after tool_use".to_string(),
                                api_error: "stream failed after tool_use".to_string(),
                                error: "stream_failed".to_string(),
                                error_details: Some("stream failed after tool_use".to_string()),
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-tool-use-then-error".to_string(),
            input: "run then fail".to_string(),
            messages: vec![RenderableMessage::user("u1", "run then fail")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                ToolUseThenErrorDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "model_error");
        let mut model_tool_result = false;
        let mut transcript_tool_result = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                QueryEvent::ModelMessage(crate::types::message::Message::User(user)) => {
                    model_tool_result |= user.content.iter().any(|content| {
                        matches!(
                            content,
                            crate::types::message::UserContent::ToolResult(result)
                                if result.tool_use_id.0 == "toolu_error_pair"
                                    && result.is_error
                                    && result.content == "stream failed after tool_use"
                        )
                    });
                }
                QueryEvent::Row(message) => {
                    if let Some(result) = row_user_tool_result(&message) {
                        transcript_tool_result |= result.tool_use_id.0 == "toolu_error_pair"
                            && result.derived_status() == ToolResultStatus::Error
                            && result.content == "stream failed after tool_use";
                    }
                }
                _ => {}
            }
        }
        assert!(model_tool_result);
        assert!(transcript_tool_result);
    }

    /// Regression: a background Agent never reached a terminal state.
    ///
    /// `AgentTool` detaches its background run with `handle.spawn` from INSIDE
    /// tool execution — that is, from inside the private per-query runtime
    /// [`spawn_query`] builds. CC's counterpart, `AgentTool.tsx:999`
    /// `void runWithAgentContext(...)`, is detached onto Node's ONE process
    /// event loop, so its lifetime is the process, not the turn.
    ///
    /// While the port resolved that spawn with `Handle::try_current()` the
    /// detached future landed on the parent turn's runtime and was destroyed
    /// with it: the subagent's `QueryHandle` (sole event `Receiver`) dropped,
    /// its actor died on a closed channel, `run_agent` never returned,
    /// `finish_async_agent_run` never ran, and the `LocalAgentTask` stayed
    /// `running` forever — no notification, and `TaskOutput` blocked.
    ///
    /// This drives the real `spawn_query`, not a fixture: `call_model` runs
    /// inside the actor, exactly where `AgentTool.call` reaches
    /// `launch_background_agent`. `spawn_query` sends `QueryEvent::Terminal`
    /// only AFTER dropping the runtime, so observing Terminal is a hard
    /// happens-after edge for "the parent turn's runtime is gone".
    ///
    /// The `Handle::try_current()` task is the negative control: it is the
    /// exact spawn that regressed, and it must NOT survive.
    #[test]
    fn detached_work_spawned_inside_a_query_actor_outlives_its_private_runtime() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // Stands in for the process runtime `main.rs` publishes.
        let process_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("process runtime");
        crate::utils::process_runtime::set_process_runtime_handle(process_runtime.handle().clone());

        let detached_finished = Arc::new(AtomicBool::new(false));
        let ambient_finished = Arc::new(AtomicBool::new(false));
        // Closed by the test only after the actor resolved; both detached
        // tasks park on it until then.
        let (release_tx, release_rx) = async_channel::unbounded::<()>();
        // Lets `call_model` prove both tasks were polled at least once before
        // the turn continues, so the control is not merely never-scheduled.
        let (started_tx, started_rx) = async_channel::unbounded::<()>();

        #[derive(Clone)]
        struct DetachedSpawnDeps {
            detached_finished: Arc<AtomicBool>,
            ambient_finished: Arc<AtomicBool>,
            release: async_channel::Receiver<()>,
            started_tx: async_channel::Sender<()>,
            started_rx: async_channel::Receiver<()>,
        }

        impl crate::query::deps::QueryDeps for DetachedSpawnDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let detached_finished = Arc::clone(&self.detached_finished);
                let ambient_finished = Arc::clone(&self.ambient_finished);
                let release_for_detached = self.release.clone();
                let release_for_ambient = self.release.clone();
                let started_for_detached = self.started_tx.clone();
                let started_for_ambient = self.started_tx.clone();
                let started_rx = self.started_rx.clone();
                Box::pin(async move {
                    // What the fix does: CC `void` semantics, process-lifetime.
                    let process_handle =
                        crate::utils::process_runtime::runtime_handle_for_detached_work()
                            .expect("detached runtime handle");
                    process_handle.spawn(async move {
                        let _ = started_for_detached.send(()).await;
                        let _ = release_for_detached.recv().await;
                        detached_finished.store(true, Ordering::SeqCst);
                    });
                    // What the bug did: bind the detached work to this turn.
                    let ambient_handle =
                        tokio::runtime::Handle::try_current().expect("ambient runtime handle");
                    ambient_handle.spawn(async move {
                        let _ = started_for_ambient.send(()).await;
                        let _ = release_for_ambient.recv().await;
                        ambient_finished.store(true, Ordering::SeqCst);
                    });
                    // Both tasks are now live and polled.
                    for _ in 0..2 {
                        let _ = started_rx.recv().await;
                    }

                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "spawned".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-detached-spawn".to_string(),
            input: "spawn detached work".to_string(),
            messages: vec![RenderableMessage::user("u1", "spawn detached work")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };

        let handle = spawn_query(
            params,
            DetachedSpawnDeps {
                detached_finished: Arc::clone(&detached_finished),
                ambient_finished: Arc::clone(&ambient_finished),
                release: release_rx,
                started_tx,
                started_rx,
            },
        );

        // Terminal is emitted after the per-query runtime's teardown
        // (`shutdown_timeout` — async tasks are cancelled at shutdown start,
        // before the send), so past this point the parent turn's runtime is
        // definitively gone.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut saw_terminal = false;
        while std::time::Instant::now() < deadline {
            match handle.events.recv_blocking() {
                Ok(QueryEvent::Terminal(_)) => {
                    saw_terminal = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(saw_terminal, "query actor must reach a terminal state");

        // Both tasks were alive before the drop; only lifetime decides now.
        release_tx.close();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !detached_finished.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            detached_finished.load(Ordering::SeqCst),
            "work detached onto the process runtime must still run after the \
             per-query runtime is dropped — this is CC's `void promise` lifetime"
        );
        assert!(
            !ambient_finished.load(Ordering::SeqCst),
            "control: work spawned via Handle::try_current() dies with the \
             per-query runtime, which is why background agents never finished"
        );
    }

    #[test]
    fn query_actor_surfaces_prompt_too_long_system_error_with_official_terminal_reason() {
        #[derive(Clone, Debug)]
        struct PromptTooLongDeps;

        impl crate::query::deps::QueryDeps for PromptTooLongDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(2);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::SystemError(
                            crate::types::message::SystemApiErrorMessage {
                                content:
                                    crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE
                                        .to_string(),
                                api_error: "invalid_request".to_string(),
                                error: "invalid_request".to_string(),
                                error_details: Some(
                                    "prompt is too long: 201000 tokens > 200000 maximum"
                                        .to_string(),
                                ),
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-prompt-too-long".to_string(),
            input: "large prompt".to_string(),
            messages: vec![RenderableMessage::user("u1", "large prompt")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                PromptTooLongDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "prompt_too_long");
        let mut saw_prompt_too_long = false;
        let mut saw_typed_metadata = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                QueryEvent::Message(message)
                    if model_is_api_error(&message)
                        && model_assistant_text(&message)
                            == Some(
                                crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE,
                            ) =>
                {
                    saw_prompt_too_long = true;
                }
                QueryEvent::ApiError(error)
                    if error.api_error == "invalid_request"
                        && error.error_details.as_deref().is_some_and(|details| {
                            details.contains("201000 tokens > 200000 maximum")
                        }) =>
                {
                    saw_typed_metadata = true;
                }
                _ => {}
            }
        }
        assert!(saw_prompt_too_long);
        assert!(saw_typed_metadata);
    }

    #[test]
    fn query_actor_surfaces_media_size_system_error_with_official_terminal_reason() {
        #[derive(Clone, Debug)]
        struct MediaSizeDeps;

        impl crate::query::deps::QueryDeps for MediaSizeDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(2);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::SystemError(
                            crate::types::message::SystemApiErrorMessage {
                                content: "image exceeds 5 MB maximum".to_string(),
                                api_error: "invalid_request".to_string(),
                                error: "invalid_request".to_string(),
                                error_details: Some("image exceeds 5 MB maximum".to_string()),
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-media-size".to_string(),
            input: "large image".to_string(),
            messages: vec![RenderableMessage::user("u1", "large image")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(params, MediaSizeDeps, event_tx, command_rx));

        assert_eq!(terminal.reason, "image_error");
        let mut saw_media_error = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(
                &event,
                QueryEvent::Message(message)
                    if model_is_api_error(message)
                        && model_assistant_text(message) == Some("image exceeds 5 MB maximum")
            ) {
                saw_media_error = true;
            }
        }
        assert!(saw_media_error);
    }

    #[test]
    fn query_actor_withholds_prompt_too_long_for_context_collapse_before_surface() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CONTEXT_COLLAPSE", "1");

        #[derive(Clone, Debug)]
        struct WithheldPromptTooLongDeps;

        impl crate::query::deps::QueryDeps for WithheldPromptTooLongDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::SystemError(
                            crate::types::message::SystemApiErrorMessage {
                                content:
                                    crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE
                                        .to_string(),
                                api_error: "invalid_request".to_string(),
                                error: "invalid_request".to_string(),
                                error_details: Some(
                                    "prompt is too long: 201000 tokens > 200000 maximum"
                                        .to_string(),
                                ),
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-withheld-prompt-too-long".to_string(),
            input: "large prompt".to_string(),
            messages: vec![RenderableMessage::user("u1", "large prompt")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                WithheldPromptTooLongDeps,
                event_tx,
                command_rx,
            ));

        crate::utils::process_env::remove("CLAUDE_CONTEXT_COLLAPSE");

        assert_eq!(terminal.reason, "prompt_too_long");
        let mut saw_prompt_too_long = false;
        let mut saw_empty_assistant_model_message = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                QueryEvent::Message(message)
                    if model_is_api_error(&message)
                        && model_assistant_text(&message)
                            == Some(
                                crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE,
                            ) =>
                {
                    saw_prompt_too_long = true;
                }
                QueryEvent::ModelMessage(crate::types::message::Message::Assistant(assistant))
                    if assistant.content.is_empty() =>
                {
                    saw_empty_assistant_model_message = true;
                }
                _ => {}
            }
        }
        assert!(saw_prompt_too_long);
        assert!(!saw_empty_assistant_model_message);
    }

    #[tokio::test]
    async fn query_actor_pairs_streamed_tool_use_with_interrupted_result_on_abort() {
        #[derive(Clone, Debug)]
        struct PendingToolUseDeps;

        impl crate::query::deps::QueryDeps for PendingToolUseDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tokio::spawn(async move {
                        tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                            crate::services::api::claude::ClaudeStreamItem::ToolUse {
                                id: "toolu_abort_pair".to_string(),
                                name: "Bash".to_string(),
                                input: serde_json::json!({"command": "echo abort"}),
                                is_server: false,
                            },
                        ))
                        .await
                        .ok();
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    });
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-tool-use-abort".to_string(),
            input: "run then abort".to_string(),
            messages: vec![RenderableMessage::user("u1", "run then abort")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();
        let actor = tokio::spawn(run_query_actor(
            params,
            PendingToolUseDeps,
            event_tx,
            command_rx,
        ));

        loop {
            match event_rx.recv().await.unwrap() {
                QueryEvent::Message(message)
                    if model_assistant_tool_use(&message)
                        .is_some_and(|tool_use| tool_use.id.0 == "toolu_abort_pair") =>
                {
                    break;
                }
                _ => {}
            }
        }
        command_tx.send(QueryCommand::Abort).await.unwrap();
        let terminal = actor.await.unwrap();

        assert_eq!(terminal.reason, "aborted_streaming");
        let mut model_tool_result = false;
        let mut transcript_tool_result = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                QueryEvent::ModelMessage(crate::types::message::Message::User(user)) => {
                    model_tool_result |= user.content.iter().any(|content| {
                        matches!(
                            content,
                            crate::types::message::UserContent::ToolResult(result)
                                if result.tool_use_id.0 == "toolu_abort_pair"
                                    && result.is_error
                                    && result.content == "Interrupted by user"
                        )
                    });
                }
                QueryEvent::Row(message) => {
                    if let Some(result) = row_user_tool_result(&message) {
                        transcript_tool_result |= result.tool_use_id.0 == "toolu_abort_pair"
                            && result.derived_status() == ToolResultStatus::Error
                            && result.content == "Interrupted by user";
                    }
                }
                _ => {}
            }
        }
        assert!(model_tool_result);
        assert!(transcript_tool_result);
    }

    #[tokio::test]
    async fn query_actor_pairs_unresolved_tool_uses_with_interrupted_results_on_permission_abort() {
        #[derive(Clone, Debug)]
        struct TwoToolUseDeps;

        impl crate::query::deps::QueryDeps for TwoToolUseDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![
                                    crate::types::message::AssistantContent::ToolUse(
                                        crate::types::message::ToolUseBlock {
                                            id: crate::types::ids::ToolUseId(
                                                "toolu_abort_permission_1".to_string(),
                                            ),
                                            name: "Bash".to_string(),
                                            input: serde_json::json!({"command": "printf one"}),
                                        },
                                    ),
                                    crate::types::message::AssistantContent::ToolUse(
                                        crate::types::message::ToolUseBlock {
                                            id: crate::types::ids::ToolUseId(
                                                "toolu_abort_permission_2".to_string(),
                                            ),
                                            name: "Bash".to_string(),
                                            input: serde_json::json!({"command": "printf two"}),
                                        },
                                    ),
                                ],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-permission-abort".to_string(),
            input: "run two then abort".to_string(),
            messages: vec![RenderableMessage::user("u1", "run two then abort")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();
        let actor = tokio::spawn(run_query_actor(
            params,
            TwoToolUseDeps,
            event_tx,
            command_rx,
        ));

        // The REPL's in-progress set as the actor's setter leaves it.
        fn track_in_progress(set: &mut std::collections::HashSet<String>, event: &QueryEvent) {
            if let QueryEvent::SetInProgressToolUse {
                tool_use_id,
                in_progress,
            } = event
            {
                if *in_progress {
                    set.insert(tool_use_id.clone());
                } else {
                    set.remove(tool_use_id);
                }
            }
        }
        let mut in_progress = std::collections::HashSet::new();
        loop {
            let event = event_rx.recv().await.unwrap();
            track_in_progress(&mut in_progress, &event);
            match event {
                QueryEvent::PermissionRequest(request) => {
                    assert_eq!(request.tool_use_id, "toolu_abort_permission_1");
                    break;
                }
                _ => {}
            }
        }
        let waiting_tool_was_in_progress = in_progress.contains("toolu_abort_permission_1");
        command_tx.send(QueryCommand::Abort).await.unwrap();
        let terminal = actor.await.unwrap();

        assert_eq!(terminal.reason, "aborted_tools");
        let mut tool_result_ids = std::collections::HashSet::new();
        while let Ok(event) = event_rx.try_recv() {
            track_in_progress(&mut in_progress, &event);
            if let QueryEvent::ModelMessage(crate::types::message::Message::User(user)) = event {
                for content in user.content {
                    if let crate::types::message::UserContent::ToolResult(result) = content {
                        if result.is_error && result.content == "Interrupted by user" {
                            tool_result_ids.insert(result.tool_use_id.0);
                        }
                    }
                }
            }
        }
        assert!(tool_result_ids.contains("toolu_abort_permission_1"));
        assert!(tool_result_ids.contains("toolu_abort_permission_2"));
        // CC runs `markToolUseAsComplete` after every tool call, interrupted
        // ones included, so nothing outlives the aborted turn in the set.
        assert!(
            waiting_tool_was_in_progress,
            "precondition: the tool waiting on permission is in progress"
        );
        assert!(
            in_progress.is_empty(),
            "tools left in progress after the abort: {in_progress:?}"
        );
    }

    #[tokio::test]
    async fn query_actor_emits_max_turns_attachment_on_aborted_tools_when_bound_reached() {
        #[derive(Clone, Debug)]
        struct SingleToolUseDeps;

        impl crate::query::deps::QueryDeps for SingleToolUseDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::ToolUse(
                                    crate::types::message::ToolUseBlock {
                                        id: crate::types::ids::ToolUseId(
                                            "toolu_abort_max_turns".to_string(),
                                        ),
                                        name: "Bash".to_string(),
                                        input: serde_json::json!({"command": "printf one"}),
                                    },
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-abort-max-turns".to_string(),
            input: "run then abort at max turns".to_string(),
            messages: vec![RenderableMessage::user("u1", "run then abort at max turns")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: Some(1),
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();
        let actor = tokio::spawn(run_query_actor(
            params,
            SingleToolUseDeps,
            event_tx,
            command_rx,
        ));

        loop {
            match event_rx.recv().await.unwrap() {
                QueryEvent::PermissionRequest(request) => {
                    assert_eq!(request.tool_use_id, "toolu_abort_max_turns");
                    break;
                }
                _ => {}
            }
        }
        command_tx.send(QueryCommand::Abort).await.unwrap();
        let terminal = actor.await.unwrap();

        assert_eq!(terminal.reason, "aborted_tools");
        // Batch D3: one whole `Message` (CC query.ts:1706-1710) carries both
        // the render and model halves; the old Row+ModelMessage pair is gone.
        let mut saw_max_turns_message = false;
        while let Ok(event) = event_rx.try_recv() {
            if let QueryEvent::Message(crate::types::message::Message::Attachment(attachment)) =
                event
            {
                saw_max_turns_message = matches!(
                    attachment.attachment,
                    Attachment::MaxTurnsReached {
                        max_turns: 1,
                        turn_count: 2,
                    }
                );
            }
        }
        assert!(saw_max_turns_message);
    }

    #[test]
    fn query_actor_forwards_final_assistant_before_system_error_terminal() {
        #[derive(Clone, Debug)]
        struct AssistantThenErrorDeps;

        impl crate::query::deps::QueryDeps for AssistantThenErrorDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "partial refusal text".to_string(),
                                )],
                                model: Some("claude-opus-4-20250514".to_string()),
                                stop_reason: Some(crate::types::message::StopReason::Refusal),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::SystemError(
                            crate::types::message::SystemApiErrorMessage {
                                content: "refusal system error".to_string(),
                                api_error: "refusal system error".to_string(),
                                error: "invalid_request".to_string(),
                                error_details: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-assistant-then-error".to_string(),
            input: "refusal".to_string(),
            messages: vec![RenderableMessage::user("u1", "refusal")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                AssistantThenErrorDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "model_error");
        let mut saw_final_assistant = false;
        let mut saw_system_error = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                // Streamed-assistant convergence: the streamed refusal
                // assistant arrives as a whole `QueryEvent::Message`; the
                // error path no longer re-flushes it as ModelMessage.
                QueryEvent::Message(crate::types::message::Message::Assistant(assistant)) => {
                    saw_final_assistant |= assistant.stop_reason
                        == Some(crate::types::message::StopReason::Refusal)
                        && assistant.content.iter().any(|content| {
                            matches!(
                                content,
                                crate::types::message::AssistantContent::Text(text)
                                    if text == "partial refusal text"
                            )
                        });
                    saw_system_error |= assistant.content.iter().any(|content| {
                        matches!(
                            content,
                            crate::types::message::AssistantContent::MessageIdentity(identity)
                                if identity.is_api_error_message
                        )
                    }) && assistant.content.iter().any(|content| {
                        matches!(
                            content,
                            crate::types::message::AssistantContent::Text(text)
                                if text == "refusal system error"
                        )
                    });
                }
                _ => {}
            }
        }
        assert!(saw_final_assistant);
        assert!(saw_system_error);
    }

    #[test]
    fn query_loop_executes_post_sampling_hooks_after_model_response() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::hooks::post_sampling_hooks::clear_post_sampling_hooks();

        #[derive(Clone, Debug)]
        struct FinalTextDeps;

        impl crate::query::deps::QueryDeps for FinalTextDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "post sampling done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_hook = calls.clone();
        crate::utils::hooks::post_sampling_hooks::register_post_sampling_hook(std::sync::Arc::new(
            move |context: crate::utils::hooks::post_sampling_hooks::REPLHookContext| {
                let calls_for_hook = calls_for_hook.clone();
                Box::pin(async move {
                    calls_for_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    assert!(context.messages.iter().any(|message| matches!(
                        message,
                        crate::types::message::Message::Assistant(assistant)
                            if assistant.content.iter().any(|content| matches!(
                                content,
                                crate::types::message::AssistantContent::Text(text)
                                    if text == "post sampling done"
                            ))
                    )));
                    let tracking = context
                        .tool_use_context
                        .query_tracking
                        .expect("post-sampling hook should receive query tracking");
                    assert_eq!(tracking.depth, 0);
                    assert!(!tracking.chain_id.is_empty());
                })
            },
        ));

        let params = QueryParams {
            turn_id: "turn-post-sampling".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(params, FinalTextDeps, event_tx, command_rx));

        assert_eq!(terminal.reason, "completed");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        crate::utils::hooks::post_sampling_hooks::clear_post_sampling_hooks();
    }

    fn typed_user_message(text: &str) -> crate::types::message::Message {
        crate::types::message::Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(text.to_string())],
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

    fn typed_compact_boundary() -> crate::types::message::Message {
        crate::types::message::Message::System(
            crate::types::message::SystemMessage::compact_boundary(None),
        )
    }

    #[test]
    fn query_loop_prefers_typed_model_messages_over_transcript_bridge() {
        #[derive(Clone, Debug)]
        struct CaptureDeps {
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for CaptureDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let typed_history = vec![
            typed_user_message("before typed compact"),
            typed_compact_boundary(),
            typed_user_message("typed history prompt"),
        ];
        let params = QueryParams {
            turn_id: "turn-typed-history".to_string(),
            input: "fallback should not be used".to_string(),
            messages: vec![RenderableMessage::user("u1", "transcript-only prompt")],
            model_messages: typed_history,
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                CaptureDeps {
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen_messages.lock().unwrap();
        let first_request = seen.first().expect("callModel should be invoked");
        assert!(first_request.iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if matches!(
                    user.content.first(),
                    Some(crate::types::message::UserContent::Text(text))
                        if text == "typed history prompt"
                )
        )));
        assert!(!first_request.iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if matches!(
                    user.content.first(),
                    Some(crate::types::message::UserContent::Text(text))
                        if text == "before typed compact" || text == "transcript-only prompt"
                )
        )));
    }

    #[test]
    fn query_actor_yields_typed_model_messages_for_repl_history() {
        #[derive(Clone, Debug)]
        struct FinalAssistantDeps;

        impl crate::query::deps::QueryDeps for FinalAssistantDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "typed assistant".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-model-event".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                FinalAssistantDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut saw_model_message = false;
        while let Ok(event) = event_rx.try_recv() {
            if let QueryEvent::Message(crate::types::message::Message::Assistant(assistant)) = event
            {
                saw_model_message = assistant.content.iter().any(|content| {
                    matches!(
                        content,
                        crate::types::message::AssistantContent::Text(text)
                            if text == "typed assistant"
                    )
                });
            }
        }
        assert!(saw_model_message);
    }

    #[test]
    fn query_directory_contains_only_official_aligned_modules() {
        let allowed = [
            "config.rs",
            "deps.rs",
            // Rust-only A2/A6 transport for async-generator next()/return().
            "event_channel.rs",
            // Test-only split of this owner, not an additional runtime module.
            "prompt_context_tests.rs",
            "stop_hooks.rs",
            "token_budget.rs",
            "transitions.rs",
        ];
        let allowed = allowed
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let entries = std::fs::read_dir("src/query")
            .expect("query module directory should exist")
            .map(|entry| entry.expect("query dir entry should be readable"))
            .collect::<Vec<_>>();

        for entry in entries {
            let file_name = entry.file_name().to_string_lossy().to_string();
            assert!(
                entry.file_type().is_ok_and(|file_type| file_type.is_file())
                    && allowed.contains(file_name.as_str()),
                "src/query contains non-official-aligned module: {file_name}"
            );
        }
    }

    // C3c-3: the boundary-walk / assistant-rehydration / adjacent-block-merge
    // unit tests died with `renderable_messages_to_model_messages` — typed
    // history is accumulated from whole-message events, never reconstructed
    // from transcript rows. The surviving legacy shim only reads the carried
    // `UserMessage` (tests below).

    #[test]
    fn renderable_meta_prompt_stays_model_visible_like_official_is_meta() {
        let params = QueryParams {
            turn_id: "turn-meta-prompt".to_string(),
            input: "/init".to_string(),
            // The command row carries the wire text itself; the meta
            // expansion is a `MetaText` block (model-visible, render-hidden).
            messages: vec![
                RenderableMessage::user(
                    "init-command",
                    "<command-message>init</command-message>\n<command-name>/init</command-name>",
                ),
                RenderableMessage::user_block(
                    "meta-init",
                    UserContent::MetaText("hidden model-facing command expansion".to_string()),
                ),
            ],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };

        let model_messages = legacy_model_messages_from_params(&params);
        assert!(matches!(
            model_messages.as_slice(),
            [
                crate::types::message::Message::User(metadata),
                crate::types::message::Message::User(prompt),
            ] if matches!(
                metadata.content.as_slice(),
                [crate::types::message::UserContent::Text(text)]
                    if text == "<command-message>init</command-message>\n<command-name>/init</command-name>"
            ) && matches!(
                prompt.content.as_slice(),
                [crate::types::message::UserContent::MetaText(text)]
                    if text == "hidden model-facing command expansion"
            )
        ));
    }

    #[test]
    fn renderable_plan_command_preserves_model_visible_local_jsx_messages() {
        let params = QueryParams {
            turn_id: "turn-plan".to_string(),
            input: "/plan implement carefully".to_string(),
            // Rows carry the wire text; the old `model_content` dual
            // track is gone, so the same text is display AND model history.
            messages: vec![
                RenderableMessage::user("plan-command", "/plan implement carefully"),
                RenderableMessage::user(
                    "plan-output",
                    "<local-command-stdout>Enabled plan mode</local-command-stdout>",
                ),
            ],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };

        let model_messages = legacy_model_messages_from_params(&params);
        assert!(matches!(
            model_messages.as_slice(),
            [
                crate::types::message::Message::User(command),
                crate::types::message::Message::User(output),
            ] if matches!(
                command.content.as_slice(),
                [crate::types::message::UserContent::Text(text)]
                    if text == "/plan implement carefully"
            ) && matches!(
                output.content.as_slice(),
                [crate::types::message::UserContent::Text(text)]
                    if text == "<local-command-stdout>Enabled plan mode</local-command-stdout>"
            )
        ));
    }

    /// No Glob display shape — every row keeps its raw; the by-tool-name
    /// renderer consumes it and a malformed success renders nothing.
    #[test]
    fn model_messages_restore_glob_success_emit_gate() {
        let tool_use_id = crate::types::ids::ToolUseId("toolu-glob-success".to_string());
        let messages = vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: tool_use_id.clone(),
                        name: "Glob".to_string(),
                        input: serde_json::json!({"pattern": "src/**/*.rs"}),
                    },
                )],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            crate::types::message::Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id,
                        content: "src/lib.rs\nsrc/main.rs".to_string(),
                        is_error: false,
                        content_blocks: Vec::new(),
                        tool_use_result: Some(serde_json::json!({
                            "durationMs": 19,
                            "numFiles": 2,
                            "filenames": ["src/lib.rs", "src/main.rs"],
                            "truncated": true
                        })),
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
            }),
        ];

        let rendered = crate::utils::messages::normalize_messages(&messages);
        let result = rendered
            .last()
            .and_then(|row| match &row.kind {
                crate::types::message::RenderableMessageKind::User { message } => {
                    match message.first_content_block() {
                        Some(crate::types::message::UserContent::ToolResult(result)) => {
                            Some(result.clone())
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .expect("glob tool result row");
        let raw = result.tool_use_result.expect("raw rides the row");
        assert_eq!(raw["numFiles"], serde_json::json!(2));
        assert_eq!(raw["truncated"], serde_json::json!(true));
    }

    #[test]
    fn model_messages_use_canonical_read_output_parser_and_keep_raw_shape() {
        fn pair(raw_output: serde_json::Value) -> Vec<crate::types::message::Message> {
            let tool_use_id = crate::types::ids::ToolUseId("toolu-read-output".to_string());
            vec![
                crate::types::message::Message::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![crate::types::message::AssistantContent::ToolUse(
                            crate::types::message::ToolUseBlock {
                                id: tool_use_id.clone(),
                                name: "Read".to_string(),
                                input: serde_json::json!({"file_path": "src/lib.rs"}),
                            },
                        )],
                        model: None,
                        stop_reason: Some(crate::types::message::StopReason::ToolUse),
                        usage: None,
                    },
                ),
                crate::types::message::Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id,
                            content: "1\tcontent".to_string(),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: Some(raw_output),
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
                }),
            ]
        }

        let valid = serde_json::json!({
            "type": "text",
            "unknownOuter": true,
            "file": {
                "filePath": "src/lib.rs",
                "content": "content",
                "numLines": 1,
                "startLine": 1,
                "totalLines": 1,
                "unknownFile": [1, 2, 3]
            }
        });
        // The raw itself rides the row (`tool_use_result`); visibility
        // is a render-time decision.
        let rendered = crate::utils::messages::normalize_messages(&pair(valid.clone()));
        assert!(matches!(
            rendered.last().and_then(row_user_tool_result),
            Some(result) if result.tool_use_result.as_ref() == Some(&valid)
        ));

        let invalid = serde_json::json!({
            "type": "text",
            "file": {"filePath": "src/lib.rs", "content": "content", "numLines": 1}
        });
        // Visibility is a render-time decision; the seed keeps
        // the malformed raw on the row.
        let rendered = crate::utils::messages::normalize_messages(&pair(invalid.clone()));
        assert!(matches!(
            rendered.last().and_then(row_user_tool_result),
            Some(result) if result.tool_use_result.as_ref() == Some(&invalid)
        ));
    }

    #[test]
    fn model_messages_preserve_glob_error_tool_use_result() {
        let tool_use_id = crate::types::ids::ToolUseId("toolu-glob-error".to_string());
        let messages = vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: tool_use_id.clone(),
                        name: "Glob".to_string(),
                        input: serde_json::json!({"pattern": "*", "path": "missing"}),
                    },
                )],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            crate::types::message::Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id,
                        content:
                            "<tool_use_error>Directory does not exist: missing.</tool_use_error>"
                                .to_string(),
                        is_error: true,
                        content_blocks: Vec::new(),
                        tool_use_result: Some(serde_json::Value::String(
                            "Error: Directory does not exist: missing.".to_string(),
                        )),
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
            }),
        ];

        let rendered = crate::utils::messages::normalize_messages(&messages);
        // The error raw stays on the row; the error leaf renders from
        // the tool-result content.
        assert!(matches!(
            rendered.last().and_then(row_user_tool_result),
            Some(result) if matches!(
                result.tool_use_result.as_ref(),
                Some(serde_json::Value::String(raw))
                    if raw == "Error: Directory does not exist: missing."
            )
        ));
    }

    #[test]
    fn model_messages_restore_strict_grep_success_error_and_malformed_tool_use_results() {
        fn pair(
            id: &str,
            is_error: bool,
            tool_use_result: Option<serde_json::Value>,
        ) -> Vec<crate::types::message::Message> {
            let tool_use_id = crate::types::ids::ToolUseId(id.to_string());
            vec![
                crate::types::message::Message::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![crate::types::message::AssistantContent::ToolUse(
                            crate::types::message::ToolUseBlock {
                                id: tool_use_id.clone(),
                                name: "Grep".to_string(),
                                input: serde_json::json!({"pattern": "needle"}),
                            },
                        )],
                        model: None,
                        stop_reason: Some(crate::types::message::StopReason::ToolUse),
                        usage: None,
                    },
                ),
                crate::types::message::Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id,
                            content: if is_error {
                                "<tool_use_error>boom</tool_use_error>".to_string()
                            } else {
                                "result".to_string()
                            },
                            is_error,
                            content_blocks: Vec::new(),
                            tool_use_result,
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
                }),
            ]
        }

        // No Grep display shape — a parseable success keeps its raw on
        // the row and the strict-number fidelity now lives in the raw value
        // the by-tool-name renderer parses.
        let raw = serde_json::json!({
            "mode": "count",
            "numFiles": 1.5,
            "filenames": [],
            "content": "src/a.rs:2",
            "numMatches": -0.5,
            "appliedLimit": 2.5
        });
        let success = crate::utils::messages::normalize_messages(&pair(
            "toolu-grep-success",
            false,
            Some(raw.clone()),
        ));
        assert!(matches!(
            success.last().and_then(row_user_tool_result),
            Some(result) if result.tool_use_result.as_ref() == Some(&raw)
        ));
        let parsed = crate::tools::grep_tool::ui::parse_output(&raw).expect("strict output parses");
        assert_eq!(parsed.num_files, serde_json::Number::from_f64(1.5).unwrap());
        assert_eq!(parsed.num_matches, serde_json::Number::from_f64(-0.5));
        assert_eq!(parsed.applied_limit, serde_json::Number::from_f64(2.5));

        let error = crate::utils::messages::normalize_messages(&pair(
            "toolu-grep-error",
            true,
            Some(serde_json::json!("Error: ripgrep failed")),
        ));
        assert!(matches!(
            error.last().and_then(row_user_tool_result),
            Some(result) if result.tool_use_result
                == Some(serde_json::json!("Error: ripgrep failed"))
        ));

        let malformed = crate::utils::messages::normalize_messages(&pair(
            "toolu-grep-malformed",
            false,
            Some(serde_json::json!({"numFiles": 1})),
        ));
        assert!(matches!(
            malformed.last().and_then(row_user_tool_result),
            Some(result) if result.tool_use_result
                == Some(serde_json::json!({"numFiles": 1}))
        ));
    }

    #[test]
    fn model_messages_preserve_notebook_validation_error_tool_use_result() {
        let tool_use_id = crate::types::ids::ToolUseId("toolu-notebook-error".to_string());
        let messages = vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: tool_use_id.clone(),
                        name: "NotebookEdit".to_string(),
                        input: serde_json::json!({
                            "notebook_path": "/tmp/demo.ipynb",
                            "cell_id": "missing",
                            "new_source": "new"
                        }),
                    },
                )],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            crate::types::message::Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id,
                        content: "<tool_use_error>Cell missing.</tool_use_error>".to_string(),
                        is_error: true,
                        content_blocks: Vec::new(),
                        tool_use_result: Some(serde_json::Value::String(
                            "Error: Cell missing.".to_string(),
                        )),
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
            }),
        ];

        let rendered = crate::utils::messages::normalize_messages(&messages);
        // The error raw stays on the row; the display is Generic and the
        // error leaf renders from the tool-result content.
        assert!(matches!(
            rendered.last().and_then(row_user_tool_result),
            Some(result) if result.content == "<tool_use_error>Cell missing.</tool_use_error>"
                && matches!(
                    result.tool_use_result.as_ref(),
                    Some(serde_json::Value::String(raw)) if raw == "Error: Cell missing."
                )
        ));
    }

    #[test]
    fn model_messages_restore_notebook_success_tool_use_result() {
        let tool_use_id = crate::types::ids::ToolUseId("toolu-notebook-success".to_string());
        let messages = vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: tool_use_id.clone(),
                        name: "NotebookEdit".to_string(),
                        input: serde_json::json!({
                            "notebook_path": "/tmp/demo.ipynb",
                            "cell_id": "cell-a",
                            "new_source": "print('new')"
                        }),
                    },
                )],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            crate::types::message::Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id,
                        content: "Updated cell cell-a".to_string(),
                        is_error: false,
                        content_blocks: Vec::new(),
                        tool_use_result: Some(serde_json::json!({
                            "new_source": "print('new')",
                            "cell_id": "cell-a",
                            "cell_type": "code",
                            "language": "python",
                            "edit_mode": "replace",
                            "error": "",
                            "notebook_path": "/tmp/demo.ipynb",
                            "original_file": "old notebook",
                            "updated_file": "new notebook"
                        })),
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
            }),
        ];

        let rendered = crate::utils::messages::normalize_messages(&messages);
        // No NotebookEdit display shape — a parseable success keeps its
        // raw on the row and the by-tool-name renderer consumes it.
        let result = rendered
            .last()
            .and_then(row_user_tool_result)
            .expect("notebook tool result row");
        let raw = result.tool_use_result.as_ref().expect("raw rides the row");
        assert_eq!(raw["cell_id"], serde_json::json!("cell-a"));
        assert!(crate::tools::notebook_edit_tool::ui::parse_output(raw).is_some());
    }

    #[test]
    fn query_actor_preempts_model_call_at_blocking_limit() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _blocking_limit = EnvVarGuard::set("CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE", "10");
        let _disable_auto_compact = EnvVarGuard::set("DISABLE_AUTO_COMPACT", "1");

        #[derive(Clone, Debug)]
        struct BlockingDeps {
            called: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }

        impl crate::query::deps::QueryDeps for BlockingDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.called.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (_tx, rx) = tokio::sync::mpsc::channel(1);
                    Ok(rx)
                })
            }
        }

        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let params = QueryParams {
            turn_id: "turn-blocking-limit".to_string(),
            input: "large prompt".to_string(),
            messages: Vec::new(),
            model_messages: vec![crate::types::message::Message::User(
                crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        "x".repeat(1_000_000),
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
                },
            )],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                BlockingDeps {
                    called: called.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "blocking_limit");
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        // Maps to: CC `query.ts:974,990` yielding ONE `createAssistantAPIErrorMessage`,
        // which `REPL.tsx:3496` appends to `messages` — the next turn's model
        // history. C3c-3: the single `QueryEvent::Message` carries that whole
        // assistant; the receiving edge records it in history AND derives the
        // transcript row via `normalize_messages`, so `query.ts:1262`'s
        // `lastMessage?.isApiErrorMessage` stop-hook guard can read it.
        let history_error = events
            .iter()
            .find_map(|event| match event {
                QueryEvent::Message(Message::Assistant(assistant)) => Some(assistant),
                _ => None,
            })
            .expect("API error must enter model history, not just the transcript");
        assert!(history_error.content.iter().any(|block| matches!(
            block,
            crate::types::message::AssistantContent::MessageIdentity(identity)
                if identity.is_api_error_message
        )));
        assert!(history_error.content.iter().any(|block| matches!(
            block,
            crate::types::message::AssistantContent::Text(text)
                if text == crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE
        )));
    }

    #[test]
    fn query_actor_runs_microcompact_and_autocompact_before_call_model() {
        #[derive(Clone, Debug)]
        struct CompactingDeps {
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for CompactingDeps {
            fn microcompact(
                &self,
                mut messages_for_query: Vec<crate::types::message::Message>,
                _tool_use_context: &crate::tool::ToolUseContext,
                _query_source: &QuerySource,
            ) -> crate::query::deps::MicrocompactResult {
                messages_for_query.push(crate::types::message::Message::User(
                    crate::types::message::UserMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![crate::types::message::UserContent::Text(
                            "microcompact marker".to_string(),
                        )],
                        is_compact_summary: true,
                        plan_content: None,
                        image_paste_ids: None,
                        is_visible_in_transcript_only: false,
                        mcp_meta: None,
                        source_tool_assistant_uuid: None,
                        permission_mode: None,
                        origin: None,
                        summarize_metadata: None,
                    },
                ));
                crate::query::deps::MicrocompactResult {
                    messages: messages_for_query,
                    // CC query.ts yields boundary messages as whole Messages.
                    boundary_messages: vec![crate::types::message::Message::System(
                        crate::types::message::SystemMessage::informational(
                            "microcompact boundary",
                            crate::types::message::SystemMessageLevel::Info,
                        ),
                    )],
                    compaction_info: None,
                }
            }

            fn autocompact(
                &self,
                mut messages_for_query: Vec<crate::types::message::Message>,
                tool_use_context: &crate::tool::ToolUseContext,
                _cache_safe_params: crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
                _query_source: &QuerySource,
                _tracking: Option<crate::services::compact::auto_compact::AutoCompactTrackingState>,
                _snip_tokens_freed: i64,
            ) -> futures::future::BoxFuture<'static, crate::query::deps::AutocompactResult>
            {
                // Reproduce the real contract: `compact_conversation` clears and
                // rebuilds the read-file state THROUGH THE SHARED HANDLE
                // (`compact.rs:653-665` clones the context, and
                // `ToolUseContext::read_file_state` is a
                // `SharedFileStateCache(Arc<Mutex<..>>)`, so the clone is the
                // same cache). The `rebuilt_read_file_state` this returns is the
                // durable snapshot, not the transport — `query.rs:1042` only
                // uses it as a "did it rebuild?" signal. A mock that filled the
                // Vec without touching the handle was testing a contract the
                // production path does not have.
                tool_use_context.read_file_state.set_entry(
                    crate::utils::query_helpers::ReadFileStateEntry {
                        path: "/tmp/post-compact.rs".to_string(),
                        content: Some("fresh".to_string()),
                        timestamp_ms: Some(1),
                        offset: Some(serde_json::json!(1)),
                        limit: None,
                        is_partial_view: false,
                        source: crate::utils::query_helpers::ReadFileStateSource::Read,
                    },
                );
                messages_for_query.push(crate::types::message::Message::User(
                    crate::types::message::UserMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![crate::types::message::UserContent::Text(
                            "autocompact marker".to_string(),
                        )],
                        is_compact_summary: true,
                        plan_content: None,
                        image_paste_ids: None,
                        is_visible_in_transcript_only: false,
                        mcp_meta: None,
                        source_tool_assistant_uuid: None,
                        permission_mode: None,
                        origin: None,
                        summarize_metadata: None,
                    },
                ));
                // CC autocompact carries no separate boundary: the boundary is
                // postCompactMessages[0] (query.ts:528-535). Mirror that shape
                // so the replay assertions below see the boundary reset.
                let mut post_compact_messages = vec![crate::types::message::Message::System(
                    crate::types::message::SystemMessage::compact_boundary(None),
                )];
                post_compact_messages.append(&mut messages_for_query);
                Box::pin(async move {
                    crate::query::deps::AutocompactResult {
                        messages: post_compact_messages,
                        compacted: true,
                        consecutive_failures: None,
                        rebuilt_read_file_state: Some(vec![
                            crate::utils::query_helpers::ReadFileStateEntry {
                                path: "/tmp/post-compact.rs".to_string(),
                                content: Some("fresh".to_string()),
                                timestamp_ms: Some(1),
                                offset: Some(serde_json::json!(1)),
                                limit: None,
                                is_partial_view: false,
                                source: crate::utils::query_helpers::ReadFileStateSource::Read,
                            },
                        ]),
                    }
                })
            }

            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let params = QueryParams {
            turn_id: "turn-compact-flow".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                CompactingDeps {
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen_messages.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let user_texts = seen[0]
            .iter()
            .filter_map(|message| match message {
                crate::types::message::Message::User(user) => Some(
                    user.content
                        .iter()
                        .filter_map(|content| match content {
                            crate::types::message::UserContent::Text(text) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(
            user_texts,
            vec!["hello", "microcompact marker", "autocompact marker"]
        );

        let mut compacted_model_texts = Vec::new();
        let mut saw_compact_boundary_message = false;
        let mut rebuilt_read_state = None;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                // CC query.ts:528-535: the post-compact flood arrives as whole
                // Message yields (boundary first), not ModelMessage replays.
                QueryEvent::Message(crate::types::message::Message::User(user)) => {
                    for content in user.content {
                        if let crate::types::message::UserContent::Text(text) = content {
                            compacted_model_texts.push(text);
                        }
                    }
                }
                QueryEvent::Message(message)
                    if crate::utils::messages::is_compact_boundary_message(&message) =>
                {
                    saw_compact_boundary_message = true;
                }
                QueryEvent::ToolContextUpdate(context) => {
                    if context
                        .read_file_state
                        .snapshot()
                        .into_iter()
                        .any(|entry| entry.path == "/tmp/post-compact.rs")
                    {
                        rebuilt_read_state = Some(context.read_file_state.snapshot());
                    }
                }
                _ => {}
            }
        }
        assert!(
            compacted_model_texts
                .iter()
                .any(|text| text == "autocompact marker")
        );
        assert!(
            saw_compact_boundary_message,
            "the compact boundary must arrive as a whole Message (CC query.ts:528-535)"
        );
        assert_eq!(
            rebuilt_read_state.unwrap()[0].content.as_deref(),
            Some("fresh")
        );
    }

    #[test]
    fn query_actor_defers_cached_microcompact_boundary_until_api_usage_delta() {
        let _cached_mc_guard = crate::services::compact::micro_compact::TEST_CACHED_MC_LOCK
            .lock()
            .unwrap();
        crate::services::compact::micro_compact::reset_microcompact_state();

        #[derive(Clone, Debug)]
        struct CachedMcDeps {
            seen_requests:
                std::sync::Arc<std::sync::Mutex<Vec<crate::query::deps::CallModelRequest>>>,
        }

        impl crate::query::deps::QueryDeps for CachedMcDeps {
            fn microcompact(
                &self,
                messages_for_query: Vec<crate::types::message::Message>,
                _tool_use_context: &crate::tool::ToolUseContext,
                _query_source: &QuerySource,
            ) -> crate::query::deps::MicrocompactResult {
                crate::query::deps::MicrocompactResult {
                    messages: messages_for_query,
                    boundary_messages: Vec::new(),
                    compaction_info: Some(
                        crate::services::compact::micro_compact::MicrocompactCompactionInfo {
                            pending_cache_edits: Some(
                                crate::services::compact::micro_compact::PendingCacheEdits::auto(
                                    vec!["toolu_cached_old".to_string()],
                                    10,
                                ),
                            ),
                        },
                    ),
                }
            }

            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_requests.lock().unwrap().push(request);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: Some(crate::types::message::TokenUsage {
                                    cache_deleted_input_tokens: 42,
                                    ..Default::default()
                                }),
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-cached-mc".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();
        let seen_requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                CachedMcDeps {
                    seen_requests: seen_requests.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let requests = seen_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].options.cached_mc_enabled);
        assert_eq!(
            requests[0]
                .options
                .cached_mc_new_cache_edits
                .as_ref()
                .and_then(|block| block.edits.first())
                .map(|edit| edit.cache_reference.as_str()),
            Some("toolu_cached_old")
        );
        drop(requests);
        let mut saw_boundary = false;
        while let Ok(event) = event_rx.try_recv() {
            // CC query.ts:884: the deferred cached-MC boundary is a whole
            // system Message yield, not a render-only row.
            if let QueryEvent::Message(crate::types::message::Message::System(
                crate::types::message::SystemMessage::MicrocompactBoundary {
                    microcompact_metadata: Some(metadata),
                    ..
                },
            )) = event
            {
                saw_boundary = true;
                assert_eq!(metadata.tokens_saved, 32);
                assert_eq!(
                    metadata.compacted_tool_ids,
                    vec!["toolu_cached_old".to_string()]
                );
            }
        }
        assert!(saw_boundary);
    }

    #[test]
    fn query_actor_threads_autocompact_tracking_across_tool_continuation() {
        #[derive(Clone, Debug)]
        struct TrackingDeps {
            seen_tracking: std::sync::Arc<
                std::sync::Mutex<
                    Vec<Option<crate::services::compact::auto_compact::AutoCompactTrackingState>>,
                >,
            >,
            call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl crate::query::deps::QueryDeps for TrackingDeps {
            fn autocompact(
                &self,
                messages_for_query: Vec<crate::types::message::Message>,
                _tool_use_context: &crate::tool::ToolUseContext,
                _cache_safe_params: crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
                _query_source: &QuerySource,
                tracking: Option<crate::services::compact::auto_compact::AutoCompactTrackingState>,
                _snip_tokens_freed: i64,
            ) -> futures::future::BoxFuture<'static, crate::query::deps::AutocompactResult>
            {
                let call_index = {
                    let mut seen = self.seen_tracking.lock().unwrap();
                    let index = seen.len();
                    seen.push(tracking);
                    index
                };
                Box::pin(async move {
                    crate::query::deps::AutocompactResult {
                        messages: messages_for_query,
                        compacted: call_index == 0,
                        consecutive_failures: (call_index == 0).then_some(0),
                        rebuilt_read_file_state: None,
                    }
                })
            }

            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let attempt = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    let content = if attempt == 0 {
                        vec![crate::types::message::AssistantContent::ToolUse(
                            crate::types::message::ToolUseBlock {
                                id: crate::types::ids::ToolUseId("toolu_tracking_todo".to_string()),
                                name: "TodoWrite".to_string(),
                                input: serde_json::json!({
                                    "todos": [
                                        {
                                            "content": "Check autocompact tracking",
                                            "status": "completed",
                                            "activeForm": "Checking autocompact tracking"
                                        }
                                    ]
                                }),
                            },
                        )]
                    } else {
                        vec![crate::types::message::AssistantContent::Text(
                            "done after tracking".to_string(),
                        )]
                    };
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content,
                                model: None,
                                stop_reason: Some(if attempt == 0 {
                                    crate::types::message::StopReason::ToolUse
                                } else {
                                    crate::types::message::StopReason::EndTurn
                                }),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let seen_tracking = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let params = QueryParams {
            turn_id: "turn-autocompact-tracking".to_string(),
            input: "track autocompact".to_string(),
            messages: vec![RenderableMessage::user("u1", "track autocompact")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                TrackingDeps {
                    seen_tracking: seen_tracking.clone(),
                    call_count,
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen_tracking.lock().unwrap();
        assert!(
            seen.len() >= 2,
            "expected tracking on at least two loop iterations"
        );
        assert!(seen[0].is_none());
        let second = seen[1]
            .as_ref()
            .expect("second autocompact call should receive tracking");
        assert!(second.compacted);
        assert_eq!(second.turn_counter, 1);
        assert!(!second.turn_id.is_empty());
        assert_eq!(second.consecutive_failures, Some(0));
    }

    #[test]
    fn query_actor_stops_before_tool_result_continuation_when_max_turns_reached() {
        #[derive(Clone, Debug)]
        struct MaxTurnsDeps {
            call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl crate::query::deps::QueryDeps for MaxTurnsDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::ToolUse(
                                    crate::types::message::ToolUseBlock {
                                        id: crate::types::ids::ToolUseId(
                                            "toolu_max_turns_todo".to_string(),
                                        ),
                                        name: "TodoWrite".to_string(),
                                        input: serde_json::json!({
                                            "todos": [{
                                                "content": "Stop at max turns",
                                                "status": "completed",
                                                "activeForm": "Stopping at max turns"
                                            }]
                                        }),
                                    },
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let params = QueryParams {
            turn_id: "turn-max-turns".to_string(),
            input: "stop after one turn".to_string(),
            messages: vec![RenderableMessage::user("u1", "stop after one turn")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: Some(1),
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                MaxTurnsDeps {
                    call_count: call_count.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "max_turns");
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Batch D3: one whole `Message` (CC query.ts:1706-1710) carries both
        // halves; the old Row+ModelMessage pair is gone.
        let mut saw_max_turns_message = false;
        while let Ok(event) = event_rx.try_recv() {
            if let QueryEvent::Message(Message::Attachment(attachment)) = event {
                saw_max_turns_message = matches!(
                    attachment.attachment,
                    Attachment::MaxTurnsReached {
                        max_turns: 1,
                        turn_count: 2,
                    }
                ) && attachment.attachment_type() == "max_turns_reached";
            }
        }
        assert!(saw_max_turns_message);
    }

    #[test]
    fn query_actor_materializes_streaming_text_once_after_delta_preview() {
        #[derive(Clone, Debug)]
        struct StreamingTextDeps;

        impl crate::query::deps::QueryDeps for StreamingTextDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                        crate::services::api::claude::ClaudeStreamItem::Text("hel".to_string()),
                    ))
                    .await
                    .ok();
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                        crate::services::api::claude::ClaudeStreamItem::Text("lo".to_string()),
                    ))
                    .await
                    .ok();
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::CompletedContent(
                            crate::services::api::claude::ClaudeStreamItem::Text(
                                "hello".to_string(),
                            ),
                        ),
                    )
                    .await
                    .ok();
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "hello".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }

            fn uuid(&self) -> String {
                "stream-text-message".to_string()
            }
        }

        let params = QueryParams {
            turn_id: "turn-streaming-update".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                StreamingTextDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut text_messages = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let QueryEvent::Message(message) = event {
                if let Some(text) = model_assistant_text(&message) {
                    if !model_is_api_error(&message) {
                        text_messages.push((message.uuid().to_string(), text.to_string()));
                    }
                }
            }
        }
        // Maps to CC `handleMessageFromStream`: text_delta updates the
        // transient streaming preview, and only the completed assistant
        // message enters the formal transcript — as ONE whole `Message`
        // (claude.ts:2192-2210); the later envelope with the same block stays
        // actor-internal.
        assert_eq!(text_messages.len(), 1);
        assert_eq!(
            text_messages[0],
            ("stream-text-message".to_string(), "hello".to_string())
        );
    }

    #[test]
    fn query_actor_resumes_streaming_permission_before_stream_drain() {
        #[derive(Clone, Debug)]
        struct StreamingPermissionDeps {
            finish_first_stream: std::sync::Arc<tokio::sync::Notify>,
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for StreamingPermissionDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_messages.lock().unwrap().push(request.messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let finish_first_stream = self.finish_first_stream.clone();
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(16);
                    if call == 0 {
                        let tool_use = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_stream_perm".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "printf streamed-permission" }),
                        };
                        tokio::spawn(async move {
                            tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                                crate::services::api::claude::ClaudeStreamItem::ToolUse {
                                    id: tool_use.id.0.clone(),
                                    name: tool_use.name.clone(),
                                    input: tool_use.input.clone(),
                                    is_server: false,
                                },
                            ))
                            .await
                            .ok();
                            finish_first_stream.notified().await;
                            tx.send(
                                crate::services::api::claude::QueryModelStreamItem::Assistant(
                                    crate::types::message::AssistantMessage {
                                        uuid: uuid::Uuid::new_v4().to_string(),
                                        timestamp: chrono::Utc::now(),
                                        content: vec![
                                            crate::types::message::AssistantContent::ToolUse(
                                                tool_use,
                                            ),
                                        ],
                                        model: None,
                                        stop_reason: Some(
                                            crate::types::message::StopReason::ToolUse,
                                        ),
                                        usage: None,
                                    },
                                ),
                            )
                            .await
                            .ok();
                        });
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done after streamed permission".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let _streaming_gate = crate::query::config::set_streaming_tool_execution_for_test(true);
        let finish_first_stream = std::sync::Arc::new(tokio::sync::Notify::new());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let params = QueryParams {
            turn_id: "turn-streaming-permission-resume".to_string(),
            input: "read missing file".to_string(),
            messages: vec![RenderableMessage::user("u1", "read missing file")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let actor = tokio::spawn(run_query_actor(
                    params,
                    StreamingPermissionDeps {
                        finish_first_stream: finish_first_stream.clone(),
                        calls: calls.clone(),
                        seen_messages: seen_messages.clone(),
                    },
                    event_tx,
                    command_rx,
                ));

                let mut saw_permission = false;
                let mut saw_tool_result_before_final_assistant = false;
                while !saw_tool_result_before_final_assistant {
                    let event =
                        tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
                            .await
                            .expect("timed out waiting for streaming permission event")
                            .expect("event channel closed before streaming permission completed");

                    match event {
                        QueryEvent::PermissionRequest(request) => {
                            assert_eq!(request.tool_use_id, "toolu_stream_perm");
                            saw_permission = true;
                            command_tx
                                .send(QueryCommand::PermissionDecision {
                                    tool_use_id: request.tool_use_id,
                                    choice: PermissionPromptChoice::AllowOnce,
                                })
                                .await
                                .unwrap();
                        }
                        QueryEvent::Row(message)
                            if saw_permission
                                && row_user_tool_result(&message).is_some_and(|result| {
                                    result.tool_use_id.0 == "toolu_stream_perm"
                                }) =>
                        {
                            saw_tool_result_before_final_assistant = true;
                        }
                        QueryEvent::ModelMessage(Message::Assistant(_)) => {
                            panic!(
                                "assistant model message arrived before first stream was released"
                            )
                        }
                        _ => {}
                    }
                }

                finish_first_stream.notify_one();
                let terminal = tokio::time::timeout(std::time::Duration::from_secs(2), actor)
                    .await
                    .expect("timed out waiting for query actor")
                    .expect("query actor join failed");
                assert_eq!(terminal.reason, "completed");

                assert!(
                    calls.load(std::sync::atomic::Ordering::SeqCst) >= 2,
                    "tool_result continuation should call the model again"
                );
                let seen = seen_messages.lock().unwrap();
                assert!(seen.len() >= 2);
                assert!(seen[1].iter().any(|message| match message {
                    Message::User(user) => user.content.iter().any(|content| match content {
                        crate::types::message::UserContent::ToolResult(result) => {
                            result.tool_use_id.0 == "toolu_stream_perm"
                        }
                        _ => false,
                    }),
                    _ => false,
                }));
            });
    }

    #[test]
    fn query_actor_token_budget_continue_adds_nudge_to_next_request() {
        #[derive(Clone, Debug)]
        struct BudgetDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
            seen_tracking: std::sync::Arc<
                std::sync::Mutex<Vec<Option<crate::services::api::claude::QueryChainTracking>>>,
            >,
        }

        impl crate::query::deps::QueryDeps for BudgetDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                self.seen_tracking
                    .lock()
                    .unwrap()
                    .push(request.options.query_tracking.clone());
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    let output_tokens = if call == 0 { 500 } else { 600 };
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    format!("answer {call}"),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: Some(crate::types::message::TokenUsage {
                                    input_tokens: 0,
                                    output_tokens,
                                    cache_creation_input_tokens: 0,
                                    cache_read_input_tokens: 0,
                                    ..Default::default()
                                }),
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_tracking = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let params = QueryParams {
            turn_id: "turn-token-budget".to_string(),
            input: "+1k keep working".to_string(),
            messages: vec![RenderableMessage::user("u1", "+1k keep working")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: Some(1_000),
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                BudgetDeps {
                    calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    seen_messages: seen_messages.clone(),
                    seen_tracking: seen_tracking.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let seen = seen_messages.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::UserContent::MetaText(text)
                        if text.contains("Stopped at 50% of token target")
                ))
        )));
        // CC `query.ts:1309-1340` never yields the nudge — it only lands in
        // the next `State.messages`. Emitting it would put it in REPL history
        // and, through `useLogMessages`, in the transcript CC never writes.
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            !events.iter().any(|event| matches!(
                event,
                QueryEvent::Message(crate::types::message::Message::User(user))
                    | QueryEvent::ModelMessage(crate::types::message::Message::User(user))
                    if user.content.iter().any(|content| matches!(
                        content,
                        crate::types::message::UserContent::MetaText(text)
                            if text.contains("Stopped at 50% of token target")
                    ))
            )),
            "the token-budget nudge must not cross the actor boundary"
        );
        let tracking = seen_tracking.lock().unwrap();
        assert_eq!(tracking.len(), 2);
        let first = tracking[0]
            .as_ref()
            .expect("first request should carry query tracking");
        let second = tracking[1]
            .as_ref()
            .expect("continued request should carry query tracking");
        assert_eq!(first.depth, 0);
        assert_eq!(second.depth, 1);
        assert_eq!(first.chain_id, second.chain_id);
    }

    #[test]
    fn query_actor_stop_hook_blocking_error_continues_next_request() {
        #[derive(Clone, Debug)]
        struct StopHookDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for StopHookDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_messages.lock().unwrap().push(request.messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(2);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    format!("assistant {call}"),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let test_id = format!("stop-hook-{}", uuid::Uuid::new_v4());
        crate::query::stop_hooks::push_test_stop_hook_results(
            test_id.clone(),
            vec![crate::services::hooks::HookResult {
                blocking_error: Some(crate::services::hooks::HookBlockingError {
                    blocking_error: "Stop hook requires one more answer".to_string(),
                    command: "echo block".to_string(),
                }),
                ..Default::default()
            }],
        );

        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut system_context = std::collections::BTreeMap::new();
        system_context.insert("__test_stop_hook_id".to_string(), test_id.clone());
        let params = QueryParams {
            turn_id: "turn-stop-hook-continue".to_string(),
            input: "finish".to_string(),
            messages: vec![RenderableMessage::user("u1", "finish")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context,
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                StopHookDeps {
                    calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));
        crate::query::stop_hooks::clear_test_stop_hook_results(&test_id);

        assert_eq!(terminal.reason, "completed");
        let seen = seen_messages.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::Assistant(assistant)
                if assistant.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::AssistantContent::Text(text)
                        if text == "assistant 0"
                ))
        )));
        assert!(seen[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    // CC `createUserMessage({ …, isMeta: true })` — the model
                    // reads the feedback, the render list hides it.
                    crate::types::message::UserContent::MetaText(text)
                        if text == "Stop hook feedback:\nStop hook requires one more answer"
                ))
        )));
    }

    #[test]
    fn query_model_stream_items_map_realistic_message_types_to_transcript_events() {
        let pending = query_model_stream_items_to_pending_events(
            "turn-sdk-items",
            vec![
                crate::services::api::claude::QueryModelStreamItem::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![
                            crate::types::message::AssistantContent::Thinking {
                                text: "plan first".to_string(),
                                signature: "sig".to_string(),
                            },
                            crate::types::message::AssistantContent::Text(
                                "I will read a file.".to_string(),
                            ),
                            crate::types::message::AssistantContent::ToolUse(
                                crate::types::message::ToolUseBlock {
                                    id: crate::types::ids::ToolUseId(
                                        "toolu_mock_read_001".to_string(),
                                    ),
                                    name: "Read".to_string(),
                                    input: serde_json::json!({ "file_path": "README.md" }),
                                },
                            ),
                            crate::types::message::AssistantContent::RedactedThinking {
                                data: "redacted".to_string(),
                            },
                        ],
                        model: Some("claude-sonnet-4-20250514".to_string()),
                        stop_reason: Some(crate::types::message::StopReason::ToolUse),
                        usage: None,
                    },
                ),
            ],
        );

        assert_eq!(pending.len(), 4);
        assert!(matches!(
            &pending[0].event.kind,
            QuerySourceEventKind::AssistantThinking { text, expanded: false }
                if text == "plan first"
        ));
        assert!(matches!(
            &pending[1].event.kind,
            QuerySourceEventKind::AssistantText { text, .. } if text == "I will read a file."
        ));
        assert!(matches!(
            &pending[2].event.kind,
            QuerySourceEventKind::AssistantToolUse {
                tool_use_id: Some(tool_use_id),
                tool_name,
                description,
                status: ToolUseStatus::Queued,
                ..
            } if tool_use_id == "toolu_mock_read_001"
                && tool_name == "Read"
                && description == "README.md"
        ));
        assert!(matches!(
            &pending[3].event.kind,
            QuerySourceEventKind::AssistantRedactedThinking
        ));
    }

    #[test]
    fn query_model_stream_items_dedup_completed_text_from_final_assistant() {
        let pending = query_model_stream_items_to_pending_events(
            "turn-sdk-completed",
            vec![
                crate::services::api::claude::QueryModelStreamItem::CompletedContent(
                    crate::services::api::claude::ClaudeStreamItem::Text("hello".to_string()),
                ),
                crate::services::api::claude::QueryModelStreamItem::Assistant(
                    crate::types::message::AssistantMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![
                            crate::types::message::AssistantContent::Text("hello".to_string()),
                            crate::types::message::AssistantContent::ToolUse(
                                crate::types::message::ToolUseBlock {
                                    id: crate::types::ids::ToolUseId(
                                        "toolu_after_text".to_string(),
                                    ),
                                    name: "Read".to_string(),
                                    input: serde_json::json!({ "file_path": "README.md" }),
                                },
                            ),
                        ],
                        model: None,
                        stop_reason: Some(crate::types::message::StopReason::ToolUse),
                        usage: None,
                    },
                ),
            ],
        );

        assert_eq!(pending.len(), 2);
        assert!(matches!(
            &pending[0].event.kind,
            QuerySourceEventKind::AssistantText { text, .. } if text == "hello"
        ));
        assert!(matches!(
            &pending[1].event.kind,
            QuerySourceEventKind::AssistantToolUse {
                tool_use_id: Some(tool_use_id),
                ..
            } if tool_use_id == "toolu_after_text"
        ));
    }

    #[test]
    fn query_actor_preserves_server_tool_use_without_running_local_tools() {
        #[derive(Clone, Debug, Default)]
        struct ServerToolUseDeps;

        impl crate::query::deps::QueryDeps for ServerToolUseDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                        crate::services::api::claude::ClaudeStreamItem::ToolUse {
                            id: "srvu_search".to_string(),
                            name: "web_search".to_string(),
                            input: serde_json::json!({"query": "cometix"}),
                            is_server: true,
                        },
                    ))
                    .await
                    .ok();
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                        crate::services::api::claude::ClaudeStreamItem::WebSearchToolResult {
                            tool_use_id: "srvu_search".to_string(),
                            content: serde_json::json!([
                                {
                                    "type": "web_search_result",
                                    "encrypted_content": "encrypted",
                                    "title": "Cometix",
                                    "url": "https://example.com"
                                }
                            ]),
                        },
                    ))
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-server-tool-use".to_string(),
            input: "search".to_string(),
            messages: vec![RenderableMessage::user("u1", "search")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                ServerToolUseDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::PermissionRequest(_)))
        );
        assert!(events.iter().all(|event| match event {
            QueryEvent::Row(message) => row_user_tool_result(message).is_none(),
            _ => true,
        }));
        // C3c/#6: streamed assistant messages ride QueryEvent::Message (the
        // single carrier); ModelMessage remains only for non-streamed halves.
        let assistant = events.iter().find_map(|event| match event {
            QueryEvent::Message(crate::types::message::Message::Assistant(assistant))
            | QueryEvent::ModelMessage(crate::types::message::Message::Assistant(assistant)) => {
                Some(assistant)
            }
            _ => None,
        });
        let assistant = assistant.expect("server tool use assistant message should be preserved");
        assert!(assistant.content.iter().any(|content| matches!(
            content,
            crate::types::message::AssistantContent::ServerToolUse(block)
                if block.id.0 == "srvu_search"
                    && block.name == "web_search"
                    && block.input.get("query").and_then(|value| value.as_str())
                        == Some("cometix")
        )));
        assert!(assistant.content.iter().any(|content| matches!(
            content,
            crate::types::message::AssistantContent::WebSearchToolResult {
                tool_use_id,
                content,
            } if tool_use_id.0 == "srvu_search"
                && content.as_array().is_some_and(|items| items.len() == 1)
        )));
    }

    #[test]
    fn query_actor_recovers_from_max_output_tokens_before_stop_hooks() {
        #[derive(Clone, Debug)]
        struct MaxTokensDeps {
            attempts: std::sync::Arc<std::sync::Mutex<u32>>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for MaxTokensDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                let attempts = self.attempts.clone();
                Box::pin(async move {
                    let attempt = {
                        let mut attempts = attempts.lock().unwrap();
                        let current = *attempts;
                        *attempts = attempts.saturating_add(1);
                        current
                    };
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    let (text, stop_reason) = if attempt < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT {
                        (
                            format!("partial-{attempt}"),
                            crate::types::message::StopReason::MaxTokens,
                        )
                    } else {
                        (
                            "done".to_string(),
                            crate::types::message::StopReason::EndTurn,
                        )
                    };
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(text)],
                                model: None,
                                stop_reason: Some(stop_reason),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-max-tokens".to_string(),
            input: "continue long answer".to_string(),
            messages: vec![RenderableMessage::user("u1", "continue long answer")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(0));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                MaxTokensDeps {
                    attempts: attempts.clone(),
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        assert_eq!(
            *attempts.lock().unwrap(),
            MAX_OUTPUT_TOKENS_RECOVERY_LIMIT + 1
        );
        let seen = seen_messages.lock().unwrap();
        assert_eq!(seen.len() as u32, MAX_OUTPUT_TOKENS_RECOVERY_LIMIT + 1);
        assert!(seen[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::UserContent::MetaText(text)
                        if text == MAX_OUTPUT_TOKENS_RECOVERY_MESSAGE
                ))
        )));

        // CC `query.ts:1223-1250` builds the recovery message straight into
        // the next `State.messages` and never yields it: it is invisible to
        // the REPL array, to the render list, and to the transcript, and it
        // dies with this one query call. Emitting an event would leak it into
        // REPL history and — via `useLogMessages` — into the JSONL.
        let mut recovery_events = 0usize;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(
                event,
                QueryEvent::Message(crate::types::message::Message::User(user))
                    | QueryEvent::ModelMessage(crate::types::message::Message::User(user))
                    if user.content.iter().any(|content| matches!(
                        content,
                        crate::types::message::UserContent::MetaText(text)
                            if text == MAX_OUTPUT_TOKENS_RECOVERY_MESSAGE
                    ))
            ) {
                recovery_events += 1;
            }
        }
        assert_eq!(
            recovery_events, 0,
            "the max_output_tokens recovery message must not cross the actor boundary"
        );
    }

    #[test]
    fn query_actor_recovers_from_withheld_max_output_error_without_final_max_tokens() {
        #[derive(Clone, Debug)]
        struct WithheldMaxOutputDeps {
            attempts: std::sync::Arc<std::sync::Mutex<u32>>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for WithheldMaxOutputDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_messages.lock().unwrap().push(request.messages);
                let attempts = self.attempts.clone();
                Box::pin(async move {
                    let attempt = {
                        let mut attempts = attempts.lock().unwrap();
                        let current = *attempts;
                        *attempts = attempts.saturating_add(1);
                        current
                    };
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    if attempt == 0 {
                        tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                            crate::services::api::claude::ClaudeStreamItem::Text(
                                "partial answer".to_string(),
                            ),
                        ))
                        .await
                        .ok();
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::SystemError(
                                crate::types::message::SystemApiErrorMessage {
                                    content: "API Error: max output".to_string(),
                                    api_error: "max_output_tokens".to_string(),
                                    error: "invalid_request".to_string(),
                                    error_details: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-withheld-max-output".to_string(),
            input: "continue long answer".to_string(),
            messages: vec![RenderableMessage::user("u1", "continue long answer")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(0));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                WithheldMaxOutputDeps {
                    attempts: attempts.clone(),
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        assert_eq!(*attempts.lock().unwrap(), 2);
        let seen = seen_messages.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::UserContent::MetaText(text)
                        if text == MAX_OUTPUT_TOKENS_RECOVERY_MESSAGE
                ))
        )));
    }

    #[test]
    fn query_actor_surfaces_max_output_error_after_recovery_exhaustion() {
        #[derive(Clone, Debug)]
        struct ExhaustedMaxTokensDeps {
            attempts: std::sync::Arc<std::sync::Mutex<u32>>,
        }

        impl crate::query::deps::QueryDeps for ExhaustedMaxTokensDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let attempts = self.attempts.clone();
                Box::pin(async move {
                    {
                        let mut attempts = attempts.lock().unwrap();
                        *attempts = attempts.saturating_add(1);
                    }
                    let (tx, rx) = tokio::sync::mpsc::channel(4);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "partial answer".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::MaxTokens),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    tx.send(crate::services::api::claude::QueryModelStreamItem::SystemError(
                        crate::types::message::SystemApiErrorMessage {
                            content: "API Error: Claude's response exceeded the 4096 output token maximum. To configure this behavior, set the CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable.".to_string(),
                            api_error: "max_output_tokens".to_string(),
                            error: "max_output_tokens".to_string(),
                            error_details: None,
                        },
                    ))
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-max-tokens-exhausted".to_string(),
            input: "continue forever".to_string(),
            messages: vec![RenderableMessage::user("u1", "continue forever")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(0));
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                ExhaustedMaxTokensDeps {
                    attempts: attempts.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        assert_eq!(
            *attempts.lock().unwrap(),
            MAX_OUTPUT_TOKENS_RECOVERY_LIMIT + 1
        );
        let mut recovery_events = 0usize;
        let mut surfaced_error = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                QueryEvent::Message(message) if model_is_api_error(&message) => {
                    surfaced_error |= model_assistant_text(&message).is_some_and(|error| {
                        error.contains("Claude's response exceeded the 4096 output token maximum")
                    });
                }
                QueryEvent::Message(crate::types::message::Message::User(user))
                | QueryEvent::ModelMessage(crate::types::message::Message::User(user)) => {
                    if user.content.iter().any(|content| {
                        matches!(
                            content,
                            crate::types::message::UserContent::MetaText(text)
                                if text == MAX_OUTPUT_TOKENS_RECOVERY_MESSAGE
                        )
                    }) {
                        recovery_events += 1;
                    }
                }
                _ => {}
            }
        }
        // The recovery loop ran `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT` times
        // (asserted via `attempts` above), but CC keeps every one of those
        // recovery messages inside `State.messages` — none is yielded.
        assert_eq!(
            recovery_events, 0,
            "recovery messages must stay inside the query loop"
        );
        assert!(surfaced_error);
    }

    #[test]
    fn query_actor_forwards_api_stream_events_like_official_generator() {
        #[derive(Clone, Debug, Default)]
        struct StreamEventDeps;

        impl crate::query::deps::QueryDeps for StreamEventDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Stream(
                        crate::types::message::StreamEvent::ApiEvent {
                            event: serde_json::json!({ "type": "message_start" }),
                            ttft_ms: Some(42),
                        },
                    ))
                    .await
                    .ok();
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "done".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-stream-event".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                StreamEventDeps,
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(events.iter().any(|event| matches!(
            event,
            QueryEvent::Stream(crate::types::message::StreamEvent::ApiEvent {
                event,
                ttft_ms: Some(42),
            }) if event.get("type").and_then(|value| value.as_str()) == Some("message_start")
        )));
    }

    #[test]
    fn query_actor_emits_final_assistant_message_when_stream_has_no_content_events() {
        #[derive(Clone, Debug, Default)]
        struct FinalOnlyDeps;

        impl crate::query::deps::QueryDeps for FinalOnlyDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "final only answer".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-final-only".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(params, FinalOnlyDeps, event_tx, command_rx));

        assert_eq!(terminal.reason, "completed");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(events.iter().any(|event| matches!(
            event,
            QueryEvent::Message(message)
                if model_assistant_text(message) == Some("final only answer")
        )));
    }

    #[test]
    fn query_actor_preserves_one_typed_assistant_envelope_per_content_block_stop() {
        #[derive(Clone, Debug, Default)]
        struct PerBlockDeps;

        impl crate::query::deps::QueryDeps for PerBlockDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    for (uuid, text) in [
                        ("assistant-block-1", "first"),
                        ("assistant-block-2", "second"),
                    ] {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    // The ENVELOPE uuid. The named value used to
                                    // sit on the identity block with a random
                                    // envelope here, so the assertions below
                                    // pinned the identity uuid.
                                    uuid: uuid.to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::Text(
                                            text.to_string(),
                                        ),
                                        crate::types::message::AssistantContent::MessageIdentity(
                                            crate::types::message::AssistantMessageIdentity {
                                                request_id: Some("req-shared".to_string()),
                                                api_message_id: Some("msg-shared".to_string()),
                                                ..Default::default()
                                            },
                                        ),
                                    ],
                                    model: Some("claude-test".to_string()),
                                    stop_reason: None,
                                    usage: Some(crate::types::message::TokenUsage {
                                        input_tokens: 10,
                                        ..Default::default()
                                    }),
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::AssistantDelta {
                            stop_reason: Some(crate::types::message::StopReason::EndTurn),
                            usage: Some(crate::types::message::TokenUsage {
                                input_tokens: 10,
                                output_tokens: 7,
                                ..Default::default()
                            }),
                        },
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-per-block".to_string(),
            input: "hello".to_string(),
            messages: vec![RenderableMessage::user("u1", "hello")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();
        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(params, PerBlockDeps, event_tx, command_rx));
        assert_eq!(terminal.reason, "completed");

        // Streamed-assistant convergence: each content_block_stop yields ONE
        // whole `QueryEvent::Message` (CC claude.ts:2192-2210); the final
        // usage/stop_reason travel as `QueryEvent::AssistantDelta` keyed by
        // the last envelope's uuid (claude.ts:2229-2248), which every
        // consumer applies in place — replayed here before the tail asserts.
        let mut assistants = Vec::new();
        let mut delta = None;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                QueryEvent::Message(Message::Assistant(assistant)) => assistants.push(assistant),
                QueryEvent::AssistantDelta {
                    uuid,
                    stop_reason,
                    usage,
                } => delta = Some((uuid, stop_reason, usage)),
                QueryEvent::ModelMessage(Message::Assistant(_)) => {
                    panic!("streamed assistants must not ride ModelMessage anymore")
                }
                _ => {}
            }
        }
        assert_eq!(assistants.len(), 2);
        let (delta_uuid, delta_stop_reason, delta_usage) =
            delta.expect("message_delta write-back event");
        assert_eq!(delta_uuid, assistants[1].uuid);
        if let Some(assistant) = assistants
            .iter_mut()
            .rev()
            .find(|assistant| assistant.uuid == delta_uuid)
        {
            assistant.stop_reason = delta_stop_reason;
            assistant.usage = delta_usage;
        }
        assert_eq!(assistants[0].uuid, "assistant-block-1");
        assert_eq!(assistants[1].uuid, "assistant-block-2");
        assert_eq!(assistants[0].request_id(), Some("req-shared"));
        assert_eq!(assistants[1].api_message_id(), Some("msg-shared"));
        assert_eq!(assistants[0].stop_reason, None);
        assert_eq!(
            assistants[1].stop_reason,
            Some(crate::types::message::StopReason::EndTurn)
        );
        assert_eq!(
            assistants[1]
                .usage
                .as_ref()
                .map(|usage| usage.output_tokens),
            Some(7)
        );
        assert_eq!(assistant_output_tokens(&assistants), 7);
    }

    #[test]
    fn query_actor_uses_injected_tool_permission_context_for_tool_flow() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();

        #[derive(Clone, Debug)]
        struct PreallowedToolDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl crate::query::deps::QueryDeps for PreallowedToolDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_preallowed".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "printf preallowed" }),
                        };
                        tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                            crate::services::api::claude::ClaudeStreamItem::ToolUse {
                                id: block.id.0.clone(),
                                name: block.name.clone(),
                                input: block.input.clone(),
                                is_server: false,
                            },
                        ))
                        .await
                        .ok();
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                            crate::services::api::claude::ClaudeStreamItem::Text(
                                "done".to_string(),
                            ),
                        ))
                        .await
                        .ok();
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let mut permission_context = crate::tool::ToolPermissionContext::default();
        let mut rules = std::collections::HashMap::new();
        rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash",
                Some("printf preallowed".to_string()),
            )],
        );
        permission_context.always_allow_rules = rules;

        let params = QueryParams {
            turn_id: "turn-preallowed".to_string(),
            input: "please run it".to_string(),
            messages: Vec::new(),
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::with_permission_context(
                permission_context,
            ),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let deps = PreallowedToolDeps {
            calls: calls.clone(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(
                    QUERY_ACTOR_TEST_TIMEOUT,
                    run_query_actor(params, deps, event_tx, command_rx),
                )
                .await
                .expect("preallowed query actor should not block on permission")
            });

        assert_eq!(terminal.reason, "completed");
        assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::PermissionRequest(_)))
        );
        assert!(events.iter().any(|event| {
            match event {
                QueryEvent::Row(message) => row_user_tool_result(message)
                    .is_some_and(|result| result.tool_use_id.0 == "toolu_preallowed"),
                _ => false,
            }
        }));
        // The lifecycle is no longer three copies of the row carrying three
        // different statuses. It is the REPL-owned set moving (CC `Tool.ts:227`
        // `setInProgressToolUseIDs`): added when the tool starts, removed when
        // it finishes. `AssistantToolUseMessage.tsx:120-121` derives queued,
        // running and resolved from that membership plus `resolvedToolUseIDs`.
        let in_progress_deltas = events
            .iter()
            .filter_map(|event| match event {
                QueryEvent::SetInProgressToolUse {
                    tool_use_id,
                    in_progress,
                } if tool_use_id == "toolu_preallowed" => Some(*in_progress),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            in_progress_deltas,
            vec![true, false],
            "a preallowed tool should enter the live set once and leave it once"
        );
    }

    #[test]
    fn query_actor_pauses_for_permission_and_resumes_with_decision() {
        #[derive(Clone, Debug)]
        struct PermissionDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for PermissionDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_permission".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "printf allowed" }),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-permission".to_string(),
            input: "please run it".to_string(),
            messages: vec![RenderableMessage::user("u1", "please run it")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let deps = PermissionDeps {
            calls,
            seen_messages: seen_messages.clone(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();

        let (terminal, saw_permission) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let actor = run_query_actor(params, deps, event_tx, command_rx);
                tokio::pin!(actor);
                let mut saw_permission = false;
                let terminal = loop {
                    tokio::select! {
                        terminal = &mut actor => break terminal,
                        event = event_rx.recv() => match event {
                            Ok(QueryEvent::PermissionRequest(request)) => {
                                saw_permission = true;
                                assert_eq!(request.tool_use_id, "toolu_permission");
                                command_tx.send(QueryCommand::PermissionDecision {
                                    tool_use_id: request.tool_use_id,
                                    choice: PermissionPromptChoice::AllowOnce,
                                }).await.unwrap();
                            }
                            Ok(_) => {}
                            Err(_) => panic!("query event channel closed before terminal"),
                        }
                    }
                };
                (terminal, saw_permission)
            });

        assert_eq!(terminal.reason, "completed");
        assert!(saw_permission);
        let seen = seen_messages.lock().unwrap();
        assert!(
            seen.len() >= 2,
            "expected model continuation after permission"
        );
        assert!(seen[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_permission"
                            && result.content == "allowed"
                            && !result.is_error
                ))
        )));
    }

    #[test]
    fn query_actor_preserves_original_edit_request_for_user_modified_response() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-query-edit-user-modified-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let _config_dir =
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", config_dir.to_string_lossy().as_ref());
        let file = root.join("target.txt");
        std::fs::write(&file, "old").unwrap();

        #[derive(Clone, Debug)]
        struct EditPermissionDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            path: String,
        }

        impl crate::query::deps::QueryDeps for EditPermissionDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let path = self.path.clone();
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    let message = if call == 0 {
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::ToolUse(
                                crate::types::message::ToolUseBlock {
                                    id: crate::types::ids::ToolUseId(
                                        "toolu_edit_user_modified_query".to_string(),
                                    ),
                                    name: "Edit".to_string(),
                                    input: serde_json::json!({
                                        "file_path": path,
                                        "old_string": "old",
                                        "new_string": "model-new"
                                    }),
                                },
                            )],
                            model: None,
                            stop_reason: Some(crate::types::message::StopReason::ToolUse),
                            usage: None,
                        }
                    } else {
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::Text(
                                "done".to_string(),
                            )],
                            model: None,
                            stop_reason: Some(crate::types::message::StopReason::EndTurn),
                            usage: None,
                        }
                    };
                    tx.send(crate::services::api::claude::QueryModelStreamItem::Assistant(message))
                        .await
                        .ok();
                    Ok(rx)
                })
            }
        }

        let _streaming_gate = crate::query::config::set_streaming_tool_execution_for_test(true);
        let mut tool_context = crate::tool::ToolUseContext::default();
        tool_context.cwd_override = Some(root.clone());
        tool_context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: file.display().to_string(),
                content: Some("old".to_string()),
                timestamp_ms: crate::utils::file::get_file_modification_time(&file),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });
        let params = QueryParams {
            turn_id: "turn-edit-user-modified".to_string(),
            input: "edit it".to_string(),
            messages: vec![RenderableMessage::user("u1", "edit it")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: tool_context,
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();
        let path = file.display().to_string();
        let deps = EditPermissionDeps {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            path: path.clone(),
        };

        let (terminal, saw_user_modified, observed) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let actor = run_query_actor(params, deps, event_tx, command_rx);
                tokio::pin!(actor);
                let mut saw_user_modified = false;
                let mut observed = Vec::new();
                let terminal = loop {
                    tokio::select! {
                        terminal = &mut actor => break terminal,
                        event = event_rx.recv() => match event {
                            Ok(QueryEvent::PermissionRequest(request)) => {
                                assert_eq!(request.tool_use_id, "toolu_edit_user_modified_query");
                                command_tx.send(QueryCommand::PermissionResponse {
                                    tool_use_id: request.tool_use_id,
                                    response: PermissionPromptResponse::allow_once_with_input(
                                        serde_json::json!({
                                            "file_path": path,
                                            "old_string": "old",
                                            "new_string": "user-new",
                                            "replace_all": false
                                        }),
                                    ),
                                }).await.unwrap();
                            }
                            Ok(QueryEvent::Row(message)) => {
                                // The row carries the raw `toolUseResult`;
                                // user_modified/newString ride the wire shape.
                                if let Some(output) = row_user_tool_result(&message)
                                    .and_then(|result| result.tool_use_result.as_ref())
                                    .and_then(crate::tools::file_edit_tool::ui::parse_output)
                                {
                                    observed.push(format!(
                                        "raw user_modified={:?} new_string={:?}",
                                        output.user_modified, output.new_string
                                    ));
                                    if output.user_modified && output.new_string == "user-new" {
                                        saw_user_modified = true;
                                    }
                                }
                            }
                            Ok(QueryEvent::ToolContextUpdate(context)) => {
                                observed.push(format!(
                                    "context user_modified={:?}",
                                    context.user_modified
                                ));
                            }
                            Ok(_) => {}
                            Err(_) => panic!("query event channel closed before terminal"),
                        }
                    }
                };
                while let Ok(event) = event_rx.try_recv() {
                    match event {
                        QueryEvent::Row(message) => {
                            if let Some(output) = row_user_tool_result(&message)
                                .and_then(|result| result.tool_use_result.as_ref())
                                .and_then(crate::tools::file_edit_tool::ui::parse_output)
                            {
                                observed.push(format!(
                                    "raw user_modified={:?} new_string={:?}",
                                    output.user_modified, output.new_string
                                ));
                                if output.user_modified && output.new_string == "user-new" {
                                    saw_user_modified = true;
                                }
                            }
                        }
                        QueryEvent::ToolContextUpdate(context) => observed
                            .push(format!("context user_modified={:?}", context.user_modified)),
                        _ => {}
                    }
                }
                (terminal, saw_user_modified, observed)
            });

        assert_eq!(terminal.reason, "completed");
        assert!(saw_user_modified, "observed={observed:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "user-new");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn query_actor_abort_interrupts_streaming_wait() {
        #[derive(Clone, Debug, Default)]
        struct BlockingStreamDeps {
            hold_sender: std::sync::Arc<
                std::sync::Mutex<
                    Option<
                        tokio::sync::mpsc::Sender<
                            crate::services::api::claude::QueryModelStreamItem,
                        >,
                    >,
                >,
            >,
        }

        impl crate::query::deps::QueryDeps for BlockingStreamDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let hold_sender = self.hold_sender.clone();
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    *hold_sender.lock().unwrap() = Some(tx);
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-abort-streaming".to_string(),
            input: "wait".to_string(),
            messages: vec![RenderableMessage::user("u1", "wait")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let actor =
                    run_query_actor(params, BlockingStreamDeps::default(), event_tx, command_rx);
                let driver = async {
                    // Wait for the condition, do not assert on POSITION. The
                    // actor legitimately emits attachment rows first — a
                    // `SkillListing` for whatever skills the cwd has — so
                    // "first event is StreamRequestStart" was an assertion
                    // about the developer's `.claude/skills/` directory, and it
                    // started failing when that directory grew.
                    let mut saw_stream_start = false;
                    for _ in 0..32 {
                        match event_rx.recv().await.unwrap() {
                            QueryEvent::StreamRequestStart => {
                                saw_stream_start = true;
                                break;
                            }
                            _ => continue,
                        }
                    }
                    assert!(
                        saw_stream_start,
                        "actor should reach StreamRequestStart before the abort"
                    );
                    command_tx.send(QueryCommand::Abort).await.unwrap();
                };
                let (terminal, _) = tokio::join!(actor, driver);
                terminal
            });

        assert_eq!(terminal.reason, "aborted_streaming");

        // CC `query.ts:1044-1050` yields the marker on abort. It is a whole
        // `QueryEvent::Message`, since CC yields it into the messages array.
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events.iter().any(|event| matches!(
                event,
                QueryEvent::Message(crate::types::message::Message::User(user))
                    if user.content.iter().any(|content| matches!(
                        content,
                        crate::types::message::UserContent::Text(text)
                            if text == crate::utils::messages::INTERRUPT_MESSAGE
                    ))
            )),
            "a plain Esc must produce the interrupt marker"
        );
    }

    /// Maps to: CC `query.ts:1045-1046` — `if (signal.reason !== 'interrupt')`.
    /// A submit-interrupt is followed immediately by the queued user message
    /// that caused it, so CC skips the marker; only the REPL's submit paths
    /// (`repl.rs:3816,4304`) use that reason.
    #[test]
    fn query_actor_submit_interrupt_skips_the_interruption_marker() {
        #[derive(Clone, Debug, Default)]
        struct BlockingStreamDeps {
            hold_sender: std::sync::Arc<
                std::sync::Mutex<
                    Option<
                        tokio::sync::mpsc::Sender<
                            crate::services::api::claude::QueryModelStreamItem,
                        >,
                    >,
                >,
            >,
        }

        impl crate::query::deps::QueryDeps for BlockingStreamDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let hold_sender = self.hold_sender.clone();
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    *hold_sender.lock().unwrap() = Some(tx);
                    Ok(rx)
                })
            }
        }

        // The actor's own controller, i.e. what `spawn_query` hands the REPL as
        // `QueryHandle::abort_controller` — `run_query_actor` would otherwise
        // build a private default that no test could reach.
        let abort_controller = crate::tool::AbortController::default();
        let tool_use_context = crate::tool::ToolUseContext::default();
        let params = QueryParams {
            turn_id: "turn-submit-interrupt".to_string(),
            input: "wait".to_string(),
            messages: vec![RenderableMessage::user("u1", "wait")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context,
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let actor = run_query_actor_with_abort(
                    params,
                    BlockingStreamDeps::default(),
                    event_tx,
                    command_rx,
                    abort_controller.clone(),
                );
                let driver = async {
                    let mut saw_stream_start = false;
                    for _ in 0..32 {
                        match event_rx.recv().await.unwrap() {
                            QueryEvent::StreamRequestStart => {
                                saw_stream_start = true;
                                break;
                            }
                            _ => continue,
                        }
                    }
                    assert!(saw_stream_start, "actor should reach StreamRequestStart");
                    // The REPL submit path publishes the reason BEFORE waking
                    // the actor; first abort wins, so the actor's own
                    // `abort()` cannot overwrite it.
                    abort_controller.abort_with_reason("interrupt");
                    command_tx.send(QueryCommand::Abort).await.unwrap();
                };
                let (terminal, _) = tokio::join!(actor, driver);
                terminal
            });

        assert_eq!(terminal.reason, "aborted_streaming");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            !events.iter().any(|event| matches!(
                event,
                QueryEvent::Message(crate::types::message::Message::User(user))
                    if user.content.iter().any(|content| matches!(
                        content,
                        crate::types::message::UserContent::Text(text)
                            if text == crate::utils::messages::INTERRUPT_MESSAGE
                                || text == crate::utils::messages::INTERRUPT_MESSAGE_FOR_TOOL_USE
                    ))
            )),
            "submit-interrupt must not add the marker"
        );
    }

    #[test]
    fn query_actor_todowrite_runs_without_permission_prompt() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        // Current CC enables TodoWrite only when TodoV2 is off. The Rust
        // bootstrap mapping uses this flag for a non-interactive legacy-todo
        // context; force-disable the explicit Task-tool override as well.
        let _non_interactive = EnvVarGuard::set("COMETIX_NON_INTERACTIVE_SESSION", "1");
        let _tasks_override = EnvVarGuard::unset("CLAUDE_CODE_ENABLE_TASKS");

        #[derive(Clone, Debug)]
        struct TodoDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for TodoDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_todos".to_string()),
                            name: "TodoWrite".to_string(),
                            input: serde_json::json!({
                                "todos": [
                                    {"content": "inspect", "status": "pending", "activeForm": "inspecting"}
                                ]
                            }),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let params = QueryParams {
            turn_id: "turn-todos".to_string(),
            input: "track work".to_string(),
            messages: vec![RenderableMessage::user("u1", "track work")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                TodoDeps {
                    calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::PermissionRequest(_)))
        );
        let seen_messages = seen_messages.lock().unwrap();
        assert!(seen_messages.len() >= 2, "seen_messages={seen_messages:#?}");
        assert!(
            seen_messages[1].iter().any(|message| matches!(
                message,
                crate::types::message::Message::User(user)
                    if user.content.iter().any(|content| matches!(
                        content,
                        crate::types::message::UserContent::ToolResult(result)
                            if result.tool_use_id.0 == "toolu_todos"
                                && result.content.contains("Todos have been modified successfully")
                                && !result.is_error
                    ))
            )),
            "second model request={:#?}",
            seen_messages[1]
        );
    }

    #[test]
    fn query_actor_uses_deny_rule_without_opening_permission_prompt() {
        #[derive(Clone, Debug)]
        struct DenyRuleDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for DenyRuleDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_rule_denied".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "echo blocked" }),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let mut permission_context = ToolPermissionContext::default();
        let mut deny_rules = std::collections::HashMap::new();
        deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash", None,
            )],
        );
        permission_context.always_deny_rules = deny_rules;
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let params = QueryParams {
            turn_id: "turn-deny-rule".to_string(),
            input: "please run it".to_string(),
            messages: vec![RenderableMessage::user("u1", "please run it")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::with_permission_context(
                permission_context,
            ),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                DenyRuleDeps {
                    calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    seen_messages: seen_messages.clone(),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::PermissionRequest(_)))
        );
        assert!(
            seen_messages.lock().unwrap()[1]
                .iter()
                .any(|message| matches!(
                    message,
                    crate::types::message::Message::User(user)
                        if user.content.iter().any(|content| matches!(
                            content,
                            crate::types::message::UserContent::ToolResult(result)
                                if result.tool_use_id.0 == "toolu_rule_denied" && result.is_error
                        ))
                ))
        );
    }

    #[test]
    fn query_actor_emits_permission_context_update_for_plan_mode_tool_effect() {
        #[derive(Clone, Debug)]
        struct EnterPlanDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl crate::query::deps::QueryDeps for EnterPlanDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_enter_plan".to_string()),
                            name: "EnterPlanMode".to_string(),
                            input: serde_json::json!({}),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "plan mode entered".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-enter-plan-context".to_string(),
            input: "make a plan".to_string(),
            messages: vec![RenderableMessage::user("u1", "make a plan")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_query_actor(
                params,
                EnterPlanDeps {
                    calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                },
                event_tx,
                command_rx,
            ));

        assert_eq!(terminal.reason, "completed");
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::PermissionRequest(_)))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            QueryEvent::PermissionContextUpdate(context)
                if context.mode == crate::types::permissions::PermissionMode::Plan
        )));
    }

    /// Re-derived 2026-08-26 (#135). This test used to assert `completed` for
    /// BOTH answers. That was pinning a missing abort, not CC:
    ///
    /// - `interactiveHandler.ts:183-203` `onReject(feedback, contentBlocks)`
    ///   resolves with `ctx.cancelAndAbort(feedback, undefined, contentBlocks)`;
    /// - `PermissionContext.ts:166-171` aborts the controller when
    ///   `isAbort || (!feedback && !contentBlocks?.length && !sub)` — true for a
    ///   bare main-loop "No";
    /// - `query.ts:1485-1515` then returns `{ reason: 'aborted_tools' }` after
    ///   the tool loop, so there is no continuation request.
    ///
    /// The error `tool_result` is still emitted first (`toolExecution.ts:1064-1071`),
    /// which is the half the original test was really about.
    ///
    /// A `Cancel` matrix row (asserting `completed` + continuation) was removed
    /// with the `PermissionPromptChoice::Cancel` variant itself (#143): CC's
    /// cancellation is the pre-`canUseTool` abort gate, not a permission
    /// answer, and its query-level outcome is the aborted turn — not a
    /// continuation.
    #[test]
    fn query_actor_deny_emits_error_tool_result_then_aborted_tools() {
        #[derive(Clone, Debug)]
        struct DenyDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for DenyDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_denied".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "printf denied" }),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        fn run_choice(
            choice: PermissionPromptChoice,
        ) -> (
            transitions::Terminal,
            Vec<Vec<crate::types::message::Message>>,
            Vec<ToolResultStatus>,
        ) {
            let params = QueryParams {
                turn_id: format!("turn-{choice:?}"),
                input: "please run it".to_string(),
                messages: vec![RenderableMessage::user("u1", "please run it")],
                model_messages: Vec::new(),
                system_prompt: Vec::new(),
                user_context: std::collections::BTreeMap::new(),
                system_context: std::collections::BTreeMap::new(),
                query_source: QuerySource::Prompt,
                token_budget: None,
                task_budget: None,
                max_turns: None,
                tool_use_context: crate::tool::ToolUseContext::default(),
            };
            let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let deps = DenyDeps {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                seen_messages: seen_messages.clone(),
            };
            let (event_tx, event_rx) = async_channel::unbounded();
            let (command_tx, command_rx) = async_channel::unbounded();

            let (terminal, statuses) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let actor = run_query_actor(params, deps, event_tx, command_rx);
                    tokio::pin!(actor);
                    let mut statuses = Vec::new();
                    let terminal = loop {
                        tokio::select! {
                            terminal = &mut actor => break terminal,
                            event = event_rx.recv() => match event {
                                Ok(QueryEvent::PermissionRequest(request)) => {
                                    command_tx.send(QueryCommand::PermissionDecision {
                                        tool_use_id: request.tool_use_id,
                                        choice,
                                    }).await.unwrap();
                                }
                                // Rows no longer store a status — derive
                                // it the way the renderer does (content
                                // sentinels + is_error).
                                Ok(QueryEvent::Row(message)) => {
                                    if let Some(result) = row_user_tool_result(&message) {
                                        statuses.push(result.derived_status());
                                    }
                                }
                                Ok(_) => {}
                                Err(_) => panic!("query event channel closed before terminal"),
                            }
                        }
                    };
                    while let Ok(event) = event_rx.try_recv() {
                        if let QueryEvent::Row(message) = event {
                            if let Some(result) = row_user_tool_result(&message) {
                                statuses.push(result.derived_status());
                            }
                        }
                    }
                    (terminal, statuses)
                });

            let seen = seen_messages.lock().unwrap().clone();
            (terminal, seen, statuses)
        }

        // A `Cancel` row used to sit next to Deny here, asserting a
        // non-aborting "cancelled" answer that continued the query — a path
        // CC does not have: cancellation is only the pre-canUseTool abort
        // gate (`toolExecution.ts:415-453`), never a permission response.
        for (choice, expected_status, expected_reason, expects_continuation) in [(
            PermissionPromptChoice::Deny,
            ToolResultStatus::Rejected,
            "aborted_tools",
            false,
        )] {
            let (terminal, seen, statuses) = run_choice(choice);
            assert_eq!(terminal.reason, expected_reason, "choice={choice:?}");
            assert!(statuses.contains(&expected_status), "choice={choice:?}");
            if expects_continuation {
                assert!(seen.len() >= 2, "expected continuation after {choice:?}");
                assert!(seen[1].iter().any(|message| matches!(
                    message,
                    crate::types::message::Message::User(user)
                        if user.content.iter().any(|content| matches!(
                            content,
                            crate::types::message::UserContent::ToolResult(result)
                                if result.tool_use_id.0 == "toolu_denied" && result.is_error
                        ))
                )));
            } else {
                assert_eq!(
                    seen.len(),
                    1,
                    "CC returns aborted_tools before the continuation request \
                     (query.ts:1485-1515); choice={choice:?}",
                );
            }
        }
    }

    #[test]
    fn query_actor_always_allow_suppresses_later_identical_prompt_in_same_batch() {
        #[derive(Clone, Debug)]
        struct RepeatedPermissionDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl crate::query::deps::QueryDeps for RepeatedPermissionDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let first = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_repeat_1".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "git push" }),
                        };
                        let second = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_repeat_2".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "git push" }),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(first),
                                        crate::types::message::AssistantContent::ToolUse(second),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-always-allow-batch".to_string(),
            input: "please run both".to_string(),
            messages: vec![RenderableMessage::user("u1", "please run both")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let deps = RepeatedPermissionDeps { calls };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::unbounded();

        let (terminal, permission_request_count, tool_result_ids, context_updates) =
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let actor = run_query_actor(params, deps, event_tx, command_rx);
                    tokio::pin!(actor);
                    let mut permission_request_count = 0usize;
                    let mut tool_result_ids = Vec::new();
                    let mut context_updates = Vec::new();
                    let terminal = loop {
                        tokio::select! {
                            terminal = &mut actor => break terminal,
                            event = event_rx.recv() => match event {
                                Ok(QueryEvent::PermissionRequest(request)) => {
                                    permission_request_count += 1;
                                    command_tx.send(QueryCommand::PermissionDecision {
                                        tool_use_id: request.tool_use_id,
                                        choice: PermissionPromptChoice::AlwaysAllow,
                                    }).await.unwrap();
                                }
                                Ok(QueryEvent::Row(message)) => {
                                    // Old `tool_use_id: Some(..)` guard ⇔
                                    // non-empty id.
                                    if let Some(result) = row_user_tool_result(&message) {
                                        if !result.tool_use_id.0.is_empty() {
                                            tool_result_ids.push(result.tool_use_id.0.clone());
                                        }
                                    }
                                }
                                Ok(QueryEvent::PermissionContextUpdate(context)) => {
                                    context_updates.push(context);
                                }
                                Ok(_) => {}
                                Err(_) => panic!("query event channel closed before terminal"),
                            }
                        }
                    };
                    while let Ok(event) = event_rx.try_recv() {
                        match event {
                            QueryEvent::Row(message) => {
                                if let Some(result) = row_user_tool_result(&message) {
                                    if !result.tool_use_id.0.is_empty() {
                                        tool_result_ids.push(result.tool_use_id.0.clone());
                                    }
                                }
                            }
                            QueryEvent::PermissionContextUpdate(context) => {
                                context_updates.push(context);
                            }
                            _ => {}
                        }
                    }
                    (
                        terminal,
                        permission_request_count,
                        tool_result_ids,
                        context_updates,
                    )
                });

        assert_eq!(terminal.reason, "completed");
        assert_eq!(permission_request_count, 1);
        assert_eq!(
            tool_result_ids,
            vec!["toolu_repeat_1".to_string(), "toolu_repeat_2".to_string()]
        );
        assert!(context_updates.iter().any(|context| {
            context.always_allow_rules.values().any(|rules| {
                rules.iter().any(|rule| {
                    rule.tool_name == "Bash" && rule.rule_content.as_deref() == Some("git push:*")
                })
            })
        }));
    }

    #[test]
    fn query_actor_always_allow_context_update_suppresses_next_turn_prompt() {
        #[derive(Clone, Debug)]
        struct SingleBashDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            tool_use_id: &'static str,
        }

        impl crate::query::deps::QueryDeps for SingleBashDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let tool_use_id = self.tool_use_id.to_string();
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId(tool_use_id),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "git push" }),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        fn run_turn(
            turn_id: &'static str,
            tool_use_id: &'static str,
            context: ToolPermissionContext,
            prompt_choice: PermissionPromptChoice,
        ) -> (transitions::Terminal, usize, ToolPermissionContext) {
            let params = QueryParams {
                turn_id: turn_id.to_string(),
                input: "please run remembered".to_string(),
                messages: vec![RenderableMessage::user("u1", "please run remembered")],
                model_messages: Vec::new(),
                system_prompt: Vec::new(),
                user_context: std::collections::BTreeMap::new(),
                system_context: std::collections::BTreeMap::new(),
                query_source: QuerySource::Prompt,
                token_budget: None,
                task_budget: None,
                max_turns: None,
                tool_use_context: crate::tool::ToolUseContext::with_permission_context(
                    context.clone(),
                ),
            };
            let (event_tx, event_rx) = async_channel::unbounded();
            let (command_tx, command_rx) = async_channel::unbounded();
            let deps = SingleBashDeps {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                tool_use_id,
            };

            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let actor = run_query_actor(params, deps, event_tx, command_rx);
                    tokio::pin!(actor);
                    let mut permission_request_count = 0usize;
                    let mut latest_context = context;
                    let terminal = loop {
                        tokio::select! {
                            terminal = &mut actor => break terminal,
                            event = event_rx.recv() => match event {
                                Ok(QueryEvent::PermissionRequest(request)) => {
                                    permission_request_count += 1;
                                    command_tx.send(QueryCommand::PermissionDecision {
                                        tool_use_id: request.tool_use_id,
                                        choice: prompt_choice,
                                    }).await.unwrap();
                                }
                                Ok(QueryEvent::PermissionContextUpdate(context)) => {
                                    latest_context = context;
                                }
                                Ok(_) => {}
                                Err(_) => panic!("query event channel closed before terminal"),
                            }
                        }
                    };
                    while let Ok(event) = event_rx.try_recv() {
                        if let QueryEvent::PermissionContextUpdate(context) = event {
                            latest_context = context;
                        }
                    }
                    (terminal, permission_request_count, latest_context)
                })
        }

        let (first_terminal, first_prompt_count, context_after_always_allow) = run_turn(
            "turn-always-allow-first",
            "toolu_remembered_1",
            ToolPermissionContext::default(),
            PermissionPromptChoice::AlwaysAllow,
        );
        assert_eq!(first_terminal.reason, "completed");
        assert_eq!(first_prompt_count, 1);
        assert!(
            context_after_always_allow
                .always_allow_rules
                .values()
                .any(|rules| rules.iter().any(|rule| {
                    rule.tool_name == "Bash" && rule.rule_content.as_deref() == Some("git push:*")
                }))
        );

        let (second_terminal, second_prompt_count, _) = run_turn(
            "turn-always-allow-second",
            "toolu_remembered_2",
            context_after_always_allow,
            PermissionPromptChoice::AllowOnce,
        );
        assert_eq!(second_terminal.reason, "completed");
        assert_eq!(second_prompt_count, 0);
    }

    #[test]
    fn query_actor_continues_next_model_request_with_tool_result() {
        #[derive(Clone, Debug)]
        struct ToolContinuationDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
        }

        impl crate::query::deps::QueryDeps for ToolContinuationDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let messages = request.messages;
                self.seen_messages.lock().unwrap().push(messages);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_continuation".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({ "command": "printf continuation" }),
                        };
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let mut permission_context = crate::tool::ToolPermissionContext::default();
        let mut rules = std::collections::HashMap::new();
        rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash",
                Some("printf continuation".to_string()),
            )],
        );
        permission_context.always_allow_rules = rules;
        let params = QueryParams {
            turn_id: "turn-tool-continuation".to_string(),
            input: "please run it".to_string(),
            messages: vec![RenderableMessage::user("u1", "please run it")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::with_permission_context(
                permission_context,
            ),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let deps = ToolContinuationDeps {
            calls,
            seen_messages: seen_messages.clone(),
        };
        let (event_tx, _event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(
                    QUERY_ACTOR_TEST_TIMEOUT,
                    run_query_actor(params, deps, event_tx, command_rx),
                )
                .await
                .expect("tool continuation actor should finish")
            });

        assert_eq!(terminal.reason, "completed");
        let seen = seen_messages.lock().unwrap();
        assert!(seen.len() >= 2, "expected a continuation model request");
        assert!(seen[1].iter().any(|message| matches!(
            message,
            crate::types::message::Message::User(user)
                if user.content.iter().any(|content| matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_continuation"
                            && result.content == "continuation"
                            && !result.is_error
                ))
        )));
    }

    #[test]
    fn query_actor_brief_tool_continuation_uses_official_model_ack_without_permission_prompt() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        // `USER_MSG_OPT_IN` seeds itself from `CLAUDE_CODE_BRIEF` on its first
        // read and never re-reads it (`bootstrap/state.rs:86-90`), so setting
        // the variable here only activated the tool while this test happened to
        // be the process's first reader. Drive the opt-in the way CC's
        // `maybeActivateBrief` does at startup.
        struct BriefOptInGuard(bool);
        impl Drop for BriefOptInGuard {
            fn drop(&mut self) {
                crate::bootstrap::state::set_user_msg_opt_in(self.0);
            }
        }

        let _opt_in = BriefOptInGuard(crate::bootstrap::state::get_user_msg_opt_in());
        crate::utils::process_env::set("CLAUDE_CODE_BRIEF", "1");
        crate::bootstrap::state::set_user_msg_opt_in(true);

        #[derive(Clone, Debug)]
        struct BriefContinuationDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            seen_messages:
                std::sync::Arc<std::sync::Mutex<Vec<Vec<crate::types::message::Message>>>>,
            seen_tools: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        }

        impl crate::query::deps::QueryDeps for BriefContinuationDeps {
            fn call_model(
                &self,
                request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                self.seen_messages.lock().unwrap().push(request.messages);
                self.seen_tools.lock().unwrap().push(
                    request
                        .tools
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect::<Vec<_>>(),
                );
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if call == 0 {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(
                                            crate::types::message::ToolUseBlock {
                                                id: crate::types::ids::ToolUseId(
                                                    "toolu_brief_query".to_string(),
                                                ),
                                                name: "SendUserMessage".to_string(),
                                                input: serde_json::json!({
                                                    "message": "Hello **there**",
                                                    "status": "normal"
                                                }),
                                            },
                                        ),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let params = QueryParams {
            turn_id: "turn-brief-continuation".to_string(),
            input: "reply briefly".to_string(),
            messages: vec![RenderableMessage::user("u1", "reply briefly")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::default(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_tools = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();

        let terminal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(
                    QUERY_ACTOR_TEST_TIMEOUT,
                    run_query_actor(
                        params,
                        BriefContinuationDeps {
                            calls,
                            seen_messages: seen_messages.clone(),
                            seen_tools: seen_tools.clone(),
                        },
                        event_tx,
                        command_rx,
                    ),
                )
                .await
                .expect("brief continuation actor should finish")
            });
        crate::utils::process_env::remove("CLAUDE_CODE_BRIEF");

        assert_eq!(terminal.reason, "completed");
        assert!(
            seen_tools
                .lock()
                .unwrap()
                .first()
                .is_some_and(|tools| tools.iter().any(|name| name == "SendUserMessage"))
        );
        assert!(
            seen_messages.lock().unwrap()[1]
                .iter()
                .any(|message| matches!(
                    message,
                    crate::types::message::Message::User(user)
                        if user.content.iter().any(|content| matches!(
                            content,
                            crate::types::message::UserContent::ToolResult(result)
                                if result.tool_use_id.0 == "toolu_brief_query"
                                    && result.content == "Message delivered to user."
                                    && !result.is_error
                        ))
                ))
        );
        while let Ok(event) = event_rx.try_recv() {
            assert!(!matches!(event, QueryEvent::PermissionRequest(_)));
        }
    }

    #[test]
    fn structured_output_tool_emits_out_of_band_query_event() {
        #[derive(Clone, Debug)]
        struct StructuredDeps {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl crate::query::deps::QueryDeps for StructuredDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    assert!(call <= 1, "unexpected structured-output model call {call}");
                    if call == 0 {
                        let block = crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_structured".to_string()),
                            name: crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME
                                .to_string(),
                            input: serde_json::json!({"answer": "ok"}),
                        };
                        tx.send(crate::services::api::claude::QueryModelStreamItem::Content(
                            crate::services::api::claude::ClaudeStreamItem::ToolUse {
                                id: block.id.0.clone(),
                                name: block.name.clone(),
                                input: block.input.clone(),
                                is_server: false,
                            },
                        ))
                        .await
                        .ok();
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![
                                        crate::types::message::AssistantContent::ToolUse(block),
                                    ],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::ToolUse),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    } else {
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                crate::types::message::AssistantMessage {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now(),
                                    content: vec![crate::types::message::AssistantContent::Text(
                                        "done".to_string(),
                                    )],
                                    model: None,
                                    stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                    usage: None,
                                },
                            ),
                        )
                        .await
                        .ok();
                    }
                    Ok(rx)
                })
            }
        }

        let schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
            "additionalProperties": false
        });
        let mut context = crate::tool::ToolUseContext::default();
        context.is_non_interactive_session = true;
        context.tools = vec![
            crate::tools::synthetic_output_tool::create_synthetic_output_tool(schema).unwrap(),
        ];
        let params = QueryParams {
            turn_id: "turn-structured".to_string(),
            input: "return JSON".to_string(),
            messages: Vec::new(),
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Sdk,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: context,
        };
        let handle = spawn_query(
            params,
            StructuredDeps {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
        );
        let events = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    let mut events = Vec::new();
                    loop {
                        let event = handle
                            .events
                            .recv()
                            .await
                            .expect("query event channel should remain open");
                        let terminal = matches!(&event, QueryEvent::Terminal(terminal) if terminal.reason == "completed");
                        events.push(event);
                        if terminal {
                            break events;
                        }
                    }
                })
                .await
                .expect("structured output query should finish")
            });
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::PermissionRequest(_)))
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                QueryEvent::StructuredOutput(value) if value == &serde_json::json!({"answer": "ok"})
            )),
            "events: {events:#?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            QueryEvent::ToolContextUpdate(context)
                if context.structured_output == Some(serde_json::json!({"answer": "ok"}))
        )));
    }

    #[test]
    fn grouped_tool_use_source_event_preserves_rows_for_visual_smoke() {
        // The group carries whole rows (CC types/message.ts:140-144);
        // summary/status are render-time derivations, not event payload.
        let member = RenderableMessage::assistant_block(
            "agent-row",
            AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_agent_1".to_string()),
                name: "Agent".to_string(),
                input: serde_json::json!({
                    "description": "Inspect auth",
                    "subagent_type": "reviewer",
                }),
            }),
        );
        let result = RenderableMessage::user_tool_result(
            "agent-result",
            "toolu_agent_1",
            "<usage>\ntool_uses: 2\ntotal_tokens: 1200\n</usage>",
            false,
        );
        let mut rows = QuerySourceEvent {
            uuid: "grouped-agent".to_string(),
            kind: QuerySourceEventKind::GroupedToolUse {
                tool_name: "Agent".to_string(),
                messages: vec![member.clone()],
                results: vec![result.clone()],
            },
        }
        .into_message()
        .into_rows();
        assert_eq!(rows.len(), 1);
        let message = rows.remove(0);

        match message.kind {
            RenderableMessageKind::GroupedToolUse(group) => {
                assert_eq!(group.tool_name, "Agent");
                assert_eq!(group.messages, vec![member]);
                assert_eq!(group.results, vec![result]);
            }
            other => panic!("unexpected message kind: {other:?}"),
        }
    }

    #[test]
    fn assistant_tool_use_source_event_preserves_official_tool_use_id() {
        let source_progress = vec![ToolUseProgressMessage::QueryUpdate {
            query: "rust progress seam".to_string(),
        }];
        let message = QuerySourceEvent {
            uuid: "message-id".to_string(),
            kind: QuerySourceEventKind::AssistantToolUse {
                tool_use_id: Some("toolu_explicit".to_string()),
                tool_name: "WebSearch".to_string(),
                input: Some(serde_json::json!({ "query": "rust progress seam" })),
                description: "rust progress seam".to_string(),
                status: ToolUseStatus::Running,
                progress_messages: source_progress.clone(),
            },
        }
        .into_message()
        .into_rows()
        .remove(0);

        assert_eq!(message.uuid, "message-id");
        let tool_use = row_assistant_tool_use(&message).expect("tool_use block");
        assert_eq!(tool_use.id.0, "toolu_explicit");
        assert_eq!(tool_use.name, "WebSearch");
        assert_eq!(
            tool_use.input,
            serde_json::json!({ "query": "rust progress seam" })
        );
        // Description and progress died at the seam: both are render-time
        // derivations now (render_tool_use_message and the lookups).
        let _ = &source_progress;
    }

    #[test]
    fn transparent_tool_use_progress_source_event_maps_to_progress_only_transcript_row() {
        let message = QuerySourceEvent {
            uuid: "transparent-message".to_string(),
            kind: QuerySourceEventKind::AssistantTransparentToolUseProgress {
                tool_use_id: Some("toolu_repl".to_string()),
                tool_name: "REPL".to_string(),
                progress_output: Some("VM output".to_string()),
                progress_status: None,
            },
        }
        .into_message()
        .into_rows()
        .remove(0);

        assert_eq!(message.uuid, "transparent-message");
        // CC has no transparent-progress message type: the same tool_use block
        // renders as progress because `tool.isTransparentWrapper?.()` says so
        // at render time (AssistantToolUseMessage.tsx:124). The seam now maps
        // the legacy source event to a plain REPL tool_use block.
        let tool_use = row_assistant_tool_use(&message).expect("tool_use block");
        assert_eq!(tool_use.id.0, "toolu_repl");
        assert_eq!(tool_use.name, "REPL");
    }
}

#[cfg(test)]
mod prompt_context_contract_tests {
    //! Caller-owned cache-prefix regression tests for CC `query.ts`.

    use super::*;
    use crate::query::deps::{CallModelRequest, CallModelStreamFuture, QueryDeps};
    use crate::services::api::claude::QueryModelStreamItem;
    use crate::types::message::{AssistantContent, AssistantMessage, StopReason};
    use crate::utils::test_env::{IsolatedProjectSettings, TEST_ENV_LOCK};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct CaptureRequests(Arc<Mutex<Vec<CallModelRequest>>>);

    impl QueryDeps for CaptureRequests {
        fn call_model(&self, request: CallModelRequest) -> CallModelStreamFuture {
            self.0.lock().unwrap().push(request);
            Box::pin(async {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                tx.send(QueryModelStreamItem::Assistant(AssistantMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![AssistantContent::Text("Done.".to_string())],
                    model: Some("claude-sonnet-4-6".to_string()),
                    stop_reason: Some(StopReason::EndTurn),
                    usage: None,
                }))
                .await
                .unwrap();
                Ok(rx)
            })
        }
    }

    /// CC `query.ts:182-190,252-269` requires and directly destructures all three
    /// inputs; `:449-451,659-661` passes only appendSystemContext/prependUserContext to the
    /// model. `QueryConfig` (`query/config.ts:20-51`) is runtime state, not copy.
    #[test]
    fn query_context_matches_official_empty_and_explicit_values_without_runtime_gate_leaks() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _settings = IsolatedProjectSettings::pin();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for source in [
            QuerySource::Prompt,
            QuerySource::AgentCustom,
            QuerySource::Sdk,
        ] {
            for nonempty in [false, true] {
                let params = QueryParams {
                    turn_id: "context-contract".to_string(),
                    input: "Inspect.".to_string(),
                    messages: vec![RenderableMessage::user("context-user", "Inspect.")],
                    model_messages: Vec::new(),
                    system_prompt: if nonempty {
                        vec!["CALLER_PROMPT".to_string()]
                    } else {
                        Vec::new()
                    },
                    user_context: if nonempty {
                        BTreeMap::from([("claudeMd".to_string(), "CALLER_MEMORY".to_string())])
                    } else {
                        BTreeMap::new()
                    },
                    system_context: if nonempty {
                        BTreeMap::from([
                            ("gitStatus".to_string(), "CALLER_GIT".to_string()),
                            (
                                "arbitraryCallerField".to_string(),
                                "CALLER_VALUE".to_string(),
                            ),
                        ])
                    } else {
                        BTreeMap::new()
                    },
                    query_source: source.clone(),
                    token_budget: None,
                    task_budget: None,
                    max_turns: None,
                    tool_use_context: crate::tool::ToolUseContext::default()
                        .with_main_loop_model("claude-sonnet-4-6"),
                };
                let captured = Arc::new(Mutex::new(Vec::new()));
                let deps = CaptureRequests(captured.clone());
                runtime
                    .block_on(run_query_once(params.clone(), deps.clone()))
                    .unwrap();
                let (event_tx, _event_rx) = async_channel::unbounded();
                let (_command_tx, command_rx) = async_channel::unbounded();
                let terminal =
                    runtime.block_on(run_query_actor(params, deps, event_tx, command_rx));
                assert_eq!(terminal.reason, "completed");
                let requests = captured.lock().unwrap();
                assert_eq!(requests.len(), 2, "legacy and streaming actor both queried");
                for request in requests.iter() {
                    assert_eq!(request.query_source, source);
                    let prompt = request.system_prompt.join("\n");
                    let messages = format!("{:?}", request.messages);
                    if nonempty {
                        assert!(prompt.contains("CALLER_PROMPT"));
                        assert!(prompt.contains("gitStatus: CALLER_GIT"));
                        assert!(prompt.contains("arbitraryCallerField: CALLER_VALUE"));
                        assert!(messages.contains("CALLER_MEMORY"));
                    } else {
                        assert!(request.system_prompt.is_empty(), "{prompt}");
                        assert!(!messages.contains("claudeMd"));
                    }
                    for gate in [
                        "session_id:",
                        "streaming_tool_execution:",
                        "emit_tool_use_summaries:",
                        "is_ant:",
                        "fast_mode_enabled:",
                    ] {
                        assert!(
                            !prompt.contains(gate),
                            "runtime field {gate} leaked: {prompt}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod permission_request_resolution_tests {
    use super::*;
    use crate::services::tools::streaming_tool_executor::streaming_hook_decision_tests::PermissionRequestFixture;
    use crate::types::message::{
        AssistantContent, AssistantMessage, Message, StopReason, UserContent,
    };
    use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
    use crate::utils::test_env::TEST_ENV_LOCK;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct HookReadDeps {
        block: crate::types::message::ToolUseBlock,
        requests: Arc<Mutex<Vec<crate::query::deps::CallModelRequest>>>,
    }

    impl crate::query::deps::QueryDeps for HookReadDeps {
        fn call_model(
            &self,
            request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            let first = {
                let mut requests = self.requests.lock().unwrap();
                let first = requests.is_empty();
                requests.push(request);
                first
            };
            let block = self.block.clone();
            Box::pin(async move {
                let (sender, receiver) = tokio::sync::mpsc::channel(8);
                sender
                    .send(
                        crate::services::api::claude::QueryModelStreamItem::Assistant(
                            AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: if first {
                                    vec![AssistantContent::ToolUse(block)]
                                } else {
                                    vec![AssistantContent::Text("done".into())]
                                },
                                model: None,
                                stop_reason: Some(if first {
                                    StopReason::ToolUse
                                } else {
                                    StopReason::EndTurn
                                }),
                                usage: None,
                            },
                        ),
                    )
                    .await
                    .unwrap();
                Ok(receiver)
            })
        }
    }

    /// CC PermissionContext.ts:319-335 resolves canUseTool directly; query.ts
    /// consumes the resulting tool execution without another PreToolUse check.
    #[tokio::test]
    async fn serial_query_permission_request_hook_resolution_matches_official() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _environment = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let fixture = PermissionRequestFixture::new("1");
        fixture.install_hooks(true);
        let _streaming = crate::query::config::set_streaming_tool_execution_for_test(false);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let deps = HookReadDeps {
            block: fixture.block(),
            requests: requests.clone(),
        };
        let context = fixture.context();
        let abort = context.abort_controller.clone();
        let params = QueryParams {
            turn_id: "turn-pr-serial-resolution".into(),
            input: "read the authorized file".into(),
            messages: vec![RenderableMessage::user("u-pr", "read the authorized file")],
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: context,
        };
        let (event_tx, event_rx) = async_channel::unbounded();
        let (_command_tx, command_rx) = async_channel::unbounded();
        let mut events = Vec::new();
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let actor = run_query_actor(params, deps, event_tx, command_rx);
            tokio::pin!(actor);
            loop {
                tokio::select! {
                    terminal = &mut actor => break terminal,
                    event = event_rx.recv() => {
                        let event = event.expect("actor event");
                        assert!(!matches!(&event, QueryEvent::PermissionRequest(_)),
                            "PermissionRequest hook already resolved both pending Ask sources");
                        events.push(event);
                    }
                }
            }
        })
        .await
        .expect("serial actor must complete without a permission dialog");
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert_eq!(terminal.reason, "completed");
        assert!(!abort.is_aborted());
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, QueryEvent::PermissionRequest(_)))
        );
        let requests = requests.lock().unwrap();
        let following = requests
            .get(1)
            .expect("model receives the actual Read result on the next turn");
        let result = following
            .messages
            .iter()
            .find_map(|message| match message {
                Message::User(message) => {
                    message.content.iter().find_map(|content| match content {
                        UserContent::ToolResult(result)
                            if result.tool_use_id.0 == "toolu_pr_resolved" =>
                        {
                            Some(result)
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("actual serial tool_result reaches callModel");
        assert!(!result.is_error, "{}", result.content);
        assert!(
            result
                .content
                .contains("PermissionRequest authorized exact content")
        );
        assert!(!result.content.contains("must not read original"));
        assert_eq!(
            result.tool_use_result.as_ref().unwrap()["file"]["filePath"],
            serde_json::json!(fixture.target)
        );
        assert!(events.iter().any(|event| matches!(event,
            QueryEvent::Row(RenderableMessage { kind: RenderableMessageKind::Attachment(attachment), .. })
            if attachment == &crate::types::message::Attachment::HookPermissionDecision {
                decision: "allow".into(), tool_use_id: "toolu_pr_resolved".into(), hook_event: "PermissionRequest".into(),
            }
        )), "serial execution keeps PermissionRequest attribution");
        assert_eq!(
            following
                .permission_context
                .always_allow_rules
                .get(&PermissionRuleSource::LocalSettings),
            Some(&vec![PermissionRuleValue::new("Read", None)])
        );
        assert_eq!(
            following
                .permission_context
                .always_ask_rules
                .get(&PermissionRuleSource::Session),
            Some(&vec![PermissionRuleValue::new("Read", None)])
        );
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixture.local_settings()).unwrap())
                .unwrap();
        assert_eq!(
            persisted["permissions"]["allow"],
            serde_json::json!(["Read"])
        );
    }
}
