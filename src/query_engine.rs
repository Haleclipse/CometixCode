//! Headless/SDK query engine.
//!
//! Maps to CC `QueryEngine.ts:130-1295`. This module owns the stateful `QueryEngine`
//! abstraction and its `submitMessage`/`interrupt`/history/context boundary.
//! CLI structured-I/O parsing and control dispatch remain in `cli/print.rs`.

use crate::cli::CliConfig;
use crate::query::{QueryCommand, QueryEvent};
use crate::types::message::{AssistantContent, Message, TokenUsage};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

struct HeadlessCarry {
    history: Vec<Message>,
    context: crate::tool::ToolUseContext,
}

/// Initial history/state supplied by `cli/print.ts` resume/fork loading.
pub(crate) struct QueryEngineResumeSeed {
    history: Vec<Message>,
    processed: Option<crate::utils::session_restore::ProcessedResume>,
    persist_full_history: bool,
}

impl QueryEngineResumeSeed {
    pub(crate) fn new(
        history: Vec<Message>,
        processed: Option<crate::utils::session_restore::ProcessedResume>,
        persist_full_history: bool,
    ) -> Self {
        Self {
            history,
            processed,
            persist_full_history,
        }
    }
}

/// SDK replay metadata carried beside a submitted user message.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QueryEngineReplayInput {
    pub(crate) uuid: Option<String>,
    pub(crate) timestamp: Option<serde_json::Value>,
}

#[derive(Clone, Default)]
struct QueryEngineOverrides {
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    model: Option<String>,
    thinking: Option<crate::utils::thinking::ThinkingConfig>,
    permission_mode: Option<crate::types::permissions::PermissionMode>,
    mcp_state: Option<crate::state::app_state_store::McpState>,
    read_state_seeds: Vec<crate::utils::query_helpers::ReadFileStateEntry>,
}

/// Values consumed by `cli/print.ts` result framing.
#[derive(Default)]
pub(crate) struct QueryEngineOutcome {
    pub(crate) text: String,
    pub(crate) structured: Option<serde_json::Value>,
    pub(crate) usage: TokenUsage,
    pub(crate) turns: usize,
    pub(crate) reason: String,
    pub(crate) stop_reason: Option<String>,
    pub(crate) errors: Vec<String>,
    pub(crate) permission_denials: Vec<serde_json::Value>,
    carry: Option<HeadlessCarry>,
}

/// L1 transport: CC async-generator `yield SDKMessage` ≙ ordered sink calls
/// consumed immediately by `cli/print.rs`; query state and stdout ownership
/// remain separated.
pub(crate) type QueryEngineOutputSink = Arc<dyn Fn(serde_json::Value) + Send + Sync>;
type QueryEnginePermissionFuture =
    Pin<Box<dyn Future<Output = crate::types::permissions::PermissionPromptResponse> + Send>>;
/// The third argument is CC `toolUseContext.agentId` (`Tool.ts:245`), which
/// `cli/structuredIO.ts:601` sends as the `agent_id` key of the `can_use_tool`
/// control request. CC reads it from the `toolUseContext` its `canUseTool`
/// closure is called with; this port's resolver is a free function, so the
/// engine passes the id from the same context it takes the abort controller from.
///
/// The fourth argument is CC `toolUseContext.setAppState` reaching the SDK
/// permission normalizer (`PermissionPromptToolResultSchema.ts:98-104`): an
/// allow's `updatedPermissions` is projected into the live AppState there,
/// BEFORE persistence — same sourcing rule as the other two.
pub(crate) type QueryEnginePermissionResolver = Arc<
    dyn Fn(
            crate::types::permissions::PermissionRequest,
            crate::tool::AbortController,
            Option<String>,
            crate::tool::AppStoreRef,
        ) -> QueryEnginePermissionFuture
        + Send
        + Sync,
>;

#[derive(Default)]
struct ActiveQueryState {
    abort_controller: Option<crate::tool::AbortController>,
    queued_users: usize,
    starting_query: bool,
    pending_abort: bool,
}

type SharedActiveQueryState = Arc<std::sync::Mutex<ActiveQueryState>>;

/// Cloneable stdin-side interrupt/queue handle.
#[derive(Clone)]
pub(crate) struct QueryEngineControl {
    state: SharedActiveQueryState,
}

impl QueryEngineControl {
    pub(crate) fn queue_user(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .queued_users += 1;
    }

    pub(crate) fn cancel_queued_user(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.queued_users = state.queued_users.saturating_sub(1);
    }

    fn begin_user(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.queued_users = state.queued_users.saturating_sub(1);
        state.starting_query = true;
    }

    /// Rust control-handle projection of CC `QueryEngine.ts:1158-1160` `interrupt()`.
    pub(crate) fn interrupt(&self) {
        let abort = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let abort = state.abort_controller.clone();
            if abort.is_none() && (state.queued_users > 0 || state.starting_query) {
                state.pending_abort = true;
            }
            abort
        };
        if let Some(abort) = abort {
            abort.abort();
        }
    }

    fn reset_after_query(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.abort_controller = None;
        state.starting_query = false;
        state.pending_abort = false;
    }
}

/// Maps to CC `QueryEngine.ts:130-183` `QueryEngineConfig`.
pub(crate) struct QueryEngineConfig {
    pub(crate) cli_config: CliConfig,
    pub(crate) schema: Option<serde_json::Value>,
    pub(crate) resume_seed: Option<QueryEngineResumeSeed>,
    pub(crate) mcp_state: Option<crate::state::app_state_store::McpState>,
    pub(crate) output_sink: Option<QueryEngineOutputSink>,
    pub(crate) permission_resolver: Option<QueryEnginePermissionResolver>,
    /// Maps to: CC `QueryEngineConfig.handleElicitation` (QueryEngine.ts:153)
    /// — the print/SDK elicitation forwarding leg carried onto the tool-use
    /// context.
    pub(crate) handle_elicitation: crate::tool::HandleElicitationCallback,
    /// Maps to: CC `QueryEngineConfig.userSpecifiedModel` (QueryEngine.ts:143):
    /// print's `activeUserSpecifiedModel`, first the launch `effectiveModel`
    /// (`print.ts:1220`).
    pub(crate) user_specified_model: Option<String>,
}

/// Maps to CC `QueryEngine.ts:184-1183` `QueryEngine`.
pub(crate) struct QueryEngine {
    config: CliConfig,
    /// The argv `--json-schema` value (CC `options.jsonSchema`).
    schema: Option<serde_json::Value>,
    /// The init-control `jsonSchema` (CC `setInitJsonSchema`,
    /// print.ts:4450-4452 — stored unconditionally, no validation). The two
    /// slots have different winners at the two CC use sites: tool creation
    /// prefers argv (print.ts:1493 `initJsonSchema && !options.jsonSchema`),
    /// the engine-level jsonSchema option prefers init (print.ts:2166
    /// `getInitJsonSchema() ?? options.jsonSchema`).
    init_schema: Option<serde_json::Value>,
    overrides: QueryEngineOverrides,
    /// CC QueryEngineConfig.getAppState/setAppState refer to the print-owned
    /// store (main.tsx:3728-3731), independently of any submit's outcome.
    app_store: Option<crate::state::store::AppStore>,
    carry: Option<HeadlessCarry>,
    resume_seed: Option<QueryEngineResumeSeed>,
    output_sink: Option<QueryEngineOutputSink>,
    permission_resolver: Option<QueryEnginePermissionResolver>,
    handle_elicitation: crate::tool::HandleElicitationCallback,
    session_start_completed: bool,
    control: QueryEngineControl,
}

impl QueryEngine {
    pub(crate) fn new(config: QueryEngineConfig) -> Self {
        let state = SharedActiveQueryState::default();
        Self {
            overrides: QueryEngineOverrides {
                mcp_state: config.mcp_state,
                model: config.user_specified_model,
                ..QueryEngineOverrides::default()
            },
            config: config.cli_config,
            schema: config.schema,
            init_schema: None,
            app_store: None,
            carry: None,
            resume_seed: config.resume_seed,
            output_sink: config.output_sink,
            permission_resolver: config.permission_resolver,
            handle_elicitation: config.handle_elicitation,
            session_start_completed: false,
            control: QueryEngineControl { state },
        }
    }

    pub(crate) fn output_sink(
        emit: impl Fn(serde_json::Value) + Send + Sync + 'static,
    ) -> QueryEngineOutputSink {
        Arc::new(emit)
    }

    pub(crate) fn control_handle(&self) -> QueryEngineControl {
        self.control.clone()
    }

    /// Maps to CC `QueryEngine.ts:209-1156` `QueryEngine.submitMessage()`.
    pub(crate) async fn submit_message(
        &mut self,
        prompt: String,
        replay_input: Option<QueryEngineReplayInput>,
    ) -> Result<QueryEngineOutcome, String> {
        self.control.begin_user();
        let result = run_query(QueryRunInput {
            config: &self.config,
            prompt,
            tool_schema: self.tool_schema().cloned(),
            tool_schema_from_init: self.schema.is_none() && self.init_schema.is_some(),
            engine_schema: self.schema().cloned(),
            app_store: &mut self.app_store,
            stream_json: self.output_sink.is_some(),
            carry: self.carry.take(),
            resume_seed: self.resume_seed.take(),
            replay_input,
            run_session_start_hooks: !self.session_start_completed,
            overrides: &self.overrides,
            active_abort: Some(&self.control.state),
            output_sink: self.output_sink.as_ref(),
            permission_resolver: self.permission_resolver.as_ref(),
            handle_elicitation: self.handle_elicitation.clone(),
        })
        .await;
        self.session_start_completed = true;
        self.overrides.read_state_seeds.clear();
        self.control.reset_after_query();
        if let Ok(outcome) = &result {
            self.carry = outcome.carry.as_ref().map(|carry| HeadlessCarry {
                history: carry.history.clone(),
                context: carry.context.clone(),
            });
        }
        result.map(|mut outcome| {
            outcome.carry = None;
            outcome
        })
    }

    /// Maps to CC `QueryEngine.ts:1158-1160` `QueryEngine.interrupt()`.
    pub(crate) fn interrupt(&self) {
        self.control.interrupt();
    }

