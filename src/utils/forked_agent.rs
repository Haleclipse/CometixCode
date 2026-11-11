//! Maps to: CC `utils/forkedAgent.ts` — isolated ToolUseContext for subagents.
//!
//! Batch 3g: `create_subagent_context` mirrors CC isolation for abort linking,
//! denial tracking, UI callback clearing, fresh in-progress tool IDs, and
//! content-replacement / read-file-state cloning.

use crate::constants::query_source::QuerySource;
use crate::tool::{AbortController, ToolUseContext};
use crate::utils::permissions::denial_tracking::create_denial_tracking_state;

/// @nyan: forks whose prompt prefix must stay byte-identical to the main
/// thread so they hit its prompt cache (README §1.8).
///
/// The list is closed and lives in exactly one place. Every consumer — the
/// masking pipeline (compaction C2), the `X-Claude-Code-Agent-Id` header
/// decision (compaction C13-3 / slice G) and the background-subagent tool
/// allowlist (retrieval slice I) — asks this helper rather than matching on
/// `QuerySource` itself, so a new cache-sharing fork is added once.
///
/// Three rules apply to every member:
/// 1. No `X-Claude-Code-Agent-Id` header, so the gateway keeps the main
///    thread's `prompt_cache_key`.
/// 2. Replay the parent's `ContentReplacementState`; never add masking
///    decisions (see [`crate::utils::tool_result_storage::ContentReplacementState`]).
/// 3. Never rewrite the request prefix (system prompt, tools, context).
pub fn is_cache_sharing_fork(source: &QuerySource) -> bool {
    matches!(
        source,
        QuerySource::Compact
            | QuerySource::SideQuestion
            | QuerySource::PromptSuggestion
            | QuerySource::ExtractMemories
            | QuerySource::AutoDream
            | QuerySource::CacheWarm
    )
}

/// API adapters receive the serialized source rather than the enum. Keep this
/// boundary projection beside the enum predicate so callers do not grow a
/// second cache-sharing allowlist.
pub fn is_cache_sharing_api_source(source: &str) -> bool {
    matches!(
        source,
        "compact"
            | "side_question"
            | "prompt_suggestion"
            | "extract_memories"
            | "auto_dream"
            | "cache_warm"
    )
}

pub fn is_agent_api_source(source: &str) -> bool {
    // `hook_agent` is the hook-launched agent loop: an independent context with
    // its own prefix, so it must not share the main thread's gateway cache key
    // (compaction C13-3).
    source == "agent" || source.starts_with("agent:") || source == "hook_agent"
}

/// Maps to: CC `SubagentContextOverrides` (`utils/forkedAgent.ts:260-306`).
#[derive(Clone, Debug, Default)]
pub struct SubagentContextOverrides {
    /// Maps to CC `options` as the flattened Rust options projection.
    pub options: Option<crate::tool::ToolUseContextOptions>,
    /// Maps to: CC `shareSetAppState` — sync agents share writes with parent.
    pub share_set_app_state: bool,
    /// Maps to: CC `shareAbortController` — interactive agents share parent's
    /// abort handle. Ignored when [`Self::abort_controller`] is `Some`.
    pub share_abort_controller: bool,
    /// Maps to: CC `abortController` override (AgentTool async = unlinked new;
    /// sync = parent handle).
    pub abort_controller: Option<AbortController>,
    /// Maps to CC `agentId` / `agentType` overrides.
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    /// Maps to CC `messages` override.
    pub messages: Option<Vec<crate::types::message::Message>>,
    /// Maps to: CC `readFileState?: ToolUseContext['readFileState']`
    /// (`utils/forkedAgent.ts:269-270` "Override the readFileState (e.g., fresh
    /// cache instead of clone)").
    ///
    /// The carrier is the CACHE, not a list of entries, because CC's override
    /// also supplies the `max` / `maxSize` limits `cloneFileStateCache` rebuilds
    /// the child's cache with (`fileStateCache.ts:122-126`). `runAgent.ts:375-378`
    /// is the only CC caller, and it uses this to hand a non-fork subagent an
    /// EMPTY `createFileStateCacheWithSizeLimit(READ_FILE_STATE_CACHE_SIZE)`.
    pub read_file_state: Option<crate::tool::SharedFileStateCache>,
    /// Maps to CC `getAppState` override.
    pub get_app_state: Option<crate::tool::GetAppStateCallback>,
    /// Maps to CC `shareSetResponseLength`.
    pub share_set_response_length: bool,
    /// Maps to CC `requireCanUseTool`.
    pub require_can_use_tool: Option<bool>,
    /// Maps to: CC `contentReplacementState` override.
    pub content_replacement_state:
        Option<crate::utils::tool_result_storage::ContentReplacementState>,
    /// Maps to: CC `criticalSystemReminder_EXPERIMENTAL`.
    pub critical_system_reminder_experimental: Option<String>,
    /// L1 test/headless override for the getAppState prompt-avoidance projection.
    /// `None` follows CC: custom getAppState > shared abort > avoid prompts.
    pub avoid_permission_prompts_overlay: Option<bool>,
}

