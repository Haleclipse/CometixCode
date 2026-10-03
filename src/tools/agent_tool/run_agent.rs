//! In-process subagent execution.
//!
//! Maps to: CC `tools/AgentTool/runAgent.ts`.
//!
//! Current parity slice: foreground, synchronous subagents execute through the
//! production query loop (model streaming + local tool-use continuations) and
//! return the official completed-output shape. SubagentStart/Stop hooks and
//! agent frontmatter MCP server initialization are wired on the official
//! runAgent seam. The full official loop features still pending here are
//! background task files, live sidechain transcript persistence,
//! remote isolation, and full teammate swarms.

use super::agent_tool_utils::{
    CompletedAgentRun, finalize_agent_tool_result_from_messages, resolve_agent_tools,
};
use super::load_agents_dir::{AgentDefinition, AgentMcpServerSpec};
use crate::constants::query_source::QuerySource;
use crate::services::api::claude::SystemPrompt;
use crate::tool::{ToolCallProgressFn, ToolUseContext};
use crate::types::message::{AssistantContent, Message, TokenUsage, UserContent};
use crate::types::permissions::{
    PermissionBehavior, PermissionMode, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest,
};
use crate::types::tools::Tool;
use crate::utils::model::agent::get_agent_model;
use crate::utils::thinking::ThinkingConfig;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

pub type AgentMessageCallbackFn<'a> = &'a (dyn Fn(&Message) + Send + Sync);

/// Maps to CC `runAgent.ts:281-287`:
///
/// ```ts
/// override?: {
///   userContext?: { [k: string]: string }
///   systemContext?: { [k: string]: string }
///   systemPrompt?: SystemPrompt
///   abortController?: AbortController
///   agentId?: AgentId
/// }
/// ```
///
/// One grouped optional object in CC; mirrored as one struct whose members are
/// all `Option` (the whole-object `undefined` and the all-members-absent object
/// are indistinguishable at every consumer). Consumers: `:347` (`agentId`),
/// `:381-382` (`userContext`/`systemContext` with `?? getUserContext()` /
/// `?? getSystemContext()` fallbacks), `:508-509` (`systemPrompt`), `:524-525`
/// (`abortController`).
#[derive(Clone, Default)]
pub struct RunAgentOverride<'a> {
    /// Maps to CC `override.userContext` (`runAgent.ts:282`, read at `:381`).
    /// CC `:388-392`: "Explicit override.userContext from callers is preserved
    /// untouched" — the omitClaudeMd slimming never applies to it.
    pub user_context: Option<std::collections::BTreeMap<String, String>>,
    /// Maps to CC `override.systemContext` (`runAgent.ts:283`, read at `:382`).
    pub system_context: Option<std::collections::BTreeMap<String, String>>,
    /// Maps to CC `override.systemPrompt` (`runAgent.ts:284`, read at
    /// `:508-509`) — fork resume passes the parent's rendered prompt here so
    /// the child request prefix stays cache-identical.
    pub system_prompt: Option<SystemPrompt>,
    /// Maps to CC `override.abortController` (`runAgent.ts:285`) — the
    /// controller the agent actually runs under, chosen FIRST at `:524-528`
    /// (`override?.abortController ? … : isAsync ? new AbortController() :
    /// toolUseContext.abortController`). Every CC async caller supplies it
    /// (AgentTool.tsx:1003/:1010/:1245, resumeAgent.ts:234/:241,
    /// inProcessRunner.ts:1197 `currentWorkAbortController`), so the
    /// unlinked-controller arm is effectively dead in CC: an async agent still
    /// stops when its owner's controller aborts. `None` here selects the same
    /// fallback CC would.
    pub abort_controller: Option<crate::tool::AbortController>,
    /// Maps to CC `override.agentId` (`runAgent.ts:286`, read at `:347`), used
    /// by LocalAgentTask so the registered task id and sidechain agent id are
    /// identical.
    pub agent_id: Option<&'a str>,
}

#[derive(Clone)]
pub struct RunAgentInput<'a> {
    pub agent_definition: &'a AgentDefinition,
    pub prompt: &'a str,
    pub description: Option<&'a str>,
    pub model_override: Option<&'a str>,
    pub context: &'a ToolUseContext,
    /// Maps to: CC `runAgent.ts:280` required caller-selected `querySource`.
    /// Used unchanged by query and by exact-tool fork child options.
    pub query_source: QuerySource,
    /// Maps to CC `runAgent.ts:275` `isAsync: boolean` — an EXPLICIT required
    /// parameter set by each caller (AgentTool sync path false, background
    /// spawn/backgrounded continuation/resume true, inProcessRunner true,
    /// SkillTool false). Never derived from background fields.
    pub is_async: bool,
    /// Maps to CC `runAgent.ts:276-278` `canShowPermissionPrompts?: boolean` —
    /// "Defaults to !isAsync. Set to true for in-process teammates that run
    /// async but share the terminal." Only inProcessRunner passes it
    /// (`allowPermissionPrompts ?? true`).
    pub can_show_permission_prompts: Option<bool>,
    /// Maps to CC `runAgent.ts:292-296` `availableTools: Tools` — in CC a
    /// REQUIRED caller-computed pool ("Always contains the full tool pool
    /// assembled with the worker's own permission mode"; computed in
    /// AgentTool.tsx to break a runAgent↔tools.ts import cycle). Rust has no
    /// cycle, so `None` keeps the equivalent internal workerTools computation
    /// for the AgentTool paths; callers whose CC counterpart passes a
    /// DIFFERENT pool (inProcessRunner: leader's `toolUseContext.options.tools`;
    /// SkillTool: parent `context.options.tools`) must pass `Some`.
    pub available_tools: Option<Vec<Tool>>,
    /// Maps to CC `runAgent.ts:255`/`:279` `forkContextMessages?: Message[]` —
    /// the PARENT conversation a fork child inherits, produced only by the fork
    /// spawn path (`AgentTool.tsx:907` `isForkPath ? toolUseContext.messages :
    /// undefined`).
    ///
    /// Two consumers at `runAgent.ts:370-378`, and they read the carrier
    /// DIFFERENTLY:
    ///
    /// 1. `:370-373` — `forkContextMessages ? filterIncompleteToolCalls(...) :
    ///    []` prefixed onto `promptMessages` to form `initialMessages`. JS
    ///    truthiness on an array means `[]` is truthy, so an empty carrier still
    ///    takes the filter branch; the filter of an empty slice is empty, so
    ///    `Option` matching is equivalent here.
    /// 2. `:375-378` — `forkContextMessages !== undefined` (an explicit
    ///    `undefined` check, NOT truthiness) picks
    ///    `cloneFileStateCache(toolUseContext.readFileState)` over a fresh
    ///    size-limited cache; see [`agent_read_file_state`].
    ///
    /// `resumeAgent.ts:187-189` pins this to `undefined` on purpose ("Transcript
    /// already contains the parent context slice from the original fork.
    /// Re-supplying it would cause duplicate tool_use IDs").
    pub fork_context_messages: Option<Vec<Message>>,
    /// Maps to CC `runAgent.ts:290-291` `preserveToolUseResults?: boolean`,
    /// applied AFTER `createSubagentContext` (runAgent.ts:716-719).
    pub preserve_tool_use_results: bool,
    /// Maps to CC `runAgent.ts:257`/`:281-287` `override` — see
    /// [`RunAgentOverride`].
    pub r#override: RunAgentOverride<'a>,
    /// Maps to CC `runAgent.ts:265`/`:309-314` `useExactTools?: boolean` —
    /// "When true, use availableTools directly without filtering through
    /// resolveAgentTools(). Also inherits the parent's thinkingConfig and
    /// isNonInteractiveSession instead of overriding them. Used by the fork
    /// subagent path to produce byte-identical API request prefixes for
    /// prompt cache hits." Consumed at `:500` (skip resolveAgentTools), `:668`
    /// (isNonInteractiveSession passthrough), `:679-684` (thinking config
    /// inheritance), and `:688-694` (querySource on child options).
    pub use_exact_tools: bool,
    /// Maps to CC `runAgent.ts:323` `transcriptSubdir?: string` — registers a
    /// transcript grouping subdirectory for this agent (runAgent.ts:351-353),
    /// released in the finally block. No CC 2.1.88 caller passes it; every
    /// Rust caller passes `None` until one does.
    pub transcript_subdir: Option<&'a str>,
    /// Maps to CC `tools/AgentTool/runAgent.ts` `allowedTools`: when present, the child keeps
    /// only the parent's CLI allow bucket plus these session rules.
    pub allowed_tools: Option<&'a [String]>,
    /// Maps to CC `tools/AgentTool/runAgent.ts` `worktreePath`: metadata records the isolated
    /// worktree path separately from generic cwd overrides.
    pub worktree_path: Option<&'a str>,
    // #156: the former `can_use_tool`/`parent_message` params are DELETED, not
    // moved. The two cases are different and both matter:
    //
    // `canUseTool` EXISTS in CC (`runAgent.ts:252`/`:274`); its only consumer
    // is `:753` handing it into `query()`, which threads it to the child's own
    // tool execution (`toolExecution.ts:926` via
    // `resolveHookPermissionDecision`). That parameter shape is UNCARRIABLE
    // here: the query actor is detached (PORTING.md § "Node-async → tokio"
    // A2), and a borrow-typed `CanUseToolFn<'a>` cannot cross into it — the
    // forced carrier (A2-derived) is the owned `ToolUseContext.can_use_tool`
    // field, which `create_subagent_context`'s clone brings to the SAME
    // decide sites, plus the inherited `interactive_permission_sink` dialog
    // leg that `resolve_agent_permission_request` raises. The deleted field
    // was a redundant second pipe for the same callback, never read (#141:
    // reading it was the second evaluation) — the CC capability lives on, its
    // carrier moved. `AgentTool.call`/`SkillTool` keep `_`-prefixed params so
    // the CC call shape survives at the boundary CC declares it.
    //
    // `parentMessage` does NOT exist in CC's `runAgent` signature (its old
    // note pointed at `checkPermissionsAndCallTool`'s deep argument — a
    // different function); it was an invented field, deleted as such.
    /// Maps to: CC `ToolUseContext.toolUseId` for the parent Agent tool-use row
    /// receiving subagent progress messages.
    pub parent_tool_use_id: Option<&'a str>,
    /// Maps to: CC `Tool.call(...)` `onProgress` callback used by
    /// `AgentTool.tsx` to forward foreground subagent progress.
    pub on_progress: Option<ToolCallProgressFn<'a>>,
    /// Maps to CC `AgentTool.tsx#runAsyncAgentLifecycle` progress tracker
    /// updates for background LocalAgentTask state.
    pub background_task_id: Option<&'a str>,
    /// Maps to CC `runAgent.ts` `contentReplacementState`: callers can pass
    /// a reconstructed or sustained state so query re-applies large tool-result
    /// replacements byte-identically across resumed/subsequent agent turns.
    pub content_replacement_state:
        Option<crate::utils::tool_result_storage::ContentReplacementState>,
    /// Maps to CC `AgentTool.tsx` `backgroundPromise` race against
    /// `agentIterator.next()`: foreground AgentTool callers pass the task's
    /// background signal so runAgent can stop the active query and hand control
    /// back for background continuation.
    pub background_signal: Option<tokio::sync::watch::Receiver<bool>>,
    /// Maps to CC `runAgent.ts` async iterator yield: callers such as
    /// `utils/swarm/inProcessRunner.ts` observe each generated message as soon
    /// as query emits it, before the completed aggregate is returned.
    pub on_message: Option<AgentMessageCallbackFn<'a>>,
    /// Maps to CC `runAgent.ts` `promptMessages`, used by resumed agents to
    /// continue from a sidechain transcript plus the new SendMessage prompt.
    pub prompt_messages: Option<Vec<Message>>,
}

/// Maps to CC `AgentTool.tsx` foreground `backgroundSignal` branch: the
/// current foreground iterator is stopped and the already-yielded messages are
/// carried into the background continuation owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundedAgentRun {
    pub agent_id: String,
    pub agent_type: String,
    pub messages: Vec<Message>,
    pub content_replacement_state:
        Option<crate::utils::tool_result_storage::ContentReplacementState>,
    pub elapsed_ms: u64,
}

/// Rust return boundary for CC `runAgent.ts` async iteration consumed by
/// `AgentTool.tsx`: either the iterator reaches completion, or the foreground
/// `backgroundSignal` wins the race and ownership transfers to a background
/// task continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunAgentOutcome {
    Completed(CompletedAgentRun),
    Backgrounded(BackgroundedAgentRun),
}

/// Maps to: CC's `AbortError` thrown out of the agent loop. The catch
/// discriminates `error instanceof AbortError` (`agentToolUtils.ts:641`,
/// `AgentTool.tsx:1339`) and reads the closed-over `agentMessages` for
/// `extractPartialResult(agentMessages)` (`:658`/`:1356`) — the killed
/// notification's `finalMessage`. Rust has no closure over the loop's locals,
/// so the error itself carries what the agent accomplished.
///
/// `content_replacement_state` rides along for the same reason, one caller
/// further out. CC's `contentReplacementState` is ONE object the caller owns
/// and `enforceToolResultBudget` mutates in place —
/// `toolResultStorage.ts:759-762`: "@param state — MUTATED: seenIds and
/// replacements are updated in place to record choices made this call. The
/// caller holds a stable reference across turns; returning a new object would
/// require error-prone ref updates after every query." So when
/// `inProcessRunner.ts:1213-1219` breaks the turn on Escape, the runner's
/// `let teammateReplacementState` (`:1043`, reassigned only by the compaction
/// reset at `:1111-1113`) still points at the object the aborted turn wrote
/// into: an aborted CC turn KEEPS its replacement decisions. This port has no
/// shared mutable object — `query.rs:916-926` hands each new state out as a
/// `QueryEvent::ContentReplacementStateUpdate` and `run_agent` rebinds a local
/// — so the abort return is where that local has to be handed back, exactly as
/// `CompletedAgentRun::content_replacement_state` and
/// [`BackgroundedAgentRun::content_replacement_state`] already do on the two
/// non-abort exits.
#[derive(Debug)]
pub struct AgentExecutionAborted {
    pub agent_messages: Vec<crate::types::message::Message>,
    pub content_replacement_state:
        Option<crate::utils::tool_result_storage::ContentReplacementState>,
}

impl std::fmt::Display for AgentExecutionAborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Agent execution aborted")
    }
}

impl std::error::Error for AgentExecutionAborted {}

/// Maps to CC `utils/uuid.ts#createAgentId`.
pub fn create_agent_id(label: Option<&str>) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..16];
    match label.filter(|value| !value.trim().is_empty()) {
        Some(label) => format!("a{label}-{suffix}"),
        None => format!("a{suffix}"),
    }
}

/// Maps to CC `runAgent.ts` `contentReplacementState` parameter and
/// `createSubagentContext(...)` fallback to the parent's state when omitted.
fn initial_agent_content_replacement_state(
    input: &RunAgentInput<'_>,
) -> Option<crate::utils::tool_result_storage::ContentReplacementState> {
    input
        .content_replacement_state
        .clone()
        .or_else(|| input.context.content_replacement_state.clone())
}

#[derive(Clone, Debug, Default)]
struct AgentMcpInitialization {
    mcp_state: crate::state::app_state_store::McpState,
    tools: Vec<Tool>,
    cleanup_server_names: Vec<String>,
}

/// Maps to: CC `runAgent.ts#initializeAgentMcpServers`.
async fn initialize_agent_mcp_servers(
    agent_definition: &AgentDefinition,
    parent_state: &crate::state::app_state_store::McpState,
) -> AgentMcpInitialization {
    let Some(specs) = agent_definition.mcp_servers.as_ref() else {
        return AgentMcpInitialization {
            mcp_state: parent_state.clone(),
            tools: Vec::new(),
            cleanup_server_names: Vec::new(),
        };
    };
    if specs.is_empty() {
        return AgentMcpInitialization {
            mcp_state: parent_state.clone(),
            tools: Vec::new(),
            cleanup_server_names: Vec::new(),
        };
    }

    // Maps to: CC `isRestrictedToPluginOnly('mcp') && !isSourceAdminTrusted(...)`.
    if crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("mcp")
        && !crate::utils::settings::plugin_only_policy::is_source_admin_trusted(
            agent_definition.source.official_name(),
        )
    {
        tracing::debug!(
            agent_type = %agent_definition.agent_type,
            source = %agent_definition.source.official_name(),
            "skipping user-controlled agent MCP servers due to strict plugin-only MCP policy"
        );
        return AgentMcpInitialization {
            mcp_state: parent_state.clone(),
            tools: Vec::new(),
            cleanup_server_names: Vec::new(),
        };
    }

    let mut state = parent_state.clone();
    let mut agent_only_state = crate::state::app_state_store::McpState::default();
    let mut cleanup_server_names = Vec::new();

    for spec in specs {
        let (name, config, cleanup_after_agent) = match spec {
            AgentMcpServerSpec::Reference(name) => {
                let Some(config) =
                    crate::services::mcp::config::get_mcp_config_by_name_readonly(name)
                else {
                    tracing::warn!(
                        agent_type = %agent_definition.agent_type,
                        server = %name,
                        "agent MCP server reference was not found"
                    );
                    continue;
                };
                (name.clone(), config, false)
            }
            AgentMcpServerSpec::Inline { name, config } => (name.clone(), config.clone(), true),
        };

        let discovery = crate::services::mcp::client::connect_to_server(&name, &config).await;
        if cleanup_after_agent {
            cleanup_server_names.push(name.clone());
        }
        upsert_mcp_server_snapshot(&mut state, discovery.server.clone());
        upsert_mcp_server_snapshot(&mut agent_only_state, discovery.server);
    }

    let tools = agent_only_state.tools.clone();
    AgentMcpInitialization {
        mcp_state: state,
        tools,
        cleanup_server_names,
    }
}

/// Maps to: CC `runAgent.ts#initializeAgentMcpServers` cleanup function for
/// inline dynamic MCP servers only.
async fn cleanup_agent_mcp_servers(server_names: &[String]) {
    for server_name in server_names {
        crate::services::mcp::client::clear_server_cache(server_name, None).await;
    }
}

fn upsert_mcp_server_snapshot(
    state: &mut crate::state::app_state_store::McpState,
    server: crate::services::mcp::types::McpServerSnapshot,
) {
    crate::services::mcp::use_manage_mcp_connections::apply_mcp_server_update(state, server);
}

fn merge_agent_mcp_tools(mut resolved_tools: Vec<Tool>, agent_mcp_tools: Vec<Tool>) -> Vec<Tool> {
    // Maps to: CC `uniqBy([...resolvedTools, ...agentMcpTools], 'name')`.
    if agent_mcp_tools.is_empty() {
        return resolved_tools;
    }
    let mut seen = resolved_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<HashSet<_>>();
    for tool in agent_mcp_tools {
        if seen.insert(tool.name.clone()) {
            resolved_tools.push(tool);
        }
    }
    resolved_tools
}

/// Maps to: CC `runAgent.ts:524-528` — "Override takes precedence; async
/// agents get a new unlinked controller (runs independently); sync agents
/// share parent's controller".
fn select_agent_abort_controller(
    override_controller: Option<crate::tool::AbortController>,
    is_async: bool,
    parent: &crate::tool::AbortController,
) -> crate::tool::AbortController {
    match override_controller {
        Some(controller) => controller,
        None if is_async => crate::tool::AbortController::default(),
        None => parent.clone(),
    }
}

/// Maps to: CC `runAgent.ts` finally-block tail (:832-843): release the
/// transcript-subdir mapping (`clearAgentTranscriptSubdir`) and this agent's
/// todos entry (`delete prev.todos[agentId]` — without it every subagent that
/// called TodoWrite leaves a key in AppState.todos forever). The absent-key
/// guard matches CC's `if (!(agentId in prev.todos)) return prev`.
///
/// CC writes through `rootSetAppState` (`runAgent.ts:337-338`
/// `toolUseContext.setAppStateForTasks ?? toolUseContext.setAppState`), so
/// this uses the PARENT context's tasks-store-preferring setter rather than
/// its plain store: nested async agents get an isolated `setAppState`, and
/// `utils/forked_agent.rs` populates `tasks_store` precisely so task-shaped
/// writes still reach the root.
///
/// CC's `unregisterPerfettoAgent` / MONITOR_TOOL steps have no Rust surface
/// (profiling/monitoring rails are out of scope per the analytics ruling).
///
/// Runs on EVERY exit, matching CC — including the backgrounding handoff,
/// where CC drives the foreground iterator's finally explicitly
/// (`AgentTool.tsx:1217-1224`, "Clean up the foreground iterator so its
/// finally block runs") even though the continuation reuses the same id
/// (`:1205` `backgroundedTaskId = foregroundTaskId`). The continuation
/// re-registers what it still needs; skipping the handoff exits instead
/// leaked both registrations whenever `continue_backgrounded_agent` bailed
/// out before starting.
fn release_agent_registrations(agent_id: &str, root_context: &ToolUseContext) {
    crate::utils::session_storage::clear_agent_transcript_subdir(agent_id);
    if root_context
        .get_app_state()
        .is_some_and(|state| state.todos.contains_key(agent_id))
    {
        root_context.set_app_state_for_tasks(|state| {
            std::sync::Arc::make_mut(&mut state.todos).remove(agent_id);
        });
    }
}

/// Maps to: CC `runAgent.ts:436-445` `shouldAvoidPrompts` — the exact ternary
/// chain: explicit `canShowPermissionPrompts` wins when provided
/// (`!canShowPermissionPrompts`); otherwise `bubble` mode always shows prompts
/// (they surface on the parent terminal); otherwise the `isAsync` default
/// (sync agents show prompts, async agents don't).
fn agent_should_avoid_permission_prompts(
    can_show_permission_prompts: Option<bool>,
    agent_permission_mode: Option<PermissionMode>,
    is_async: bool,
) -> bool {
    if let Some(can_show) = can_show_permission_prompts {
        return !can_show;
    }
    if agent_permission_mode == Some(PermissionMode::Bubble) {
        return false;
    }
    is_async
}