    /// Maps to CC `QueryEngine.ts:1162-1164` `QueryEngine.getMessages()`.
    pub(crate) fn get_messages(&self) -> &[Message] {
        self.carry
            .as_ref()
            .map(|carry| carry.history.as_slice())
            .or_else(|| {
                self.resume_seed
                    .as_ref()
                    .map(|seed| seed.history.as_slice())
            })
            .unwrap_or(&[])
    }

    pub(crate) fn push_history(&mut self, message: Message) {
        if let Some(carry) = self.carry.as_mut() {
            carry.history.push(message);
            carry.context.messages = carry.history.clone();
        } else {
            self.resume_seed
                .get_or_insert_with(|| QueryEngineResumeSeed::new(Vec::new(), None, false))
                .history
                .push(message);
        }
    }

    pub(crate) fn context(&self) -> Option<&crate::tool::ToolUseContext> {
        self.carry.as_ref().map(|carry| &carry.context)
    }

    /// Maps to CC `QueryEngine.ts:1166-1168` `QueryEngine.getReadFileState()`.
    pub(crate) fn get_read_file_state(
        &self,
    ) -> Vec<crate::utils::query_helpers::ReadFileStateEntry> {
        self.context()
            .map(|context| context.read_file_state.snapshot())
            .unwrap_or_default()
    }

    /// Maps to CC `QueryEngine.ts:1170-1172` `QueryEngine.getSessionId()`.
    pub(crate) fn get_session_id(&self) -> String {
        crate::bootstrap::state::get_session_id()
    }

    /// The engine-effective schema — CC print.ts:2166
    /// `getInitJsonSchema() ?? options.jsonSchema` (init wins, argv falls
    /// back). Drives `structured_required` and the result-framing flag.
    pub(crate) fn schema(&self) -> Option<&serde_json::Value> {
        self.init_schema.as_ref().or(self.schema.as_ref())
    }

    /// The tool-creation schema — CC print.ts:1492-1498
    /// `initJsonSchema && !options.jsonSchema`: the argv schema wins, the
    /// init-control schema only creates the tool when argv had none.
    fn tool_schema(&self) -> Option<&serde_json::Value> {
        self.schema.as_ref().or(self.init_schema.as_ref())
    }

    /// Maps to: CC `setInitJsonSchema` (print.ts:4450-4452) — the
    /// init-control jsonSchema is stored unconditionally and unvalidated;
    /// schema problems surface only as a silently absent tool at creation
    /// time (main.tsx:2781-2801, telemetry-only failure branch).
    pub(crate) fn set_init_schema(&mut self, schema: serde_json::Value) {
        self.init_schema = Some(schema);
    }

    pub(crate) fn set_system_prompt(&mut self, prompt: String) {
        self.overrides.system_prompt = Some(prompt);
    }

    pub(crate) fn set_append_system_prompt(&mut self, prompt: String) {
        self.overrides.append_system_prompt = Some(prompt);
    }

    /// Maps to CC `QueryEngine.ts:1174-1176` `QueryEngine.setModel()`, which
    /// sets `config.userSpecifiedModel`. Print keeps that value as
    /// `activeUserSpecifiedModel` and passes it to every `ask`.
    pub(crate) fn set_model(&mut self, model: String) {
        self.overrides.model = Some(model);
    }

    /// CC `this.config.userSpecifiedModel`.
    #[cfg(test)]
    pub(crate) fn user_specified_model_for_test(&self) -> Option<&str> {
        self.overrides.model.as_deref()
    }

    pub(crate) fn set_thinking(
        &mut self,
        thinking: Option<crate::utils::thinking::ThinkingConfig>,
    ) {
        self.overrides.thinking = thinking;
    }

    pub(crate) fn set_permission_mode(&mut self, mode: crate::types::permissions::PermissionMode) {
        self.overrides.permission_mode = Some(mode);
    }

    pub(crate) fn seed_read_state(
        &mut self,
        seed: crate::utils::query_helpers::ReadFileStateEntry,
    ) {
        self.overrides
            .read_state_seeds
            .retain(|entry| entry.path != seed.path);
        self.overrides.read_state_seeds.push(seed);
    }

    pub(crate) fn mcp_state(&self) -> Option<&crate::state::app_state_store::McpState> {
        self.overrides.mcp_state.as_ref()
    }

    pub(crate) fn mcp_state_mut(&mut self) -> &mut crate::state::app_state_store::McpState {
        self.overrides
            .mcp_state
            .get_or_insert_with(Default::default)
    }
}

/// Maps to CC `QueryEngine.ts:1186-1295` exported `ask(...)`, retaining one engine across SDK turns.
pub(crate) async fn ask(
    engine: &mut QueryEngine,
    prompt: String,
    replay_input: Option<QueryEngineReplayInput>,
) -> Result<QueryEngineOutcome, String> {
    engine.submit_message(prompt, replay_input).await
}

fn emit_output(sink: Option<&QueryEngineOutputSink>, value: serde_json::Value) {
    if let Some(sink) = sink {
        sink(value);
    }
}

struct QueryRunInput<'a> {
    config: &'a CliConfig,
    prompt: String,
    /// Schema for StructuredOutput tool creation (argv wins, print.ts:1493).
    tool_schema: Option<serde_json::Value>,
    /// Whether `tool_schema` came from the SDK `initialize` control request
    /// rather than argv `--json-schema`. Decides WHERE the tool joins the pool:
    /// argv → `main.tsx:2785` (`tools` before `buildAllTools` sorts), init →
    /// `print.ts:1492-1498` (appended after the sorted pool).
    tool_schema_from_init: bool,
    /// Schema for `structured_required` / result framing (init wins,
    /// print.ts:2166).
    engine_schema: Option<serde_json::Value>,
    app_store: &'a mut Option<crate::state::store::AppStore>,
    stream_json: bool,
    carry: Option<HeadlessCarry>,
    resume_seed: Option<QueryEngineResumeSeed>,
    replay_input: Option<QueryEngineReplayInput>,
    run_session_start_hooks: bool,
    overrides: &'a QueryEngineOverrides,
    active_abort: Option<&'a SharedActiveQueryState>,
    output_sink: Option<&'a QueryEngineOutputSink>,
    permission_resolver: Option<&'a QueryEnginePermissionResolver>,
    handle_elicitation: crate::tool::HandleElicitationCallback,
}