/// Maps to: CC `createSubagentContext(parentContext, overrides)`.
///
/// Default isolation:
/// - `read_file_state` cloned from the override when given, else from the parent
/// - `setAppState` no-op unless `share_set_app_state`
/// - `setAppStateForTasks` always reaches the parent's root store
/// - abort: override > share parent > `AbortController::child_of(parent)`
/// - fresh `localDenialTracking` when setAppState is isolated
/// - UI sinks cleared (`it2_setup_prompt_sink`, channel permission callbacks)
/// - fresh `in_progress_tool_use_ids`
pub fn create_subagent_context(
    parent: &ToolUseContext,
    overrides: SubagentContextOverrides,
) -> ToolUseContext {
    let SubagentContextOverrides {
        options,
        share_set_app_state,
        share_abort_controller,
        abort_controller,
        agent_id,
        agent_type,
        messages,
        read_file_state,
        get_app_state,
        share_set_response_length,
        require_can_use_tool,
        content_replacement_state,
        critical_system_reminder_experimental,
        avoid_permission_prompts_overlay,
    } = overrides;
    let root_store = parent
        .app_store
        .tasks_store
        .clone()
        .or_else(|| parent.app_store.store.clone());
    let has_get_app_state_override = get_app_state.is_some();
    let avoid_prompts = avoid_permission_prompts_overlay
        .unwrap_or(!has_get_app_state_override && !share_abort_controller);
    let abort_controller = abort_controller.unwrap_or_else(|| {
        if share_abort_controller {
            parent.abort_controller.clone()
        } else {
            AbortController::child_of(parent.abort_controller.clone())
        }
    });

    let mut child = parent.clone();
    // Maps to: CC `utils/forkedAgent.ts:377-381`
    //
    // ```ts
    // // Clone overrides.readFileState if provided, otherwise clone from parent
    // readFileState: cloneFileStateCache(
    //   overrides?.readFileState ?? parentContext.readFileState,
    // ),
    // ```
    //
    // `??`, not truthiness, so an EMPTY override cache still wins over the
    // parent's — that is exactly what `runAgent.ts:375-378` relies on to give a
    // non-fork subagent no inherited Read state.
    //
    // The clone (rather than the Arc) is what keeps the child's LRU identity its
    // own: `SharedFileStateCache` is `Arc<Mutex<FileStateCache>>` and `get`
    // promotes the hit to MRU, so sharing the handle would let a subagent's
    // reads reorder and evict the parent's entries. Trigger Sets are
    // independently replaced with fresh identities below.
    child.read_file_state = read_file_state
        .as_ref()
        .unwrap_or(&parent.read_file_state)
        .cloned_contents();
    if let Some(options) = options {
        child.apply_options(options);
    }
    if let Some(messages) = messages {
        child.messages = messages;
    }
    child.abort_controller = abort_controller;
    child.app_store.writable = share_set_app_state;
    child.app_store.avoid_permission_prompts_overlay = avoid_prompts;
    // Same CC line (`forkedAgent.ts:362-374`), on the object the permission
    // engine actually reads. CC's wrapper is `getAppState`, and everything
    // downstream of `hasPermissionsToUseTool` reads
    // `context.getAppState().toolPermissionContext`; this port passes
    // `ToolUseContext.tool_permission_context` into the gate instead, so the
    // overlay above only covers `get_app_state()` callers. CC only ever raises
    // the flag (`if (state...shouldAvoidPermissionPrompts) return state`), never
    // clears it, so this is `|=`.
    //
    // Assigned directly, NOT through `update_permission_context`: that helper
    // also writes the value back to the store, and CC's wrapper is a read-side
    // projection over the parent's state (`return { ...state, ... }`) which the
    // parent must never observe.
    if avoid_prompts {
        child
            .tool_permission_context
            .should_avoid_permission_prompts = true;
    }
    child.app_store.tasks_store = root_store;
    if let Some(get_app_state) = get_app_state {
        child.app_store.get_state_override = get_app_state;
    }

    // Fresh per-subagent collections and mutation sinks.
    child.in_progress_tool_use_ids.clear();
    // Maps to: CC `utils/forkedAgent.ts:425` `setInProgressToolUseIDs: () => {}`
    // — a subagent's tool uses must not appear in the parent REPL's live set.
    child.set_in_progress_tool_use_ids = crate::tool::SetInProgressToolUseIds::default();
    // The interruptible projection is likewise REPL-local. A forked agent's
    // cancel-capable tools must not make the parent accept an interrupt while
    // the parent turn is blocked on that fork.
    child.set_has_interruptible_tool_in_progress =
        crate::tool::SetHasInterruptibleToolInProgress::default();
    child.interruptible_tool_use_ids.clear();
    child.nested_memory_attachment_triggers = parent
        .nested_memory_attachment_triggers
        .as_ref()
        .map(|_| crate::tool::SharedOrderedTriggerSet::fresh());
    child.loaded_nested_memory_paths.clear();
    child.dynamic_skill_dir_triggers = parent
        .dynamic_skill_dir_triggers
        .as_ref()
        .map(|_| crate::tool::SharedOrderedTriggerSet::fresh());
    child.discovered_skill_names.clear();
    child.tool_decisions = None;
    // Maps to: CC `utils/forkedAgent.ts:420-422`. A sharing fork gets the SAME
    // object (clone the Arc, not the value); an isolated one gets its own
    // `createDenialTrackingState()`, which is what keeps its denials off the
    // parent's store — `persistDenialState` writes locally whenever this is set.
    child.local_denial_tracking = if share_set_app_state {
        parent.local_denial_tracking.clone()
    } else {
        Some(crate::tool::SharedDenialTracking::new(
            create_denial_tracking_state(),
        ))
    };

    // UI callbacks are unavailable to isolated subagents. Response/API metrics
    // are the one explicit opt-in, exactly matching shareSetResponseLength.
    child.it2_setup_prompt_sink = crate::tool::It2SetupPromptSink::default();
    child.channel_permission_callbacks = None;
    child.tool_progress_sink = crate::tool::ToolProgressSink::default();
    if !share_set_response_length {
        child.response_length_sink = crate::tool::ResponseLengthSink::default();
        child.api_metrics_sink = crate::tool::ApiMetricsSink::default();
    }

    child.agent_id = Some(
        agent_id.unwrap_or_else(|| crate::tools::agent_tool::run_agent::create_agent_id(None)),
    );
    child.agent_type = agent_type;
    child.require_can_use_tool = require_can_use_tool.unwrap_or(false);
    child.content_replacement_state =
        content_replacement_state.or_else(|| parent.content_replacement_state.clone());
    // CC assigns this directly from the override; omission clears an inherited
    // reminder rather than leaking it into unrelated forks.
    child.critical_system_reminder_experimental = critical_system_reminder_experimental;

    child.query_tracking = Some(crate::tool::QueryChainTracking {
        chain_id: uuid::Uuid::new_v4().to_string(),
        depth: parent
            .query_tracking
            .as_ref()
            .map(|tracking| tracking.depth.saturating_add(1))
            .unwrap_or(0),
    });
    child
}

/// Maps to CC `PreparedForkedContext` (`utils/forkedAgent.ts:176-184`).
pub struct PreparedForkedContext {
    pub skill_content: String,
    pub modified_get_app_state: crate::tool::GetAppStateCallback,
    pub base_agent: crate::tools::agent_tool::load_agents_dir::AgentDefinition,
    pub prompt_messages: Vec<crate::types::message::Message>,
}