/// Maps to: CC `runAgent.ts` `agentGetAppState` permission-mode override.
fn agent_permission_context_for_run(
    parent: &crate::tool::ToolPermissionContext,
    agent_permission_mode: Option<PermissionMode>,
    allowed_tools: Option<&[String]>,
) -> crate::tool::ToolPermissionContext {
    let mut context = parent.clone();
    // Maps to CC `runAgent.ts:421-434`: the RUNTIME context only overrides
    // when the agent DEFINES a permissionMode, and never against a parent in
    // `bypassPermissions`, `acceptEdits`, or (under TRANSCRIPT_CLASSIFIER)
    // `auto`. An agent without one keeps the parent mode — the
    // `?? 'acceptEdits'` fallback lives solely in AgentTool.tsx:838-841's
    // workerPermissionContext, which feeds assembleToolPool (the tool POOL),
    // never the runtime context.
    let parent_mode_wins = matches!(
        parent.mode,
        PermissionMode::BypassPermissions | PermissionMode::AcceptEdits
    ) || (parent.mode == PermissionMode::Auto
        && crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled());
    if !parent_mode_wins {
        if let Some(mode) = agent_permission_mode {
            context.mode = mode;
        }
    }
    // Maps to CC `tools/AgentTool/runAgent.ts:465-478`: this is replacement, not a merge.
    // Parent approvals from user/project/local/flag/policy/command/session must
    // not leak when an explicit child allowlist is supplied.
    if let Some(allowed_tools) = allowed_tools {
        let cli_rules = parent
            .always_allow_rules
            .get(&crate::types::permissions::PermissionRuleSource::CliArg)
            .cloned()
            .unwrap_or_default();
        context.always_allow_rules = HashMap::from([
            (
                crate::types::permissions::PermissionRuleSource::CliArg,
                cli_rules,
            ),
            (
                crate::types::permissions::PermissionRuleSource::Session,
                allowed_tools
                    .iter()
                    .map(|rule| {
                        crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(
                            rule,
                        )
                    })
                    .collect(),
            ),
        ]);
    }
    context
}

/// Maps to: CC `runAgent.ts#filterIncompleteToolCalls(...)`.
pub fn filter_incomplete_tool_calls(messages: &[Message]) -> Vec<Message> {
    let tool_use_ids_with_results = messages
        .iter()
        .flat_map(|message| match message {
            Message::User(user) => user
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContent::ToolResult(result) => Some(result.tool_use_id.0.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect::<HashSet<_>>();

    messages
        .iter()
        .filter(|message| match message {
            Message::Assistant(assistant) => !assistant.content.iter().any(|block| {
                matches!(
                    block,
                    AssistantContent::ToolUse(tool_use)
                        if !tool_use_ids_with_results.contains(&tool_use.id.0)
                )
            }),
            _ => true,
        })
        .cloned()
        .collect()
}

/// Maps to CC `runAgent.ts:368-373`:
///
/// ```ts
/// // Handle message forking for context sharing
/// // Filter out incomplete tool calls from parent messages to avoid API errors
/// const contextMessages: Message[] = forkContextMessages
///   ? filterIncompleteToolCalls(forkContextMessages)
///   : []
/// const initialMessages: Message[] = [...contextMessages, ...promptMessages]
/// ```
///
/// The filter matters because the parent's last assistant row holds the very
/// `tool_use` that is spawning this agent, and it has no `tool_result` yet — the
/// API rejects that pairing. [`filter_incomplete_tool_calls`] is the any-orphan
/// form and serves ONLY this carrier; resume takes the every-orphan
/// `filter_unresolved_tool_uses` instead (`resume_agent.rs:47-52`).
///
/// CC's ternary is JS-truthy, where `[]` is truthy and takes the filter branch;
/// filtering an empty slice yields empty, so matching on `Option` agrees for
/// every input.
///
/// The result is CC's `initialMessages`, which is what every later push appends
/// to (the SubagentStart hook attachment at `:554`, preloaded skills at `:639`),
/// so the fork context must land in front before those run.
fn initial_agent_messages(
    fork_context_messages: Option<&[Message]>,
    prompt_messages: Vec<Message>,
) -> Vec<Message> {
    match fork_context_messages {
        Some(fork_context_messages) => {
            let mut initial_messages = filter_incomplete_tool_calls(fork_context_messages);
            initial_messages.extend(prompt_messages);
            initial_messages
        }
        None => prompt_messages,
    }
}

/// Maps to CC `runAgent.ts:375-378` `agentReadFileState`.
///
/// ```ts
/// const agentReadFileState =
///   forkContextMessages !== undefined
///     ? cloneFileStateCache(toolUseContext.readFileState)
///     : createFileStateCacheWithSizeLimit(READ_FILE_STATE_CACHE_SIZE)
/// ```
///
/// `!== undefined`, NOT the JS truthiness used three lines above at `:370`, so
/// an empty parent conversation (`Some(&[])`) still takes the clone arm.
///
/// Only a FORK child inherits the parent's Read state, because only a fork child
/// inherits the parent's conversation: the transcript it is handed already
/// contains those Read `tool_result` rows, so the cache has to agree with them.
/// Every other subagent starts EMPTY and must Read a file itself before
/// `FileEditTool`/`FileWriteTool` will touch it —
/// `read_file_state.get(path)` returning `None` is their
/// "File has not been read yet. Read it first before writing to it." branch
/// (`file_edit_tool/mod.rs:263-268`, `file_write_tool/mod.rs:206-211`), and an
/// inherited entry also carries the parent's `timestamp_ms`/`content`, which
/// clears the staleness check at `file_edit_tool/mod.rs:281-291`.
///
/// [`crate::tool::SharedFileStateCache::fresh`] is
/// `FileStateCache::default()` =
/// `with_size_limit(READ_FILE_STATE_CACHE_SIZE, DEFAULT_MAX_CACHE_SIZE_BYTES)`,
/// which is exactly `createFileStateCacheWithSizeLimit(READ_FILE_STATE_CACHE_SIZE)`
/// with its own default second argument (`fileStateCache.ts:101-106`).
fn agent_read_file_state(
    fork_context_messages: Option<&[Message]>,
    parent_read_file_state: &crate::tool::SharedFileStateCache,
) -> crate::tool::SharedFileStateCache {
    match fork_context_messages {
        Some(_) => parent_read_file_state.cloned_contents(),
        None => crate::tool::SharedFileStateCache::fresh(),
    }
}

/// Maps to CC `runAgent(...)` foreground path.
/// Maps to CC `runAgent.ts#createSubagentContext` `effortValue =
/// agentDefinition.effort !== undefined ? agentDefinition.effort : state.effortValue`.
fn effective_agent_effort(
    agent_definition: &AgentDefinition,
    context: &ToolUseContext,
) -> Option<crate::utils::effort::EffortValue> {
    agent_definition
        .effort
        .clone()
        .or_else(|| context.effort_value.clone())
}

pub async fn run_agent(input: RunAgentInput<'_>) -> anyhow::Result<RunAgentOutcome> {
    let start = Instant::now();
    // Maps to CC `runAgent.ts:347` `override?.agentId ? override.agentId :
    // createAgentId()`.
    let agent_id = input
        .r#override
        .agent_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| create_agent_id(None));
    // Maps to CC `runAgent.ts:351-353`: route this agent's transcript into a
    // grouping subdirectory if requested. JS-truthy — an empty string does
    // not register.
    if let Some(subdir) = input.transcript_subdir.filter(|subdir| !subdir.is_empty()) {
        crate::utils::session_storage::set_agent_transcript_subdir(&agent_id, subdir);
    }
    let run_cwd = input.context.effective_cwd();
    // Maps to: CC `runAgent.ts` `writeAgentMetadata(agentId, ...)`.
    // The call lives on the official AgentTool/runAgent data-flow seam and
    // writes under the shared session persistence policy.
    let _ = crate::utils::session_storage::write_agent_metadata(
        &agent_id,
        &crate::utils::session_storage::AgentMetadata {
            agent_type: input.agent_definition.agent_type.clone(),
            worktree_path: input.worktree_path.map(ToOwned::to_owned),
            description: input.description.map(ToOwned::to_owned),
        },
    );
    let model = resolve_agent_model(input.agent_definition, input.model_override, input.context);
    let caller_prompt_messages = input.prompt_messages.clone().unwrap_or_else(|| {
        vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text(input.prompt.to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })]
    });
    let mut prompt_messages = initial_agent_messages(
        input.fork_context_messages.as_deref(),
        caller_prompt_messages,
    );

    // Maps to: CC `runAgent.ts:380-410`: explicit maps (including empty maps)
    // win over the memoized parent context. Only the implicit user context is
    // eligible for omitClaudeMd; Explore/Plan always discard stale gitStatus.
    let mut resolved_user_context = input
        .r#override
        .user_context
        .clone()
        .unwrap_or_else(crate::context::get_user_context);
    let mut resolved_system_context = input
        .r#override
        .system_context
        .clone()
        .unwrap_or_else(crate::context::get_system_context);
    let should_omit_claude_md = input.agent_definition.omit_claude_md
        && input.r#override.user_context.is_none()
        // CC's generic cached read is a cast, not validation. Its result is
        // used by && / a ternary, so retain JavaScript truthiness even when
        // an override/cache contains null, 0, an empty string, or an object.
        && match crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
            "tengu_slim_subagent_claudemd",
            serde_json::Value::Bool(true),
        ) {
            serde_json::Value::Null => false,
            serde_json::Value::Bool(value) => value,
            serde_json::Value::Number(value) => value.as_f64() != Some(0.0),
            serde_json::Value::String(value) => !value.is_empty(),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => true,
        };
    if should_omit_claude_md {
        resolved_user_context.remove("claudeMd");
    }
    if matches!(
        input.agent_definition.agent_type.as_str(),
        "Explore" | "Plan"
    ) {
        resolved_system_context.remove("gitStatus");
    }

    let mut permission_context = agent_permission_context_for_run(
        &input.context.tool_permission_context,
        input.agent_definition.permission_mode,
        input.allowed_tools,
    );

    // Maps to CC `runAgent.ts:275` — `isAsync` is the caller's explicit flag,
    // never derived from background bookkeeping fields.
    let is_async = input.is_async;
    let should_avoid_permission_prompts = agent_should_avoid_permission_prompts(
        input.can_show_permission_prompts,
        input.agent_definition.permission_mode,
        is_async,
    );
    // Maps to: CC `runAgent.ts:446-451` — `agentGetAppState` stamps
    // `shouldAvoidPermissionPrompts: true` onto the permission context the
    // agent's tools are evaluated against, which is what makes
    // `hasPermissionsToUseTool` turn a would-be dialog into a deny
    // (`utils/permissions/permissions.ts:932-952`). CC only ever sets the flag,
    // never clears it, so a nested foreground agent inside a background one
    // keeps the parent's true (the context is cloned from the parent at
    // `agent_permission_context_for_run`).
    permission_context.should_avoid_permission_prompts =
        permission_context.should_avoid_permission_prompts || should_avoid_permission_prompts;
    // Maps to: CC `runAgent.ts:458-463` — a background agent that can still
    // show prompts waits for the automated checks before the dialog opens.
    if is_async && !should_avoid_permission_prompts {
        permission_context.await_automated_checks_before_dialog = true;
    }
    // Maps to CC `runAgent.ts:292-296` `availableTools` — the caller-computed
    // pool when provided. `None` runs the same workerTools computation CC's
    // AgentTool.tsx performs at 838-845 (`assembleToolPool` under a pool
    // context whose mode is `permissionMode ?? 'acceptEdits'`, separate from
    // the runtime context which keeps the parent mode when the agent defines
    // none).
    let available_tools = match input.available_tools.clone() {
        Some(tools) => tools,
        None => {
            let mut worker_pool_context = input.context.tool_permission_context.clone();
            worker_pool_context.mode = input
                .agent_definition
                .permission_mode
                .unwrap_or(PermissionMode::AcceptEdits);
            // Maps to: CC AgentTool.tsx:838-845 synchronous assembleToolPool.
            // A4: our eager Agent schema additionally reads plugin promises;
            // source tool metadata is lazy. Keep that existing native catalog
            // off the published process loop used by background-agent runs.
            let context = input.context.clone();
            let teammate = crate::utils::teammate_context::capture_teammate_context();
            tokio::task::spawn_blocking(move || {
                crate::utils::teammate_context::with_teammate_context_sync(teammate, || {
                    available_tools_for_agent(&context, &worker_pool_context)
                })
            })
            .await?
        }
    };
    // Maps to: CC `runAgent.ts:500-502` `const resolvedTools = useExactTools
    // ? availableTools : resolveAgentTools(agentDefinition, availableTools,
    // isAsync).resolvedTools` — fork children skip the per-agent tool filter so
    // the tool list bytes match the parent's request prefix. The in-process
    // teammate exception is read inside `filterToolsForAgent` from the teammate
    // task-local scope, exactly where CC calls `isInProcessTeammate()`.
    let resolved_tools = if input.use_exact_tools {
        available_tools.clone()
    } else {
        resolve_agent_tools(input.agent_definition, &available_tools, is_async, false)
            .resolved_tools
    };
    // Maps to: CC `runAgent.ts:504-506` appState permission-directory keys.
    let app_state = input.context.get_app_state();
    let additional_working_directories: Vec<String> = app_state
        .as_ref()
        .map(|state| state.tool_permission_context.as_ref())
        .unwrap_or(&input.context.tool_permission_context)
        .additional_working_directories
        .keys()
        .cloned()
        .collect();
    // Maps to CC `runAgent.ts:508-518` `override?.systemPrompt ?
    // override.systemPrompt : asSystemPrompt(await getAgentSystemPrompt(...))`.
    // JS truthiness on an ARRAY: an empty override prompt is truthy and wins,
    // so the `Some(vec![])` carrier is used as-is.
    let system_prompt = match input.r#override.system_prompt.clone() {
        Some(prompt) => prompt,
        None => get_agent_system_prompt(
            input.agent_definition,
            input.context,
            &model,
            &additional_working_directories,
            &resolved_tools,
        ),
    };

    let subagent_start_results =
        execute_subagent_start_hooks_from_settings(&agent_id, &input.agent_definition.agent_type)
            .await;
    // Maps to: CC `runAgent.ts:530-555` — collect every hook's
    // additionalContexts and, when any exist, push the
    // `createAttachmentMessage` equivalent: a `hook_additional_context`
    // attachment the shared projection renders as an isMeta user row wrapping
    // "SubagentStart hook additional context: {join('\n')}" in a
    // system-reminder (utils/messages.ts:4117-4128).
    //
    // The emptiness gate is CC's, one layer up: `hooks.ts:2783`
    // `if (result.additionalContext)` is JS-truthy, so `""` is never yielded
    // while whitespace-only IS (no trim anywhere on this path).
    let additional_contexts = subagent_start_results
        .iter()
        .filter_map(|result| result.additional_context.as_deref())
        .filter(|context| !context.is_empty())
        .collect::<Vec<_>>();
    if !additional_contexts.is_empty() {
        prompt_messages.push(Message::Attachment(
            crate::types::message::AttachmentMessage::new(serde_json::json!({
                "type": "hook_additional_context",
                "content": additional_contexts,
                "hookName": "SubagentStart",
                "toolUseID": uuid::Uuid::new_v4().to_string(),
                "hookEvent": "SubagentStart",
            })),
        ));
    }

    let _agent_frontmatter_hooks =
        register_agent_frontmatter_hooks_for_run(&agent_id, input.agent_definition);
    // Maps to: CC runAgent.ts:580/623-632 awaited catalog and callbacks.
    // A4: retain the sync callback carrier off the process executor; plugin
    // promises can themselves be scheduled on that published executor.
    let preload_definition = input.agent_definition.clone();
    let preload_context = input.context.clone();
    let preload_agent_scope = crate::utils::agent_context::get_agent_context();
    let preload_teammate_scope = crate::utils::teammate_context::capture_teammate_context();
    let preloaded = tokio::task::spawn_blocking(move || {
        crate::utils::process_runtime::block_on_from_sync(async move {
            let preload = async move {
                crate::utils::teammate_context::with_teammate_context_sync(
                    preload_teammate_scope,
                    || preload_agent_skill_messages(&preload_definition, &preload_context),
                )
            };
            match preload_agent_scope {
                Some(scope) => {
                    crate::utils::agent_context::run_with_agent_context(scope, || preload).await
                }
                None => preload.await,
            }
        })
        .ok_or_else(|| anyhow::anyhow!("Unable to initialize agent skill preload runtime"))?
    })
    .await??;
    prompt_messages.extend(preloaded);

    // Maps to: CC `runAgent.ts:735` `recordSidechainTranscript(initialMessages,
    // agentId)` plus the `lastRecordedUuid` cursor (`:745`) and the
    // per-recordable-message call inside the loop below (`:793-803`).
    //
    // `prompt_messages` is CC's `initialMessages` and goes in WHOLE, hook
    // attachment included — that write is the one place CC hands an attachment
    // to `recordSidechainTranscript`, and `cleanMessagesForLogging` is what
    // decides its fate per audience. Everything the query LOOP produces passes
    // [`is_recordable_message`] first (CC `:231-246`), so no other attachment
    // can reach the file.
    //
    // This used to hand-roll its own message→entry conversion and hand the
    // result straight to `record_sidechain_transcript`, i.e. it wrote CC's
    // whole drop set to disk: the SubagentStart `hook_additional_context`
    // attachment pushed above on EVERY audience, every `Message::Attachment`
    // the query loop yields (CC yields those WITHOUT recording,
    // `runAgent.ts:770-790`), hook results, and progress as a literal `null`
    // JSONL line. The conversion and the `cleanMessagesForLogging` privacy
    // filter stay owned by session storage — one writer, one filter — and so
    // does the parked assistant record that stands in for the live reference
    // CC's write queue holds (`claude.ts:2235-2247`).
    //
    // `run_cwd` is the stamp, never the route; see
    // `record_typed_sidechain_transcript` for the worktree split that cost a
    // resume.
    let mut sidechain = crate::utils::session_storage::SidechainTranscriptRecorder::start(
        &agent_id,
        &prompt_messages,
        &run_cwd,
    );

    let agent_mcp =
        initialize_agent_mcp_servers(input.agent_definition, &input.context.mcp_state).await;
    let resolved_tools = merge_agent_mcp_tools(resolved_tools, agent_mcp.tools.clone());

    let mut agent_content_replacement_state = initial_agent_content_replacement_state(&input);

    // Maps to: CC `runAgent.ts:694,755`: one required parameter feeds both
    // the exact-tool child's options and its own query/retry source.
    let query_source = input.query_source.clone();

    let agent_abort_controller = select_agent_abort_controller(
        input.r#override.abort_controller.clone(),
        is_async,
        &input.context.abort_controller,
    );
    // Maps to CC `runAgent.ts:668-704` `agentOptions` followed by the complete
    // `createSubagentContext` override object.
    let mut agent_options = input.context.options();
    agent_options.commands = std::sync::Arc::new(Vec::new());
    agent_options.tools = resolved_tools;
    agent_options.main_loop_model = Some(model);
    // Maps to CC `runAgent.ts:679-684`: "For fork children (useExactTools),
    // inherit thinking config to match the parent's API request prefix for
    // prompt cache hits. For regular sub-agents, disable thinking to control
    // output token costs."
    agent_options.thinking_config = if input.use_exact_tools {
        input.context.thinking_config.clone()
    } else {
        Some(ThinkingConfig::Disabled)
    };
    agent_options.mcp_state = agent_mcp.mcp_state.clone();
    // Maps to CC `runAgent.ts:668-672` `isNonInteractiveSession: useExactTools
    // ? toolUseContext.options.isNonInteractiveSession : isAsync ? true :
    // (toolUseContext.options.isNonInteractiveSession ?? false)` — fork
    // children keep the parent's value verbatim.
    agent_options.is_non_interactive_session = if input.use_exact_tools {
        input.context.is_non_interactive_session
    } else {
        is_async || input.context.is_non_interactive_session
    };
    // Maps to CC `runAgent.ts:688-694` `...(useExactTools && { querySource })`:
    //
    // > Fork children (useExactTools path) need querySource on context.options
    // > for the recursive-fork guard at AgentTool.tsx call() — it checks
    // > options.querySource === 'agent:builtin:fork'. This survives autocompact
    // > (which rewrites messages, not context.options). Without this, the guard
    // > reads undefined and only the message-scan fallback fires — which
    // > autocompact defeats by replacing the fork-boilerplate message.
    //
    // The value is the caller-selected `querySource`, which
    // for a fork is `agent:builtin:fork` and is what
    // `mod.rs#selected_agent_definition` compares against. It used to be the
    // coarse `QuerySource::Agent`, which no comparison could distinguish from
    // any other subagent, so the primary check was unrepresentable and only
    // the fallback CC calls a fallback ran.
    agent_options.query_source = input.use_exact_tools.then(|| query_source.clone());
    let agent_read_file_state = agent_read_file_state(
        input.fork_context_messages.as_deref(),
        &input.context.read_file_state,
    );
    let mut agent_tool_use_context = crate::utils::forked_agent::create_subagent_context(
        input.context,
        crate::utils::forked_agent::SubagentContextOverrides {
            options: Some(agent_options),
            // Maps to CC `runAgent.ts:705` `readFileState: agentReadFileState`.
            read_file_state: Some(agent_read_file_state),
            // Maps to: CC `shareSetAppState: !isAsync`.
            share_set_app_state: !is_async,
            share_set_response_length: true,
            abort_controller: Some(agent_abort_controller),
            agent_id: Some(agent_id.clone()),
            agent_type: Some(input.agent_definition.agent_type.clone()),
            messages: Some(prompt_messages.clone()),
            content_replacement_state: agent_content_replacement_state.clone(),
            critical_system_reminder_experimental: input
                .agent_definition
                .critical_system_reminder_experimental
                .clone(),
            avoid_permission_prompts_overlay: Some(should_avoid_permission_prompts),
            ..Default::default()
        },
    );
    agent_tool_use_context.tool_permission_context = permission_context.clone();
    agent_tool_use_context.resume_restore_stores = input.context.resume_restore_stores.clone();
    agent_tool_use_context.content_replacement_state = agent_content_replacement_state.clone();
    agent_tool_use_context.cwd_override = input.context.cwd_override.clone();
    agent_tool_use_context.critical_system_reminder_experimental = input
        .agent_definition
        .critical_system_reminder_experimental
        .clone();
    agent_tool_use_context.effort_value =
        effective_agent_effort(input.agent_definition, input.context);
    agent_tool_use_context.fast_mode = Some(false);
    agent_tool_use_context.channel_permission_callbacks =
        input.context.channel_permission_callbacks.clone();
    // Maps to CC `runAgent.ts:716-719`: preserve tool use results for
    // subagents with viewable transcripts (in-process teammates).
    if input.preserve_tool_use_results {
        agent_tool_use_context.preserve_tool_use_results = true;
    }

    let query_params = crate::query::QueryParams {
        turn_id: agent_id.clone(),
        input: input.prompt.to_string(),
        messages: Vec::new(),
        model_messages: prompt_messages,
        system_prompt,
        user_context: resolved_user_context,
        system_context: resolved_system_context,
        // Maps to CC `runAgent.ts:755` `querySource,` — the SAME value the
        // child context options carry, so a fork child's API request reports
        // `agent:builtin:fork` and every `startsWith('agent:')` consumer
        // (`query.ts:377`, `claude.ts:1067`) sees the real source.
        query_source,
        token_budget: None,
        task_budget: None,
        max_turns: input.agent_definition.max_turns,
        tool_use_context: agent_tool_use_context.clone(),
    };
    // A4: spawn_query synchronously hydrates an empty native tool pool before
    // starting its actor. CC runAgent.ts:753 awaits query with prepared options;
    // keep this existing Rust fallback off the published background executor.
    // Capture teammate identity before handoff so spawn_query forwards it to
    // the actor exactly as it did when constructed on this task.
    let query_teammate_scope = crate::utils::teammate_context::capture_teammate_context();
    let handle = tokio::task::spawn_blocking(move || {
        crate::utils::teammate_context::with_teammate_context_sync(query_teammate_scope, || {
            crate::query::spawn_query(query_params, crate::query::deps::production_deps())
        })
    })
    .await?;

    let mut agent_messages = Vec::<Message>::new();
    let mut background_signal = input.background_signal.clone();
    let mut background_progress_tracker = input
        .background_task_id
        .map(|_| crate::tasks::local_agent_task::create_progress_tracker());
    let mut terminal_reason: Option<String> = None;
    loop {
        let event = if let Some(signal) = background_signal.as_mut() {
            if *signal.borrow() {
                handle.abort_controller.abort();
                let _ = handle
                    .commands
                    .send(crate::query::QueryCommand::Abort)
                    .await;
                // CC's write queue drains on its own 100 ms timer regardless of
                // how `runAgent` returns, so a parked record still lands.
                sidechain.flush();
                cleanup_agent_mcp_servers(&agent_mcp.cleanup_server_names).await;
                release_agent_registrations(&agent_id, input.context);
                return Ok(RunAgentOutcome::Backgrounded(BackgroundedAgentRun {
                    agent_id,
                    agent_type: input.agent_definition.agent_type.clone(),
                    messages: agent_messages,
                    content_replacement_state: agent_content_replacement_state,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                }));
            }
            tokio::select! {
                event = handle.events.recv() => match event {
                    Ok(event) => event,
                    Err(_) => break,
                },
                changed = signal.changed() => {
                    if changed.is_ok() && *signal.borrow() {
                        handle.abort_controller.abort();
                        let _ = handle.commands.send(crate::query::QueryCommand::Abort).await;
                        sidechain.flush();
                        cleanup_agent_mcp_servers(&agent_mcp.cleanup_server_names).await;
                        release_agent_registrations(&agent_id, input.context);
                        return Ok(RunAgentOutcome::Backgrounded(BackgroundedAgentRun {
                            agent_id,
                            agent_type: input.agent_definition.agent_type.clone(),
                            messages: agent_messages,
                            content_replacement_state: agent_content_replacement_state,
                            elapsed_ms: start.elapsed().as_millis() as u64,
                        }));
                    }
                    continue;
                }
            }
        } else {
            match handle.events.recv().await {
                Ok(event) => event,
                Err(_) => break,
            }
        };
        if input.context.abort_controller.is_aborted() {
            handle.abort_controller.abort();
            let _ = handle
                .commands
                .send(crate::query::QueryCommand::Abort)
                .await;
            sidechain.flush();
            cleanup_agent_mcp_servers(&agent_mcp.cleanup_server_names).await;
            release_agent_registrations(&agent_id, input.context);
            crate::tasks::local_shell_task::kill_shell_tasks::kill_shell_tasks_for_agent(&agent_id);
            return Err(anyhow::Error::new(AgentExecutionAborted {
                agent_messages,
                // CC's break leaves the caller holding the same mutated object
                // (`toolResultStorage.ts:759-762`), so the decisions this turn
                // made survive it. Here the local is the only copy — hand it
                // back or the caller silently reverts to its pre-turn state and
                // re-decides holistically next turn (cache miss).
                content_replacement_state: agent_content_replacement_state,
            }));
        }

        // The transcript half of the loop, over a borrow, before the arms below
        // consume the event. Both halves have ONE owner so the uuid the query
        // actor stamps on `QueryEvent::Message` cannot drift from the uuid it
        // later keys `QueryEvent::AssistantDelta` on — see
        // [`record_query_event_to_agent_transcript`].
        record_query_event_to_agent_transcript(&mut sidechain, &event);

        match event {
            crate::query::QueryEvent::Stream(crate::types::message::StreamEvent::ApiEvent {
                event,
                ttft_ms,
            }) => {
                if event.get("type").and_then(serde_json::Value::as_str) == Some("message_start") {
                    if let Some(ttft_ms) = ttft_ms {
                        agent_tool_use_context.api_metrics_sink.push(ttft_ms);
                    }
                }
            }
            // C3c-3: `Message` carries a whole model message (the seam's CC
            // yield); the agent loop consumes it exactly like the
            // history-only `ModelMessage` transport (no row projection).
            crate::query::QueryEvent::Message(message)
            | crate::query::QueryEvent::ModelMessage(message) => {
                if let crate::types::message::Message::Assistant(assistant) = &message {
                    let delta = assistant
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            crate::types::message::AssistantContent::Text(text) => {
                                Some(text.encode_utf16().count())
                            }
                            _ => None,
                        })
                        .sum();
                    agent_tool_use_context.response_length_sink.add(delta);
                }
                forward_subagent_progress_from_message(
                    &message,
                    input.parent_tool_use_id,
                    &agent_id,
                    input.on_progress,
                );
                if let (Some(task_id), Some(tracker)) = (
                    input.background_task_id,
                    background_progress_tracker.as_mut(),
                ) {
                    crate::tasks::local_agent_task::update_progress_from_message(tracker, &message);
                    crate::tasks::local_agent_task::update_agent_progress(
                        task_id,
                        crate::tasks::local_agent_task::get_progress_update(tracker),
                    );
                    crate::tasks::local_agent_task::append_message_to_local_agent(
                        task_id,
                        message.clone(),
                    );
                }
                if let Some(on_message) = input.on_message {
                    on_message(&message);
                }
                agent_messages.push(message);
            }
            crate::query::QueryEvent::AssistantDelta {
                uuid,
                stop_reason,
                usage,
            } => {
                apply_assistant_delta_to_agent_messages(
                    &mut agent_messages,
                    &uuid,
                    stop_reason,
                    usage,
                );
            }
            crate::query::QueryEvent::ContentReplacementStateUpdate(state) => {
                agent_content_replacement_state = Some(state);
            }
            crate::query::QueryEvent::PermissionRequest(request) => {
                // CC `runAgent.ts:744-753` hands `query()` `canUseTool` and
                // `agentToolUseContext` and never evaluates permission a second
                // time. `query.rs` already ran the child's permission system
                // (`:3160-3178`) and only emits this event when `should_ask`.
                //
                // This used to call `can_use_tool` again, first against the
                // PARENT context (`input.context`) and then, after 2579377,
                // against the child. Either way it was a second evaluation CC
                // does not have: a parent allow-rule could rewrite the Ask,
                // and classifier/denial counters could run twice. The two
                // other consumers of this same event already do it right:
                // `repl.rs:4051` only queues a dialog, `query_engine.rs:925`
                // only asks the SDK resolver.
                //
                // The child inherits the parent's `interactive_permission_sink`
                // (`create_subagent_context` is `parent.clone()`, and the sink
                // is `Option<Arc<dyn Fn>>`), so the dialog still raises on the
                // parent terminal.
                let response = resolve_agent_permission_request(
                    &request,
                    &agent_tool_use_context,
                    &permission_context,
                )
                .await;
                let _ = handle
                    .commands
                    .send(crate::query::QueryCommand::PermissionResponse {
                        tool_use_id: request.tool_use_id,
                        response,
                    })
                    .await;
            }
            crate::query::QueryEvent::Terminal(terminal) => {
                terminal_reason = Some(terminal.reason);
                break;
            }
            _ => {}
        }
    }

    // The `for await (const message of makeStream(...))` in CC
    // `agentToolUtils.ts:554-593` ends here. For a detached background agent
    // this is the ONLY route to a terminal status, so log it: a background
    // agent stuck at `running` means this line never printed.
    crate::utils::debug::log_for_debugging(&format!(
        "runAgent[{agent_id}]: agent stream ended, terminal={terminal_reason:?}, messages={}",
        agent_messages.len()
    ));
    // The stream ended. Any record still parked belongs on disk before the
    // SubagentStop hooks run — they hand the agent's transcript path to
    // user commands (`services/hooks/teammate.rs`), and CC's queue would have
    // drained it on its own timer.
    sidechain.flush();

    cleanup_agent_mcp_servers(&agent_mcp.cleanup_server_names).await;

    execute_subagent_stop_hooks_from_settings(
        &agent_id,
        &input.agent_definition.agent_type,
        last_assistant_text_from_messages(&agent_messages).as_deref(),
    )
    .await;

    // Maps to CC `runAgent.ts` finally-block order (:834-847):
    // clearAgentTranscriptSubdir → todos release → killShellTasksForAgent.
    release_agent_registrations(&agent_id, input.context);
    // Foreground/background-shell tasks owned by a completed subagent must
    // not outlive that subagent.
    crate::tasks::local_shell_task::kill_shell_tasks::kill_shell_tasks_for_agent(&agent_id);

    let mut completed = finalize_agent_tool_result_from_messages(
        &agent_messages,
        &agent_id,
        &input.agent_definition.agent_type,
        start.elapsed().as_millis() as u64,
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "{error}{}",
            terminal_reason
                .as_deref()
                .map(|reason| format!(" (terminal: {reason})"))
                .unwrap_or_default()
        )
    })?;
    completed.content_replacement_state = agent_content_replacement_state;
    Ok(RunAgentOutcome::Completed(completed))
}