/// Internal Rust lowering of CC `QueryEngine.ts:209-1156` `submitMessage()`
/// onto the shared `query.ts` actor.
async fn run_query(input: QueryRunInput<'_>) -> Result<QueryEngineOutcome, String> {
    let QueryRunInput {
        config,
        mut prompt,
        tool_schema,
        tool_schema_from_init,
        engine_schema,
        app_store,
        stream_json,
        carry,
        resume_seed,
        replay_input,
        run_session_start_hooks,
        overrides,
        active_abort,
        output_sink,
        permission_resolver,
        handle_elicitation,
    } = input;
    // Task-required L2 validation before startup/context construction. QueryEngine
    // owns only the unknown-name error; the source-shaped permission owner parses
    // `--tools`, and the canonical Tool registry owns primary/alias lookup.
    if let Some(raw_tools) = config.tools.as_deref() {
        let requested_tools =
            crate::utils::permissions::permission_setup::parse_base_tools_from_cli(raw_tools);
        let available_tools = tokio::task::spawn_blocking(crate::tools::get_all_base_tools)
            .await
            .map_err(|error| error.to_string())?;
        let unknown_tools = requested_tools
            .into_iter()
            .filter(|name| crate::types::tools::find_tool_by_name(&available_tools, name).is_none())
            .collect::<Vec<_>>();
        if !unknown_tools.is_empty() {
            return Err(format!(
                "Unknown tool(s) passed to --tools: {}",
                unknown_tools.join(", ")
            ));
        }
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    // A4: source command loading is awaited; the process executor must stay
    // available for the canonical plugin promises while this sync facade waits.
    let command_cwd = cwd.clone();
    let commands = Arc::new(
        tokio::task::spawn_blocking(move || crate::commands::get_commands(&command_cwd))
            .await
            .map_err(|error| error.to_string())?,
    );
    let deps = crate::query::deps::ProductionDeps;
    let submitter =
        crate::utils::handle_prompt_submit::HandlePromptSubmit::new(deps, commands.clone());
    let settings = crate::utils::settings::get_settings_with_errors();
    if settings.settings.cleanup_period_days == Some(0) {
        crate::bootstrap::state::set_session_persistence_disabled(true);
    }
    let workspace_trusted = crate::utils::config::check_has_trust_dialog_accepted();
    // CC main.tsx:2596-2634 initializes permissions once; QueryEngine.ts:273
    // reads the existing getAppState store on later turns.
    // Re-running startup here would re-stat directories and repeat warnings.
    let mut initial_state = if let Some(store) = app_store.as_ref() {
        store.get().as_ref().clone()
    } else {
        let startup_settings = settings.settings.clone();
        let startup_errors = settings.errors.clone();
        let startup_config = config.clone();
        tokio::task::spawn_blocking(move || {
            crate::main::build_headless_initial_app_state(
                &startup_settings,
                &startup_errors,
                workspace_trusted,
                &startup_config,
            )
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?
    };
    if let Some(processed) = resume_seed
        .as_ref()
        .and_then(|seed| seed.processed.as_ref())
    {
        processed.apply_to_app_state(&mut initial_state);
    }
    if app_store.is_none() {
        crate::main::initialize_lsp_if_trusted(workspace_trusted);
        initial_state.mcp = Arc::new(overrides.mcp_state.clone().unwrap_or_default());
    }
    let permission = (*initial_state.tool_permission_context).clone();
    // CC main.tsx:3728-3731: `createStore(headlessInitialState, onChangeAppState)`
    // — the headless store shares the canonical onChange choke point (mode
    // externalization, model/verbose/expandedView persistence, settings-driven
    // auth-cache reset). (2026-08-02 2nd addendum: was `None`, which silently
    // bypassed all of it.)
    let app_store = app_store
        .get_or_insert_with(|| {
            crate::state::store::AppStore::new(
                initial_state.clone(),
                Some(crate::state::on_change_app_state::default_on_change()),
            )
        })
        .clone();
    // This existing constructor also builds the eager tool registry; keep
    // that read off the single-thread process executor as well.
    let context_permission = permission.clone();
    let mut context = tokio::task::spawn_blocking(move || {
        crate::tool::ToolUseContext::with_permission_context(context_permission)
    })
    .await
    .map_err(|error| error.to_string())?;
    context.app_store = crate::tool::AppStoreRef::new(app_store);
    context.commands = commands;
    context.agent_definitions = initial_state.agent_definitions.clone();
    context.mcp_state = initial_state.mcp.as_ref().clone();
    context.is_non_interactive_session = true;
    // Maps to CC `QueryEngine.ts:360-361` `customSystemPrompt, appendSystemPrompt`
    // on the tool-use context options. `query.ts:685-686` reads the latter back
    // as `hasAppendSystemPrompt`, which is what flips `getCLISyspromptPrefix`
    // from "You are a Claude agent, built on Anthropic's Claude Agent SDK." to
    // the "...running within the Claude Agent SDK." preset for `-p
    // --append-system-prompt` runs — the block the server's cache policy
    // prefix-matches on.
    let (custom_system_prompt, append_system_prompt) =
        resolve_system_prompt_overrides(config, overrides)?;
    context.custom_system_prompt = custom_system_prompt;
    context.append_system_prompt = append_system_prompt;
    let initial_main_loop_model = initial_main_loop_model(overrides);
    context.main_loop_model = Some(initial_main_loop_model.clone());
    context.thinking_config = Some(resolve_thinking(config));
    context.effort_value = initial_state.effort_value.clone();
    // CC QueryEngine.ts:348/496 — the config's handleElicitation leg rides the
    // tool-use context so MCP URL-elicitation retries reach the SDK consumer.
    context.handle_elicitation = handle_elicitation;
    context.fast_mode = Some(initial_state.fast_mode);
    context.max_budget_usd = config
        .max_budget_usd
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok());
    context.fallback_model = config.fallback_model.as_deref().map(|fallback| {
        if fallback == "default" {
            crate::utils::model::model::get_default_main_loop_model()
        } else {
            crate::utils::model::model::parse_user_specified_model(fallback)
        }
    });
    let structured_required = engine_schema.is_some();
    // The Rust engine derives from `CliConfig` what CC threads through three
    // layers, so the two upstream owners are called here rather than copied:
    //   main.tsx:2755-2785  -> `crate::main::build_headless_tools` (the `tools`
    //                          argument, incl. the argv `--json-schema` tool)
    //   print.ts:1474-1500  -> `crate::cli::print::build_all_tools`
    //                          (`assembleToolPool` + `mergeAndFilterTools`,
    //                          plus the init-control schema tool)
    // Both re-enter the eager tool registry, hence `spawn_blocking` (see
    // `build_all_tools_off_executor`). `tool_schema` reaches exactly one of
    // them: argv → main.tsx (sorted into the built-ins), init → print.ts
    // (appended after the sort).
    let (argv_json_schema, init_json_schema) = if tool_schema_from_init {
        (None, tool_schema)
    } else {
        (tool_schema, None)
    };
    let tool_permission = permission.clone();
    let launch_tools = tokio::task::spawn_blocking(move || {
        crate::main::build_headless_tools(&tool_permission, argv_json_schema.as_ref())
    })
    .await
    .map_err(|error| error.to_string())?;
    // CC main.tsx:2781-2801 / print.ts:1494-1497: an invalid schema means the
    // tool is silently absent — creation failure never fails the query itself.
    let init_schema_tool = init_json_schema.and_then(|schema| {
        crate::tools::synthetic_output_tool::create_synthetic_output_tool(schema).ok()
    });
    context.tools = build_all_tools_off_executor(
        launch_tools.clone(),
        permission.clone(),
        context.mcp_state.tools.clone(),
        init_schema_tool.clone(),
    )
    .await?;
    let mut prior_history = Vec::new();
    let mut persist_full_history = false;
    if let Some(seed) = resume_seed {
        prior_history = seed.history;
        persist_full_history = seed.persist_full_history;
        if let Some(processed) = seed.processed {
            context = context
                .with_resume_restore_stores(processed.resume_restore_stores.as_ref().clone());
        }
        context.messages = prior_history.clone();
    }
    if let Some(carry) = carry {
        context = carry.context;
        // QueryEngine reuses its session cache but each submit constructs new
        // optional trigger Sets, matching QueryEngine.ts's processUserInput
        // context literals rather than retaining a prior submit's Set identity.
        context.nested_memory_attachment_triggers = context
            .nested_memory_attachment_triggers
            .as_ref()
            .map(|_| crate::tool::SharedOrderedTriggerSet::fresh());
        context.dynamic_skill_dir_triggers = context
            .dynamic_skill_dir_triggers
            .as_ref()
            .map(|_| crate::tool::SharedOrderedTriggerSet::fresh());
        context.structured_output = None;
        // CC `print.ts:2004` calls `buildAllTools(appState)` on EVERY `ask()`,
        // so a carried-over context gets the pool rebuilt from the current
        // launch tools / permission context / MCP set rather than patched.
        context.tools = build_all_tools_off_executor(
            launch_tools.clone(),
            context.tool_permission_context.clone(),
            context.mcp_state.tools.clone(),
            init_schema_tool.clone(),
        )
        .await?;
        prior_history = carry.history;
        context.messages = prior_history.clone();
    }
    if let Some(mcp_state) = &overrides.mcp_state {
        context.mcp_state = mcp_state.clone();
        // Maps to CC `cli/print.ts:1474-1486` `buildAllTools`, which re-runs
        // `assembleToolPool(appState.toolPermissionContext, appState.mcp.tools)`
        // when late-connecting/dynamic servers change the MCP set — the
        // replacement set is deny-filtered and re-sorted with the rest.
        context.tools = build_all_tools_off_executor(
            launch_tools.clone(),
            context.tool_permission_context.clone(),
            context.mcp_state.tools.clone(),
            init_schema_tool.clone(),
        )
        .await?;
    }
    // CC computes `initialMainLoopModel` on every submit; a carried context
    // still holds the previous turn's.
    context.main_loop_model = Some(initial_main_loop_model.clone());
    if let Some(thinking) = &overrides.thinking {
        context.thinking_config = Some(thinking.clone());
    }
    if let Some(mode) = overrides.permission_mode {
        let mut permission = context.tool_permission_context.clone();
        permission.mode = mode;
        context.update_permission_context(permission);
    }
    for seed in &overrides.read_state_seeds {
        context.read_file_state.set_entry(seed.clone());
    }
    let mut prior_persist_start = prior_history.len();
    if run_session_start_hooks {
        let source = if prior_history.is_empty() {
            "startup"
        } else {
            "resume"
        };
        let hook_messages = crate::utils::session_start::process_session_start_hooks_async(
            source,
            Some(&crate::bootstrap::state::get_session_id()),
            initial_state.agent.as_deref(),
            context.main_loop_model.as_deref(),
        )
        .await;
        if !hook_messages.is_empty() {
            prior_persist_start = prior_history.len();
            let (model_messages, _) =
                crate::utils::session_start::project_hook_result_messages(&hook_messages);
            prior_history.extend(model_messages);
            context.messages = prior_history.clone();
        }
        if let Some(initial_user_message) = crate::utils::session_start::take_initial_user_message()
        {
            prompt = format!("{initial_user_message}\n{prompt}");
        }
    }
    if context.abort_controller.is_aborted() {
        context.abort_controller = crate::tool::AbortController::default();
    }
    let command_permission_store = context.app_store.store.clone();
    // Headless input carries no pasted images.
    // Maps to QueryEngine.ts:416 await processUserInput. The existing Rust
    // async submitter still includes synchronous local command callbacks; run
    // that whole subtree off the print executor, not just a warmed catalog.
    let submitted = tokio::task::spawn_blocking(move || {
        crate::utils::process_runtime::block_on_from_sync(async move {
            submitter
                .submit_prompt_deferred_query_with_context(prompt, context, Vec::new(), Vec::new())
                .await
        })
    })
    .await
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "Unable to initialize input processing runtime".to_string())?;
    // Maps to CC `QueryEngine.ts:476-486`: every processed headless turn
    // replaces the command-scoped allow bucket, including the empty reset on a
    // normal prompt or non-querying local command.
    let command_rules = submitted
        .allowed_tools
        .iter()
        .map(|rule| {
            crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(
                rule,
            )
        })
        .collect::<Vec<_>>();
    let mut command_permission_context = command_permission_store
        .as_ref()
        .map(crate::state::store::AppStore::tool_permission_context)
        .or_else(|| {
            submitted
                .query_params
                .as_ref()
                .map(|params| params.tool_use_context.tool_permission_context.clone())
        })
        .unwrap_or_default();
    command_permission_context.always_allow_rules.insert(
        crate::types::permissions::PermissionRuleSource::Command,
        command_rules,
    );
    if let Some(store) = &command_permission_store {
        store.set_tool_permission_context(command_permission_context.clone());
    }
    let Some(mut base) = submitted.query_params else {
        if let Some(active_abort) = active_abort {
            let mut state = active_abort
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.abort_controller = None;
            state.starting_query = false;
            state.pending_abort = false;
        }
        return Err("prompt produced no model query".to_string());
    };
    base.tool_use_context.tool_permission_context = command_permission_context;
    // Maps to CC `QueryEngine.ts:292-298` `fetchSystemPromptParts({...,
    // additionalWorkingDirectories: Array.from(initialAppState
    // .toolPermissionContext.additionalWorkingDirectories.keys()), mcpClients})`.
    let additional_working_directories: Vec<String> = base
        .tool_use_context
        .tool_permission_context
        .additional_working_directories
        .keys()
        .cloned()
        .collect();
    // Maps to QueryEngine.ts:292 await fetchSystemPromptParts; this subtree
    // also reads the skill catalog, including after settings/cache invalidation.
    let prompt_config = config.clone();
    let prompt_tools = base.tool_use_context.tools.clone();
    let prompt_clients = base.tool_use_context.mcp_state.clients.clone();
    let prompt_overrides = overrides.clone();
    // `fetchSystemPromptParts({ mainLoopModel: initialMainLoopModel })`: the
    // value the context took, computed once per submit.
    let prompt_model = initial_main_loop_model.clone();
    let (system_prompt, mut user_context, system_context) =
        tokio::task::spawn_blocking(move || {
            resolve_system_prompt(
                &prompt_config,
                &prompt_model,
                &prompt_tools,
                &additional_working_directories,
                &prompt_clients,
                &prompt_overrides,
            )
        })
        .await
        .map_err(|error| error.to_string())??;
    // Maps to: CC `QueryEngine.ts:301-307`: coordinator context is an
    // entrypoint-owned addition to fetchSystemPromptParts' base user context.
    let mcp_names: Vec<&str> = base
        .tool_use_context
        .mcp_state
        .clients
        .iter()
        .map(|server| server.client.name.as_str())
        .collect();
    let scratchpad_dir = crate::utils::permissions::filesystem::is_scratchpad_enabled()
        .then(crate::utils::permissions::filesystem::get_scratchpad_dir);
    user_context.extend(
        crate::coordinator::coordinator_mode::get_coordinator_user_context(
            &mcp_names,
            scratchpad_dir.as_deref(),
        ),
    );
    if let Some(active_abort) = active_abort {
        let controller = base.tool_use_context.abort_controller.clone();
        let should_abort = {
            let mut state = active_abort
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.abort_controller = Some(controller.clone());
            state.starting_query = false;
            std::mem::take(&mut state.pending_abort)
        };
        if should_abort {
            controller.abort();
        }
    }
    if stream_json {
        emit_output(
            output_sink,
            crate::utils::messages::system_init::build_system_init_message(&base.tool_use_context),
        );
        if config.replay_user_messages {
            // Maps to: CC `QueryEngine.ts:466-474` — the replay-ack set is
            // filtered from the SUBMITTED messages, not minted from the raw
            // prompt: user rows that are not meta, carry no tool result, and
            // pass `selectableUserMessagesFilter` (MessageSelector.tsx:895).
            // Compact boundaries join CC's set but its ack loop only yields
            // `type === 'user'` entries (`:735-750`), so they emit nothing.
            // Declared deviation: CC acks after the first transcript record
            // inside the query loop; this emits at the same pre-query slot
            // the init message uses.
            if base.messages.is_empty() {
                // Legacy pure-prompt shim: the prompt itself is the one
                // user-authored message.
                emit_output(
                    output_sink,
                    replayed_user_message_value(&base.input, replay_input.as_ref()),
                );
            } else {
                for row in base.messages.iter().filter(|row| replayable_user_row(row)) {
                    emit_output(output_sink, replayed_row_value(row));
                }
            }
        }
    }
    let mut history = prior_history;
    let new_history_start = if persist_full_history {
        0
    } else {
        prior_persist_start
    };
    // C3c-3: submitted rows carry their whole `UserMessage`; history
    // extends with those directly. The old row back-projection retired; a
    // rows-empty submit still seeds from `input` inside the query actor's
    // legacy-parameter shim.
    history.extend(crate::query::user_model_messages_from_rows(&base.messages));
    let mut context = base.tool_use_context;
    // Maps to CC `QueryEngine.submitMessage()` file-history snapshot gate.
    // SDK checkpointing is opt-in through `file_history_enabled()` and only
    // runs for persisted sessions.
    if crate::utils::env_utils::is_cometix_write_enabled()
        && crate::utils::file_history::file_history_enabled()
        && crate::utils::session_storage::is_session_write_enabled()
    {
        if let Some(store) = context.app_store.store.as_ref() {
            crate::utils::file_history::file_history_make_snapshot(store, &base.turn_id).await;
        }
    }
    let mut outcome = QueryEngineOutcome::default();
    let max_attempts = crate::utils::process_env::var("MAX_STRUCTURED_OUTPUT_RETRIES")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5);
    let mut attempt = 0usize;

    loop {
        attempt += 1;
        let params = crate::query::QueryParams {
            turn_id: uuid::Uuid::new_v4().to_string(),
            input: if history.is_empty() {
                base.input.clone()
            } else {
                String::new()
            },
            messages: if history.is_empty() {
                base.messages.clone()
            } else {
                Vec::new()
            },
            model_messages: history.clone(),
            query_source: crate::constants::query_source::QuerySource::Sdk,
            token_budget: base.token_budget,
            task_budget: config
                .task_budget
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                .map(|total| crate::services::api::claude::TaskBudget {
                    total,
                    remaining: None,
                })
                .or_else(|| base.task_budget.clone()),
            max_turns: config
                .max_turns
                .as_deref()
                .and_then(|value| value.parse::<u32>().ok())
                .or(base.max_turns),
            tool_use_context: context.clone(),
            system_prompt: system_prompt.clone(),
            user_context: user_context.clone(),
            system_context: system_context.clone(),
        };
        // A4: the native query constructor synchronously hydrates empty tool
        // pools before starting its actor. Let the published process executor
        // keep driving plugin readers during that fallback. Preserve the scope
        // that spawn_query captures for the actor before crossing the boundary.
        let query_teammate_scope = crate::utils::teammate_context::capture_teammate_context();
        let handle = tokio::task::spawn_blocking(move || {
            crate::utils::teammate_context::with_teammate_context_sync(query_teammate_scope, || {
                crate::query::spawn_query(params, deps)
            })
        })
        .await
        .map_err(|error| error.to_string())?;

        while let Ok(event) = handle.events.recv().await {
            match event {
                QueryEvent::Stream(crate::types::message::StreamEvent::ApiEvent {
                    event, ..
                }) => {
                    if let Some(stop_reason) = event
                        .pointer("/delta/stop_reason")
                        .and_then(serde_json::Value::as_str)
                    {
                        outcome.stop_reason = Some(stop_reason.to_string());
                    }
                    if stream_json && config.include_partial_messages {
                        emit_output(
                            output_sink,
                            serde_json::json!({
                                "type": "stream_event",
                                "event": event,
                                "session_id": crate::bootstrap::state::get_session_id(),
                                "parent_tool_use_id": null,
                                "uuid": uuid::Uuid::new_v4().to_string(),
                            }),
                        );
                    }
                }
                // C3c-3: `Message` carries a whole model message (the seam's
                // CC yield); the headless engine consumes it exactly like the
                // history-only `ModelMessage` transport (no row projection).
                QueryEvent::Message(message) | QueryEvent::ModelMessage(message) => {
                    if let Message::Assistant(assistant) = &message {
                        if let Some(stop_reason) = &assistant.stop_reason {
                            outcome.stop_reason = serde_json::to_value(stop_reason)
                                .ok()
                                .and_then(|value| value.as_str().map(str::to_string));
                        }
                        for content in &assistant.content {
                            if let AssistantContent::Text(text) = content {
                                outcome.text.push_str(text);
                            }
                        }
                        if let Some(usage) = &assistant.usage {
                            add_usage(&mut outcome.usage, usage);
                        }
                    }
                    history.push(message.clone());
                    if stream_json {
                        if let Some(value) = stream_message_value(&message) {
                            emit_output(output_sink, value);
                        }
                    }
                }
                // CC claude.ts:2229-2248 / QueryEngine.ts:718-726: the last
                // per-block assistant is mutated in place after message_delta;
                // the engine's copy converges here before the post-turn
                // transcript record, exactly like CC's lazy jsonStringify
                // observing the mutated reference. The usage total swaps the
                // pre-delta contribution for the final one.
                QueryEvent::AssistantDelta {
                    uuid,
                    stop_reason,
                    usage,
                } => {
                    if let Some(Message::Assistant(assistant)) = history
                        .iter_mut()
                        .rev()
                        .find(|message| message.uuid() == uuid.as_str())
                    {
                        if let Some(old) = &assistant.usage {
                            sub_usage(&mut outcome.usage, old);
                        }
                        if let Some(new) = &usage {
                            add_usage(&mut outcome.usage, new);
                        }
                        if stop_reason.is_some() {
                            outcome.stop_reason = serde_json::to_value(&stop_reason)
                                .ok()
                                .and_then(|value| value.as_str().map(str::to_string));
                        }
                        assistant.stop_reason = stop_reason;
                        assistant.usage = usage;
                    }
                }
                QueryEvent::StreamRequestStart => {
                    outcome.turns += 1;
                    outcome.text.clear();
                }
                QueryEvent::StructuredOutput(value) => outcome.structured = Some(value),
                QueryEvent::ToolContextUpdate(next) => {
                    if next.structured_output.is_some() {
                        outcome.structured = next.structured_output.clone();
                    }
                    context = next.as_ref().clone();
                }
                QueryEvent::PermissionContextUpdate(next) => {
                    context.update_permission_context(next);
                }
                QueryEvent::PermissionRequest(request) => {
                    if let Some(permission_resolver) = permission_resolver {
                        let tool_use_id = request.tool_use_id.clone();
                        let response = permission_resolver(
                            request.clone(),
                            context.abort_controller.clone(),
                            context.agent_id.clone(),
                            context.app_store.clone(),
                        )
                        .await;
                        if matches!(
                            response.choice,
                            crate::types::permissions::PermissionPromptChoice::Deny
                        ) {
                            outcome
                                .permission_denials
                                .push(permission_denial_value(&request));
                        }
                        let _ = handle
                            .commands
                            .send(QueryCommand::PermissionResponse {
                                tool_use_id,
                                response,
                            })
                            .await;
                        continue;
                    }
                    outcome
                        .permission_denials
                        .push(permission_denial_value(&request));
                    let _ = handle
                        .commands
                        .send(QueryCommand::PermissionDecision {
                            tool_use_id: request.tool_use_id,
                            choice: crate::types::permissions::PermissionPromptChoice::Deny,
                        })
                        .await;
                }
                QueryEvent::ApiError(error) => outcome.errors.push(error.error),
                QueryEvent::Terminal(terminal) => {
                    outcome.reason = terminal.reason;
                    if outcome.reason == "max_turns" && outcome.errors.is_empty() {
                        if let Some(max_turns) = config.max_turns.as_deref() {
                            outcome
                                .errors
                                .push(format!("Reached maximum number of turns ({max_turns})"));
                        }
                    } else if outcome.reason.starts_with("aborted_") && outcome.errors.is_empty() {
                        outcome.errors.push("Request aborted".to_string());
                    }
                    break;
                }
                _ => {}
            }
            if let Some(max_budget_usd) = config
                .max_budget_usd
                .as_deref()
                .and_then(|value| value.parse::<f64>().ok())
            {
                if crate::cost_tracker::get_total_cost() >= max_budget_usd {
                    handle.abort_controller.abort();
                    outcome.reason = "max_budget_usd".to_string();
                    outcome
                        .errors
                        .push(format!("Reached maximum budget (${max_budget_usd})"));
                    break;
                }
            }
        }

        // L1 stand-in for CC's Stop-hook enforcement: QueryEngine.ts:330-333
        // registers `registerStructuredOutputEnforcement` (a Stop function
        // hook, hookHelpers.ts:70-83) and the hook engine injects the same
        // copy when the turn stops without a successful StructuredOutput
        // call. The Rust function-hook registration exists and is faithful
        // (hook_helpers.rs `register_structured_output_enforcement`) but the
        // Stop-evaluation side of the function-hook engine is not wired into
        // the query loop yet, so this manual nudge-and-retry reproduces the
        // observable behavior (same message, same keep-going semantics)
        // until that wiring lands (separate batch).
        if structured_required
            && outcome.structured.is_none()
            && outcome.errors.is_empty()
            && attempt < max_attempts
        {
            let nudge = crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(format!(
                    "You MUST call the {} tool to complete this request. Call this tool now.",
                    crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME
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
            };
            history.push(Message::User(nudge));
            outcome.text.clear();
            outcome.reason.clear();
            continue;
        }
        break;
    }

    // CC never errors on repeated StructuredOutput calls: QueryEngine.ts:838
    // overwrites `structuredOutputFromTool` per attachment, so the last call
    // wins silently. The former "must be called exactly once" error here had
    // no CC counterpart.
    let persistence_error =
        match crate::utils::session_storage::record_typed_messages(&history[new_history_start..]) {
            Err(error) => Some(error.to_string()),
            Ok(()) if crate::utils::session_storage::is_session_write_enabled() => {
                crate::utils::session_storage::flush_session_storage()
                    .await
                    .err()
                    .map(|error| error.to_string())
            }
            Ok(()) => None,
        };
    if let Some(error) = persistence_error {
        outcome
            .errors
            .push(format!("Failed to persist session transcript: {error}"));
        if outcome.reason.is_empty() || outcome.reason == "completed" {
            outcome.reason = "session_persistence_error".to_string();
        }
    }
    context.messages = history.clone();
    if context.abort_controller.is_aborted() {
        context.abort_controller = crate::tool::AbortController::default();
    }
    outcome.carry = Some(HeadlessCarry { history, context });
    if let Some(active_abort) = active_abort {
        let mut state = active_abort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.abort_controller = None;
        state.starting_query = false;
        state.pending_abort = false;
    }
    Ok(outcome)
}

pub(crate) fn replayed_user_message_value(
    prompt: &str,
    input: Option<&QueryEngineReplayInput>,
) -> serde_json::Value {
    let uuid = input
        .and_then(|input| input.uuid.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut replay = serde_json::Map::from_iter([
        ("type".to_string(), serde_json::json!("user")),
        (
            "message".to_string(),
            serde_json::json!({
                "role": "user",
                "content": [{"type":"text", "text": prompt}],
            }),
        ),
        ("parent_tool_use_id".to_string(), serde_json::Value::Null),
        (
            "session_id".to_string(),
            serde_json::json!(crate::bootstrap::state::get_session_id()),
        ),
        ("uuid".to_string(), serde_json::json!(uuid)),
        ("isReplay".to_string(), serde_json::json!(true)),
    ]);
    if let Some(timestamp) = input.and_then(|input| input.timestamp.clone()) {
        replay.insert("timestamp".to_string(), timestamp);
    }
    replay.into()
}

/// Maps to: CC `QueryEngine.ts:466-473` replayable filter — `!msg.isMeta`,
/// `!msg.toolUseResult`, and `selectableUserMessagesFilter`
/// (`MessageSelector.tsx:895-940`: first-block tool_result, synthetic text
/// (`utils/messages.ts:302-319` SYNTHETIC_MESSAGES), compact-summary /
/// transcript-only envelopes, and non-user-authored text tags all excluded).
/// On single-block rows the envelope `toolUseResult` test and the filter's
/// first-block tool_result test collapse into one.
fn replayable_user_row(row: &crate::types::message::RenderableMessage) -> bool {
    use crate::types::message::{RenderableMessageKind, UserContent};
    let RenderableMessageKind::User { message } = &row.kind else {
        return false;
    };
    let first = message.first_content_block();
    if matches!(first, Some(UserContent::ToolResult(_))) {
        return false;
    }
    if matches!(
        first,
        Some(
            UserContent::MetaText(_)
                | UserContent::MetaImage { .. }
                | UserContent::RawImage { is_meta: true, .. }
                | UserContent::MetaDocument { .. }
        )
    ) {
        return false;
    }
    if let Some(UserContent::Text(text)) = first {
        let synthetic = [
            crate::utils::messages::INTERRUPT_MESSAGE,
            crate::utils::messages::INTERRUPT_MESSAGE_FOR_TOOL_USE,
            crate::utils::messages::CANCEL_MESSAGE,
            crate::utils::messages::REJECT_MESSAGE,
            crate::utils::messages::NO_RESPONSE_REQUESTED,
        ];
        if synthetic.contains(&text.as_str()) {
            return false;
        }
    }
    if message.is_compact_summary || message.is_visible_in_transcript_only {
        return false;
    }
    // CC reads the LAST content block and only if it is text
    // (`MessageSelector.tsx:918-925`); anything else compares as "".
    let last_text = match message.content.last() {
        Some(UserContent::Text(text)) => text.trim(),
        _ => "",
    };
    !crate::components::message_selector::is_non_user_authored_message_text(last_text)
}

/// The per-row SDK replay event — CC `QueryEngine.ts:738-749`: the row's own
/// message content, uuid and timestamp, `isReplay: true`.
fn replayed_row_value(row: &crate::types::message::RenderableMessage) -> serde_json::Value {
    let crate::types::message::RenderableMessageKind::User { message } = &row.kind else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": message
                .content
                .iter()
                .map(user_content_value)
                .collect::<Vec<_>>(),
        },
        "parent_tool_use_id": null,
        "session_id": crate::bootstrap::state::get_session_id(),
        "uuid": message.uuid,
        "timestamp": message.timestamp,
        "isReplay": true,
    })
}