fn parsed_allowed_tool_rules(
    allowed_tools: &[String],
) -> Vec<crate::types::permissions::PermissionRuleValue> {
    use crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string;
    use crate::utils::permissions::permission_setup::parse_tool_list_from_cli;

    parse_tool_list_from_cli(allowed_tools)
        .iter()
        .map(|spec| permission_rule_value_from_string(spec))
        .collect()
}

/// Maps to CC `createGetAppStateWithAllowedTools`
/// (`utils/forkedAgent.ts:147-169`).
pub fn create_get_app_state_with_allowed_tools(
    parent: &ToolUseContext,
    allowed_tools: &[String],
) -> crate::tool::GetAppStateCallback {
    use crate::types::permissions::PermissionRuleSource;

    let parent = parent.clone();
    let rules = parsed_allowed_tool_rules(allowed_tools);
    crate::tool::GetAppStateCallback::new(move || {
        let state = parent.get_app_state().unwrap_or_else(|| {
            // Rust headless/test contexts may carry only the local snapshot;
            // project it through the canonical getAppState boundary too.
            let mut state = crate::state::app_state_store::AppState::default();
            state.tool_permission_context =
                std::sync::Arc::new(parent.tool_permission_context.clone());
            std::sync::Arc::new(state)
        });
        if rules.is_empty() {
            return Some(state);
        }
        let mut projected = (*state).clone();
        let mut permission = (*projected.tool_permission_context).clone();
        let entry = permission
            .always_allow_rules
            .entry(PermissionRuleSource::Command)
            .or_default();
        for rule in &rules {
            if !entry.contains(rule) {
                entry.push(rule.clone());
            }
        }
        projected.tool_permission_context = std::sync::Arc::new(permission);
        Some(std::sync::Arc::new(projected))
    })
}

/// Maps to CC `prepareForkedCommandContext` (`utils/forkedAgent.ts`:191).
pub fn prepare_forked_command_context(
    command: &crate::skills::load_skills_dir::SkillCommand,
    args: Option<&str>,
    context: &ToolUseContext,
) -> Result<PreparedForkedContext, String> {
    let skill_content = command
        .get_prompt_for_command(args, &crate::bootstrap::state::get_session_id(), context)
        .map_err(|error| error.to_string())?;

    let modified_get_app_state =
        create_get_app_state_with_allowed_tools(context, &command.allowed_tools);

    // CC reads the exact invocation context's already-resolved activeAgents;
    // do not rescan disk or bypass source/policy ordering here.
    let agent_type_name = command.agent.as_deref().unwrap_or("general-purpose");
    let base_agent = context
        .agent_definitions
        .active_agents
        .iter()
        .find(|agent| agent.agent_type == agent_type_name)
        .or_else(|| {
            context
                .agent_definitions
                .active_agents
                .iter()
                .find(|agent| agent.agent_type == "general-purpose")
        })
        .or_else(|| context.agent_definitions.active_agents.first())
        .cloned()
        .ok_or_else(|| "No agent available for forked execution".to_string())?;

    let prompt_messages = vec![crate::types::message::Message::User(
        crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                skill_content.clone(),
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
    )];

    Ok(PreparedForkedContext {
        skill_content,
        modified_get_app_state,
        base_agent,
        prompt_messages,
    })
}

/// Maps to CC `extractResultText` (`utils/forkedAgent.ts`:237).
pub fn extract_result_text(
    agent_messages: &[crate::types::message::Message],
    default_text: &str,
) -> String {
    use crate::types::message::{AssistantContent, Message};

    let Some(assistant) = agent_messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant),
            _ => None,
        })
    else {
        return default_text.to_string();
    };
    let text = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        default_text.to_string()
    } else {
        text
    }
}

/// Maps to: CC `CacheSafeParams` (`utils/forkedAgent.ts`).
#[derive(Clone)]
pub struct CacheSafeParams {
    pub system_prompt: crate::services::api::request::SystemPrompt,
    pub user_context: std::collections::BTreeMap<String, String>,
    pub system_context: std::collections::BTreeMap<String, String>,
    pub tool_use_context: ToolUseContext,
    /// Shared immutable parent history, matching CC's shared array reference.
    pub fork_context_messages: std::sync::Arc<Vec<crate::types::message::Message>>,
}

/// Slot written by handleStopHooks after each turn so post-turn forks
/// (promptSuggestion, postTurnSummary, /btw) can share the main loop's
/// prompt cache without each caller threading params through.
static LAST_CACHE_SAFE_PARAMS: std::sync::Mutex<Option<std::sync::Arc<CacheSafeParams>>> =
    std::sync::Mutex::new(None);

/// Maps to: CC `saveCacheSafeParams`.
pub fn save_cache_safe_params(params: Option<CacheSafeParams>) {
    save_cache_safe_params_arc(params.map(std::sync::Arc::new));
}

pub fn save_cache_safe_params_arc(params: Option<std::sync::Arc<CacheSafeParams>>) {
    *LAST_CACHE_SAFE_PARAMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = params;
}