/// Maps to: CC `runAgent.ts:577-645` frontmatter skill preload block
/// (`skillsToPreload`, `resolveSkillName`, `formatSkillLoadingMetadata`, and
/// `skill.getPromptForCommand('', toolUseContext)`).
fn preload_agent_skill_messages(
    agent_definition: &AgentDefinition,
    context: &ToolUseContext,
) -> anyhow::Result<Vec<Message>> {
    let Some(skills_to_preload) = agent_definition.skills.as_ref() else {
        return Ok(Vec::new());
    };
    if skills_to_preload.is_empty() {
        return Ok(Vec::new());
    }

    // Maps to: CC `runAgent.ts:580`
    // `const allSkills = await getSkillToolCommands(getProjectRoot())`.
    //
    // NOT `get_skill_dir_commands`: that is `loadSkillsDir.ts:802`, whose
    // return value is the unconditional filesystem skills and nothing else.
    // `getSkillToolCommands` (`commands.ts:563-581`) sits on top of
    // `getCommands`, so it additionally carries plugin skills and the DYNAMIC
    // skills merged at `commands.ts:479-516`, and it drops
    // `disable-model-invocation` entries — none of which the directory loader
    // can either see or filter. While the dynamic merge still lived inside
    // `get_skill_dir_commands` this call resolved dynamic skills by accident;
    // moving that merge to its CC owner made an agent whose `skills:`
    // frontmatter names a discovered skill silently stop preloading it.
    //
    // cwd: CC passes `getProjectRoot()` — session-start-anchored process state
    // that `EnterWorktreeTool` deliberately leaves alone so "skills/history
    // stay anchored to where the session started" (`bootstrap/state.ts:519-522`).
    // This port has no `projectRoot` field in `bootstrap/state.rs` at all, so
    // there is nothing to call here; `effective_cwd()` (`cwd_override`, else
    // `get_original_cwd()`) stays the carrier rather than one of the two CC
    // roots being silently substituted for the other.
    let cwd = context.effective_cwd();
    let all_skills = crate::commands::get_skill_tool_commands(&cwd);
    preload_agent_skill_messages_from_commands(agent_definition, context, &all_skills)
}

fn preload_agent_skill_messages_from_commands(
    agent_definition: &AgentDefinition,
    context: &ToolUseContext,
    all_skills: &[crate::commands::Command],
) -> anyhow::Result<Vec<Message>> {
    let Some(skills_to_preload) = agent_definition.skills.as_ref() else {
        return Ok(Vec::new());
    };

    // CC resolves and validates the whole list before starting any
    // `getPromptForCommand` promise (`runAgent.ts:583-615`). Keep that phase
    // separate from callback execution so a callback failure cannot suppress
    // warnings for later entries.
    let mut resolved = Vec::new();
    for skill_name in skills_to_preload {
        let Some(resolved_name) =
            resolve_agent_skill_name(skill_name, all_skills, agent_definition)
        else {
            tracing::warn!(
                "[Agent: {}] Skill '{}' specified in frontmatter was not found",
                agent_definition.agent_type,
                skill_name
            );
            continue;
        };
        // Maps to: CC `runAgent.ts:605` `getCommand(resolvedName, allSkills)`.
        // CC throws when absent; `resolveSkillName` has already proven the name
        // resolves, so the miss arm is unreachable rather than fatal here.
        let Some(skill) = crate::commands::find_command(&resolved_name, all_skills) else {
            continue;
        };
        // Maps to: CC runAgent.ts:606-612, 626-630. Invoke the command's
        // actual callback: plugin commands capture different execution state.
        if skill.kind != crate::commands::CommandKind::Prompt {
            tracing::warn!(
                "[Agent: {}] Skill '{}' is not a prompt-based skill",
                agent_definition.agent_type,
                skill_name
            );
            continue;
        }
        let Some(callback) = skill.get_prompt_for_command else {
            // Treat a malformed prompt command like a rejected preload. Keep
            // resolving the remaining entries first, as Promise.all does.
            resolved.push((skill_name.as_str(), skill, None));
            continue;
        };
        resolved.push((skill_name.as_str(), skill, Some(callback)));
    }

    // `GetPromptForCommand` is currently a synchronous function pointer. Invoke
    // every resolved callback even after one fails, matching Promise.all's
    // already-started siblings and preserving the first error in list order as
    // the only ordering available to this synchronous carrier. The outer
    // caller still publishes messages only after the whole batch succeeds.
    let mut messages = Vec::new();
    let mut first_error = None;
    for (skill_name, skill, callback) in resolved {
        let Some(callback) = callback else {
            if first_error.is_none() {
                first_error = Some(anyhow::anyhow!(
                    "Prompt command '{}' has no executable callback",
                    skill.name
                ));
            }
            continue;
        };
        let prompt = match callback(skill, "", context) {
            Ok(prompt) => prompt,
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                continue;
            }
        };
        // CC `runAgent.ts:642` formats with `skillName`, the raw frontmatter
        // entry, not the resolved name — the resolved name only ever picks the
        // command. The two differ exactly on the plugin prefix/suffix arms of
        // `resolveSkillName`, which this call site could not reach at all while
        // it was reading the directory loader (plugin skills never appear there).
        let metadata =
            crate::utils::process_user_input::process_slash_command::format_skill_loading_metadata(
                skill_name,
                skill.progress_message.as_deref().unwrap_or("loading"),
            );
        let mut message = crate::utils::messages::create_user_message_with_meta(metadata, true);
        message
            .content
            .extend(prompt.into_iter().map(|block| match block {
                UserContent::Text(text) => UserContent::MetaText(text),
                UserContent::Image { media_type, data } => {
                    UserContent::MetaImage { media_type, data }
                }
                UserContent::RawImage { block, .. } => UserContent::RawImage {
                    block,
                    is_meta: true,
                },
                UserContent::Document { media_type, data } => {
                    UserContent::MetaDocument { media_type, data }
                }
                block => block,
            }));
        messages.push(Message::User(message));
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(messages)
}

/// Maps to: CC `runAgent.ts:945-973` local `resolveSkillName(...)` helper.
///
/// The lookups are CC `hasCommand` (`commands.ts:700-702`), which matches on
/// `name`, `getCommandName(...)` and `aliases` — not the skill-directory
/// `find_skill_command`, which sees only `name` and additionally strips a
/// leading `/` off the query that CC never strips.
fn resolve_agent_skill_name(
    skill_name: &str,
    all_skills: &[crate::commands::Command],
    agent_definition: &AgentDefinition,
) -> Option<String> {
    if crate::commands::has_command(skill_name, all_skills) {
        return Some(skill_name.to_string());
    }

    let plugin_prefix = agent_definition
        .agent_type
        .split(':')
        .next()
        .unwrap_or_default();
    if !plugin_prefix.is_empty() {
        let qualified_name = format!("{plugin_prefix}:{skill_name}");
        if crate::commands::has_command(&qualified_name, all_skills) {
            return Some(qualified_name);
        }
    }

    let suffix = format!(":{skill_name}");
    all_skills
        .iter()
        .find(|skill| skill.name.ends_with(&suffix))
        .map(|skill| skill.name.to_string())
}

struct AgentFrontmatterHookGuard {
    session_id: Option<String>,
}

impl Drop for AgentFrontmatterHookGuard {
    fn drop(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            crate::utils::hooks::session_hooks::clear_session_hooks(&session_id);
        }
    }
}

/// Maps to: CC `runAgent.ts#runAgent` `registerFrontmatterHooks(...)` block.
fn register_agent_frontmatter_hooks_for_run(
    agent_id: &str,
    agent_definition: &AgentDefinition,
) -> AgentFrontmatterHookGuard {
    let Some(hooks) = agent_definition.hooks.as_ref() else {
        return AgentFrontmatterHookGuard { session_id: None };
    };
    let hooks_allowed_for_this_agent =
        !crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("hooks")
            || crate::utils::settings::plugin_only_policy::is_source_admin_trusted(
                agent_definition.source.official_name(),
            );
    if !hooks_allowed_for_this_agent {
        return AgentFrontmatterHookGuard { session_id: None };
    }
    let registered = crate::utils::hooks::register_frontmatter_hooks::register_frontmatter_hooks(
        agent_id,
        hooks,
        &format!("agent '{}'", agent_definition.agent_type),
        true,
    );
    AgentFrontmatterHookGuard {
        session_id: (registered > 0).then(|| agent_id.to_string()),
    }
}

/// Maps to: CC `runAgent.ts` executing `executeSubagentStartHooks(...)` and
/// adding `hook_additional_context` before the nested query loop.
async fn execute_subagent_start_hooks_from_settings(
    agent_id: &str,
    agent_type: &str,
) -> Vec<crate::services::hooks::HookResult> {
    let Some(config) = subagent_hook_config(agent_id) else {
        return Vec::new();
    };
    crate::services::hooks::teammate::execute_subagent_start_hooks(
        &config,
        agent_id,
        agent_type,
        Vec::new(),
    )
    .await
}

/// Maps to: CC `executeStopHooks(...)` SubagentStop branch invoked at the end
/// of `runAgent.ts` subagent lifecycle. Blocking behavior and progress rows are
/// still transitional; this slice executes configured hooks and records metadata
/// while preserving the foreground Agent result flow.
async fn execute_subagent_stop_hooks_from_settings(
    agent_id: &str,
    agent_type: &str,
    last_assistant_message: Option<&str>,
) -> Vec<crate::services::hooks::HookResult> {
    let Some(config) = subagent_hook_config(agent_id) else {
        return Vec::new();
    };
    let transcript_path = crate::utils::session_storage::get_agent_transcript_path(agent_id)
        .display()
        .to_string();
    crate::services::hooks::teammate::execute_subagent_stop_hooks(
        &config,
        agent_id,
        agent_type,
        &transcript_path,
        last_assistant_message,
        Vec::new(),
    )
    .await
}

fn subagent_hook_config(agent_id: &str) -> Option<crate::services::hooks::RegisteredHooks> {
    let loaded_hooks = crate::services::hooks::load_hooks_config();
    let mut config = loaded_hooks.config;
    if !loaded_hooks.allow_managed_hooks_only {
        crate::utils::hooks::session_hooks::merge_session_hooks_into_config(&mut config, agent_id);
    }
    (!config.is_empty()).then_some(config)
}

fn last_assistant_text_from_messages(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => {
            let text: Vec<String> = assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    AssistantContent::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect();
            let text = text.join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    })
}

/// Maps to: CC `AgentTool.tsx:833-845` — the `workerTools` the caller computes
/// and hands to `runAgent` as `availableTools`, and `resumeAgent.ts:158-164`
/// for the non-fork resume. Both are
/// `assembleToolPool(workerPermissionContext, appState.mcp.tools)`, with CC's
/// stated reason at `:834-837`: "Workers always get their tools from
/// assembleToolPool with their own permission mode, so they aren't affected by
/// the parent's tool restrictions."
///
/// That is why the parent's `context.tools` is NOT unioned in: a nested agent
/// must see the full MCP set, not the frontmatter-narrowed pool its parent was
/// left with.
///
/// `appState` is `toolUseContext.getAppState()` (`AgentTool.tsx:407`), so the
/// MCP source follows the same live-store rule as `agent_tool/mod.rs
/// #live_mcp_state`, including its fallback: CC always has a store, so the
/// query-start `context.mcp_state` snapshot branch has no CC counterpart and
/// exists only for contexts built without one (tests, direct headless callers).
fn available_tools_for_agent(
    context: &ToolUseContext,
    permission_context: &crate::tool::ToolPermissionContext,
) -> Vec<Tool> {
    let mcp_tools = context
        .get_app_state()
        .map(|state| state.mcp.tools.clone())
        .unwrap_or_else(|| context.mcp_state.tools.clone());
    crate::tools::assemble_tool_pool(permission_context, &mcp_tools)
}