fn permission_denial_value(
    request: &crate::types::permissions::PermissionRequest,
) -> serde_json::Value {
    serde_json::json!({
        "tool_name": request.tool_name,
        "tool_use_id": request.tool_use_id,
        "tool_input": request.input,
    })
}

/// Maps to: CC `QueryEngine.ts:274-276` `initialMainLoopModel`:
/// `userSpecifiedModel ? parseUserSpecifiedModel(userSpecifiedModel) :
/// getMainLoopModel()`. `userSpecifiedModel` is print's
/// `activeUserSpecifiedModel`: the launch `effectiveModel`
/// ([`QueryEngineConfig::user_specified_model`]), then what `set_model` and
/// `apply_flag_settings` set. Parsed as given, it does not pass the
/// `availableModels` check `getMainLoopModel()` applies, as in CC.
fn initial_main_loop_model(overrides: &QueryEngineOverrides) -> String {
    overrides
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
        .map(crate::utils::model::model::parse_user_specified_model)
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model)
}

/// `cli/print.rs#build_all_tools` (CC `print.ts:1474-1500`) off the
/// single-thread process executor.
///
/// Executor adapter only — no CC logic lives here. `assemble_tool_pool`
/// re-enters `get_tools`, which renders the eager tool registry (Agent's
/// schema goes through `block_on_from_sync`); on the current-thread runtime
/// the headless entrypoint runs on, doing that inline deadlocks the first
/// query — the same reason the `build_headless_tools` call in [`run_query`]
/// is `spawn_blocking` (pinned by
/// `first_local_query_without_sdk_initialize_keeps_process_executor_live`).
async fn build_all_tools_off_executor(
    launch_tools: Vec<crate::types::tools::Tool>,
    permission_context: crate::tool::ToolPermissionContext,
    mcp_tools: Vec<crate::types::tools::Tool>,
    init_schema_tool: Option<crate::types::tools::Tool>,
) -> Result<Vec<crate::types::tools::Tool>, String> {
    tokio::task::spawn_blocking(move || {
        crate::cli::print::build_all_tools(
            &launch_tools,
            &permission_context,
            &mcp_tools,
            init_schema_tool,
        )
    })
    .await
    .map_err(|error| error.to_string())
}