/// Maps to: CC `getLastCacheSafeParams`.
///
/// CC returns the same object reference. Rust returns an `Arc` snapshot so a
/// `/btw` render never deep-clones the full cached conversation merely to read
/// the cache-critical prompt/context bytes.
pub fn get_last_cache_safe_params() -> Option<std::sync::Arc<CacheSafeParams>> {
    LAST_CACHE_SAFE_PARAMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Context shape for [`create_cache_safe_params`] — maps to CC `REPLHookContext`
/// fields consumed by `createCacheSafeParams`.
pub struct CacheSafeParamsContext {
    pub messages: Vec<crate::types::message::Message>,
    pub system_prompt: crate::services::api::request::SystemPrompt,
    pub user_context: std::collections::BTreeMap<String, String>,
    pub system_context: std::collections::BTreeMap<String, String>,
    pub tool_use_context: ToolUseContext,
}

/// Maps to: CC `createCacheSafeParams`.
pub fn create_cache_safe_params(context: CacheSafeParamsContext) -> CacheSafeParams {
    CacheSafeParams {
        system_prompt: context.system_prompt,
        user_context: context.user_context,
        system_context: context.system_context,
        tool_use_context: context.tool_use_context,
        fork_context_messages: std::sync::Arc::new(context.messages),
    }
}

/// Typed progress transport for Rust query events that correspond to CC
/// `ProgressMessage` / tool-use-summary output messages.
#[derive(Clone, Debug, PartialEq)]
pub enum ForkedAgentProgress {
    Tool(crate::types::tools::ToolProgress),
    StopHook(crate::query::StopHookProgressEvent),
    ToolUseSummary(crate::types::message::ToolUseSummaryMessage),
}

/// Maps to: CC `ForkedAgentParams` (`utils/forkedAgent.ts:81-114`).
pub struct ForkedAgentParams {
    pub prompt_messages: Vec<crate::types::message::Message>,
    pub cache_safe_params: CacheSafeParams,
    /// Maps to the required CC `canUseTool` callback. Forks must not silently
    /// fall back to the parent session's permission rules.
    pub can_use_tool: crate::tool::CanUseToolCallback,
    pub query_source: crate::constants::query_source::QuerySource,
    pub fork_label: String,
    pub overrides: Option<SubagentContextOverrides>,
    /// Maps to CC `maxOutputTokens`; changing this can invalidate cache sharing
    /// on non-adaptive-thinking models, so callers must opt in explicitly.
    pub max_output_tokens: Option<u32>,
    pub max_turns: Option<u32>,
    pub on_message: Option<std::sync::Arc<dyn Fn(&crate::types::message::Message) + Send + Sync>>,
    pub on_progress: Option<std::sync::Arc<dyn Fn(&ForkedAgentProgress) + Send + Sync>>,
    pub skip_cache_write: bool,
    pub skip_transcript: bool,
}

/// Maps to: CC `ForkedAgentResult`.
pub struct ForkedAgentResult {
    pub messages: Vec<crate::types::message::Message>,
    pub total_usage: crate::services::api::request::NonNullableUsage,
    /// Typed metadata for API errors yielded by query. Display-compatible
    /// System messages remain in `messages` for official output semantics.
    pub api_errors: Vec<crate::types::message::SystemApiErrorMessage>,
    /// Request IDs from assistant `message_start` stream events, in turn order.
    pub request_ids: Vec<String>,
    pub progress: Vec<ForkedAgentProgress>,
}

fn usage_from_stream_event(
    event: &crate::types::message::StreamEvent,
) -> Option<crate::services::api::request::NonNullableUsage> {
    use crate::services::api::request::{EMPTY_USAGE, update_usage};

    let crate::types::message::StreamEvent::ApiEvent { event, .. } = event;
    if event.get("type").and_then(serde_json::Value::as_str) != Some("message_delta") {
        return None;
    }
    let usage = event.get("usage")?.as_object()?;
    Some(update_usage(&EMPTY_USAGE, Some(usage)))
}

/// Maps to: CC `recordSidechainTranscript(...)` at `forkedAgent.ts:531` and
/// `:588` — neither passes a project path, `getProject()` owns it — plus the
/// `lastRecordedUuid` cursor between them (`:529`, `:536-539`, `:596`).
///
/// `stamp_cwd` reaches only the entry's `cwd` field (CC `getCwd()` inside
/// `insertMessageChain`, `sessionStorage.ts:1059`); the transcript's directory
/// is resolved by `getAgentTranscriptPath` (`:250`) from
/// `getSessionProjectDir() ?? getProjectDir(getOriginalCwd())`.
///
/// `None` is CC's `skipTranscript` branch (`forkedAgent.ts:528`): no agent id,
/// so no transcript I/O at all. The fork label survives inside the id —
/// `create_agent_id(Some(label))` yields `a{label}-{suffix}` — so the
/// recorder's `agent_id`-keyed debug log still names the fork.
fn start_fork_sidechain_recorder(
    agent_id: Option<&str>,
    initial_messages: &[crate::types::message::Message],
    stamp_cwd: &std::path::Path,
) -> Option<crate::utils::session_storage::SidechainTranscriptRecorder> {
    agent_id.map(|agent_id| {
        crate::utils::session_storage::SidechainTranscriptRecorder::start(
            agent_id,
            initial_messages,
            stamp_cwd,
        )
    })
}

/// Maps to: CC `runForkedAgent` (`utils/forkedAgent.ts:483-625`).
///
/// Runs an isolated query loop with cache-safe params, the caller's exact
/// permission callback, message-delta usage accumulation, optional sidechain
/// recording, and `skipCacheWrite` forwarded to every API request.
pub async fn run_forked_agent(params: ForkedAgentParams) -> anyhow::Result<ForkedAgentResult> {
    run_forked_agent_with_deps(params, crate::query::deps::production_deps()).await
}

pub(crate) async fn run_forked_agent_with_deps<D>(
    params: ForkedAgentParams,
    deps: D,
) -> anyhow::Result<ForkedAgentResult>
where
    D: crate::query::deps::QueryDeps,
{
    use crate::query::{QueryCommand, QueryEvent, QueryParams};
    use crate::services::api::request::{EMPTY_USAGE, accumulate_usage};
    use crate::types::permissions::{PermissionPromptChoice, PermissionPromptResponse};

    let ForkedAgentParams {
        prompt_messages,
        cache_safe_params: cache,
        can_use_tool,
        query_source,
        fork_label,
        overrides,
        max_output_tokens,
        max_turns,
        on_message,
        on_progress,
        skip_cache_write,
        skip_transcript,
    } = params;
    // Stand-in for CC's `getCwd()` (ALS store ?? global, `utils/cwd.ts:26-32`)
    // as read by `insertMessageChain` (`sessionStorage.ts:1059`). Stamp only —
    // it never chooses the transcript's directory.
    let stamp_cwd = cache.tool_use_context.effective_cwd();
    let mut isolated =
        create_subagent_context(&cache.tool_use_context, overrides.unwrap_or_default());
    isolated.can_use_tool = can_use_tool;
    isolated.skip_cache_write = skip_cache_write;
    isolated.max_output_tokens_override = max_output_tokens;

    let mut initial_messages = std::sync::Arc::unwrap_or_clone(cache.fork_context_messages);
    initial_messages.extend(prompt_messages);

    let agent_id = (!skip_transcript)
        .then(|| crate::tools::agent_tool::run_agent::create_agent_id(Some(&fork_label)));
    let mut sidechain =
        start_fork_sidechain_recorder(agent_id.as_deref(), &initial_messages, &stamp_cwd);

    let query_params = QueryParams {
        turn_id: format!("fork:{fork_label}"),
        input: String::new(),
        messages: Vec::new(),
        model_messages: initial_messages,
        system_prompt: cache.system_prompt,
        user_context: cache.user_context,
        system_context: cache.system_context,
        query_source,
        token_budget: None,
        task_budget: None,
        max_turns,
        tool_use_context: isolated,
    };

    let handle = crate::query::spawn_query(query_params, deps);
    let mut output_messages = Vec::new();
    let mut total_usage = EMPTY_USAGE;
    let mut api_errors = Vec::new();
    let mut request_ids = Vec::new();
    let mut progress = Vec::new();

    loop {
        match handle.events.recv().await {
            Ok(QueryEvent::Stream(event)) => {
                if let Some(turn_usage) = usage_from_stream_event(&event) {
                    total_usage = accumulate_usage(&total_usage, &turn_usage);
                }
                let crate::types::message::StreamEvent::ApiEvent { event, .. } = &event;
                if let Some(request_id) = event
                    .get("request_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|request_id| !request_ids.iter().any(|seen| seen == request_id))
                {
                    request_ids.push(request_id.to_string());
                }
            }
            Ok(QueryEvent::ApiError(error)) => {
                api_errors.push(error);
            }
            Ok(QueryEvent::ToolProgress(event)) => {
                let event = ForkedAgentProgress::Tool(event);
                if let Some(callback) = on_progress.as_ref() {
                    callback(&event);
                }
                progress.push(event);
            }
            Ok(QueryEvent::StopHookProgress(event)) => {
                let event = ForkedAgentProgress::StopHook(event);
                if let Some(callback) = on_progress.as_ref() {
                    callback(&event);
                }
                progress.push(event);
            }
            Ok(QueryEvent::ToolUseSummary(event)) => {
                let event = ForkedAgentProgress::ToolUseSummary(event);
                if let Some(callback) = on_progress.as_ref() {
                    callback(&event);
                }
                progress.push(event);
            }
            // C3c-3: `Message` carries a whole model message (the seam's CC
            // yield); forks consume it exactly like the history-only
            // `ModelMessage` transport — headless output has no row
            // projection.
            Ok(QueryEvent::Message(message)) | Ok(QueryEvent::ModelMessage(message)) => {
                if let crate::types::message::Message::Assistant(assistant) = &message
                    && let Some(request_id) = assistant
                        .request_id()
                        .filter(|request_id| !request_ids.iter().any(|seen| seen == request_id))
                {
                    request_ids.push(request_id.to_string());
                }
                if let Some(callback) = on_message.as_ref() {
                    callback(&message);
                }
                if matches!(
                    &message,
                    crate::types::message::Message::Assistant(_)
                        | crate::types::message::Message::User(_)
                ) && let Some(sidechain) = sidechain.as_mut()
                {
                    sidechain.record(&message);
                }
                output_messages.push(message);
            }
            // CC claude.ts:2229-2248: final usage/stop_reason mutate the last
            // yielded per-block assistant through its shared reference; the
            // fork's accumulated copy converges here, and so does the sidechain
            // JSONL row — `forkedAgent.ts:588` queued that same object, so the
            // lazy stringify sees the finalised values (`claude.ts:2235-2243`).
            Ok(QueryEvent::AssistantDelta {
                uuid,
                stop_reason,
                usage,
            }) => {
                if let Some(crate::types::message::Message::Assistant(assistant)) = output_messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.uuid() == uuid.as_str())
                {
                    assistant.stop_reason = stop_reason.clone();
                    assistant.usage = usage.clone();
                }
                if let Some(sidechain) = sidechain.as_mut() {
                    sidechain.apply_assistant_delta(&uuid, stop_reason, usage);
                }
            }
            Ok(QueryEvent::PermissionRequest(request)) => {
                // A caller-owned `canUseTool` callback should resolve to allow
                // or deny before this actor boundary. Ask cannot open the
                // parent REPL from an isolated fork, so fail closed.
                let _ = handle
                    .commands
                    .send(QueryCommand::PermissionResponse {
                        tool_use_id: request.tool_use_id,
                        response: PermissionPromptResponse::new(PermissionPromptChoice::Deny),
                    })
                    .await;
            }
            Ok(QueryEvent::Terminal(_)) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    // The stream ended; a record still parked belongs on disk. CC's write
    // queue would have drained it on its own 100 ms timer.
    if let Some(sidechain) = sidechain.as_mut() {
        sidechain.flush();
    }

    // CC logs `tengu_fork_agent_query` here. Telemetry remains intentionally
    // out of scope; all behavior-bearing metrics are still returned to callers.
    Ok(ForkedAgentResult {
        messages: output_messages,
        total_usage,
        api_errors,
        request_ids,
        progress,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolPermissionContext;

    /// @nyan: the cache-sharing fork list is closed (README §1.8). Every
    /// member is a fork that replays the main thread's prefix; the two agent
    /// sources, host services and the main thread itself are not members.
    #[test]
    fn cache_sharing_forks_are_exactly_the_documented_set() {
        for source in [
            QuerySource::Compact,
            QuerySource::SideQuestion,
            QuerySource::PromptSuggestion,
            QuerySource::ExtractMemories,
            QuerySource::AutoDream,
            QuerySource::CacheWarm,
        ] {
            assert!(
                is_cache_sharing_fork(&source),
                "{source:?} shares the cache"
            );
            assert!(is_cache_sharing_api_source(source.as_api_source()));
        }
        for source in [
            QuerySource::Prompt,
            QuerySource::Sdk,
            QuerySource::Agent,
            QuerySource::agent_builtin("fork"),
            QuerySource::AgentDefault,
            QuerySource::AgentCustom,
            QuerySource::AgentSummary,
            QuerySource::HookAgent,
            QuerySource::SessionMemory,
            QuerySource::MarbleOrigami,
            QuerySource::Unknown,
        ] {
            assert!(
                !is_cache_sharing_fork(&source),
                "{source:?} must not inherit the cache-sharing rules"
            );
            assert!(!is_cache_sharing_api_source(source.as_api_source()));
        }
    }

    #[test]
    fn async_subagent_set_app_state_is_noop_but_tasks_reach_root() {
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.verbose = false;
        let store = crate::state::store::AppStore::new(initial, None);
        let parent = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_app_store(store.clone());

        let child = create_subagent_context(
            &parent,
            SubagentContextOverrides {
                share_set_app_state: false,
                avoid_permission_prompts_overlay: Some(true),
                agent_id: Some("agent-1".into()),
                ..Default::default()
            },
        );

        assert!(!child.app_store.writable);
        assert_eq!(child.agent_id.as_deref(), Some("agent-1"));
        child.set_app_state(|state| {
            state.verbose = true;
        });
        assert!(
            !store.get().verbose,
            "async setAppState must not write root"
        );

        child.set_app_state_for_tasks(|state| {
            state.foregrounded_task_id = Some("task-1".into());
        });
        assert_eq!(
            store.get().foregrounded_task_id.as_deref(),
            Some("task-1"),
            "setAppStateForTasks must still reach root"
        );

        let viewed = child.get_app_state().expect("store attached");
        assert!(
            viewed
                .tool_permission_context
                .should_avoid_permission_prompts
        );
        assert!(
            !store
                .get()
                .tool_permission_context
                .should_avoid_permission_prompts,
            "overlay must not mutate parent store"
        );
        assert!(
            child.local_denial_tracking.is_some(),
            "isolated setAppState gets fresh localDenialTracking"
        );
        assert!(child.in_progress_tool_use_ids.is_empty());
        assert!(child.it2_setup_prompt_sink.0.is_none());
        assert!(child.channel_permission_callbacks.is_none());
        // Maps to CC `forkedAgent.ts:362-374` on the object the permission gate
        // reads: an isolated fork must not be able to raise the parent's dialog
        // through an Agent call it makes.
        assert!(
            child
                .tool_permission_context
                .should_avoid_permission_prompts
        );
    }

    #[test]
    fn interactive_subagent_keeps_prompts_available_like_official() {
        // Maps to CC `forkedAgent.ts:358-361`: `shareAbortController` means "an
        // interactive agent that CAN show UI", so the wrapper is not applied and
        // the flag stays whatever the parent had.
        let parent = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let child = create_subagent_context(
            &parent,
            SubagentContextOverrides {
                share_abort_controller: true,
                ..Default::default()
            },
        );

        assert!(
            !child
                .tool_permission_context
                .should_avoid_permission_prompts
        );
    }

    #[test]
    fn sync_subagent_shares_set_app_state() {
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.verbose = false;
        let store = crate::state::store::AppStore::new(initial, None);
        let parent = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_app_store(store.clone());

        let child = create_subagent_context(
            &parent,
            SubagentContextOverrides {
                share_set_app_state: true,
                share_abort_controller: true,
                ..Default::default()
            },
        );
        child.set_app_state(|state| {
            state.verbose = true;
        });
        assert!(store.get().verbose);
        assert!(
            !child.app_store.avoid_permission_prompts_overlay,
            "sharing abort implies interactive prompts allowed by default"
        );
    }

    #[test]
    fn default_subagent_gets_child_abort_linked_to_parent() {
        let parent = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let child = create_subagent_context(&parent, SubagentContextOverrides::default());

        assert!(!child.abort_controller.is_aborted());
        parent.abort_controller.abort();
        assert!(
            child.abort_controller.is_aborted(),
            "parent abort must propagate to child-linked controller"
        );
        assert!(child.app_store.avoid_permission_prompts_overlay);
        assert!(child.local_denial_tracking.is_some());
    }

    #[test]
    fn explicit_unlinked_abort_does_not_observe_parent() {
        let parent = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let child = create_subagent_context(
            &parent,
            SubagentContextOverrides {
                abort_controller: Some(AbortController::default()),
                avoid_permission_prompts_overlay: Some(true),
                ..Default::default()
            },
        );

        parent.abort_controller.abort();
        assert!(
            !child.abort_controller.is_aborted(),
            "async AgentTool unlinked abort must not observe parent"
        );
    }

    fn read_entry(path: &str, content: &str) -> crate::utils::query_helpers::ReadFileStateEntry {
        crate::utils::query_helpers::ReadFileStateEntry {
            path: path.to_string(),
            content: Some(content.to_string()),
            timestamp_ms: Some(1),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: crate::utils::query_helpers::ReadFileStateSource::Read,
        }
    }

    /// Maps to CC `utils/forkedAgent.ts:377-381` `readFileState:
    /// cloneFileStateCache(overrides?.readFileState ??
    /// parentContext.readFileState)` — the `??` arm nobody overrides.
    ///
    /// Old shape: passed. This is the regression guard for the assignment
    /// moving up next to the CC line it maps to; every non-AgentTool caller
    /// (`sessionMemory.ts:303`/`:398`, `forkedAgent.ts:515`) still depends on
    /// inheriting the parent's reads.
    #[test]
    fn subagent_without_a_read_file_state_override_clones_the_parents_cache() {
        let parent = ToolUseContext::default();
        parent
            .read_file_state
            .set_entry(read_entry("/tmp/parent.txt", "parent"));

        let child = create_subagent_context(&parent, SubagentContextOverrides::default());

        assert!(
            child
                .read_file_state
                .has(std::path::Path::new("/tmp/parent.txt"))
        );
        // A clone, never the parent's handle: `get` promotes to MRU and `set`
        // can evict, so a shared `Arc` would let the child rewrite the parent's
        // LRU order.
        assert!(!child.read_file_state.same_identity(&parent.read_file_state));
        child
            .read_file_state
            .set_entry(read_entry("/tmp/child.txt", "child"));
        assert!(
            !parent
                .read_file_state
                .has(std::path::Path::new("/tmp/child.txt"))
        );
    }

    /// Maps to CC `utils/forkedAgent.ts:377-381`, override arm. Two things are
    /// pinned here that the entry-list carrier could not express:
    ///
    /// 1. `??` is nullish, not truthy, so an EMPTY override cache still beats a
    ///    populated parent — the whole point of `runAgent.ts:375-378`'s
    ///    non-fork branch.
    /// 2. `cloneFileStateCache` rebuilds with `cache.max, cache.maxSize`
    ///    (`fileStateCache.ts:122-126`), so the override's OWN limits reach the
    ///    child.
    ///
    /// Old shape: the override was `Option<Vec<ReadFileStateEntry>>` fed to
    /// `SharedFileStateCache::from_entries`, which builds a
    /// `FileStateCache::default()` — the `max_entries == 1` assertion read 100
    /// and the eviction assertion never fired.
    #[test]
    fn subagent_read_file_state_override_wins_and_keeps_its_own_size_limits() {
        let parent = ToolUseContext::default();
        parent
            .read_file_state
            .set_entry(read_entry("/tmp/parent.txt", "parent"));

        let override_cache = crate::tool::SharedFileStateCache::from_snapshot(
            crate::utils::file_state_cache::FileStateCacheSnapshot {
                max_entries: 1,
                max_size_bytes: 4096,
                entries_lru_to_mru: Vec::new(),
            },
        );
        let child = create_subagent_context(
            &parent,
            SubagentContextOverrides {
                read_file_state: Some(override_cache.clone()),
                ..Default::default()
            },
        );

        assert!(
            child.read_file_state.is_empty(),
            "empty override wins over a populated parent"
        );
        let snapshot = child.read_file_state.cache_snapshot();
        assert_eq!(snapshot.max_entries, 1);
        assert_eq!(snapshot.max_size_bytes, 4096);
        // The limits are live, not decorative.
        child
            .read_file_state
            .set_entry(read_entry("/tmp/one.txt", "one"));
        child
            .read_file_state
            .set_entry(read_entry("/tmp/two.txt", "two"));
        assert_eq!(
            child.read_file_state.keys(),
            vec!["/tmp/two.txt".to_string()]
        );

        // CC clones the override too, so the caller's cache stays untouched.
        assert!(!child.read_file_state.same_identity(&override_cache));
        assert!(override_cache.is_empty());
    }

    #[test]
    fn content_replacement_override_and_clone_defaults() {
        let mut parent = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut parent_state = crate::utils::tool_result_storage::ContentReplacementState::new();
        parent_state.seen_ids.insert("parent-id".into());
        parent.content_replacement_state = Some(parent_state);

        let cloned = create_subagent_context(&parent, SubagentContextOverrides::default());
        assert_eq!(
            cloned
                .content_replacement_state
                .as_ref()
                .map(|s| s.seen_ids.contains("parent-id")),
            Some(true)
        );

        let override_state = crate::utils::tool_result_storage::ContentReplacementState::new();
        let overridden = create_subagent_context(
            &parent,
            SubagentContextOverrides {
                content_replacement_state: Some(override_state.clone()),
                ..Default::default()
            },
        );
        assert_eq!(overridden.content_replacement_state, Some(override_state));
    }

    #[test]
    fn create_subagent_context_matches_official_full_override_contract() {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

        let response_total = std::sync::Arc::new(AtomicUsize::new(0));
        let metrics_total = std::sync::Arc::new(AtomicU64::new(0));
        let response_for_sink = std::sync::Arc::clone(&response_total);
        let metrics_for_sink = std::sync::Arc::clone(&metrics_total);
        let mut parent = ToolUseContext::default()
            .with_response_length_sink(crate::tool::ResponseLengthSink::new(move |delta| {
                response_for_sink.fetch_add(delta, Ordering::SeqCst);
            }))
            .with_api_metrics_sink(crate::tool::ApiMetricsSink::new(move |ttft| {
                metrics_for_sink.fetch_add(ttft, Ordering::SeqCst);
            }));
        parent
            .nested_memory_attachment_triggers
            .as_ref()
            .unwrap()
            .add("nested".into());
        parent.loaded_nested_memory_paths.insert("loaded".into());
        parent
            .dynamic_skill_dir_triggers
            .as_ref()
            .unwrap()
            .add("skill-dir".into());
        parent.discovered_skill_names.insert("skill".into());
        parent.critical_system_reminder_experimental = Some("parent reminder".into());

        let mut options = parent.options();
        options.debug = true;
        options.verbose = true;
        options.main_loop_model = Some("override-model".into());
        let override_messages = vec![crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text("override".into())],
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
        )];
        let projected_state =
            std::sync::Arc::new(crate::state::app_state_store::AppState::default());
        let projected_for_callback = std::sync::Arc::clone(&projected_state);
        let child = create_subagent_context(
            &parent,
            SubagentContextOverrides {
                options: Some(options),
                agent_id: Some("agent-id".into()),
                agent_type: Some("reviewer".into()),
                messages: Some(override_messages.clone()),
                get_app_state: Some(crate::tool::GetAppStateCallback::new(move || {
                    Some(std::sync::Arc::clone(&projected_for_callback))
                })),
                share_set_response_length: true,
                require_can_use_tool: Some(true),
                ..Default::default()
            },
        );

        assert_eq!(child.main_loop_model.as_deref(), Some("override-model"));
        assert!(child.debug && child.verbose);
        assert_eq!(child.agent_id.as_deref(), Some("agent-id"));
        assert_eq!(child.agent_type.as_deref(), Some("reviewer"));
        assert_eq!(child.messages, override_messages);
        assert!(child.require_can_use_tool);
        assert!(!child.app_store.avoid_permission_prompts_overlay);
        assert!(std::sync::Arc::ptr_eq(
            &child.get_app_state().expect("custom state"),
            &projected_state
        ));
        assert!(
            child
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .is_empty()
        );
        assert!(child.loaded_nested_memory_paths.is_empty());
        assert!(
            child
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .is_empty()
        );
        assert!(child.discovered_skill_names.is_empty());
        assert!(child.critical_system_reminder_experimental.is_none());
        child.response_length_sink.add(9);
        child.api_metrics_sink.push(4);
        assert_eq!(response_total.load(Ordering::SeqCst), 9);
        assert_eq!(metrics_total.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn create_get_app_state_with_allowed_tools_matches_official_command_scope() {
        use crate::types::permissions::PermissionRuleSource;

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let parent = ToolUseContext::default().with_app_store(store.clone());
        let projected = parent
            .clone()
            .with_get_app_state_override(create_get_app_state_with_allowed_tools(
                &parent,
                &["Read".to_string()],
            ))
            .get_app_state()
            .expect("projected app state");
        assert!(
            projected
                .tool_permission_context
                .always_allow_rules
                .get(&PermissionRuleSource::Command)
                .is_some_and(|rules| rules.iter().any(|rule| rule.tool_name == "Read"))
        );
        assert!(
            store
                .get()
                .tool_permission_context
                .always_allow_rules
                .get(&PermissionRuleSource::Command)
                .is_none()
        );
    }

    #[test]
    fn async_can_use_tool_callback_matches_official_updated_input_contract() {
        let callback = crate::tool::CanUseToolCallback::new_async(
            |_tool, input, _context, _assistant, _tool_use_id, _force| {
                let mut updated = input.clone();
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    updated["file_path"] = serde_json::Value::String("/overlay/file".into());
                    crate::types::permissions::PermissionDecision::Allow {
                        updated_input: Some(updated),
                        user_modified: None,
                        decision_reason: None,
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    }
                })
            },
        );
        let tool = crate::tools::file_read_tool::file_read_tool_schema();
        let assistant = crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let decision = callback
            .decide(
                &tool,
                &serde_json::json!({"file_path":"/original/file"}),
                &ToolUseContext::default(),
                &assistant,
                "toolu-1",
                None,
            )
            .expect("permission callback should not abort")
            .expect("async callback decision");
        let crate::types::permissions::PermissionDecision::Allow { updated_input, .. } = decision
        else {
            panic!("async callback must return the allow decision");
        };
        assert_eq!(
            updated_input
                .as_ref()
                .and_then(|input| input.get("file_path"))
                .and_then(serde_json::Value::as_str),
            Some("/overlay/file")
        );
    }

    #[test]
    fn extract_result_text_matches_official_last_assistant_only() {
        let messages = vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "stale text".into(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu-last".into()),
                        name: "Read".into(),
                        input: serde_json::json!({}),
                    },
                )],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
        ];
        assert_eq!(extract_result_text(&messages, "default"), "default");
    }

    #[derive(Clone)]
    struct ForkTransportDeps {
        request: std::sync::Arc<std::sync::Mutex<Option<crate::query::deps::CallModelRequest>>>,
    }

    impl crate::query::deps::QueryDeps for ForkTransportDeps {
        fn call_model(
            &self,
            request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            *self.request.lock().unwrap() = Some(request);
            Box::pin(async move {
                let (tx, rx) = tokio::sync::mpsc::channel(8);
                tx.send(crate::services::api::request::QueryModelStreamItem::Stream(
                    crate::types::message::StreamEvent::ApiEvent {
                        event: serde_json::json!({
                            "type": "message_delta",
                            "usage": {
                                "input_tokens": 11,
                                "cache_creation_input_tokens": 2,
                                "cache_read_input_tokens": 3,
                                "output_tokens": 7
                            }
                        }),
                        ttft_ms: Some(4),
                    },
                ))
                .await
                .unwrap();
                tx.send(
                    crate::services::api::request::QueryModelStreamItem::Assistant(
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::Text(
                                "side answer".to_string(),
                            )],
                            model: Some("current-model".to_string()),
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

    fn deny_callback() -> crate::tool::CanUseToolCallback {
        crate::tool::CanUseToolCallback::new(
            |_tool, _input, _context, _assistant, _tool_use_id, _force| {
                crate::types::permissions::PermissionDecision::Deny {
                    message: "Side questions cannot use tools".to_string(),
                    decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                        reason: "side_question".to_string(),
                    },
                    tool_use_id: None,
                }
            },
        )
    }

    #[test]
    fn run_forked_agent_matches_official_usage_and_skip_cache_write_transport() {
        let request = std::sync::Arc::new(std::sync::Mutex::new(None));
        let deps = ForkTransportDeps {
            request: std::sync::Arc::clone(&request),
        };
        let mut context = ToolUseContext::default().with_main_loop_model("current-model");
        context.thinking_config = Some(crate::utils::thinking::ThinkingConfig::Disabled);
        let cache = CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: [("user".to_string(), "context".to_string())]
                .into_iter()
                .collect(),
            system_context: [("system".to_string(), "context".to_string())]
                .into_iter()
                .collect(),
            tool_use_context: context,
            fork_context_messages: std::sync::Arc::new(Vec::new()),
        };
        let prompt_messages = vec![crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "question".to_string(),
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
        )];

        let result = futures::executor::block_on(run_forked_agent_with_deps(
            ForkedAgentParams {
                prompt_messages,
                cache_safe_params: cache,
                can_use_tool: deny_callback(),
                query_source: crate::constants::query_source::QuerySource::SideQuestion,
                fork_label: "side_question".to_string(),
                overrides: None,
                max_output_tokens: Some(1_234),
                max_turns: Some(1),
                on_message: None,
                on_progress: None,
                skip_cache_write: true,
                skip_transcript: true,
            },
            deps,
        ))
        .unwrap();

        let request = request.lock().unwrap().clone().expect("model request");
        assert_eq!(request.options.skip_cache_write, Some(true));
        assert_eq!(request.options.max_output_tokens_override, Some(1_234));
        assert_eq!(
            request.query_source,
            crate::constants::query_source::QuerySource::SideQuestion
        );
        assert_eq!(result.total_usage.input_tokens, 11);
        assert_eq!(result.total_usage.cache_creation_input_tokens, 2);
        assert_eq!(result.total_usage.cache_read_input_tokens, 3);
        assert_eq!(result.total_usage.output_tokens, 7);
        assert!(result.messages.iter().any(|message| matches!(
            message,
            crate::types::message::Message::Assistant(assistant)
                if assistant.content.iter().any(|block| matches!(
                    block,
                    crate::types::message::AssistantContent::Text(text) if text == "side answer"
                ))
        )));
    }
}