/// Maps to: the `ask` arm of the parent's `canUseTool`
/// (`useCanUseTool.tsx:189-327`) — NOT a second `hasPermissionsToUseTool`.
///
/// `query.rs:3160-3178` already ran the child's permission system and only
/// emits `QueryEvent::PermissionRequest` once `should_ask` is true. CC never
/// evaluates that Ask again: `runAgent.ts:753` hands `canUseTool` INTO
/// `query()`, so the parent's `ask` arm (`handleInteractivePermission`,
/// `:307-324`) is what this function is. The two other consumers of the same
/// event already match that: `repl.rs:4051` only queues a dialog,
/// `query_engine.rs:925` only asks the SDK resolver.
///
/// The prompt is skipped in exactly the two cases CC skips it:
/// `shouldAvoidPermissionPrompts` — set for async agents by
/// `runAgent.ts:446-451` and consumed by `permissions.ts:932-952`, which denies
/// with `decisionReason: { type: 'asyncAgent' }` rather than waiting for a
/// terminal nobody is watching — and a context with no dialog leg at all
/// (print/SDK, tests), where CC has no `setToolUseConfirmQueue` to close over.
///
/// A skipped prompt is a Deny carrying [`PermissionRequest::message`] — the
/// SYSTEM decision `toolExecution.ts:1023` gives the model — not a hang.
async fn resolve_agent_permission_request(
    request: &PermissionRequest,
    context: &ToolUseContext,
    agent_permission_context: &crate::tool::ToolPermissionContext,
) -> PermissionPromptResponse {
    if let Some(response) =
        ask_parent_for_agent_permission(request, context, agent_permission_context).await
    {
        return response;
    }
    PermissionPromptResponse::new(PermissionPromptChoice::Deny)
        .with_decision_message(request.message.clone())
}

/// The `case 'ask'` arm of the parent's `canUseTool`
/// (CC `hooks/useCanUseTool.tsx:189-327`): raise the parent terminal's dialog
/// and leave the subagent's call pending until the user answers.
///
/// `None` means CC would not have shown a dialog either:
///
/// - `should_avoid_permission_prompts` — `runAgent.ts:446-451` sets it for
///   every async agent (and `forkedAgent.ts:362-374` for isolated forks), and
///   `permissions.ts:932-952` converts the ask into
///   `{ behavior: 'deny', decisionReason: { type: 'asyncAgent' } }` *before*
///   `useCanUseTool` ever reaches its switch. Nobody is watching the terminal
///   for a background agent, and CC does not park on one.
/// - no sink installed — the context has no dialog leg (print/SDK headless,
///   direct test callers). CC's equivalent is `getCanUseToolFn`'s no-prompt-tool
///   arm (`cli/print.ts:4276-4293`), a bare `hasPermissionsToUseTool` whose
///   `ask` is rejected by `toolExecution.ts:995` (`behavior !== 'allow'`);
///   `:4654-4655` states the rule outright — "print.ts never calls
///   handleInteractivePermission". Exception (#156): an IN-PROCESS TEAMMATE
///   with no sink takes CC's mailbox fallback (`inProcessRunner.ts:337-447`)
///   instead of the bare deny.
///
/// The caller keeps the existing deny in both cases, so this never converts a
/// deny into a hang.
async fn ask_parent_for_agent_permission(
    request: &PermissionRequest,
    context: &ToolUseContext,
    agent_permission_context: &crate::tool::ToolPermissionContext,
) -> Option<PermissionPromptResponse> {
    if agent_permission_context.should_avoid_permission_prompts {
        return None;
    }
    // Maps to: CC `utils/swarm/inProcessRunner.ts#createInProcessCanUseTool` —
    // an in-process teammate's ask resolves on the leader. The task-local
    // teammate scope (`teammateContext.ts` ALS; entered by
    // `run_in_process_teammate` around this whole run) identifies the asker.
    let teammate = crate::utils::teammate_context::get_teammate_context()
        .filter(|identity| identity.is_in_process);
    if !context.interactive_permission_sink.is_some() {
        // Maps to: CC `inProcessRunner.ts:337-447` — when the leader's
        // ToolUseConfirm queue is unavailable (`getLeaderToolUseConfirmQueue()`
        // null; Rust: no sink), the teammate falls back to the mailbox:
        // forward the request to the leader's inbox and wait for the response.
        // #156 re-homed this from the dead `create_in_process_can_use_tool`
        // callback (whose destination param was never read) onto this path,
        // which is the one that actually runs.
        if let Some(identity) = teammate.as_ref() {
            let mut request = request.clone();
            crate::hooks::tool_permission::handlers::interactive_handler::fill_tool_description(
                &mut request,
            );
            return crate::utils::swarm::in_process_runner::resolve_teammate_ask_via_mailbox(
                identity,
                &request,
                &context.abort_controller,
            )
            .await;
        }
        return None;
    }
    // Maps to CC `useCanUseTool.tsx:138-143` computing `tool.description(...)`
    // before the dialog; the interactive handler owns the same fill for the
    // parent's own prompts.
    let mut request = request.clone();
    crate::hooks::tool_permission::handlers::interactive_handler::fill_tool_description(
        &mut request,
    );
    // Maps to: CC `inProcessRunner.ts:229-232` `workerBadge: identity.color ?
    // { name: identity.agentName, color: identity.color } : undefined` — the
    // standard leader-dialog leg carries the asking teammate's badge.
    let worker_badge = teammate.as_ref().and_then(|identity| {
        identity
            .color
            .clone()
            .map(|color| crate::types::permissions::PermissionWorkerBadge {
                name: identity.agent_name.clone(),
                color: Some(color),
            })
    });
    context
        .interactive_permission_sink
        .ask(
            crate::tool::InteractivePermissionAsk::new(request)
                .with_worker_badge(worker_badge)
                // Maps to: CC `interactiveHandler.ts:97` / `inProcessRunner.ts:230`
                // `toolUseContext` on the queued row — for an agent that is the
                // context `agentGetAppState` produces (`runAgent.ts:416-497`),
                // which is this run's `permission_context`
                // (`agent_tool_use_context.tool_permission_context`), NOT the
                // parent's. Without it the leader's recheck sweep would
                // re-evaluate a `plan` agent's row against a `default` leader,
                // or a scoped agent's row against allow rules `:469-478` had
                // deliberately dropped.
                .with_asking_tool_permission_context(Some(agent_permission_context.clone())),
        )
        .await
}

/// Maps to: CC `runAgent.ts:793-803` (`isRecordableMessage` →
/// `recordSidechainTranscript` → the cursor advance) plus the transcript half
/// of `claude.ts:2244-2248` (`message_delta` reaching that queued entry).
///
/// Both halves live in ONE function because they are one contract, and the
/// contract is a uuid identity: `apply_assistant_delta` can only find the
/// parked record if the uuid the actor stamped on `QueryEvent::Message` is the
/// same uuid it later keys `QueryEvent::AssistantDelta` on
/// (`query.rs` reads the envelope field `last.uuid`, never the identity-block
/// accessor `AssistantMessage::uuid()`, which is a *different* value).
///
/// A break in that identity is silent — no error, no panic, just an agent
/// JSONL whose assistant rows all keep `message_start`'s `output_tokens: 0`
/// and `stop_reason: null` — so
/// `sidechain_transcript_captures_the_delta_uuid_the_query_actor_stamped`
/// drives this function from a real `spawn_query` stream instead of supplying
/// uuids of its own.
fn record_query_event_to_agent_transcript(
    sidechain: &mut crate::utils::session_storage::SidechainTranscriptRecorder,
    event: &crate::query::QueryEvent,
) {
    match event {
        crate::query::QueryEvent::Message(message)
        | crate::query::QueryEvent::ModelMessage(message) => {
            if is_recordable_message(message) {
                sidechain.record(message);
            }
        }
        crate::query::QueryEvent::AssistantDelta {
            uuid,
            stop_reason,
            usage,
        } => sidechain.apply_assistant_delta(uuid, stop_reason.clone(), usage.clone()),
        _ => {}
    }
}

/// Maps to: CC `runAgent.ts:231-246` `isRecordableMessage` — the gate the query
/// loop applies BEFORE `recordSidechainTranscript` (`:793`). Only
/// `assistant`, `user`, `progress` and `system` with
/// `subtype === 'compact_boundary'` are admitted; the rest of the
/// `QueryMessage` union (`:220-225`: stream events, request starts, tool-use
/// summaries, tombstones) never reaches the recorder at all.
///
/// Attachments are the arm that matters here. CC handles them one branch
/// earlier (`:770-790`): `max_turns_reached` breaks the loop, every other
/// attachment is YIELDED and `continue`d — never recorded — and this guard
/// would reject them anyway. The port sends the same values through
/// `QueryEvent::Message` (`query.rs` `emit_max_turns_reached`, the
/// hook/skill/system-reminder attachments), so without this gate they reached
/// the writer, where only `isLoggableMessage`'s audience check stood between
/// them and the file: on an `ant` build every attachment was persisted, and
/// each one also took the parent cursor, so the chain CC writes as
/// `assistant → user` was written as `assistant → attachment → user`.
///
/// `hook_result` has no CC counterpart in this union (it is the port's carrier
/// for `processSessionStartHooks` output) and is rejected for the same reason
/// its wire type — `attachment` — is.
fn is_recordable_message(message: &Message) -> bool {
    match message {
        Message::Assistant(_) | Message::User(_) | Message::Progress(_) => true,
        Message::System(system) => matches!(
            system,
            crate::types::message::SystemMessage::CompactBoundary { .. }
        ),
        Message::Attachment(_) | Message::HookResult(_) => false,
    }
}

/// Maps to: CC `services/api/claude.ts:2244-2248` — `message_delta` arrives
/// after the last `content_block_stop` and writes the final usage/stop reason
/// back onto the last already-yielded per-block assistant
/// (`lastMsg.message.usage = usage`). The agent loop's retained copy is CC's
/// `newMessages` entry, so this is the whole reach of that mutation: the
/// summaries, the sidechain record and the final result read final values.
///
/// It deliberately does NOT reach the already-emitted progress payloads — see
/// [`forward_subagent_progress_from_message`] for why that matches CC.
fn apply_assistant_delta_to_agent_messages(
    agent_messages: &mut [Message],
    uuid: &str,
    stop_reason: Option<crate::types::message::StopReason>,
    usage: Option<TokenUsage>,
) {
    if let Some(Message::Assistant(assistant)) = agent_messages
        .iter_mut()
        .rev()
        .find(|message| message.uuid() == uuid)
    {
        assistant.stop_reason = stop_reason;
        assistant.usage = usage;
    }
}

/// Maps to: CC `AgentTool.tsx:1483-1508` — the foreground loop's
/// `normalizeMessages([message])` walk, forwarding one
/// `ProgressMessage<AgentToolProgress>` per normalized message that carries a
/// `tool_use` or `tool_result` block.
///
/// The payload is the WHOLE normalized message (`:1498`); no field is picked
/// out of it here. Every display row — tool name, description, status, result
/// text, the trailing search/read rollup — is derived at the renderer, which
/// is CC's owner for all of them (`AgentTool/UI.tsx`).
///
/// The payload is a SNAPSHOT, not a live reference. CC's normalize step builds
/// a FRESH inner message (`utils/messages.ts:760-764`
/// `message: { ...message.message, content: [_], … }`), so the spread copies
/// the `usage` POINTER as of this instant — the `message_start` usage the
/// per-block assistant was built from (`claude.ts:1981` `partialMessage =
/// part.message`, `:2192-2196` `{ ...partialMessage, content }`). The
/// finalisation at `claude.ts:2246` REPLACES that property on the original
/// envelope, and `updateUsage` (`:2924-2986`) returns a fresh object rather
/// than mutating the old one, so the copy never observes it. Nor does the SDK:
/// the stream is the raw SSE one (`claude.ts:1822-1823`
/// `anthropic.beta.messages.create({ ...params, stream: true })` →
/// `Stream<BetaRawMessageStreamEvent>` at `:1857`), not the accumulating
/// `MessageStream` helper. And nothing rewrites a row after the fact:
/// `agent_progress` is deliberately absent from `EPHEMERAL_PROGRESS_TYPES`
/// (`sessionStorage.ts:186-193`) and `REPL.tsx:3478-3481` spells out why —
/// "agent_progress / hook_progress / skill_progress are NOT ephemeral — each
/// carries distinct state the UI needs".
///
/// So `calculateAgentStats` (`AgentTool/UI.tsx:806-819`) reads `message_start`
/// usage in CC too, and Rust's owned `Box<Message>` clone reproduces that
/// exactly. Pinned end to end by
/// `agent_progress_usage_is_the_emission_time_snapshot_like_cc`.
fn forward_subagent_progress_from_message(
    message: &Message,
    parent_tool_use_id: Option<&str>,
    agent_id: &str,
    on_progress: Option<ToolCallProgressFn<'_>>,
) {
    let (Some(parent_tool_use_id), Some(on_progress)) = (parent_tool_use_id, on_progress) else {
        return;
    };
    for normalized in crate::utils::messages::normalize_messages(std::slice::from_ref(message)) {
        // `normalizeMessages` yields `NormalizedMessage` (≙ `RenderableMessage`);
        // the User/Assistant kinds ARE the model messages it split, so the
        // wire carrier keeps them as `Message` without loss.
        let forwarded = match normalized.kind {
            crate::types::message::RenderableMessageKind::User { message } => {
                Message::User(message)
            }
            crate::types::message::RenderableMessageKind::Assistant { message } => {
                Message::Assistant(message)
            }
            _ => continue,
        };
        // CC `:1485-1491`: one progress per tool_use/tool_result CONTENT
        // BLOCK. Normalization already guarantees one real block per message,
        // so this emits at most once per normalized message.
        let qualifying_blocks = match &forwarded {
            Message::Assistant(assistant) => assistant
                .content
                .iter()
                .filter(|block| matches!(block, AssistantContent::ToolUse(_)))
                .count(),
            Message::User(user) => user
                .content
                .iter()
                .filter(|block| matches!(block, UserContent::ToolResult(_)))
                .count(),
            _ => 0,
        };
        for _ in 0..qualifying_blocks {
            on_progress(crate::types::tools::ToolProgress::AgentProgress {
                parent_tool_use_id: crate::types::ids::ToolUseId(parent_tool_use_id.to_string()),
                message: Box::new(forwarded.clone()),
                // CC `:1500-1502` sends an EMPTY prompt from the loop: only
                // the first progress message carries it (`:1084-1092`), and
                // `UI.tsx:637-639` reads `progressMessages[0]`.
                prompt: String::new(),
                agent_id: agent_id.to_string(),
            });
        }
    }
}

/// Maps to CC `runAgent.ts` call to `utils/model/agent.ts#getAgentModel`.
pub fn resolve_agent_model(
    agent_definition: &AgentDefinition,
    model_override: Option<&str>,
    context: &ToolUseContext,
) -> String {
    let parent_model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    get_agent_model(
        agent_definition.model.as_deref(),
        &parent_model,
        model_override,
        Some(context.tool_permission_context.mode),
    )
}