fn resolve_thinking(config: &CliConfig) -> crate::utils::thinking::ThinkingConfig {
    use crate::utils::thinking::ThinkingConfig;
    if let Some(value) = config.thinking.as_deref() {
        return match value.to_ascii_lowercase().as_str() {
            "false" | "off" | "disabled" => ThinkingConfig::Disabled,
            "adaptive" => ThinkingConfig::Adaptive,
            _ => ThinkingConfig::Enabled {
                budget_tokens: config
                    .max_thinking_tokens
                    .as_deref()
                    .and_then(crate::utils::thinking::parse_js_decimal_i64),
            },
        };
    }
    crate::utils::thinking::production_thinking_config_from_env_and_config(
        &crate::utils::config::load_global_config(),
    )
}

/// The `(customSystemPrompt, appendSystemPrompt)` pair the engine sees
/// (`QueryEngine.ts:141-142`).
///
/// Two owners feed it and this only decides precedence:
/// * the SDK `initialize` request may override either half
///   (`print.ts:4369-4375` `options.systemPrompt = request.systemPrompt`
///   / `options.appendSystemPrompt = request.appendSystemPrompt`) — held in
///   [`QueryEngineOverrides`] and taken first;
/// * otherwise the argv/file value `main.tsx:2108-2170` resolved,
///   [`crate::main::resolve_headless_system_prompt`] — read lazily so a
///   `--system-prompt-file` that an override replaces is never opened.
///
/// Shared by [`resolve_system_prompt`] (prompt assembly) and [`run_query`]
/// (the `ToolUseContext` options, `QueryEngine.ts:360-361`), so both read the
/// same strings.
fn resolve_system_prompt_overrides(
    config: &CliConfig,
    overrides: &QueryEngineOverrides,
) -> Result<(Option<String>, Option<String>), String> {
    use crate::main::{HeadlessSystemPromptSlot, resolve_headless_system_prompt};
    let custom = match &overrides.system_prompt {
        Some(prompt) => Some(prompt.clone()),
        None => resolve_headless_system_prompt(config, HeadlessSystemPromptSlot::Custom)?,
    };
    let append = match &overrides.append_system_prompt {
        Some(prompt) => Some(prompt.clone()),
        None => resolve_headless_system_prompt(config, HeadlessSystemPromptSlot::Append)?,
    };
    Ok((custom, append))
}