/// Maps to: CC `runAgent.ts:906-933` `getAgentSystemPrompt`.
/// The existing Option-valued provider carrier represents unavailable prompts
/// with None; successful empty strings remain successful. Dynamic thrown JS
/// callbacks are not representable by that carrier and remain an explicit seam.
pub fn get_agent_system_prompt(
    agent_definition: &AgentDefinition,
    context: &ToolUseContext,
    resolved_agent_model: &str,
    additional_working_directories: &[String],
    resolved_tools: &[Tool],
) -> SystemPrompt {
    let enabled_tool_names = resolved_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let agent_prompt = agent_definition
        .get_system_prompt(context)
        .unwrap_or_else(|| crate::constants::prompts::DEFAULT_AGENT_PROMPT.to_string());
    crate::constants::prompts::enhance_system_prompt_with_env_details(
        &[agent_prompt],
        resolved_agent_model,
        additional_working_directories,
        Some(&enabled_tool_names),
        &context.effective_cwd(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
    use crate::types::message::{
        AssistantContent, AssistantMessage, StopReason, ToolResult, ToolUseBlock, UserMessage,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::io::Write;

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            strict: Some(true),
            ..Default::default()
        }
    }

    /// CC `runAgent.ts:524-528`. Every CC async caller supplies the override
    /// (AgentTool.tsx:1003/:1010/:1245, resumeAgent.ts:234/:241,
    /// inProcessRunner.ts:1197), so an async agent still stops when its owner
    /// aborts — the unlinked arm only applies when no owner is named.
    #[test]
    fn agent_abort_controller_prefers_the_override_then_async_then_parent() {
        let parent = crate::tool::AbortController::default();
        let owner = crate::tool::AbortController::child_of(parent.clone());

        // Async WITH an owner: aborting the owner reaches the agent. This is
        // the in-process teammate case (Escape stops the turn).
        let chosen = select_agent_abort_controller(Some(owner.clone()), true, &parent);
        assert!(!chosen.is_aborted());
        owner.abort();
        assert!(chosen.is_aborted());

        // Sync without an override shares the parent.
        let parent = crate::tool::AbortController::default();
        let chosen = select_agent_abort_controller(None, false, &parent);
        assert!(!chosen.is_aborted());
        parent.abort();
        assert!(chosen.is_aborted());

        // Async without an override is intentionally unlinked.
        let parent = crate::tool::AbortController::default();
        let chosen = select_agent_abort_controller(None, true, &parent);
        parent.abort();
        assert!(!chosen.is_aborted());
    }

    /// CC `runAgent.ts` finally tail (:832-843): the transcript-subdir
    /// mapping and this agent's todos key are released through the parent
    /// store; a missing key skips the state update entirely.
    #[test]
    fn release_agent_registrations_drops_todos_key_and_transcript_subdir() {
        let mut initial = crate::state::app_state_store::AppState::default();
        std::sync::Arc::make_mut(&mut initial.todos).insert(
            "agent-cleanup".to_string(),
            vec![crate::utils::todo::types::TodoItem {
                content: "task".to_string(),
                status: crate::utils::todo::types::TodoStatus::Completed,
                active_form: "tasking".to_string(),
            }],
        );
        let store = crate::state::store::AppStore::new(initial, None);
        let context = ToolUseContext::default().with_app_store(store.clone());
        crate::utils::session_storage::set_agent_transcript_subdir("agent-cleanup", "workflows/x");

        release_agent_registrations("agent-cleanup", &context);
        assert!(!store.get().todos.contains_key("agent-cleanup"));

        // Absent key: the guard skips the update (CC `return prev`).
        let before = store.get();
        release_agent_registrations("agent-cleanup", &context);
        assert!(std::sync::Arc::ptr_eq(&before.todos, &store.get().todos));
    }

    /// CC `runAgent.ts:436-445`: explicit `canShowPermissionPrompts` wins;
    /// otherwise an async agent auto-denies prompts it cannot show, except in
    /// `bubble` mode where the prompt reaches the parent terminal.
    #[test]
    fn bubble_agents_keep_permission_prompts_alive_while_async_agents_avoid_them() {
        assert!(agent_should_avoid_permission_prompts(None, None, true));
        assert!(!agent_should_avoid_permission_prompts(None, None, false));
        assert!(agent_should_avoid_permission_prompts(
            None,
            Some(PermissionMode::Plan),
            true
        ));
        assert!(!agent_should_avoid_permission_prompts(
            None,
            Some(PermissionMode::Bubble),
            true
        ));
        assert!(!agent_should_avoid_permission_prompts(
            None,
            Some(PermissionMode::Bubble),
            false
        ));
        // Explicit canShowPermissionPrompts overrides BOTH the bubble branch
        // and the isAsync default (CC `canShowPermissionPrompts !== undefined
        // ? !canShowPermissionPrompts : ...`).
        assert!(!agent_should_avoid_permission_prompts(
            Some(true),
            None,
            true
        ));
        assert!(agent_should_avoid_permission_prompts(
            Some(false),
            Some(PermissionMode::Bubble),
            false
        ));
    }

    /// CC `runAgent.ts:420-434`: the fork agent's `bubble` becomes the child's
    /// mode, but a parent in bypassPermissions / acceptEdits / auto wins.
    #[test]
    fn fork_agent_bubble_mode_yields_to_the_parents_stronger_modes() {
        let child_mode = |parent_mode| {
            let parent = crate::tool::ToolPermissionContext {
                mode: parent_mode,
                ..crate::tool::ToolPermissionContext::default()
            };
            agent_permission_context_for_run(&parent, Some(PermissionMode::Bubble), None).mode
        };

        assert_eq!(child_mode(PermissionMode::Default), PermissionMode::Bubble);
        assert_eq!(child_mode(PermissionMode::Plan), PermissionMode::Bubble);
        assert_eq!(
            child_mode(PermissionMode::BypassPermissions),
            PermissionMode::BypassPermissions
        );
        assert_eq!(
            child_mode(PermissionMode::AcceptEdits),
            PermissionMode::AcceptEdits
        );
        if crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled() {
            assert_eq!(child_mode(PermissionMode::Auto), PermissionMode::Auto);
        }
    }

    fn hook_command(command: &str) -> crate::services::hooks::HookCommand {
        crate::services::hooks::HookCommand {
            command: command.to_string(),
            shell: None,
            timeout: Some(5),
            condition: None,
            status: None,
            once: None,
            is_async: None,
            async_rewake: None,
        }
    }

    #[test]
    fn register_agent_frontmatter_hooks_converts_stop_and_cleans_session_scope() {
        crate::utils::hooks::session_hooks::clear_all_session_hooks();
        let mut agent = AgentDefinition::new(
            "reviewer",
            "Review code",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.hooks = Some(std::collections::HashMap::from([(
            "Stop".to_string(),
            vec![crate::services::hooks::HookConfigEntry {
                matcher: None,
                hooks: vec![hook_command("echo stop")],
                plugin_root: None,
                plugin_name: None,
                plugin_id: None,
            }],
        )]));

        {
            let _guard = register_agent_frontmatter_hooks_for_run("agent-1", &agent);
            let stop = crate::utils::hooks::session_hooks::get_session_hooks(
                "agent-1",
                Some(crate::services::hooks::HookEvent::Stop),
            );
            assert!(stop.is_empty());
            let subagent_stop = crate::utils::hooks::session_hooks::get_session_hooks(
                "agent-1",
                Some(crate::services::hooks::HookEvent::SubagentStop),
            );
            assert_eq!(
                subagent_stop["SubagentStop"][0].hooks[0].command,
                "echo stop"
            );
        }

        assert!(crate::utils::hooks::session_hooks::get_session_hooks("agent-1", None).is_empty());
        crate::utils::hooks::session_hooks::clear_all_session_hooks();
    }

    #[test]
    fn create_agent_id_matches_official_prefix_shape() {
        let id = create_agent_id(None);
        assert!(id.starts_with('a'));
        assert_eq!(id.len(), 17);
        let labeled = create_agent_id(Some("compact"));
        assert!(labeled.starts_with("acompact-"));
        assert_eq!(labeled.len(), "acompact-".len() + 16);
    }

    #[test]
    fn content_replacement_state_explicit_override_precedes_parent_context_like_official() {
        let agent = AgentDefinition::new(
            "custom",
            "Use custom",
            AgentDefinitionSource::ProjectSettings,
        );
        let mut parent_state = crate::utils::tool_result_storage::ContentReplacementState::new();
        parent_state.seen_ids.insert("parent-toolu".to_string());
        let mut explicit_state = crate::utils::tool_result_storage::ContentReplacementState::new();
        explicit_state.seen_ids.insert("explicit-toolu".to_string());
        let context = ToolUseContext {
            tools: vec![tool("Bash")],
            content_replacement_state: Some(parent_state.clone()),
            ..ToolUseContext::default()
        };
        let input = RunAgentInput {
            agent_definition: &agent,
            prompt: "inspect",
            description: None,
            model_override: None,
            context: &context,
            query_source: QuerySource::AgentCustom,
            is_async: false,
            can_show_permission_prompts: None,
            available_tools: None,
            fork_context_messages: None,
            preserve_tool_use_results: false,
            transcript_subdir: None,
            r#override: RunAgentOverride::default(),
            use_exact_tools: false,
            allowed_tools: None,
            worktree_path: None,
            parent_tool_use_id: None,
            on_progress: None,
            background_task_id: None,
            content_replacement_state: Some(explicit_state.clone()),
            background_signal: None,
            on_message: None,
            prompt_messages: None,
        };

        assert_eq!(
            initial_agent_content_replacement_state(&input),
            Some(explicit_state)
        );

        let input = RunAgentInput {
            content_replacement_state: None,
            ..input
        };
        assert_eq!(
            initial_agent_content_replacement_state(&input),
            Some(parent_state)
        );
    }

    #[test]
    fn effective_agent_effort_prefers_agent_definition_over_parent_context() {
        let mut agent = AgentDefinition::new(
            "custom",
            "Use custom",
            AgentDefinitionSource::ProjectSettings,
        );
        let context = ToolUseContext::default().with_effort_value(Some(
            crate::utils::effort::EffortValue::Named("low".to_string()),
        ));
        assert_eq!(
            effective_agent_effort(&agent, &context),
            Some(crate::utils::effort::EffortValue::Named("low".to_string()))
        );

        agent.effort = Some(crate::utils::effort::EffortValue::Named("high".to_string()));
        assert_eq!(
            effective_agent_effort(&agent, &context),
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
    }

    #[test]
    fn resolves_model_override_before_agent_model_before_parent_inherit() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SUBAGENT_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");

        let context = ToolUseContext::default().with_main_loop_model("parent-sonnet-model");
        let mut agent = AgentDefinition::new(
            "custom",
            "Use custom",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.model = Some("haiku".to_string());
        assert!(resolve_agent_model(&agent, Some("opus"), &context).contains("opus"));
        assert!(resolve_agent_model(&agent, None, &context).contains("haiku"));
        agent.model = Some("inherit".to_string());
        assert_eq!(
            resolve_agent_model(&agent, None, &context),
            "parent-sonnet-model"
        );
    }

    #[test]
    fn agent_system_prompt_appends_persistent_agent_memory_prompt() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
        let root = std::env::temp_dir().join(format!(
            "cometix-agent-system-memory-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let memory_dir = root.join(".claude/agent-memory/reviewer");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("MEMORY.md"), "- Use focused review notes\n").unwrap();

        let mut agent = AgentDefinition::new(
            "reviewer",
            "Use custom",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.system_prompt = Some("Review code.".to_string());
        agent.memory = Some(crate::tools::agent_tool::agent_memory::AgentMemoryScope::Project);
        let context = ToolUseContext::default().with_cwd_override(Some(root.clone()));

        let prompt = get_agent_system_prompt(&agent, &context, "claude-sonnet-4-6", &[], &[]);
        let joined = prompt.join("\n");
        assert!(joined.contains("Review code."));
        assert!(joined.contains("Persistent Agent Memory"));
        assert!(joined.contains("- Use focused review notes"));
        let _ = std::fs::remove_dir_all(&root);
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_AUTO_MEMORY");
    }

    #[test]
    fn agent_system_prompt_uses_cwd_override_for_worktree_env_details() {
        let agent = AgentDefinition::new(
            "custom",
            "Use custom",
            AgentDefinitionSource::ProjectSettings,
        );
        let cwd = std::env::temp_dir().join(format!(
            "cometix-agent-env-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&cwd).unwrap();
        let context = ToolUseContext::default().with_cwd_override(Some(cwd.clone()));

        let prompt = get_agent_system_prompt(&agent, &context, "claude-sonnet-4-6", &[], &[]);
        let _ = std::fs::remove_dir_all(&cwd);

        assert!(
            prompt
                .join("\n")
                .contains(&format!("Working directory: {}", cwd.display()))
        );
    }

    /// The dialog answer — not a second `canUseTool` evaluation — is what
    /// carries `updatedInput` / `permissionUpdates` back to the query
    /// continuation. CC's `onAllow(updatedInput, permissionUpdates, …)` is the
    /// dialog's callback (`useCanUseTool.tsx:307-324`); `query()` never
    /// re-decides an Ask that already left the permission system.
    #[test]
    fn nested_permission_response_carries_updated_input_to_query_continuation() {
        let (confirm_tx, confirm_rx) = async_channel::unbounded();
        let mut context = context_with_bash_tool();
        context.interactive_permission_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                confirm_tx,
            );
        let request = bash_permission_request();
        let answered = answer_first_queued_prompt(
            confirm_rx,
            PermissionPromptResponse::allow_once_with_input(serde_json::json!({
                "command": "cargo test --quiet"
            }))
            .with_permission_updates(vec![
                crate::types::permissions::PermissionUpdate::AddRules {
                    destination: crate::types::permissions::PermissionUpdateDestination::Session,
                    behavior: PermissionBehavior::Allow,
                    rules: vec![crate::types::permissions::PermissionRuleValue::new(
                        "Bash",
                        Some("cargo test --quiet".to_string()),
                    )],
                },
            ]),
        );

        let response = futures::executor::block_on(resolve_agent_permission_request(
            &request,
            &context,
            &crate::tool::ToolPermissionContext::default(),
        ));
        drop(context);

        assert!(answered.join().expect("prompt answerer panicked").is_some());
        assert_eq!(response.choice, PermissionPromptChoice::AllowOnce);
        assert_eq!(
            response.updated_input.as_ref().unwrap()["command"].as_str(),
            Some("cargo test --quiet")
        );
        assert_eq!(response.permission_updates.len(), 1);
        let updated_request = response.apply_to_request(request);
        assert_eq!(
            updated_request.input["command"].as_str(),
            Some("cargo test --quiet")
        );
    }

    /// Maps to: CC `interactiveHandler.ts:97` / `inProcessRunner.ts:230` —
    /// `toolUseContext` on the pushed `ToolUseConfirm`
    /// (`PermissionRequest.tsx:142`) is the ASKER's, and for a subagent that is
    /// the context `agentGetAppState` produces (`runAgent.ts:416-497`), not the
    /// parent's. The leader's recheck sweep
    /// (`interactiveHandler.ts:204-231`, fired from `REPL.tsx:3114-3126`) reads
    /// exactly that field, so a `plan` agent's row must not be re-evaluated
    /// against a `default` leader.
    ///
    /// OLD SHAPE: `InteractivePermissionSink::ask_with_badge` took only the
    /// request and the badge, so nothing could ride the row and the field did
    /// not exist. Both assertions below failed.
    #[test]
    fn a_subagents_queued_row_records_the_agents_own_permission_context() {
        let (confirm_tx, confirm_rx) = async_channel::unbounded();
        let mut context = context_with_bash_tool();
        context.interactive_permission_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                confirm_tx,
            );
        // CC `runAgent.ts:421-434`: a `plan` agent under a `default` parent —
        // NARROWER than the leader, the direction #179 assumed impossible.
        let agent_permission_context = agent_permission_context_for_run(
            &crate::tool::ToolPermissionContext::default(),
            Some(PermissionMode::Plan),
            Some(&["Read".to_string()]),
        );
        let answered = answer_first_queued_prompt(
            confirm_rx,
            PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
        );

        let _response = futures::executor::block_on(resolve_agent_permission_request(
            &bash_permission_request(),
            &context,
            &agent_permission_context,
        ));
        drop(context);

        let queued = answered
            .join()
            .expect("prompt answerer panicked")
            .expect("the ask queues one row");
        assert_eq!(
            queued.asking_tool_permission_context.as_ref(),
            Some(&agent_permission_context),
            "the row must carry the agent's context, not the parent's"
        );
        assert_eq!(
            queued
                .asking_tool_permission_context
                .as_ref()
                .map(|context| context.mode),
            Some(PermissionMode::Plan)
        );
    }

    fn bash_permission_request() -> PermissionRequest {
        crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-toolu_bash",
            "toolu_bash",
            "Bash",
            "cargo test",
            serde_json::json!({"command":"cargo test"}),
            PermissionMode::Default,
        )
    }

    fn context_with_bash_tool() -> ToolUseContext {
        ToolUseContext {
            tools: vec![Tool {
                name: "Bash".to_string(),
                description: "Run command".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                strict: Some(true),
                ..Default::default()
            }],
            ..ToolUseContext::default()
        }
    }

    /// Answers the first queue entry the REPL would render, standing in for the
    /// user pressing a key on it. Returns the entry so the caller can assert on
    /// what was actually queued.
    fn answer_first_queued_prompt(
        rx: async_channel::Receiver<crate::types::permissions::ToolUseConfirm>,
        response: PermissionPromptResponse,
    ) -> std::thread::JoinHandle<Option<crate::types::permissions::ToolUseConfirm>> {
        std::thread::spawn(move || {
            let confirm = rx.recv_blocking().ok()?;
            confirm.responder.respond(response);
            Some(confirm)
        })
    }

    #[test]
    fn foreground_agent_ask_raises_the_parent_dialog_and_honors_the_answer() {
        // Maps to: CC `useCanUseTool.tsx:189-327` — a subagent calls the
        // parent's `canUseTool` (`AgentTool.tsx:399` → `:879` →
        // `runAgent.ts:753`), and its `ask` arm queues a `ToolUseConfirm` on the
        // PARENT and resolves the subagent's pending call with the answer.
        let (confirm_tx, confirm_rx) = async_channel::unbounded();
        let mut context = context_with_bash_tool();
        context.interactive_permission_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                confirm_tx,
            );
        let request = bash_permission_request();
        let answered = answer_first_queued_prompt(
            confirm_rx,
            PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
        );

        let response = futures::executor::block_on(resolve_agent_permission_request(
            &request,
            &context,
            &crate::tool::ToolPermissionContext::default(),
        ));
        // The sink owns the only remaining sender; drop it so the answerer's
        // blocking recv can finish rather than parking the test process.
        drop(context);

        let queued = answered
            .join()
            .expect("prompt answerer panicked")
            .expect("an ask must reach the parent's queue, not be force-denied");
        assert_eq!(queued.tool_use_id(), "toolu_bash");
        assert!(
            queued.responder.is_some(),
            "the entry must carry the subagent call's resolver (CC onAllow/onReject)"
        );
        assert_eq!(response.choice, PermissionPromptChoice::AllowOnce);
    }

    #[test]
    fn foreground_agent_ask_honors_a_denying_answer() {
        let (confirm_tx, confirm_rx) = async_channel::unbounded();
        let mut context = context_with_bash_tool();
        context.interactive_permission_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                confirm_tx,
            );
        let request = bash_permission_request();
        let answered = answer_first_queued_prompt(
            confirm_rx,
            PermissionPromptResponse::new(PermissionPromptChoice::Deny),
        );

        let response = futures::executor::block_on(resolve_agent_permission_request(
            &request,
            &context,
            &crate::tool::ToolPermissionContext::default(),
        ));
        drop(context);

        assert!(answered.join().expect("prompt answerer panicked").is_some());
        assert_eq!(response.choice, PermissionPromptChoice::Deny);
    }

    #[test]
    fn async_agent_ask_is_denied_without_raising_a_dialog_like_official() {
        // Maps to: CC `runAgent.ts:446-451` setting `shouldAvoidPermissionPrompts`
        // for an async agent, and `permissions.ts:932-952` converting the ask to
        // `{ behavior: 'deny', decisionReason: { type: 'asyncAgent' } }` before
        // `useCanUseTool` reaches its switch. Nobody is watching the terminal for
        // a background agent, so CC never queues one — and never parks on one.
        let (confirm_tx, confirm_rx) = async_channel::unbounded();
        let mut context = context_with_bash_tool();
        context.interactive_permission_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                confirm_tx,
            );
        let request = bash_permission_request();
        // Answer anything that arrives, so dropping the guard fails the choice
        // assertion instead of hanging the suite.
        let answered = answer_first_queued_prompt(
            confirm_rx.clone(),
            PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
        );
        let mut agent_permission_context = crate::tool::ToolPermissionContext::default();
        agent_permission_context.should_avoid_permission_prompts = true;

        let response = futures::executor::block_on(resolve_agent_permission_request(
            &request,
            &context,
            &agent_permission_context,
        ));

        assert_eq!(response.choice, PermissionPromptChoice::Deny);
        // The answerer is parked on a queue that must stay empty; releasing the
        // sink's sender is what lets it observe the close and exit. If the guard
        // above is ever dropped, the answerer replies AllowOnce and BOTH
        // assertions fail — a clean failure, not a hung suite.
        //
        // (`confirm_rx.is_empty()` would be the obvious check and is worthless
        // here: the answerer drains the entry, so the queue reads empty either
        // way. What the answerer SAW is the observable.)
        drop(context);
        assert!(
            answered.join().expect("prompt answerer panicked").is_none(),
            "an async agent must not queue a prompt on the parent terminal"
        );
    }

    #[test]
    fn ask_without_a_dialog_leg_stays_denied_like_print_mode() {
        // Maps to: CC `cli/print.ts:4276-4293` — print mode's `canUseTool` is
        // `hasPermissionsToUseTool` with no dialog behind it, so an `ask` reaches
        // `toolExecution.ts:995` (`behavior !== 'allow'`) and is rejected;
        // `:4654-4655` states it outright ("print.ts never calls
        // handleInteractivePermission").
        let context = context_with_bash_tool();
        let request = bash_permission_request();

        let response = futures::executor::block_on(resolve_agent_permission_request(
            &request,
            &context,
            &crate::tool::ToolPermissionContext::default(),
        ));

        assert_eq!(response.choice, PermissionPromptChoice::Deny);
    }

    #[test]
    fn finalizes_agent_tool_result_from_query_messages_like_official() {
        let text_assistant = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("earlier result".to_string())],
            model: Some("model".to_string()),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        });
        let tool_use_assistant = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_agent".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path":"/tmp/a"}),
            })],
            model: Some("model".to_string()),
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(TokenUsage {
                input_tokens: 7,
                output_tokens: 3,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_deleted_input_tokens: 0,
            }),
        });
        let user_result = Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text("tool result".to_string())],
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

        let result = finalize_agent_tool_result_from_messages(
            &[text_assistant, tool_use_assistant, user_result],
            "agent-1",
            "general-purpose",
            99,
        )
        .unwrap();

        assert_eq!(result.content, vec!["earlier result"]);
        assert_eq!(result.messages.len(), 3);
        assert!(matches!(result.messages[0], Message::Assistant(_)));
        assert!(matches!(result.messages[2], Message::User(_)));
        assert_eq!(result.total_tool_use_count, 1);
        assert_eq!(result.total_tokens, 10);
        assert_eq!(result.total_duration_ms, 99);
    }

    #[test]
    fn preloads_agent_frontmatter_skills_like_official_run_agent() {
        let root = std::env::temp_dir().join(format!(
            "cometix-agent-skill-preload-{}",
            uuid::Uuid::new_v4()
        ));
        let skills_dir = root.join(".claude").join("skills");
        let skill_file = skills_dir.join("audit").join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(
            b"---\ndescription: Audit code\n---\nAudit instructions ${CLAUDE_SESSION_ID}",
        )
        .unwrap();

        let commands = crate::skills::load_skills_dir::load_skills_from_skills_dir(
            &skills_dir,
            crate::skills::load_skills_dir::SkillSource::ProjectSettings,
        )
        .into_iter()
        .map(|entry| crate::commands::Command::from_skill(entry.skill))
        .collect::<Vec<_>>();
        let mut agent = AgentDefinition::new(
            "custom",
            "Use custom",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.skills = Some(vec!["audit".to_string()]);
        let messages = preload_agent_skill_messages_from_commands(
            &agent,
            &ToolUseContext::default(),
            &commands,
        )
        .unwrap();

        let _ = std::fs::remove_dir_all(root);

        assert_eq!(messages.len(), 1);
        let Message::User(message) = &messages[0] else {
            panic!("expected user message");
        };
        let UserContent::MetaText(text) = &message.content[0] else {
            panic!("expected meta text content");
        };
        assert!(text.contains("<command-message>audit</command-message>"));
        assert!(text.contains("<skill-format>true</skill-format>"));
        assert!(
            matches!(&message.content[1], UserContent::MetaText(text) if text.contains(&format!("Audit instructions {}", crate::bootstrap::state::get_session_id())))
        );
    }

    /// Maps to: CC `runAgent.ts:580`
    /// `const allSkills = await getSkillToolCommands(getProjectRoot())`.
    ///
    /// `getSkillToolCommands` reads `getCommands`, so an agent's `skills:`
    /// frontmatter resolves DYNAMICALLY discovered skills — the ones
    /// `addSkillDirectories` parks in the dynamic registry and `getCommands`
    /// merges at `commands.ts:479-516` — exactly like static ones.
    ///
    /// Old shape: this call site read `get_skill_dir_commands`, which after
    /// b8d2279 correctly returns unconditional directory skills only
    /// (`loadSkillsDir.ts:802`). The dynamic skill was therefore invisible,
    /// `resolveSkillName` returned `None`, the loop logged "was not found" at
    /// debug level and `continue`d, and the agent started without the skill it
    /// declared. Nothing errors and nothing hangs — the assertion below fails
    /// on an empty message list.
    #[test]
    fn preloads_a_dynamically_discovered_skill_named_by_agent_frontmatter() {
        struct AllowedSourcesRestore(Vec<String>);
        impl Drop for AllowedSourcesRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_allowed_setting_sources(self.0.clone());
            }
        }

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _dynamic = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        let _sources =
            AllowedSourcesRestore(crate::bootstrap::state::get_allowed_setting_sources());
        crate::bootstrap::state::set_allowed_setting_sources(vec!["projectSettings".to_string()]);
        crate::commands::clear_commands_cache();
        crate::skills::load_skills_dir::clear_dynamic_skills();

        let root = std::env::temp_dir().join(format!(
            "cometix-agent-dynamic-skill-preload-{}",
            uuid::Uuid::new_v4()
        ));
        // Nested under a package dir, i.e. NOT on the cwd→home walk
        // `get_skill_dir_commands` performs. Only `add_skill_directories` (CC
        // `addSkillDirectories`) can reach it, so the skill is dynamic-only and
        // the assertion cannot pass through the static loader by accident.
        let discovered = root
            .join("packages")
            .join("pkg")
            .join(".claude")
            .join("skills");
        std::fs::create_dir_all(discovered.join("dynamic-audit")).unwrap();
        std::fs::write(
            discovered.join("dynamic-audit").join("SKILL.md"),
            "---\ndescription: Dynamically discovered audit\n---\nDynamic audit body",
        )
        .unwrap();
        crate::skills::load_skills_dir::add_skill_directories(std::slice::from_ref(&discovered));

        let mut agent = AgentDefinition::new(
            "custom",
            "Use custom",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.skills = Some(vec!["dynamic-audit".to_string()]);
        let context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let messages = preload_agent_skill_messages(&agent, &context).unwrap();

        crate::skills::load_skills_dir::clear_dynamic_skills();
        crate::commands::clear_commands_cache();
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(messages.len(), 1);
        let Message::User(message) = &messages[0] else {
            panic!("expected user message");
        };
        let UserContent::MetaText(text) = &message.content[0] else {
            panic!("expected meta text content");
        };
        assert!(text.contains("<command-name>dynamic-audit</command-name>"));
        assert!(text.contains("<skill-format>true</skill-format>"));
        assert!(
            matches!(&message.content[1], UserContent::MetaText(text) if text.contains("Dynamic audit body"))
        );
    }

    #[test]
    fn preloads_agent_plugin_skill_by_prefix_and_suffix_like_official_run_agent() {
        let root = std::env::temp_dir().join(format!(
            "cometix-agent-plugin-skill-preload-{}",
            uuid::Uuid::new_v4()
        ));
        let skills_dir = root.join(".claude").join("skills");
        let skill_file = skills_dir.join("plugin:docs").join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(b"---\nname: plugin:docs\ndescription: Docs\n---\nDocs instructions")
            .unwrap();

        let all_skills = crate::skills::load_skills_dir::load_skills_from_skills_dir(
            &skills_dir,
            crate::skills::load_skills_dir::SkillSource::ProjectSettings,
        )
        .into_iter()
        .map(|entry| crate::commands::Command::from_skill(entry.skill))
        .collect::<Vec<_>>();
        let agent =
            AgentDefinition::new("plugin:agent", "Use plugin", AgentDefinitionSource::Plugin);
        assert_eq!(
            resolve_agent_skill_name("docs", &all_skills, &agent).as_deref(),
            Some("plugin:docs")
        );
        let other_agent =
            AgentDefinition::new("other:agent", "Use other", AgentDefinitionSource::Plugin);
        assert_eq!(
            resolve_agent_skill_name("docs", &all_skills, &other_agent).as_deref(),
            Some("plugin:docs")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    struct SettingsCacheGuard;

    impl Drop for SettingsCacheGuard {
        fn drop(&mut self) {
            crate::utils::settings::settings_cache::reset_settings_cache();
        }
    }

    #[test]
    fn agent_mcp_initialization_skips_user_frontmatter_when_policy_locks_mcp() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-agent-mcp-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("managed-settings.json"),
            serde_json::json!({"strictPluginOnlyCustomization": ["mcp"]}).to_string(),
        )
        .unwrap();
        // Declared before the env guard so the settings cache is reset after
        // the managed settings path has been restored.
        let _settings_cache = SettingsCacheGuard;
        let _managed_guard = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);
        // The policy source is served from the process-global settings cache,
        // so an earlier test that read settings pins the managed layer this
        // test just wrote to disk (`settings_cache.rs:10-12`).
        crate::utils::settings::settings_cache::reset_settings_cache();

        let mut agent =
            AgentDefinition::new("custom", "Use custom", AgentDefinitionSource::UserSettings);
        let config = crate::utils::config::McpServerConfig {
            command: Some("definitely-not-executed".to_string()),
            ..crate::utils::config::McpServerConfig::default()
        };
        agent.mcp_servers = Some(vec![AgentMcpServerSpec::Inline {
            name: "local-agent".to_string(),
            config: crate::services::mcp::types::ScopedMcpServerConfig::from_config(
                crate::services::mcp::types::ConfigScope::Dynamic,
                &config,
            ),
        }]);
        let parent_state = crate::state::app_state_store::McpState {
            clients: vec![crate::services::mcp::types::McpServerSnapshot {
                connection_id: None,
                client: crate::services::mcp::types::McpClientSnapshot {
                    name: "parent".to_string(),
                    status: crate::services::mcp::types::McpServerConnectionType::Pending,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: Vec::new(),
                resources: Vec::new(),
            }],
            ..crate::state::app_state_store::McpState::default()
        };

        let initialized =
            futures::executor::block_on(initialize_agent_mcp_servers(&agent, &parent_state));

        let _ = std::fs::remove_dir_all(root);
        assert_eq!(initialized.mcp_state, parent_state);
        assert!(initialized.tools.is_empty());
        assert!(initialized.cleanup_server_names.is_empty());
    }

    /// CC `AgentTool.tsx:833-845`: the worker pool is
    /// `assembleToolPool(workerPermissionContext, appState.mcp.tools)` —
    /// deny-filtered MCP tools are out, and the pool comes from the LIVE app
    /// state rather than the parent's (possibly frontmatter-narrowed)
    /// `toolUseContext.options.tools`, "so they aren't affected by the parent's
    /// tool restrictions" (`:834-837`).
    #[test]
    fn available_tools_for_agent_assembles_the_pool_from_app_state_not_the_parents_tools() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let mcp_tool = |server: &str, name: &str| Tool {
            name: format!("mcp__{server}__{name}"),
            is_mcp: true,
            mcp_info: Some(crate::types::tools::McpToolInfo {
                server_name: server.to_string(),
                tool_name: name.to_string(),
            }),
            ..Default::default()
        };

        let mut initial = crate::state::app_state_store::AppState::default();
        initial.mcp = std::sync::Arc::new(crate::state::app_state_store::McpState {
            tools: vec![
                mcp_tool("untrusted", "read_secret"),
                mcp_tool("trusted", "read_doc"),
            ],
            ..Default::default()
        });
        let store = crate::state::store::AppStore::new(initial, None);
        let mut context = ToolUseContext::default().with_app_store(store);
        // The parent agent was narrowed to a single built-in by its frontmatter.
        context.tools = vec![Tool {
            name: "Read".to_string(),
            ..Default::default()
        }];

        let mut permission_context = crate::tool::ToolPermissionContext::default();
        permission_context.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::LocalSettings,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "mcp__untrusted",
                None,
            )],
        );

        let names = available_tools_for_agent(&context, &permission_context)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert!(
            !names
                .iter()
                .any(|name| name.starts_with("mcp__untrusted__")),
            "denied MCP tools must not reach the worker pool, got {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "mcp__trusted__read_doc"),
            "the worker pool takes MCP tools from app state, got {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "Bash"),
            "the worker pool is rebuilt from getTools, not inherited from the \
             parent's narrowed tools, got {names:?}"
        );
    }

    #[test]
    fn merge_agent_mcp_tools_deduplicates_by_model_visible_name_like_official() {
        let existing = vec![tool("Read"), tool("mcp__docs__search")];
        let incoming = vec![tool("mcp__docs__search"), tool("mcp__agent__lookup")];
        let merged = merge_agent_mcp_tools(existing, incoming);
        assert_eq!(
            merged
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Read", "mcp__docs__search", "mcp__agent__lookup"]
        );
    }

    /// The exact entry list `SidechainTranscriptRecorder` hands to the
    /// JSONL writer: `record_typed_sidechain_transcript` runs the messages
    /// through this before `insert_message_chain`, mirroring CC
    /// `recordSidechainTranscript` → `cleanMessagesForLogging`
    /// (sessionStorage.ts:1456-1457).
    fn sidechain_entries(messages: &[Message]) -> Vec<serde_json::Value> {
        crate::utils::session_storage::typed_messages_as_transcript_values(messages)
    }

    #[test]
    fn sidechain_entry_for_assistant_tool_use_matches_official_message_shape() {
        // The two uuids are deliberately different here. CC has exactly one
        // (`insertMessageChain` spreads `...message`, sessionStorage.ts:1048),
        // and the Rust envelope field is the one every join key in the port
        // holds: the `message_delta` write-back
        // (`session_storage.rs` `apply_assistant_delta`), the REPL history
        // entry lookup, and `REPL.tsx:3521-3525`'s transcript removal
        // (`repl.rs:3883`). The identity block is the out-of-band carrier for
        // `requestId` / `message.id`, and every producer mints its uuid
        // independently (`types/message.rs:340`), so it can never be the row's.
        let message = Message::Assistant(AssistantMessage {
            uuid: "assistant-sidechain".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![
                AssistantContent::ToolUse(ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path":"/tmp/a"}),
                }),
                AssistantContent::MessageIdentity(
                    crate::types::message::AssistantMessageIdentity {
                        request_id: Some("req_sidechain".to_string()),
                        api_message_id: Some("msg_sidechain".to_string()),
                        ..Default::default()
                    },
                ),
            ],
            model: Some("claude-test".to_string()),
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(TokenUsage {
                input_tokens: 1,
                output_tokens: 2,
                cache_creation_input_tokens: 3,
                cache_read_input_tokens: 4,
                cache_deleted_input_tokens: 0,
            }),
        });

        let entries = sidechain_entries(std::slice::from_ref(&message));
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];

        assert_eq!(entry["type"], "assistant");
        assert_eq!(
            entry["uuid"], "assistant-sidechain",
            "the row is written under the ENVELOPE uuid, never the identity block's"
        );
        assert_eq!(entry["requestId"], "req_sidechain");
        assert_eq!(entry["message"]["id"], "msg_sidechain");
        assert_eq!(entry["message"]["role"], "assistant");
        assert_eq!(entry["message"]["model"], "claude-test");
        assert_eq!(entry["message"]["stop_reason"], "tool_use");
        assert_eq!(entry["message"]["usage"]["input_tokens"], 1);
        assert_eq!(entry["message"]["content"][0]["type"], "tool_use");
        assert_eq!(entry["message"]["content"][0]["id"], "toolu_read");
        assert_eq!(entry["message"]["content"][0]["name"], "Read");
        assert_eq!(
            entry["message"]["content"][0]["input"],
            serde_json::json!({"file_path":"/tmp/a"})
        );
    }

    #[test]
    fn sidechain_entry_for_user_tool_result_matches_official_message_shape() {
        let message = Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                content: "done".to_string(),
                is_error: true,
                content_blocks: Vec::new(),
                tool_use_result: Some(serde_json::json!({
                    "durationMs": 7,
                    "numFiles": 1,
                    "filenames": ["src/lib.rs"],
                    "truncated": false
                })),
            })],
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

        let entries = sidechain_entries(std::slice::from_ref(&message));
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];

        assert_eq!(entry["type"], "user");
        assert_eq!(entry["message"]["role"], "user");
        assert_eq!(entry["message"]["content"][0]["type"], "tool_result");
        assert_eq!(entry["message"]["content"][0]["tool_use_id"], "toolu_read");
        assert_eq!(entry["message"]["content"][0]["content"], "done");
        assert_eq!(entry["message"]["content"][0]["is_error"], true);
        assert_eq!(entry["toolUseResult"]["numFiles"], 1);
        assert_eq!(entry["toolUseResult"]["filenames"][0], "src/lib.rs");
    }

    /// CC `recordSidechainTranscript` (sessionStorage.ts:1451-1462) filters
    /// through `cleanMessagesForLogging` → `isLoggableMessage` (:4351-4367)
    /// before writing, so the sidechain JSONL never receives progress, and on
    /// non-`ant` builds never receives attachments — "they have sensitive info
    /// for training that we don't want exposed to the public". The single
    /// exception is `hook_additional_context`, and only behind
    /// `CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT`.
    ///
    /// The regression this pins: `run_agent` pushes the SubagentStart
    /// `hook_additional_context` attachment into `prompt_messages`
    /// (CC runAgent.ts:546-555) and hands the whole vec to
    /// `SidechainTranscriptRecorder::start` (CC :735), so a writer that skips
    /// the filter puts raw hook output on disk with no env gate at all.
    ///
    /// `getUserType() !== 'ant'` is a build audience here (compile-time), so
    /// this asserts against the CURRENT build and both sides are covered by
    /// `just test-all-audiences`.
    #[test]
    fn sidechain_write_set_matches_official_is_loggable_message() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_env::remove("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");

        // Byte-for-byte the attachment `run_agent` pushes after SubagentStart.
        let subagent_start_hook_context = Message::Attachment(
            crate::types::message::AttachmentMessage::new(serde_json::json!({
                "type": "hook_additional_context",
                "content": ["SECRET-FROM-HOOK"],
                "hookName": "SubagentStart",
                "toolUseID": uuid::Uuid::new_v4().to_string(),
                "hookEvent": "SubagentStart",
            })),
        );
        let plain_attachment = Message::Attachment(crate::types::message::AttachmentMessage::new(
            serde_json::json!({"type": "file_read", "path": "/tmp/notes.txt"}),
        ));
        let progress = Message::Progress(crate::types::message::ProgressMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            tool_use_id: "toolu_progress".to_string(),
            parent_tool_use_id: "toolu_progress".to_string(),
            data: crate::types::message::ToolUseProgressMessage::BashProgress {
                output: "tick".to_string(),
                full_output: "tick".to_string(),
                elapsed_time_seconds: 1,
                total_lines: 1,
                total_bytes: None,
                task_id: None,
                timeout_ms: None,
            },
        });
        let prompt = Message::User(UserMessage {
            uuid: "prompt-uuid".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text("do the thing".to_string())],
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
        let internal = crate::utils::build_profile::build_audience().is_internal();

        let entries = sidechain_entries(&[
            prompt.clone(),
            subagent_start_hook_context.clone(),
            plain_attachment.clone(),
            progress,
        ]);
        let types = entries
            .iter()
            .map(|entry| entry["type"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();

        // Progress is dropped on EVERY audience, and never as the `null` line
        // a value-returning serializer would emit.
        assert!(
            !types.iter().any(|kind| kind == "progress"),
            "progress must never reach the sidechain JSONL"
        );
        assert!(
            entries.iter().all(|entry| entry.is_object()),
            "no entry may serialize to a bare JSON value: {entries:?}"
        );
        assert_eq!(types.first().map(String::as_str), Some("user"));

        if internal {
            assert_eq!(types, vec!["user", "attachment", "attachment"]);
        } else {
            assert_eq!(types, vec!["user"]);
            let flat = serde_json::to_string(&entries).expect("entries serialize");
            assert!(
                !flat.contains("SECRET-FROM-HOOK"),
                "SubagentStart hook context leaked into the sidechain: {flat}"
            );
        }

        // Flag on: the hook exception opens on every audience; plain
        // attachments stay withheld externally.
        crate::utils::process_env::set("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT", "1");
        let gated = sidechain_entries(&[subagent_start_hook_context, plain_attachment]);
        assert_eq!(
            gated.len(),
            if internal { 2 } else { 1 },
            "the env flag is not a blanket opt-in for attachments"
        );
        assert_eq!(gated[0]["attachment"]["type"], "hook_additional_context");
        crate::utils::process_env::remove("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");
    }

    #[test]
    fn agent_permission_context_preserves_parent_accept_and_bypass_like_official() {
        let mut parent = crate::tool::ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..Default::default()
        };
        let context = agent_permission_context_for_run(&parent, Some(PermissionMode::Plan), None);
        assert_eq!(context.mode, PermissionMode::AcceptEdits);

        parent.mode = PermissionMode::BypassPermissions;
        let context = agent_permission_context_for_run(&parent, Some(PermissionMode::Plan), None);
        assert_eq!(context.mode, PermissionMode::BypassPermissions);
    }

    #[test]
    fn agent_permission_context_runtime_keeps_parent_mode_when_agent_defines_none() {
        // CC runAgent.ts:421-434 — the RUNTIME context only overrides when
        // the agent DEFINES a permissionMode; the `?? 'acceptEdits'` fallback
        // belongs to the worker TOOL POOL (AgentTool.tsx:838-841), not here.
        let parent = crate::tool::ToolPermissionContext {
            mode: PermissionMode::Default,
            ..Default::default()
        };
        let explicit = agent_permission_context_for_run(&parent, Some(PermissionMode::Plan), None);
        assert_eq!(explicit.mode, PermissionMode::Plan);
        let implicit = agent_permission_context_for_run(&parent, None, None);
        assert_eq!(implicit.mode, PermissionMode::Default);

        let plan_parent = crate::tool::ToolPermissionContext {
            mode: PermissionMode::Plan,
            ..Default::default()
        };
        assert_eq!(
            agent_permission_context_for_run(&plan_parent, None, None).mode,
            PermissionMode::Plan
        );
    }

    #[test]
    fn agent_allowed_tools_match_official_cli_preservation_and_session_replacement() {
        let mut parent = crate::tool::ToolPermissionContext::default();
        parent.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::UserSettings,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Edit", None,
            )],
        );
        parent.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Command,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Grep", None,
            )],
        );
        parent.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::CliArg,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Read", None,
            )],
        );
        let allowed = vec!["Bash(git status)".to_string()];

        let scoped = agent_permission_context_for_run(&parent, None, Some(&allowed));

        assert_eq!(scoped.always_allow_rules.len(), 2);
        assert_eq!(
            scoped.always_allow_rules[&crate::types::permissions::PermissionRuleSource::CliArg],
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Read", None
            )]
        );
        assert_eq!(
            scoped.always_allow_rules[&crate::types::permissions::PermissionRuleSource::Session],
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash",
                Some("git status".to_string())
            )]
        );
    }

    #[test]
    fn filter_incomplete_tool_calls_drops_assistant_orphans_like_official() {
        let orphan = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_orphan".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({}),
            })],
            model: None,
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });
        let complete = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_done".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({}),
            })],
            model: None,
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });
        let result = Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu_done".to_string()),
                content: "ok".to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: None,
            })],
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
        let filtered =
            filter_incomplete_tool_calls(&[orphan.clone(), complete.clone(), result.clone()]);
        assert_eq!(filtered, vec![complete, result]);
    }

    fn user_text(text: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text(text.to_string())],
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

    /// CC `runAgent.ts:370-373` — the fork context is FILTERED and lands in
    /// FRONT of the caller's prompt messages.
    ///
    /// Old shape: `RunAgentInput` had no `fork_context_messages` field at all,
    /// so the parent conversation could not be expressed and every agent began
    /// at its own prompt. There was nothing to assert against.
    #[test]
    fn initial_agent_messages_prefixes_filtered_fork_context_like_official() {
        // The spawning tool_use, still unanswered — exactly the row CC's filter
        // exists to drop (`:369` "avoid API errors").
        let spawning_turn = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_agent_spawn".to_string()),
                name: "Agent".to_string(),
                input: serde_json::json!({}),
            })],
            model: None,
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });
        let parent_turn = user_text("parent turn one");
        let directive = user_text("fork directive");
        let parent_conversation = vec![parent_turn.clone(), spawning_turn];

        let initial = initial_agent_messages(Some(&parent_conversation), vec![directive.clone()]);

        assert_eq!(
            initial,
            vec![parent_turn, directive],
            "the orphaned spawning tool_use is dropped and the parent history \
             precedes the directive"
        );
    }

    /// CC `runAgent.ts:370-371` — `forkContextMessages` absent means NO prefix,
    /// which is what every non-fork caller relies on
    /// (`resumeAgent.ts:187-189` pins it explicitly).
    ///
    /// Old shape: no carrier existed, so this was the only possible behaviour
    /// and untestable as a choice.
    #[test]
    fn initial_agent_messages_without_fork_context_is_the_prompt_alone() {
        let prompt_messages = vec![user_text("plain subagent prompt")];
        assert_eq!(
            initial_agent_messages(None, prompt_messages.clone()),
            prompt_messages
        );
    }

    /// CC `:370` is JS-truthy on an array, where `[]` is truthy and still takes
    /// the filter branch — an empty parent conversation adds nothing, so the
    /// `Option` match agrees with the ternary.
    #[test]
    fn initial_agent_messages_with_empty_fork_context_adds_nothing() {
        let prompt_messages = vec![user_text("directive")];
        assert_eq!(
            initial_agent_messages(Some(&[]), prompt_messages.clone()),
            prompt_messages
        );
    }

    fn read_file_state_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cometix-agent-read-state-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A parent that has genuinely Read the file: the entry a completed
    /// `FileReadTool` call leaves behind (`file_read_tool/mod.rs` source-turn
    /// `readFileState.set`).
    fn parent_context_having_read(path: &std::path::Path, content: &str) -> ToolUseContext {
        let context = ToolUseContext::default();
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: path.display().to_string(),
                content: Some(content.to_string()),
                timestamp_ms: crate::utils::file::get_file_modification_time(path),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });
        context
    }

    fn subagent_context_for(
        parent: &ToolUseContext,
        fork_context_messages: Option<&[Message]>,
    ) -> ToolUseContext {
        crate::utils::forked_agent::create_subagent_context(
            parent,
            crate::utils::forked_agent::SubagentContextOverrides {
                read_file_state: Some(agent_read_file_state(
                    fork_context_messages,
                    &parent.read_file_state,
                )),
                ..Default::default()
            },
        )
    }

    fn edit_input(path: &std::path::Path) -> crate::tools::file_edit_tool::types::FileEditInput {
        crate::tools::file_edit_tool::types::FileEditInput {
            file_path: path.display().to_string(),
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            replace_all: false,
        }
    }

    /// CC `runAgent.ts:375-378`, NON-fork arm — and the reason it matters.
    ///
    /// `read_file_state` is the Read-before-Edit ledger. `FileEditTool`'s
    /// validator opens with (`file_edit_tool/mod.rs:263-268`)
    ///
    /// ```text
    /// let Some(read_state) = context.read_file_state.get(&full_path) else {
    ///     return Err(validation_error(
    ///         "File has not been read yet. Read it first before writing to it.",
    ///         6,
    ///     ));
    /// };
    /// ```
    ///
    /// and then `if last_write_time > read_state.timestamp_ms.unwrap_or(0)`
    /// (`:281-291`). An INHERITED entry clears both rungs, because it carries
    /// the parent's `timestamp_ms` and the parent's `content`.
    /// `FileWriteTool` has the same pair at `file_write_tool/mod.rs:206-230`.
    ///
    /// Old shape: `create_subagent_context` cloned the parent's cache
    /// unconditionally (`utils/forked_agent.rs:94`) and `runAgent`'s
    /// `agentReadFileState` had no counterpart at all, so an ordinary
    /// `Task(...)` subagent — which is handed NONE of the parent's transcript,
    /// so it has no idea which files the parent read or what they contained —
    /// inherited the parent's ledger wholesale and rewrote files it had never
    /// read. The first assertion below got `Ok(..)` instead of the error.
    #[test]
    fn non_fork_subagent_starts_with_an_empty_read_file_state_so_edit_demands_its_own_read() {
        let root = read_file_state_root("non-fork");
        let path = root.join("sample.txt");
        std::fs::write(&path, "old\n").unwrap();
        let parent = parent_context_having_read(&path, "old\n");
        let input = edit_input(&path);

        let child = subagent_context_for(&parent, None);
        let error = crate::tools::file_edit_tool::validate_edit_input(&input, &child)
            .expect_err("a non-fork subagent has read nothing, so Edit must be refused");
        assert_eq!(
            error.message,
            "File has not been read yet. Read it first before writing to it."
        );
        assert_eq!(error.error_code, 6);
        assert!(child.read_file_state.is_empty());

        // The fixture is otherwise valid: the same Edit on the parent — which
        // really did read the file — passes, so the refusal above is the ledger
        // and nothing else.
        crate::tools::file_edit_tool::validate_edit_input(&input, &parent)
            .expect("the parent read the file, so its own Edit is allowed");

        let _ = std::fs::remove_dir_all(root);
    }

    /// CC `runAgent.ts:375-378`, FORK arm: a fork child is handed the parent's
    /// conversation, whose Read `tool_result` rows it can see, so its cache has
    /// to agree with them — `cloneFileStateCache(toolUseContext.readFileState)`.
    ///
    /// `Some(&[])` is covered too: the guard is `forkContextMessages !==
    /// undefined`, so an empty parent conversation still clones. A port that
    /// reached for `is_empty()` instead would drop that case.
    ///
    /// Old shape: this passed, because the clone was unconditional. It is here
    /// so the split cannot be "fixed" by making everyone fresh.
    #[test]
    fn fork_child_inherits_a_clone_of_the_parent_read_file_state() {
        let root = read_file_state_root("fork");
        let path = root.join("sample.txt");
        std::fs::write(&path, "old\n").unwrap();
        let parent = parent_context_having_read(&path, "old\n");
        let input = edit_input(&path);

        let empty_parent_conversation: &[Message] = &[];
        let parent_conversation = vec![user_text("parent turn")];
        for fork_context in [empty_parent_conversation, parent_conversation.as_slice()] {
            let child = subagent_context_for(&parent, Some(fork_context));
            crate::tools::file_edit_tool::validate_edit_input(&input, &child)
                .expect("a fork child inherits the parent's reads");

            // A clone, not the parent's handle: `SharedFileStateCache` is
            // `Arc<Mutex<..>>` and `get` promotes, so a shared handle would let
            // the child reorder and evict the parent's entries.
            assert!(!child.read_file_state.same_identity(&parent.read_file_state));
            child.read_file_state.clear();
            assert!(parent.read_file_state.has(&path));
        }

        let _ = std::fs::remove_dir_all(root);
    }

    /// CC `createFileStateCacheWithSizeLimit(READ_FILE_STATE_CACHE_SIZE)`
    /// (`fileStateCache.ts:101-106`, `:18` `= 100`, `:22` 25 MB default): the
    /// non-fork arm is a size-LIMITED cache built from scratch, not "the
    /// parent's, emptied" — CC never reuses `toolUseContext.readFileState`'s
    /// own `max`/`maxSize` here, and `cloneFileStateCache` then carries the
    /// fresh cache's limits through `createSubagentContext` (`:122-126`).
    ///
    /// The parent below is deliberately given NON-default limits so the two
    /// readings are distinguishable.
    ///
    /// Old shape: no counterpart existed for either half of the ternary, and
    /// `create_subagent_context` cloned the parent — every assertion here read
    /// the parent's `7` / `4096` and its one entry.
    #[test]
    fn fresh_agent_read_file_state_carries_officials_cache_size_limits() {
        let mut parent = ToolUseContext::default();
        parent.read_file_state = crate::tool::SharedFileStateCache::from_snapshot(
            crate::utils::file_state_cache::FileStateCacheSnapshot {
                max_entries: 7,
                max_size_bytes: 4096,
                entries_lru_to_mru: vec![crate::utils::query_helpers::ReadFileStateEntry {
                    path: "/tmp/parent-read.txt".to_string(),
                    content: Some("parent".to_string()),
                    timestamp_ms: Some(1),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Read,
                }],
            },
        );

        for cache in [
            agent_read_file_state(None, &parent.read_file_state),
            subagent_context_for(&parent, None).read_file_state,
        ] {
            let snapshot = cache.cache_snapshot();
            assert!(snapshot.entries_lru_to_mru.is_empty());
            assert_eq!(
                snapshot.max_entries,
                crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE
            );
            assert_eq!(
                snapshot.max_size_bytes,
                crate::utils::file_state_cache::DEFAULT_MAX_CACHE_SIZE_BYTES
            );
        }
    }

    #[test]
    fn forwards_subagent_tool_use_and_result_progress_like_agent_tool_ui() {
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitted_for_callback = emitted.clone();
        let callback = move |progress| {
            emitted_for_callback.lock().unwrap().push(progress);
        };
        // Two real blocks in ONE assistant message: CC normalizes first
        // (AgentTool.tsx:1483), so the text block becomes its own message and
        // is dropped by the tool-content filter while the tool_use is
        // forwarded on its own single-block message.
        let assistant = Message::Assistant(AssistantMessage {
            uuid: "assistant-uuid".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![
                AssistantContent::Text("thinking out loud".to_string()),
                AssistantContent::ToolUse(ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path":"/tmp/file.txt"}),
                }),
            ],
            model: Some("model".to_string()),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });
        forward_subagent_progress_from_message(
            &assistant,
            Some("toolu_parent_agent"),
            "agent-1",
            Some(&callback),
        );

        let user = Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                content: "read ok".to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: None,
            })],
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
        forward_subagent_progress_from_message(
            &user,
            Some("toolu_parent_agent"),
            "agent-1",
            Some(&callback),
        );

        let emitted = emitted.lock().unwrap();
        assert_eq!(emitted.len(), 2);
        // The payload is the WHOLE normalized message (CC `:1498`
        // `message: m`): no field is picked out of it at the producer, and the
        // parent id stays the routing key it always was.
        assert!(matches!(
            &emitted[0],
            crate::types::tools::ToolProgress::AgentProgress {
                parent_tool_use_id,
                message,
                prompt,
                agent_id,
            } if parent_tool_use_id.0 == "toolu_parent_agent"
                && agent_id == "agent-1"
                // CC's loop literal sends an empty prompt (`:1500-1502`).
                && prompt.is_empty()
                && matches!(
                    &**message,
                    Message::Assistant(assistant)
                        // Normalized: the text block rode its own message.
                        if assistant.content.len() == 1
                            && matches!(
                                &assistant.content[0],
                                AssistantContent::ToolUse(tool_use)
                                    if tool_use.id.0 == "toolu_read"
                                        && tool_use.name == "Read"
                                        && tool_use.input["file_path"]
                                            == serde_json::json!("/tmp/file.txt")
                            )
                )
        ));
        assert!(matches!(
            &emitted[1],
            crate::types::tools::ToolProgress::AgentProgress {
                parent_tool_use_id,
                message,
                ..
            } if parent_tool_use_id.0 == "toolu_parent_agent"
                && matches!(
                    &**message,
                    Message::User(user)
                        if matches!(
                            &user.content[0],
                            UserContent::ToolResult(result)
                                if result.tool_use_id.0 == "toolu_read"
                                    && result.content == "read ok"
                                    && !result.is_error
                                    && result.tool_use_result.is_none()
                        )
                )
        ));
    }

    /// CC `AgentTool.tsx:1485-1491` forwards ONLY messages carrying a
    /// `tool_use` / `tool_result` block; a plain assistant text message
    /// produces no progress at all.
    #[test]
    fn text_only_subagent_messages_are_not_forwarded_as_progress() {
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitted_for_callback = emitted.clone();
        let callback = move |progress| {
            emitted_for_callback.lock().unwrap().push(progress);
        };
        let assistant = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("just prose".to_string())],
            model: Some("model".to_string()),
            stop_reason: None,
            usage: None,
        });
        forward_subagent_progress_from_message(
            &assistant,
            Some("toolu_parent_agent"),
            "agent-1",
            Some(&callback),
        );
        assert!(emitted.lock().unwrap().is_empty());
    }

    /// The whole `Assistant → progress → AssistantDelta` chain, driven through
    /// the production functions instead of a fixture that already carries the
    /// final usage.
    ///
    /// CC's stream fills `usage` twice: `message_start` supplies the input and
    /// cache legs with a placeholder `output_tokens` (`claude.ts:1981`
    /// `partialMessage = part.message`, `:2192-2196` builds every per-block
    /// assistant as `{ ...partialMessage, content }`), and `message_delta`
    /// finalises it AFTER the last `content_block_stop` (`:2244-2248`
    /// `lastMsg.message.usage = usage`).
    ///
    /// The progress row is emitted between those two points and keeps the
    /// `message_start` snapshot — in CC, not just here. `normalizeMessages`
    /// builds a FRESH inner message (`utils/messages.ts:760-764`
    /// `message: { ...message.message, content: [_], … }`) so the spread copies
    /// the `usage` pointer of that instant; `:2246` then REPLACES the property
    /// on the original envelope and `updateUsage` (`:2924-2986`) returns a new
    /// object rather than mutating the old one, so the copy cannot observe it.
    /// `ast-grep --lang ts -p '$A.usage = $B'` over `rebuild/src` finds exactly
    /// one site (`claude.ts:2246`) and `-p '$A.usage.$B = $C'` finds none, in
    /// either `ts` or `tsx` — there is no in-place field mutation anywhere, and
    /// the raw SSE stream (`claude.ts:1822-1823`, `:1857`) rules out the SDK
    /// accumulator doing it instead. Nothing rewrites an emitted row either:
    /// `agent_progress` is absent from `EPHEMERAL_PROGRESS_TYPES`
    /// (`sessionStorage.ts:186-193`) and `REPL.tsx:3478-3481` explicitly
    /// forbids replacing it.
    ///
    /// So the two halves diverge BY DESIGN, and both are pinned here: history
    /// converges on the final usage, the rendered progress line keeps the
    /// emission-time total.
    #[test]
    fn agent_progress_usage_is_the_emission_time_snapshot_like_cc() {
        use crate::tools::agent_tool::ui::calculate_agent_stats;
        use crate::types::message::ToolUseProgressMessage;

        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitted_for_callback = emitted.clone();
        let callback = move |progress| {
            emitted_for_callback.lock().unwrap().push(progress);
        };

        // What `message_start` carries: real input/cache legs, placeholder output.
        let start_usage = TokenUsage {
            input_tokens: 120,
            output_tokens: 1,
            cache_creation_input_tokens: 300,
            cache_read_input_tokens: 4_000,
            cache_deleted_input_tokens: 0,
        };
        let assistant = Message::Assistant(AssistantMessage {
            uuid: "assistant-uuid".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path":"/tmp/file.txt"}),
            })],
            model: Some("model".to_string()),
            stop_reason: None,
            usage: Some(start_usage.clone()),
        });

        // The `QueryEvent::Message` arm's order: forward, then retain.
        forward_subagent_progress_from_message(
            &assistant,
            Some("toolu_parent_agent"),
            "agent-1",
            Some(&callback),
        );
        let mut agent_messages = vec![assistant];

        let result = Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                content: "read ok".to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: None,
            })],
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
        forward_subagent_progress_from_message(
            &result,
            Some("toolu_parent_agent"),
            "agent-1",
            Some(&callback),
        );
        agent_messages.push(result);

        // `message_delta`: the real output token count arrives last.
        let final_usage = TokenUsage {
            output_tokens: 350,
            ..start_usage.clone()
        };
        apply_assistant_delta_to_agent_messages(
            &mut agent_messages,
            "assistant-uuid",
            Some(StopReason::EndTurn),
            Some(final_usage.clone()),
        );

        // Half one — the retained history IS the reach of CC's mutation.
        let Message::Assistant(retained) = &agent_messages[0] else {
            panic!("first agent message should be the assistant");
        };
        assert_eq!(retained.usage.as_ref(), Some(&final_usage));
        assert_eq!(retained.stop_reason, Some(StopReason::EndTurn));

        // Half two — the emitted rows, carried through the production REPL
        // converter (`repl.rs` `subagent_progress_render_message`) exactly as
        // the `QueryEvent::ToolProgress` arm does.
        let progress_messages: Vec<ToolUseProgressMessage> = emitted
            .lock()
            .unwrap()
            .iter()
            .map(|progress| {
                let crate::types::tools::ToolProgress::AgentProgress {
                    message,
                    prompt,
                    agent_id,
                    ..
                } = progress
                else {
                    panic!("agent forwarding emits AgentProgress only");
                };
                let rendered =
                    crate::screens::repl::subagent_progress_render_message((**message).clone())
                        .expect("a tool_use / tool_result row always projects");
                ToolUseProgressMessage::AgentProgress {
                    message: Box::new(rendered),
                    prompt: prompt.clone(),
                    agent_id: agent_id.clone(),
                }
            })
            .collect();
        assert_eq!(progress_messages.len(), 2);

        let stats = calculate_agent_stats(&progress_messages);
        assert_eq!(stats.tool_use_count, 1);
        assert_eq!(
            stats.tokens,
            Some(4_421),
            "120 + 1 + 300 + 4000 — the `message_start` snapshot the row was emitted with"
        );
        assert_ne!(
            stats.tokens,
            Some(4_770),
            "the finalised total belongs to the history copy, not to the emitted row"
        );
        assert_eq!(
            crate::components::agent_progress_line::agent_progress_usage_text(
                stats.tool_use_count,
                stats.tokens
            ),
            "1 tool use · 4.4k tokens"
        );
    }

    #[test]
    fn general_purpose_system_prompt_uses_official_builtin_copy() {
        let agent = super::super::built_in::general_purpose_agent::general_purpose_agent();
        let prompt = get_agent_system_prompt(
            &agent,
            &ToolUseContext::default(),
            "claude-sonnet-4-6",
            &[],
            &[tool("Read")],
        )
        .join("\n");
        assert!(prompt.contains("You are an agent for Claude Code"));
        assert!(prompt.contains("NEVER proactively create documentation files"));
        assert!(prompt.contains("<env>"));
        assert!(prompt.contains("Working directory:"));
    }

    /// The `message_delta` write-back is keyed on a uuid this test never sees.
    /// `claude.rs:4104-4137` (`assistant_message_for_completed_block`) stamps a
    /// FRESH envelope uuid on every completed content block, `query.rs`'s
    /// `Assistant` arm re-emits that message as `QueryEvent::Message`, and only
    /// then does `claude.rs:4070-4079`'s `message_delta` become
    /// `QueryEvent::AssistantDelta { uuid: last.uuid }` (`query.rs:2055-2082`).
    /// Any hop that re-stamps the message breaks the match SILENTLY:
    /// `SidechainTranscriptRecorder::apply_assistant_delta` finds nothing, no
    /// error and no panic are produced, and every assistant row in
    /// `agent-<id>.jsonl` keeps `message_start`'s `output_tokens: 0` /
    /// `stop_reason: null` — the exact shape those files had before the
    /// recorder existed.
    ///
    /// The trap is loaded and live: `AssistantMessage` has BOTH a `uuid` FIELD
    /// (`types/message.rs:295`, CC `MessageBase.uuid` — what `Message::uuid()`
    /// returns at `:1762`, what the recorder matches on, and what the JSONL row
    /// is written under) and a `uuid()` METHOD (`types/message.rs:379-381`)
    /// returning the `MessageIdentity` block's uuid — a different value
    /// entirely, since every producer mints the two independently. Reading the
    /// method where the field is meant compiles, runs, and loses the delta.
    ///
    /// So nothing here names a uuid: `call_model` replays the production stream
    /// shape, the REAL `spawn_query` actor runs, every event it emits is piped
    /// through the production consumer
    /// [`record_query_event_to_agent_transcript`], and only what reached disk
    /// is asserted. A test that hand-feeds matching uuids to the recorder
    /// cannot fail this way, which is why it was never a gate for this.
    #[tokio::test]
    async fn sidechain_transcript_captures_the_delta_uuid_the_query_actor_stamped() {
        use crate::utils::session_storage::{
            SidechainTranscriptRecorder, clear_agent_transcript_subdir, flush_session_storage,
            get_agent_transcript_path, is_session_write_enabled, reset_session_file_pointer,
            set_test_projects_dir_override,
        };

        /// `message_start` usage: the input/cache legs are known, the output
        /// count is still the placeholder (`claude.rs:4044-4049`).
        fn message_start_usage() -> TokenUsage {
            TokenUsage {
                input_tokens: 10_140,
                output_tokens: 0,
                cache_creation_input_tokens: 300,
                cache_read_input_tokens: 4_000,
                cache_deleted_input_tokens: 0,
            }
        }

        /// One completed content block, exactly as `claude.rs:4119-4126`
        /// builds it: a fresh envelope uuid, a `MessageIdentity` block holding
        /// its OWN distinct uuid, `message_start`'s usage, no `stop_reason`.
        fn completed_block(text: &str) -> AssistantMessage {
            AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    AssistantContent::Text(text.to_string()),
                    AssistantContent::MessageIdentity(
                        crate::types::message::AssistantMessageIdentity::new(
                            Some("req_streamed".to_string()),
                            Some("msg_streamed".to_string()),
                        ),
                    ),
                ],
                model: Some("claude-sonnet-4-6".to_string()),
                stop_reason: None,
                usage: Some(message_start_usage()),
            }
        }

        fn final_usage() -> TokenUsage {
            TokenUsage {
                input_tokens: 11_741,
                output_tokens: 117,
                ..message_start_usage()
            }
        }

        /// `drain_sdk_message_stream_to_channel` for a two-block response:
        /// `content_block_stop` × 2 then one `message_delta`
        /// (`claude.rs:4050-4079`). The second call streams nothing so the
        /// actor cannot loop.
        #[derive(Clone)]
        struct StreamedTurnDeps {
            /// The envelope uuid per emitted block, captured without the
            /// recorder ever being told it.
            ///
            /// This used to hold `(envelope uuid, identity-block uuid)` pairs
            /// and assert the two were distinct, because the writer could pick
            /// the wrong one. Task #126 removed `AssistantMessageIdentity.uuid`
            /// — a message now has exactly one uuid, as in CC
            /// (`utils/sessionStorage.ts:1048`) — so there is no longer a wrong
            /// one to pick, and the distinctness guard has nothing to guard.
            stamped: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }

        impl crate::query::deps::QueryDeps for StreamedTurnDeps {
            fn call_model(
                &self,
                _request: crate::query::deps::CallModelRequest,
            ) -> crate::query::deps::CallModelStreamFuture {
                let stamped = self.stamped.clone();
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    if !stamped.lock().unwrap().is_empty() {
                        return Ok(rx);
                    }
                    for text in ["thinking out loud", "here is the answer"] {
                        let assistant = completed_block(text);
                        stamped.lock().unwrap().push(assistant.uuid.clone());
                        tx.send(
                            crate::services::api::claude::QueryModelStreamItem::Assistant(
                                assistant,
                            ),
                        )
                        .await
                        .ok();
                    }
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::AssistantDelta {
                            stop_reason: Some(crate::types::message::StopReason::EndTurn),
                            usage: Some(final_usage()),
                        },
                    )
                    .await
                    .ok();
                    Ok(rx)
                })
            }
        }

        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");

        struct PersistenceRestore(bool);
        impl Drop for PersistenceRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_session_persistence_disabled(self.0);
            }
        }
        let _persistence =
            PersistenceRestore(crate::bootstrap::state::is_session_persistence_disabled());
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root =
            std::env::temp_dir().join(format!("cometix-agent-delta-{}", uuid::Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::bootstrap::state::switch_session("session-agent-delta", None);
        crate::utils::session_storage::clear_session_metadata();
        reset_session_file_pointer();

        let agent_id = "aquery-actor-delta";
        clear_agent_transcript_subdir(agent_id);

        // CC `runAgent.ts:735` + `:745`: the run's prompt is recorded first and
        // seeds the parent cursor.
        let prompt = Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text("investigate".to_string())],
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
        let prompt_uuid = prompt.uuid().to_string();
        let mut sidechain =
            SidechainTranscriptRecorder::start(agent_id, std::slice::from_ref(&prompt), &cwd);

        let stamped = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let handle = crate::query::spawn_query(
            crate::query::QueryParams {
                turn_id: "turn-agent-delta".to_string(),
                input: "investigate".to_string(),
                messages: Vec::new(),
                model_messages: vec![prompt.clone()],
                system_prompt: SystemPrompt::new(),
                user_context: std::collections::BTreeMap::new(),
                system_context: std::collections::BTreeMap::new(),
                query_source: QuerySource::Agent,
                token_budget: None,
                task_budget: None,
                max_turns: Some(1),
                tool_use_context: ToolUseContext::default(),
            },
            StreamedTurnDeps {
                stamped: stamped.clone(),
            },
        );

        // Bounded: a stuck actor must fail this test, never hang the gate.
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while let Ok(event) = handle.events.recv().await {
                let terminal = matches!(event, crate::query::QueryEvent::Terminal(_));
                // The production consumer — the same call `run_agent`'s loop
                // makes, on the same borrow, in the same place.
                record_query_event_to_agent_transcript(&mut sidechain, &event);
                if terminal {
                    break;
                }
            }
        })
        .await
        .expect("the query actor must terminate");
        sidechain.flush();
        flush_session_storage().await.unwrap();

        let stamped = stamped.lock().unwrap().clone();
        assert_eq!(stamped.len(), 2, "two completed blocks were streamed");
        let first_envelope = stamped[0].clone();
        let last_envelope = stamped[1].clone();

        let rows: Vec<serde_json::Value> =
            std::fs::read_to_string(get_agent_transcript_path(agent_id))
                .expect("the agent transcript must exist")
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();

        assert_eq!(
            rows.iter()
                .map(|row| row["uuid"].as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            vec![
                prompt_uuid.clone(),
                first_envelope.clone(),
                last_envelope.clone()
            ],
            "the JSONL is written under the ENVELOPE uuid, in arrival order"
        );
        assert_eq!(
            rows.iter()
                .map(|row| row["parentUuid"].as_str())
                .collect::<Vec<_>>(),
            vec![
                None,
                Some(prompt_uuid.as_str()),
                Some(first_envelope.as_str())
            ],
            "the parent chain survives the deferral"
        );

        // The defect: this was `0` / `null` on every agent row, always.
        assert_eq!(rows[2]["message"]["stop_reason"], "end_turn");
        assert_eq!(rows[2]["message"]["usage"]["output_tokens"], 117);
        assert_eq!(rows[2]["message"]["usage"]["input_tokens"], 11_741);
        // CC `claude.ts:2244` mutates `newMessages.at(-1)` only, so the
        // displaced per-block assistant keeps `message_start`'s placeholder
        // upstream too.
        assert_eq!(rows[1]["message"]["usage"]["output_tokens"], 0);
        assert!(rows[1]["message"]["stop_reason"].is_null());

        clear_agent_transcript_subdir(agent_id);
        crate::utils::session_storage::clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// CC gates the query loop's yield/record on `isRecordableMessage`
    /// (`runAgent.ts:231-246`, applied at `:793`): assistant, user, progress,
    /// and system `compact_boundary` — nothing else. Attachments are handled
    /// one branch earlier (`:770-790`) and are yielded WITHOUT being recorded.
    ///
    /// The port had no gate, so every `Message` variant the actor emitted went
    /// to the recorder and only `isLoggableMessage`'s audience check stood
    /// between an attachment and the file. That check passes on `ant`
    /// (`sessionStorage.ts:4357` `getUserType() !== 'ant'`), so an internal
    /// build wrote hook/system-reminder attachment rows CC never writes AND
    /// gave each one the parent cursor: `assistant → user` became
    /// `assistant → attachment → user`, and a resumed agent's chain walk
    /// (`get_agent_transcript_from_path` ← CC `:4210-4224`) reconstructed a
    /// different conversation than CC does from the same run.
    ///
    /// The assertions are audience-INDEPENDENT by design: the gate, not the
    /// privacy filter, is what keeps these off disk, so `just test` and
    /// `just test-ant` must produce the identical file. Before the gate the
    /// two audiences disagreed.
    #[test]
    fn agent_transcript_records_only_the_official_recordable_message_types() {
        use crate::utils::session_storage::{
            SidechainTranscriptRecorder, clear_agent_transcript_subdir, get_agent_transcript_path,
            is_session_write_enabled, reset_session_file_pointer, set_test_projects_dir_override,
        };

        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        // The one attachment CC does hand to the writer is `initialMessages`'
        // trailing hook context, and only behind this flag off-`ant`. Nothing
        // in this test goes through that path, so hold it cleared: any
        // attachment row here came from the loop.
        let _hook_context = EnvVarGuard::unset("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");

        struct PersistenceRestore(bool);
        impl Drop for PersistenceRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_session_persistence_disabled(self.0);
            }
        }
        let _persistence =
            PersistenceRestore(crate::bootstrap::state::is_session_persistence_disabled());
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root =
            std::env::temp_dir().join(format!("cometix-agent-recordable-{}", uuid::Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::bootstrap::state::switch_session("session-agent-recordable", None);
        crate::utils::session_storage::clear_session_metadata();
        reset_session_file_pointer();

        let agent_id = "arecordable-gate";
        clear_agent_transcript_subdir(agent_id);

        fn user(uuid: &str, text: &str) -> Message {
            Message::User(UserMessage {
                uuid: uuid.to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![UserContent::Text(text.to_string())],
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

        let prompt = user("prompt-uuid", "investigate");
        let mut sidechain =
            SidechainTranscriptRecorder::start(agent_id, std::slice::from_ref(&prompt), &cwd);

        // Exactly what the loop sees, in order. Only four of these are
        // recordable in CC.
        let events = vec![
            // `runAgent.ts:770-790`: yielded, never recorded.
            crate::query::QueryEvent::Message(Message::Attachment(
                crate::types::message::AttachmentMessage::new(serde_json::json!({
                    "type": "hook_additional_context",
                    "content": ["SECRET-FROM-HOOK"],
                    "hookName": "SubagentStart",
                    "hookEvent": "SubagentStart",
                })),
            )),
            crate::query::QueryEvent::Message(Message::HookResult(
                crate::types::message::HookResultMessage::attachment(serde_json::json!({
                    "type": "hook_additional_context",
                    "content": ["SECRET-FROM-HOOK-RESULT"],
                })),
            )),
            crate::query::QueryEvent::Message(Message::Assistant(AssistantMessage {
                uuid: "assistant-uuid".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("looking".to_string())],
                model: Some("claude-test".to_string()),
                stop_reason: None,
                usage: None,
            })),
            // A max-turns attachment: `query.rs`'s `emit_max_turns_reached`
            // sends it as a `Message`, CC logs and breaks (`:773-786`).
            crate::query::QueryEvent::Message(Message::Attachment(
                crate::types::message::AttachmentMessage::new(serde_json::json!({
                    "type": "max_turns_reached",
                    "maxTurns": 3,
                })),
            )),
            crate::query::QueryEvent::Message(Message::Progress(
                crate::types::message::ProgressMessage {
                    uuid: "progress-uuid".to_string(),
                    timestamp: chrono::Utc::now(),
                    tool_use_id: "toolu_1".to_string(),
                    parent_tool_use_id: "toolu_1".to_string(),
                    data: crate::types::message::ToolUseProgressMessage::BashProgress {
                        output: "tick".to_string(),
                        full_output: "tick".to_string(),
                        elapsed_time_seconds: 1,
                        total_lines: 1,
                        total_bytes: None,
                        task_id: None,
                        timeout_ms: None,
                    },
                },
            )),
            crate::query::QueryEvent::Message(user("result-uuid", "tool output")),
            // `system` without `subtype === 'compact_boundary'` fails the
            // guard's third clause.
            crate::query::QueryEvent::Message(Message::System(
                crate::types::message::SystemMessage::Informational {
                    base: crate::types::message::SystemBase::with_uuid("system-info-uuid"),
                    content: "heads up".to_string(),
                    level: crate::types::message::SystemMessageLevel::Info,
                    tool_use_id: None,
                    prevent_continuation: None,
                },
            )),
            crate::query::QueryEvent::Message(Message::System(
                crate::types::message::SystemMessage::CompactBoundary {
                    base: crate::types::message::SystemBase::with_uuid("boundary-uuid"),
                    compact_metadata: None,
                    logical_parent_uuid: None,
                },
            )),
        ];
        for event in &events {
            record_query_event_to_agent_transcript(&mut sidechain, event);
        }
        sidechain.flush();
        futures::executor::block_on(crate::utils::session_storage::flush_session_storage())
            .unwrap();

        let rows: Vec<serde_json::Value> =
            std::fs::read_to_string(get_agent_transcript_path(agent_id))
                .expect("the agent transcript must exist")
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();

        assert_eq!(
            rows.iter()
                .map(|row| row["uuid"].as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            vec![
                "prompt-uuid",
                "assistant-uuid",
                "result-uuid",
                "boundary-uuid"
            ],
            "only assistant / user / system compact_boundary reach the file \
             (progress is recordable but `isLoggableMessage` drops it)"
        );
        let flat = std::fs::read_to_string(get_agent_transcript_path(agent_id)).unwrap();
        assert!(
            !flat.contains("SECRET-FROM-HOOK"),
            "no attachment the query loop yields may be persisted on ANY audience: {flat}"
        );

        assert_eq!(
            rows.iter()
                .map(|row| row["parentUuid"].as_str())
                .collect::<Vec<_>>(),
            vec![
                None,
                Some("prompt-uuid"),
                // NOT the attachment or the progress: neither took the cursor.
                Some("assistant-uuid"),
                // CC `insertMessageChain:1039-1041` — a compact boundary roots
                // the chain and keeps its real parent in `logicalParentUuid`.
                None
            ],
            "the rejected messages must not appear in the chain either"
        );
        assert_eq!(rows[3]["logicalParentUuid"].as_str(), Some("result-uuid"));

        clear_agent_transcript_subdir(agent_id);
        crate::utils::session_storage::clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod input_contract_tests {
    //! Source-contract tests for CC `tools/AgentTool/runAgent.ts:380-410,906-933`.

    use super::super::load_agents_dir::AgentDefinitionSource;
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::collections::BTreeMap;

    /// CC `runAgent.ts:913-932` preserves successful strings (including empty);
    /// `prompts.ts:766-792` appends Notes and environment in that order.
    #[test]
    fn get_agent_system_prompt_matches_official_empty_prompt_and_environment_blocks() {
        let mut agent = AgentDefinition::new(
            "custom",
            "This description must never become the system prompt",
            AgentDefinitionSource::ProjectSettings,
        );
        let context = ToolUseContext::default().with_cwd_override(Some("/tmp".into()));
        for original in ["", " \n\t", "A precise instruction."] {
            agent.system_prompt = Some(original.to_string());
            let prompt = get_agent_system_prompt(
                &agent,
                &context,
                "claude-sonnet-4-6",
                &["/tmp/second".to_string(), "/tmp/first".to_string()],
                &[],
            );
            assert_eq!(prompt.len(), 3);
            assert_eq!(prompt[0], original);
            assert!(prompt[1].starts_with("Notes:\n"));
            assert!(prompt[2].starts_with(
                "Here is useful information about the environment you are running in:\n<env>\n"
            ));
            assert!(
                prompt[2].contains("Additional working directories: /tmp/second, /tmp/first\n")
            );
            assert!(!prompt.iter().any(|part| part.contains(&agent.when_to_use)));
        }
        agent.system_prompt = None;
        let fallback = get_agent_system_prompt(&agent, &context, "claude-sonnet-4-6", &[], &[]);
        assert_eq!(fallback[0], crate::constants::prompts::DEFAULT_AGENT_PROMPT);
    }

    /// CC `loadAgentsDir.ts:481-487,726-732` custom closures concatenate memory
    /// within one string; built-in `generalPurposeAgent.ts:24-28` does not.
    #[test]
    fn get_agent_system_prompt_matches_official_custom_memory_inside_original_block() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let previous = crate::utils::process_env::var("CLAUDE_CODE_DISABLE_AUTO_MEMORY");
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
        let root =
            std::env::temp_dir().join(format!("cometix-agent-input-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let mut agent =
            AgentDefinition::new("reviewer", "Review", AgentDefinitionSource::ProjectSettings);
        agent.system_prompt = Some("Review carefully.".to_string());
        agent.memory = Some(super::super::agent_memory::AgentMemoryScope::Project);
        let memory = super::super::agent_memory::load_agent_memory_prompt(
            "reviewer",
            agent.memory.unwrap(),
            &root,
        );
        let prompt = get_agent_system_prompt(&agent, &context, "claude-sonnet-4-6", &[], &[]);
        assert_eq!(prompt.len(), 3);
        assert_eq!(prompt[0], format!("Review carefully.\n\n{memory}"));
        // The closures that append memory belong only to custom-agent loaders.
        agent.source = AgentDefinitionSource::BuiltIn;
        let builtin = get_agent_system_prompt(&agent, &context, "claude-sonnet-4-6", &[], &[]);
        assert_eq!(builtin[0], "Review carefully.");
        std::fs::remove_dir_all(&root).unwrap();
        match previous {
            Some(value) => crate::utils::process_env::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", value),
            None => crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_AUTO_MEMORY"),
        }
    }

    /// The real runAgent → query → API request, with only the HTTP endpoint replaced.
    /// CC `runAgent.ts:391` preserves explicitly provided user maps, including {};
    /// `:408-410` drops gitStatus for Explore/Plan even from an explicit system map.
    #[tokio::test]
    // Keep the process-wide fixture environment pinned until the async request ends.
    #[allow(clippy::await_holding_lock)]
    async fn run_agent_matches_official_explicit_context_overrides_on_model_wire() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // The production CLI installs this provider before constructing API clients.
        crate::utils::tls_provider::install_crypto_provider();
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-agent-input-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        struct Fixture {
            root: std::path::PathBuf,
            previous_cwd: std::path::PathBuf,
            env: Vec<EnvVarGuard>,
            server: Option<tokio::task::JoinHandle<()>>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Some(server) = &self.server {
                    server.abort();
                }
                crate::bootstrap::state::set_original_cwd(self.previous_cwd.clone());
                // Restore the environment here, in the order the variables
                // were set, before the temporary root is removed.
                self.env.clear();
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
        let mut fixture = Fixture {
            root: root.clone(),
            previous_cwd: crate::bootstrap::state::get_original_cwd(),
            env: Vec::new(),
            server: None,
        };
        crate::bootstrap::state::set_original_cwd(root.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env = [
            ("ANTHROPIC_BASE_URL", format!("http://{address}")),
            (
                "ANTHROPIC_API_KEY",
                "sk-ant-test-agent-contract".to_string(),
            ),
            ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".to_string()),
            ("DISABLE_AUTO_COMPACT", "1".to_string()),
            ("NODE_ENV", String::new()),
        ];
        fixture.env = env
            .iter()
            .map(|(key, value)| EnvVarGuard::set(*key, value))
            .collect();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        fixture.server = Some(tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(45), async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0);
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                while request.len() - header_end < length {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
                tx.send(body).await.unwrap();
                let events = [
                    serde_json::json!({"type":"message_start","message":{"id":"msg_contract","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}),
                    serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Done."}}),
                    serde_json::json!({"type":"content_block_stop","index":0}),
                    serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
                    serde_json::json!({"type":"message_stop"}),
                ];
                let response = events
                    .iter()
                    .map(|event| {
                        format!(
                            "event: {}\ndata: {}\n\n",
                            event["type"].as_str().unwrap(),
                            event
                        )
                    })
                    .collect::<String>();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            }
            }).await.expect("mock agent HTTP server deadline");
        }));
        for (agent_type, user_context, expected_claude, expected_git) in [
            (
                "Explore",
                BTreeMap::from([(
                    "claudeMd".to_string(),
                    "CLAUDE_OVERRIDE_SENTINEL".to_string(),
                )]),
                true,
                false,
            ),
            ("Plan", BTreeMap::new(), false, false),
            (
                "reviewer",
                BTreeMap::from([(
                    "claudeMd".to_string(),
                    "CLAUDE_OVERRIDE_SENTINEL".to_string(),
                )]),
                true,
                true,
            ),
        ] {
            let mut agent = AgentDefinition::new(
                agent_type,
                "Test agent",
                AgentDefinitionSource::ProjectSettings,
            );
            agent.system_prompt = Some("SYSTEM_PROMPT_SENTINEL".to_string());
            agent.omit_claude_md = true;
            agent.max_turns = Some(1);
            // CC runAgent.ts:504-506 reads directory keys from live AppState, not
            // bootstrap's separate CLAUDE.md directories or the stale context copy.
            let directories = vec![
                root.join("z-directory").display().to_string(),
                root.join("a-directory").display().to_string(),
            ];
            let permission = crate::utils::permissions::permission_update::apply_permission_update(
                &crate::tool::ToolPermissionContext::default(),
                &crate::types::permissions::PermissionUpdate::AddDirectories {
                    destination: crate::types::permissions::PermissionUpdateDestination::Session,
                    directories: directories.clone(),
                },
            );
            let initial = crate::state::app_state_store::AppState {
                tool_permission_context: std::sync::Arc::new(permission),
                ..Default::default()
            };
            let mut context = ToolUseContext::default()
                .with_app_store(crate::state::store::AppStore::new(initial, None))
                .with_cwd_override(Some(root.clone()))
                .with_main_loop_model("claude-sonnet-4-6");
            // with_app_store initially synchronizes the snapshot; deliberately
            // stale it so a fallback to the context copy would fail this oracle.
            context
                .tool_permission_context
                .additional_working_directories
                .clear();
            assert!(
                context
                    .tool_permission_context
                    .additional_working_directories
                    .is_empty()
            );
            assert_eq!(
                context
                    .get_app_state()
                    .unwrap()
                    .tool_permission_context
                    .additional_working_directories
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                directories
            );
            let input = RunAgentInput {
                agent_definition: &agent,
                prompt: "Inspect.",
                description: None,
                model_override: None,
                context: &context,
                query_source: QuerySource::AgentCustom,
                is_async: false,
                can_show_permission_prompts: None,
                available_tools: Some(Vec::new()),
                fork_context_messages: None,
                preserve_tool_use_results: false,
                r#override: RunAgentOverride {
                    user_context: Some(user_context),
                    system_context: Some(BTreeMap::from([
                        ("gitStatus".to_string(), "STALE_GIT_SENTINEL".to_string()),
                        ("other".to_string(), "SYSTEM_CONTEXT_SENTINEL".to_string()),
                    ])),
                    ..Default::default()
                },
                use_exact_tools: false,
                transcript_subdir: None,
                allowed_tools: None,
                worktree_path: None,
                parent_tool_use_id: None,
                on_progress: None,
                background_task_id: None,
                content_replacement_state: None,
                background_signal: None,
                on_message: None,
                prompt_messages: None,
            };
            let outcome =
                tokio::time::timeout(std::time::Duration::from_secs(15), run_agent(input))
                    .await
                    .unwrap()
                    .unwrap();
            assert!(matches!(outcome, RunAgentOutcome::Completed(_)));
            let body = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("model request capture deadline")
                .expect("model request");
            let system = body["system"]
                .as_array()
                .unwrap()
                .iter()
                .map(|block| block["text"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(system.contains(&format!(
                "Additional working directories: {}\n",
                directories.join(", ")
            )));
            let body = body.to_string();
            assert_eq!(
                body.contains("CLAUDE_OVERRIDE_SENTINEL"),
                expected_claude,
                "{agent_type}: {body}"
            );
            assert_eq!(
                body.contains("STALE_GIT_SENTINEL"),
                expected_git,
                "{agent_type}: {body}"
            );
            assert!(
                body.contains("SYSTEM_CONTEXT_SENTINEL"),
                "{agent_type}: {body}"
            );
            assert!(
                body.contains("SYSTEM_PROMPT_SENTINEL"),
                "{agent_type}: {body}"
            );
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fixture.server.as_mut().unwrap(),
        )
        .await
        .expect("mock server completion deadline")
        .unwrap();
    }
    #[test]
    fn skill_preload_calls_command_callback_and_preserves_metadata_block_order() {
        fn callback(
            command: &crate::commands::Command,
            args: &str,
            _: &ToolUseContext,
        ) -> anyhow::Result<Vec<UserContent>> {
            assert_eq!(args, "", "source preload passes an empty arguments string");
            Ok(vec![
                UserContent::Text(format!("callback:{}", command.name)),
                UserContent::Image {
                    media_type: "image/png".into(),
                    data: "fixture-image".into(),
                },
                UserContent::Text("tail".into()),
            ])
        }
        let root = std::env::temp_dir().join(format!("plugin-callback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("fixture")).unwrap();
        std::fs::write(
            root.join("fixture/SKILL.md"),
            "---\ndescription: Fixture\n---\nWrong DTO body",
        )
        .unwrap();
        let skill = crate::skills::load_skills_dir::load_skills_from_skills_dir(
            &root,
            crate::skills::load_skills_dir::SkillSource::ProjectSettings,
        )
        .into_iter()
        .next()
        .unwrap()
        .skill;
        std::fs::remove_dir_all(root).unwrap();
        let mut first = crate::commands::Command::from_skill(skill);
        first.name = "first".into();
        first.get_prompt_for_command = Some(callback);
        // No SkillCommand DTO: this must invoke the real command callback,
        // rather than bypassing it through the filesystem skill prompt helper.
        first.prompt_command = None;
        let mut second = first.clone();
        second.name = "second".into();
        let mut agent =
            AgentDefinition::new("fixture", "fixture", AgentDefinitionSource::ProjectSettings);
        agent.skills = Some(vec!["second".into(), "first".into()]);
        let messages = preload_agent_skill_messages_from_commands(
            &agent,
            &ToolUseContext::default(),
            &[first, second],
        )
        .unwrap();
        assert_eq!(messages.len(), 2);
        for (message, name) in messages.iter().zip(["second", "first"]) {
            let Message::User(message) = message else {
                panic!("source creates user messages")
            };
            assert_eq!(message.content.len(), 4);
            let expected = format!(
                "<command-message>{name}</command-message>\n<command-name>{name}</command-name>\n<skill-format>true</skill-format>"
            );
            assert!(matches!(&message.content[0],UserContent::MetaText(text) if text==&expected));
            assert!(
                matches!(&message.content[1],UserContent::MetaText(text) if text==&format!("callback:{name}"))
            );
            assert!(
                matches!(&message.content[2],UserContent::MetaImage{media_type,data} if media_type=="image/png" && data=="fixture-image")
            );
            assert!(matches!(&message.content[3],UserContent::MetaText(text) if text=="tail"));
        }
    }
    #[test]
    fn background_agent_plugin_preparation_keeps_published_current_thread_live() {
        const CHILD: &str = "COMETIX_BACKGROUND_AGENT_PLUGIN_CHILD";
        if let Some(root) = crate::utils::process_env::var_os(CHILD) {
            assert!(crate::utils::process_runtime::process_runtime_handle().is_none());
            let root = std::path::PathBuf::from(root);
            crate::utils::process_env::set("CLAUDE_CONFIG_DIR", root.join("config"));
            crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "0");
            crate::utils::process_env::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
            crate::bootstrap::state::set_original_cwd(&root);
            crate::bootstrap::state::set_inline_plugins(vec![root.join("plugin")]);
            crate::commands::clear_commands_cache();
            crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            crate::utils::process_runtime::set_process_runtime_handle(runtime.handle().clone());
            runtime.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    // The context constructor also has an eager agent catalog;
                    // prepare it off-thread, then exercise the REAL run_agent
                    // None-tools branch again, not just this fixture setup.
                    let context = tokio::task::spawn_blocking(ToolUseContext::default)
                        .await
                        .unwrap();
                    assert!(
                        context.tools.iter().any(|tool| {
                            tool.name == "Agent"
                                && tool.description.contains("executor-fixture:reviewer")
                        }),
                        "fixture must really load non-simple plugin agents"
                    );
                    // Verify the actual loader consumed the executable
                    // `arguments` field (argument-hint is display-only), then
                    // clear its caches again so the run still starts cold.
                    let fixture_cwd = root.clone();
                    let catalog = tokio::task::spawn_blocking(move || {
                        crate::commands::get_commands(&fixture_cwd)
                    })
                    .await
                    .unwrap();
                    let broken = crate::commands::find_command("executor-fixture:broken", &catalog)
                        .expect("real plugin command");
                    assert_eq!(
                        broken.prompt_command.as_ref().unwrap().argument_names,
                        vec!["(".to_string()],
                        "fixture must throw in the real callback before reaching query/model",
                    );
                    crate::commands::clear_commands_cache();
                    let mut agent = AgentDefinition::new(
                        "executor-fixture:reviewer",
                        "Local preparation regression",
                        AgentDefinitionSource::Plugin,
                    );
                    agent.skills = Some(vec!["executor-fixture:broken".into()]);
                    let result = run_agent(RunAgentInput {
                        agent_definition: &agent,
                        prompt: "Never reach the model",
                        description: None,
                        model_override: None,
                        context: &context,
                        query_source: QuerySource::AgentCustom,
                        is_async: true,
                        can_show_permission_prompts: None,
                        available_tools: None,
                        fork_context_messages: None,
                        preserve_tool_use_results: false,
                        r#override: RunAgentOverride {
                            user_context: Some(Default::default()),
                            system_context: Some(Default::default()),
                            system_prompt: Some(Vec::new()),
                            ..Default::default()
                        },
                        use_exact_tools: false,
                        transcript_subdir: None,
                        allowed_tools: None,
                        worktree_path: None,
                        parent_tool_use_id: None,
                        on_progress: None,
                        background_task_id: None,
                        content_replacement_state: None,
                        background_signal: None,
                        on_message: None,
                        prompt_messages: None,
                    })
                    .await;
                    // Source raw named-argument RegExp throws during the real
                    // plugin callback. This proves preparation completed and
                    // terminates before query/model construction without a mock.
                    let error = result.err().expect("invalid named regexp must throw");
                    assert!(
                        error.to_string().starts_with("Invalid regular expression:"),
                        "{error}"
                    );
                })
                .await
                .expect("background preparation keeps its process loop running");
            });
            return;
        }
        let root =
            std::env::temp_dir().join(format!("background-agent-plugin-{}", uuid::Uuid::new_v4()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        for dir in [
            "config",
            "plugin/.claude-plugin",
            "plugin/commands",
            "plugin/agents",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join("plugin/.claude-plugin/plugin.json"),
            r#"{"name":"executor-fixture"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("plugin/commands/broken.md"),
            "---\ndescription: Fail before model\narguments: ['(']\n---\nNever execute",
        )
        .unwrap();
        std::fs::write(
            root.join("plugin/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Local fixture\n---\nNever execute",
        )
        .unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tools::agent_tool::run_agent::input_contract_tests::background_agent_plugin_preparation_keeps_published_current_thread_live", "--nocapture"])
            .env(CHILD, &root).current_dir(&root)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "background agent preparation deadlocked: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}