fn resolve_system_prompt(
    config: &CliConfig,
    model: &str,
    tools: &[crate::types::tools::Tool],
    additional_working_directories: &[String],
    mcp_clients: &[crate::services::mcp::types::McpServerSnapshot],
    overrides: &QueryEngineOverrides,
) -> Result<
    (
        Vec<String>,
        std::collections::BTreeMap<String, String>,
        std::collections::BTreeMap<String, String>,
    ),
    String,
> {
    let (custom, append) = resolve_system_prompt_overrides(config, overrides)?;
    // Maps to: CC `QueryEngine.ts:287-300` and `queryContext.ts:44-74`.
    let (default_system_prompt, user_context, system_context) =
        crate::utils::query_context::fetch_system_prompt_parts(
            tools,
            model,
            additional_working_directories,
            mcp_clients,
            custom.as_deref(),
        );
    // Maps to CC QueryEngine.ts:316-325: explicit custom prompts can opt into
    // memory mechanics; place it after custom copy and before appended policy.
    let memory_mechanics_prompt = if custom.is_some()
        && crate::memdir::paths::has_auto_mem_path_override()
    {
        crate::memdir::memdir::load_memory_prompt()
    } else {
        None
    };
    let mut prompt = custom
        .map(|prompt| vec![prompt])
        .unwrap_or(default_system_prompt);
    if let Some(memory) = memory_mechanics_prompt.filter(|value| !value.is_empty()) {
        prompt.push(memory);
    }
    if let Some(append) = append.filter(|value| !value.is_empty()) {
        prompt.push(append);
    }
    Ok((prompt, user_context, system_context))
}

fn add_usage(total: &mut TokenUsage, usage: &TokenUsage) {
    total.input_tokens += usage.input_tokens;
    total.output_tokens += usage.output_tokens;
    total.cache_creation_input_tokens += usage.cache_creation_input_tokens;
    total.cache_read_input_tokens += usage.cache_read_input_tokens;
    total.cache_deleted_input_tokens += usage.cache_deleted_input_tokens;
}

/// Inverse of [`add_usage`] for the AssistantDelta write-back: the pre-delta
/// contribution of the mutated message leaves the total before the final one
/// is added. Saturating because the delta always carries merged-forward
/// values (message_start usage plus message_delta output tokens).
fn sub_usage(total: &mut TokenUsage, usage: &TokenUsage) {
    total.input_tokens = total.input_tokens.saturating_sub(usage.input_tokens);
    total.output_tokens = total.output_tokens.saturating_sub(usage.output_tokens);
    total.cache_creation_input_tokens = total
        .cache_creation_input_tokens
        .saturating_sub(usage.cache_creation_input_tokens);
    total.cache_read_input_tokens = total
        .cache_read_input_tokens
        .saturating_sub(usage.cache_read_input_tokens);
    total.cache_deleted_input_tokens = total
        .cache_deleted_input_tokens
        .saturating_sub(usage.cache_deleted_input_tokens);
}

pub(crate) fn stream_message_value(message: &Message) -> Option<serde_json::Value> {
    let session_id = crate::bootstrap::state::get_session_id();
    match message {
        Message::Assistant(assistant) => {
            let content = assistant
                .content
                .iter()
                .filter_map(assistant_content_value)
                .collect::<Vec<_>>();
            // CC `normalizeMessage`'s assistant branch yields `uuid: _.uuid`
            // (`utils/queryHelpers.ts:115`) — the envelope, the same value the
            // transcript row carries.
            let uuid = assistant.uuid.clone();
            Some(serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": assistant.api_message_id().unwrap_or(&uuid),
                    "type": "message",
                    "role": "assistant",
                    "model": assistant.model,
                    "content": content,
                    "stop_reason": assistant.stop_reason,
                    "stop_sequence": null,
                    "usage": assistant.usage,
                },
                "parent_tool_use_id": null,
                "session_id": session_id,
                "uuid": uuid,
            }))
        }
        Message::User(user) => {
            // CC `normalizeMessage`'s user branch (`queryHelpers.ts:203-218`)
            // yields uuid/timestamp, `isSynthetic: isMeta ||
            // isVisibleInTranscriptOnly`, and the raw `tool_use_result` —
            // wrapped as `{ content, ...mcpMeta }` when mcpMeta rides the
            // envelope. The SDK schema carries the field
            // (`coreSchemas.ts:1279`); absence means "no tool output", so the
            // key is omitted rather than nulled (jsonStringify drops
            // undefined).
            let is_meta = matches!(
                user.first_content_block(),
                Some(
                    crate::types::message::UserContent::MetaText(_)
                        | crate::types::message::UserContent::MetaImage { .. }
                        | crate::types::message::UserContent::RawImage { is_meta: true, .. }
                        | crate::types::message::UserContent::MetaDocument { .. }
                )
            );
            let raw = user.content.iter().find_map(|block| match block {
                crate::types::message::UserContent::ToolResult(result) => {
                    result.tool_use_result.clone()
                }
                _ => None,
            });
            let tool_use_result = match (&user.mcp_meta, raw) {
                (Some(mcp_meta), raw) => {
                    let mut object = serde_json::Map::new();
                    object.insert(
                        "content".to_string(),
                        raw.unwrap_or(serde_json::Value::Null),
                    );
                    if let Some(map) = mcp_meta.as_object() {
                        for (key, value) in map {
                            object.insert(key.clone(), value.clone());
                        }
                    }
                    Some(serde_json::Value::Object(object))
                }
                (None, raw) => raw,
            };
            let mut value = serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": user.content.iter().map(user_content_value).collect::<Vec<_>>(),
                },
                "parent_tool_use_id": null,
                "session_id": session_id,
                "uuid": user.uuid,
                "timestamp": user.timestamp,
                "isSynthetic": is_meta || user.is_visible_in_transcript_only,
            });
            if let (Some(object), Some(tool_use_result)) = (value.as_object_mut(), tool_use_result)
            {
                object.insert("tool_use_result".to_string(), tool_use_result);
            }
            Some(value)
        }
        Message::System(system) => {
            // Maps to CC QueryEngine.ts:583-604/:918-941. System output has
            // its own mapper; live assistant/user normalization above remains
            // queryHelpers.ts behavior (including MCP output wrapping).
            let entry = crate::utils::session_storage::system_entry_json(system, message.uuid());
            crate::utils::messages::mappers::to_sdk_messages(&[entry])
                .into_iter()
                .next()
        }
        _ => None,
    }
}

fn assistant_content_value(content: &AssistantContent) -> Option<serde_json::Value> {
    match content {
        AssistantContent::Text(text) => Some(serde_json::json!({"type":"text","text":text})),
        AssistantContent::Thinking { text, signature } => Some(serde_json::json!({
            "type":"thinking", "thinking":text, "signature":signature
        })),
        AssistantContent::RedactedThinking { data } => {
            Some(serde_json::json!({"type":"redacted_thinking","data":data}))
        }
        AssistantContent::ToolUse(tool) => Some(serde_json::json!({
            "type":"tool_use", "id":tool.id.0, "name":tool.name, "input":tool.input
        })),
        AssistantContent::ServerToolUse(tool) => Some(serde_json::json!({
            "type":"server_tool_use", "id":tool.id.0, "name":tool.name, "input":tool.input
        })),
        AssistantContent::WebSearchToolResult {
            tool_use_id,
            content,
        } => Some(
            serde_json::json!({"type":"web_search_tool_result","tool_use_id":tool_use_id.0,"content":content}),
        ),
        AssistantContent::Advisor {
            tool_use_id,
            content,
        } => Some(serde_json::json!({
            "type":"advisor_tool_result",
            "tool_use_id":tool_use_id.0,
            "content":content
        })),
        AssistantContent::MessageIdentity(_) => None,
    }
}

fn user_content_value(content: &crate::types::message::UserContent) -> serde_json::Value {
    use crate::types::message::UserContent;
    match content {
        UserContent::Text(text) | UserContent::MetaText(text) => {
            serde_json::json!({"type":"text","text":text})
        }
        UserContent::RawImage { block, .. } => block.clone(),
        UserContent::Image { media_type, data } | UserContent::MetaImage { media_type, data } => {
            serde_json::json!({
                "type":"image", "source":{"type":"base64","media_type":media_type,"data":data}
            })
        }
        UserContent::Document { media_type, data }
        | UserContent::MetaDocument { media_type, data } => serde_json::json!({
            "type":"document", "source":{"type":"base64","media_type":media_type,"data":data}
        }),
        UserContent::ToolResult(result) => {
            let content = if result.content_blocks.is_empty() {
                serde_json::Value::String(result.content.clone())
            } else {
                serde_json::to_value(&result.content_blocks)
                    .unwrap_or_else(|_| serde_json::Value::String(result.content.clone()))
            };
            serde_json::json!({
                "type":"tool_result",
                "tool_use_id":result.tool_use_id.0,
                "content":content,
                "is_error":result.is_error
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, IsolatedProjectSettings, TEST_ENV_LOCK};

    fn mcp_tool(server: &str, name: &str) -> crate::types::tools::Tool {
        crate::types::tools::Tool {
            name: format!("mcp__{server}__{name}"),
            is_mcp: true,
            mcp_info: Some(crate::types::tools::McpToolInfo {
                server_name: server.to_string(),
                tool_name: name.to_string(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn build_all_tools_sorts_headless_pool_like_print_ts_build_all_tools() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        // CC main.tsx:2755 `getTools(toolPermissionContext)` — insertion order
        // (Agent, TaskOutput, Bash, ...), which is what the headless pool used
        // to ship verbatim.
        let permission = crate::tool::ToolPermissionContext::default();
        let mut launch_tools = crate::tools::get_tools(&permission);
        assert!(
            launch_tools.len() > 3,
            "fixture needs the full built-in registry"
        );
        assert_ne!(
            launch_tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            {
                let mut sorted = launch_tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>();
                sorted.sort_by(|left, right| crate::tools::compare_tool_names(left, right));
                sorted
            },
            "getTools() is insertion-ordered; the sort must come from buildAllTools"
        );
        // main.tsx:2779-2785: argv `--json-schema` tool joins `tools` before the sort.
        let argv_schema_tool = crate::tools::synthetic_output_tool::create_synthetic_output_tool(
            serde_json::json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}),
        )
        .unwrap();
        launch_tools.push(argv_schema_tool);
        let mcp_tools = vec![mcp_tool("zeta", "z_tool"), mcp_tool("alpha", "a_tool")];

        let pool = crate::cli::print::build_all_tools(&launch_tools, &permission, &mcp_tools, None);
        let names = pool
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        // print.ts:1475-1486 via toolPool.ts:64-70: built-ins are a name-sorted
        // contiguous prefix, MCP tools a name-sorted suffix.
        let split = names
            .iter()
            .position(|name| name.starts_with("mcp__"))
            .expect("MCP tools present");
        let (built_in, mcp) = names.split_at(split);
        assert!(built_in.iter().all(|name| !name.starts_with("mcp__")));
        assert!(mcp.iter().all(|name| name.starts_with("mcp__")));
        let mut sorted_built_in = built_in.to_vec();
        sorted_built_in.sort_by(|left, right| crate::tools::compare_tool_names(left, right));
        assert_eq!(built_in, sorted_built_in.as_slice());
        assert_eq!(mcp, ["mcp__alpha__a_tool", "mcp__zeta__z_tool"]);
        // The argv schema tool was sorted INTO the built-in partition, not appended.
        assert!(
            built_in.contains(&crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME)
        );
        // No duplicates survive the uniqBy.
        let unique = names.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), names.len());

        // print.ts:1492-1498: the init-control schema tool trails the sorted pool.
        let init_schema_tool = crate::tools::synthetic_output_tool::create_synthetic_output_tool(
            serde_json::json!({"type": "object"}),
        )
        .unwrap();
        let launch_without_argv_tool = launch_tools
            .iter()
            .filter(|tool| {
                tool.name != crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME
            })
            .cloned()
            .collect::<Vec<_>>();
        let pool = crate::cli::print::build_all_tools(
            &launch_without_argv_tool,
            &permission,
            &mcp_tools,
            Some(init_schema_tool),
        );
        assert_eq!(
            pool.last().map(|tool| tool.name.as_str()),
            Some(crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME)
        );
    }

    #[test]
    fn custom_prompt_memory_order_matches_official_query_engine_assembly() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _project = IsolatedProjectSettings::pin();
        let root =
            std::env::temp_dir().join(format!("cometix-query-memory-{}", uuid::Uuid::new_v4()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let _memory = EnvVarGuard::set(
            "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
            root.to_string_lossy().as_ref(),
        );
        let _enabled = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
        let config = CliConfig {
            system_prompt: Some("CUSTOM".into()),
            append_system_prompt: Some("APPEND".into()),
            ..Default::default()
        };
        let (prompt, _, context) =
            resolve_system_prompt(
                &config,
                &crate::utils::model::model::get_main_loop_model(),
                &[],
                &[],
                &[],
                &QueryEngineOverrides::default(),
            )
            .unwrap();
        // CC QueryEngine.ts:316-325: custom → opted-in memory mechanics → append.
        assert_eq!(prompt.len(), 3);
        assert_eq!(prompt[0], "CUSTOM");
        assert!(prompt[1].starts_with("# auto memory"));
        assert_eq!(prompt[2], "APPEND");
        // CC queryContext.ts:61-73: a custom prompt suppresses system context.
        assert!(context.is_empty());
    }

    #[test]
    fn stream_message_projects_identity_out_of_model_content() {
        let message = Message::Assistant(crate::types::message::AssistantMessage {
            // The ENVELOPE uuid, which is what CC's `normalizeMessage` yields as
            // `uuid: _.uuid` (`utils/queryHelpers.ts:115`) and what the
            // `projected["uuid"]` assertion below checks. The named value used
            // to sit on the identity block with a random envelope, so the
            // assertion pinned the identity uuid instead.
            uuid: "assistant-uuid".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![
                AssistantContent::Text("hello".to_string()),
                AssistantContent::MessageIdentity(
                    crate::types::message::AssistantMessageIdentity {
                        request_id: Some("req_1".to_string()),
                        api_message_id: Some("msg_1".to_string()),
                        ..Default::default()
                    },
                ),
            ],
            model: Some("claude-test".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        });
        let projected = stream_message_value(&message).unwrap();
        assert_eq!(projected["type"], "assistant");
        assert_eq!(projected["uuid"], "assistant-uuid");
        assert_eq!(projected["message"]["id"], "msg_1");
        assert_eq!(projected["message"]["content"].as_array().unwrap().len(), 1);
    }

    /// Maps to: CC `QueryEngine.ts:466-474` — the replay-ack set filters the
    /// submitted user rows: meta rows, tool results, synthetic sentinel text,
    /// compact-summary/transcript-only envelopes, and non-user-authored tag
    /// text are all excluded; plain user prompts pass.
    #[test]
    fn replayable_user_rows_match_official_filter() {
        use crate::types::message::{RenderableMessage, ToolResult, UserContent};
        let user_text = |uuid: &str, text: &str| {
            RenderableMessage::user_block(uuid, UserContent::Text(text.to_string()))
        };

        assert!(replayable_user_row(&user_text("u1", "please fix the bug")));

        // Meta rows (synthetic caveats) skip.
        assert!(!replayable_user_row(&RenderableMessage::user_block(
            "meta",
            UserContent::MetaText("caveat".to_string()),
        )));
        // Tool results skip — they ack from the query itself.
        assert!(!replayable_user_row(&RenderableMessage::user_block(
            "result",
            UserContent::ToolResult(ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu-1".to_string()),
                content: "ok".to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: Some(serde_json::json!({"ok": true})),
            }),
        )));
        // Synthetic sentinel text skips (SYNTHETIC_MESSAGES).
        assert!(!replayable_user_row(&user_text(
            "synthetic",
            crate::utils::messages::INTERRUPT_MESSAGE,
        )));
        // Non-user-authored tag text skips (task notifications, command output).
        assert!(!replayable_user_row(&user_text(
            "notification",
            "<local-command-stdout>done</local-command-stdout>",
        )));
        // Assistant rows are not user acks.
        assert!(!replayable_user_row(&RenderableMessage::assistant_block(
            "assistant",
            crate::types::message::AssistantContent::Text("hi".to_string()),
        )));

        // The replay event carries the row's own uuid and isReplay.
        let row = user_text("u-replay", "hello");
        let value = replayed_row_value(&row);
        assert_eq!(value["isReplay"], true);
        assert_eq!(value["message"]["content"][0]["text"], "hello");
    }

    /// Maps to: CC `normalizeMessage`'s user branch (`queryHelpers.ts:203-218`)
    /// — the outbound user event carries uuid/timestamp, `isSynthetic`, and
    /// the raw `tool_use_result` (wrapped with mcpMeta when present); a
    /// message with no tool output omits the key, as jsonStringify drops
    /// undefined.
    #[test]
    fn stream_user_message_carries_tool_use_result_like_official() {
        let raw = serde_json::json!({"stdout": "ok", "stderr": ""});
        let user = |mcp_meta: Option<serde_json::Value>| {
            Message::User(crate::types::message::UserMessage {
                uuid: "user-uuid".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id: crate::types::ids::ToolUseId("toolu-1".to_string()),
                        content: "ok".to_string(),
                        is_error: false,
                        content_blocks: Vec::new(),
                        tool_use_result: Some(raw.clone()),
                    },
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            })
        };

        let projected = stream_message_value(&user(None)).unwrap();
        assert_eq!(projected["uuid"], "user-uuid");
        assert_eq!(projected["isSynthetic"], false);
        assert!(projected["timestamp"].is_string());
        assert_eq!(projected["tool_use_result"], raw);

        // mcpMeta wraps the raw as `{ content, ...mcpMeta }`.
        let wrapped =
            stream_message_value(&user(Some(serde_json::json!({"serverName": "linear"})))).unwrap();
        assert_eq!(wrapped["tool_use_result"]["content"], raw);
        assert_eq!(wrapped["tool_use_result"]["serverName"], "linear");

        // A plain user message has no tool output: the key is absent.
        let plain = Message::User(crate::types::message::UserMessage {
            uuid: "user-plain".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text("hi".to_string())],
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
        let projected = stream_message_value(&plain).unwrap();
        assert!(projected.get("tool_use_result").is_none());
    }

    /// The two schema slots keep CC's opposite winners: tool creation
    /// prefers argv (print.ts:1492-1498 `initJsonSchema &&
    /// !options.jsonSchema`), the engine-level jsonSchema prefers init
    /// (print.ts:2166 `getInitJsonSchema() ?? options.jsonSchema`).
    #[test]
    fn schema_slots_keep_official_opposite_winners() {
        let argv = serde_json::json!({"type": "object", "required": ["a"]});
        let init = serde_json::json!({"type": "object", "required": ["b"]});

        let mut engine = QueryEngine::new(QueryEngineConfig {
            cli_config: CliConfig::default(),
            schema: Some(argv.clone()),
            resume_seed: None,
            mcp_state: Some(crate::state::app_state_store::McpState::default()),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
            user_specified_model: None,
        });
        assert_eq!(engine.schema(), Some(&argv));
        assert_eq!(engine.tool_schema(), Some(&argv));

        engine.set_init_schema(init.clone());
        assert_eq!(engine.schema(), Some(&init), "engine option: init wins");
        assert_eq!(
            engine.tool_schema(),
            Some(&argv),
            "tool creation: argv wins"
        );

        let mut init_only = QueryEngine::new(QueryEngineConfig {
            cli_config: CliConfig::default(),
            schema: None,
            resume_seed: None,
            mcp_state: Some(crate::state::app_state_store::McpState::default()),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
            user_specified_model: None,
        });
        init_only.set_init_schema(init.clone());
        assert_eq!(init_only.schema(), Some(&init));
        assert_eq!(init_only.tool_schema(), Some(&init));
    }

    #[tokio::test]
    async fn explicit_tool_selection_integration_rejects_file_read_and_accepts_real_aliases() {
        let mut file_read_config = CliConfig::default();
        file_read_config.tools = Some(vec!["FileRead,Bash".to_string()]);
        let mut file_read_engine = QueryEngine::new(QueryEngineConfig {
            cli_config: file_read_config,
            schema: None,
            resume_seed: None,
            mcp_state: Some(crate::state::app_state_store::McpState::default()),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
            user_specified_model: None,
        });
        let error = file_read_engine
            .submit_message("unused".to_string(), None)
            .await
            .err()
            .expect("FileRead must be rejected before starting a query");
        assert_eq!(error, "Unknown tool(s) passed to --tools: FileRead");

        let mut alias_config = CliConfig::default();
        alias_config.tools = Some(vec!["Task,NotAnOfficialTool".to_string()]);
        let mut alias_engine = QueryEngine::new(QueryEngineConfig {
            cli_config: alias_config,
            schema: None,
            resume_seed: None,
            mcp_state: Some(crate::state::app_state_store::McpState::default()),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
            user_specified_model: None,
        });
        let error = alias_engine
            .submit_message("unused".to_string(), None)
            .await
            .err()
            .expect("the unknown peer should reject the selection");
        assert_eq!(
            error,
            "Unknown tool(s) passed to --tools: NotAnOfficialTool"
        );
    }

    #[tokio::test]
    async fn headless_non_query_turn_clears_stale_command_permission_bucket() {
        let mut initial = crate::state::app_state_store::AppState::default();
        let mut permission = crate::tool::ToolPermissionContext::default();
        permission.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Command,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Read", None,
            )],
        );
        initial.set_tool_permission_context(permission);
        let store = crate::state::store::AppStore::new(initial, None);
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        let mut engine = QueryEngine::new(QueryEngineConfig {
            cli_config: CliConfig::default(),
            schema: None,
            resume_seed: None,
            mcp_state: Some(crate::state::app_state_store::McpState::default()),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
            user_specified_model: None,
        });
        engine.carry = Some(HeadlessCarry {
            history: Vec::new(),
            context,
        });
        engine.app_store = Some(store.clone());
        engine.session_start_completed = true;

        let result = engine.submit_message("/help".to_string(), None).await;

        assert!(
            result.is_err(),
            "local command should not start a model query"
        );
        assert_eq!(
            store.tool_permission_context().always_allow_rules
                [&crate::types::permissions::PermissionRuleSource::Command],
            Vec::new()
        );
    }

    /// CC main.tsx:2596-2634 initializes once; QueryEngine.ts:273 uses getAppState.
    /// A now-invalid CLI path must not re-enter validation on the next submit.
    #[tokio::test]
    async fn continued_query_matches_official_startup_directory_lifetime() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _settings = IsolatedProjectSettings::pin();
        let mut initial = crate::state::app_state_store::AppState::default();
        let permission = crate::utils::permissions::permission_update::apply_permission_update(
            &crate::tool::ToolPermissionContext::default(),
            &crate::types::permissions::PermissionUpdate::AddDirectories {
                destination: crate::types::permissions::PermissionUpdateDestination::Session,
                directories: vec!["/z-session-directory".into(), "/a-session-directory".into()],
            },
        );
        initial.set_tool_permission_context(permission.clone());
        let store = crate::state::store::AppStore::new(initial, None);
        let mut config = CliConfig::default();
        config
            .add_dirs
            .push(std::path::PathBuf::from("invalid\0directory"));
        let mut engine = QueryEngine::new(QueryEngineConfig {
            cli_config: config,
            schema: None,
            resume_seed: None,
            mcp_state: Some(crate::state::app_state_store::McpState::default()),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
            user_specified_model: None,
        });
        engine.carry = Some(HeadlessCarry {
            history: Vec::new(),
            context: crate::tool::ToolUseContext::default().with_app_store(store.clone()),
        });
        engine.app_store = Some(store.clone());
        engine.session_start_completed = true;
        for _ in 0..2 {
            let result = engine.submit_message("/help".into(), None).await;
            assert_eq!(
                result.err().as_deref(),
                Some("prompt produced no model query")
            );
            assert_eq!(
                engine
                    .app_store
                    .as_ref()
                    .unwrap()
                    .tool_permission_context()
                    .additional_working_directories,
                permission.additional_working_directories
            );
        }
        assert_eq!(
            store
                .tool_permission_context()
                .additional_working_directories,
            permission.additional_working_directories
        );
    }

    /// CC `QueryEngine.ts:274-276`: a model alias is resolved, both the one
    /// `setModel` set and the launch model the override holds. The saved
    /// `opus[1m]` used to reach the API as the literal `opus`.
    #[test]
    fn initial_main_loop_model_resolves_aliases_like_query_engine() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _model = EnvVarGuard::unset("ANTHROPIC_MODEL");
        let _opus = EnvVarGuard::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "custom-opus");
        let _sonnet = EnvVarGuard::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "custom-sonnet");
        let original = crate::bootstrap::state::get_main_loop_model_override();

        crate::bootstrap::state::set_main_loop_model_override(Some(Some("opus[1m]".into())));
        let mut overrides = QueryEngineOverrides::default();
        assert_eq!(initial_main_loop_model(&overrides), "custom-opus[1m]");

        overrides.model = Some("sonnet".into());
        assert_eq!(initial_main_loop_model(&overrides), "custom-sonnet");
        // `userSpecifiedModel ? ...`: an empty one is unset.
        overrides.model = Some(String::new());
        assert_eq!(initial_main_loop_model(&overrides), "custom-opus[1m]");

        crate::bootstrap::state::set_main_loop_model_override(original);
    }

    #[test]
    fn query_engine_exposes_official_stateful_control_surface() {
        let mut engine = QueryEngine::new(QueryEngineConfig {
            cli_config: CliConfig::default(),
            schema: None,
            resume_seed: None,
            mcp_state: Some(crate::state::app_state_store::McpState::default()),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
            user_specified_model: None,
        });
        engine.set_model("claude-test".to_string());
        assert_eq!(initial_main_loop_model(&engine.overrides), "claude-test");
        assert!(engine.get_messages().is_empty());
        assert!(engine.get_read_file_state().is_empty());
        assert!(!engine.get_session_id().is_empty());
        engine.interrupt();
    }
}

#[cfg(test)]
mod sdk_mapper_tests {
    use super::*;

    #[test]
    fn system_stream_output_matches_official_compact_and_local_command_mappers() {
        let boundary = Message::System(crate::types::message::SystemMessage::compact_boundary(
            Some(crate::types::message::CompactMetadata {
                trigger: Some("manual".to_string()),
                pre_tokens: Some(11),
                preserved_segment: Some(
                    serde_json::json!({"headUuid":"h","anchorUuid":"a","tailUuid":"t"}),
                ),
                ..Default::default()
            }),
        ));
        let sdk = stream_message_value(&boundary).unwrap();
        assert_eq!(sdk["type"], "system");
        assert_eq!(sdk["subtype"], "compact_boundary");
        assert_eq!(sdk["uuid"], boundary.uuid());
        assert_eq!(sdk["compact_metadata"]["pre_tokens"], 11);
        assert_eq!(
            sdk["compact_metadata"]["preserved_segment"]["anchor_uuid"],
            "a"
        );
        let command = crate::utils::conversation::into_typed_messages(vec![serde_json::json!({
            "type":"system","subtype":"local_command","uuid":"local-output",
            "content":"<local-command-stdout>\u{1b}[2mcost\u{1b}[0m</local-command-stdout>",
        })])
        .pop()
        .unwrap();
        let sdk = stream_message_value(&command).unwrap();
        assert_eq!(sdk["type"], "assistant");
        assert_eq!(sdk["uuid"], "local-output");
        assert_eq!(sdk["message"]["content"][0]["text"], "cost");
        let input = crate::utils::conversation::into_typed_messages(vec![serde_json::json!({
            "type":"system","subtype":"local_command","uuid":"local-input",
            "content":"<command-name>/cost</command-name>",
        })])
        .pop()
        .unwrap();
        assert!(stream_message_value(&input).is_none());
    }
    #[test]
    fn first_local_query_without_sdk_initialize_keeps_process_executor_live() {
        const CHILD: &str = "COMETIX_FIRST_QUERY_PLUGIN_CHILD";
        if let Some(root) = crate::utils::process_env::var_os(CHILD) {
            assert!(crate::utils::process_runtime::process_runtime_handle().is_none());
            let root = std::path::PathBuf::from(root);
            crate::utils::process_env::set("CLAUDE_CONFIG_DIR", root.join("config"));
            // Exercise the full tool registry, including Agent's eager schema.
            crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "0");
            crate::bootstrap::state::set_original_cwd(&root);
            crate::bootstrap::state::set_inline_plugins(vec![root.join("plugin")]);
            crate::commands::clear_commands_cache();
            crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            crate::utils::process_runtime::set_process_runtime_handle(runtime.handle().clone());
            let mut engine = QueryEngine::new(QueryEngineConfig {
                cli_config: CliConfig {
                    print: true,
                    tools: Some(vec!["Read".into()]),
                    ..Default::default()
                },
                schema: None,
                resume_seed: None,
                mcp_state: None,
                output_sink: None,
                permission_resolver: None,
                handle_elicitation: crate::tool::HandleElicitationCallback::default(),
                user_specified_model: None,
            });
            let result = runtime.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(15),
                    engine.submit_message("/cost".into(), None),
                )
                .await
                .expect("first local query kept its event loop running")
            });
            // Existing headless handling of a non-querying local command. This
            // fixture never reaches a model call, and does not alter its result.
            match result {
                Err(error) => assert_eq!(error, "prompt produced no model query"),
                Ok(_) => panic!("local /cost must not launch a model query"),
            }
            let state = engine.app_store.as_ref().unwrap().get();
            assert!(
                state
                    .agent_definitions
                    .active_agents
                    .iter()
                    .any(|agent| agent.agent_type == "first-query:reviewer")
            );
            return;
        }
        let root =
            std::env::temp_dir().join(format!("first-query-plugin-{}", uuid::Uuid::new_v4()));
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
            r#"{"name":"first-query"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("plugin/commands/probe.md"),
            "---\ndescription: Never execute\n---\nFixture",
        )
        .unwrap();
        std::fs::write(
            root.join("plugin/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Never execute\n---\nFixture",
        )
        .unwrap();
        let mut child=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","query_engine::sdk_mapper_tests::first_local_query_without_sdk_initialize_keeps_process_executor_live","--nocapture"])
            .env(CHILD,&root).current_dir(&root).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "first local query deadlocked: {}",
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
