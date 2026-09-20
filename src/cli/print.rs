//! Non-interactive `-p` and structured-I/O execution.
//! Maps to CC `cli/print.ts:453-5251`.
//!
//! Owner boundary: CC splits this surface across TWO files. `cli/print.ts`
//! declares the headless entry (`runHeadless` `:455-975`,
//! `runHeadlessStreaming` `:976`), the control-request handlers
//! (`handleInitializeRequest` `:4336`, `handleSetPermissionMode` `:4568`,
//! `handleMcpSetServers` `:5353`, …) and the result framing; `cli/structuredIO.ts`
//! declares the `StructuredIO` class that owns the SDK control protocol.
//! The latter's port lives in [`crate::cli::structured_io`], not here.
//!
//! Stateful model/query ownership lives in `crate::query_engine`, mapping
//! `QueryEngine.ts`; this module only owns CLI startup and wire protocol I/O.

use crate::cli::CliConfig;
use crate::cli::structured_io::{
    SdkControlBridge, create_hook_callback, request_sdk_elicitation, request_sdk_tool_permission,
};
use crate::query_engine::{
    QueryEngine, QueryEngineConfig, QueryEngineOutcome, QueryEnginePermissionResolver,
    QueryEngineReplayInput, QueryEngineResumeSeed,
};
#[cfg(test)]
use crate::types::message::AssistantContent;
use crate::types::message::Message;
use std::io::Read;

/// Callback lifetime carrier for CC `cli/print.ts:1129-1142` and
/// `:2677/:4137`: register once for headless streaming, unregister on exit.
/// The quota service retains its own equality deduplication.
struct SdkRateLimitListener(u64);

impl SdkRateLimitListener {
    fn new(emit: crate::query_engine::QueryEngineOutputSink) -> Self {
        Self(crate::services::claude_ai_limits::subscribe(
            move |limits| {
                if let Some(info) =
                    crate::utils::messages::mappers::to_sdk_rate_limit_info(Some(&limits))
                {
                    emit(serde_json::json!({
                        "type": "rate_limit_event",
                        "rate_limit_info": info,
                        "uuid": uuid::Uuid::new_v4().to_string(),
                        "session_id": crate::bootstrap::state::get_session_id(),
                    }));
                }
            },
        ))
    }
}

impl Drop for SdkRateLimitListener {
    fn drop(&mut self) {
        crate::services::claude_ai_limits::unsubscribe(self.0);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ControlInput {
    request_id: String,
    request: serde_json::Value,
}

/// The parsed `control_response` payload. CC's counterpart is
/// `SDKControlResponse` (`entrypoints/sdk/controlTypes.ts`), imported by BOTH
/// `cli/print.ts` and `cli/structuredIO.ts` — hence `pub(super)`: it stays with
/// the line parser that produces it and is read by
/// [`crate::cli::structured_io`], which awaits it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ControlResponseInput {
    pub(super) request_id: String,
    pub(super) subtype: String,
    pub(super) response: Option<serde_json::Value>,
    pub(super) error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct UserInput {
    text: String,
    uuid: Option<String>,
    timestamp: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
enum StructuredInput {
    User(UserInput),
    Control(ControlInput),
    ControlResponse(ControlResponseInput),
    ControlCancel(String),
    KeepAlive,
    EnvironmentUpdate(indexmap::IndexMap<String, String>),
    History(Message),
}

pub fn run(config: &CliConfig) -> i32 {
    crate::utils::tls_provider::install_crypto_provider();
    crate::utils::debug::init_from_parts(
        config.debug || config.mcp_debug,
        true,
        config.debug_file.clone(),
        config.debug_filter.as_deref(),
    );
    crate::utils::secure_storage::keychain_prefetch::start_keychain_prefetch();
    crate::plugins::bundled::init_builtin_plugins();
    if let Err(error) = crate::main::apply_live_startup_flags(&config.argv) {
        return fail(&error);
    }
    crate::utils::secure_storage::keychain_prefetch::ensure_keychain_prefetch_completed();
    crate::utils::managed_env::apply_safe_config_environment_variables();
    // Maps to CC main.tsx:1273-1274 / setup.ts:371. This headless launcher
    // bypasses main::run, so it must attach the same sink before tool work.
    if let Err(error) = crate::utils::sinks::init_sinks() {
        return fail(&error.to_string());
    }
    // Print mode bypasses the trust dialog and is documented as trusted, so
    // the FULL merged settings env (including project-scoped sources) applies
    // here. Maps to: CC `main.tsx:3657-3660` (and the equivalent
    // non-interactive apply at `main.tsx:2866-2879`).
    crate::utils::managed_env::apply_config_environment_variables();
    // Cometix extension (no CC counterpart): apply active proxy channel env overrides.
    crate::services::proxy_channel::apply_active_channel_env();
    crate::utils::workload_context::set_process_workload(config.workload.clone());
    crate::bootstrap::state::set_session_persistence_disabled(
        config.session_persistence == Some(false),
    );

    let format = effective_output_format(config);
    let input_format = config.input_format.as_deref().unwrap_or("text");
    if !matches!(input_format, "text" | "stream-json") {
        return fail(&format!("invalid input format `{input_format}`"));
    }
    if input_format == "stream-json" && format != "stream-json" {
        return fail("--input-format=stream-json requires output-format=stream-json");
    }
    if !matches!(format.as_str(), "text" | "json" | "stream-json") {
        return fail(&format!("unsupported output format `{format}`"));
    }
    if config.permission_prompt_tool.is_some() && format != "stream-json" {
        return fail("--permission-prompt-tool requires output-format=stream-json");
    }
    if config
        .permission_prompt_tool
        .as_deref()
        .is_some_and(|name| name != "stdio")
    {
        return fail("only --permission-prompt-tool=stdio is currently supported");
    }
    if let Err(error) = validate_headless_options(config, &format, input_format) {
        return fail(&error);
    }
    let schema = match parse_schema(config.json_schema.as_deref()) {
        Ok(schema) => schema,
        Err(error) => return fail(&error),
    };
    let resume_seed = if input_format == "stream-json" {
        None
    } else {
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => return fail(&error.to_string()),
        };
        match load_headless_resume(config, &cwd) {
            Ok(seed) => seed,
            Err(error) => return fail(&error),
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fail(&format!("failed to initialize async runtime: {error}")),
    };
    // Publish this session runtime as the process runtime (PORTING.md § "Node
    // 异步模型 → tokio" A3: every entrypoint publishes). CC's `void <promise>`
    // detach semantics are process-lifetime in print mode too — cli/print.ts
    // serves the same AgentTool/teammate detach paths as the REPL. Print
    // branches at cli/dispatch.rs BEFORE main.rs::run() ever publishes, and
    // inside tool execution the ambient runtime is a per-query current_thread
    // runtime (`query.rs#spawn_query`), so without this publish
    // `runtime_handle_for_detached_work()` has nothing process-lifetime to
    // hand out and a background Agent in a stream-json multi-turn session dies
    // with its turn (#150, headless re-creation). This current_thread runtime
    // IS the process runtime here: it is block_on-driven for the whole session
    // (both branches below), so tasks spawned onto its handle DO progress.
    // (Secondary native-Rust precedent: grok-build registers its
    // process-lifetime handle from every agent entrypoint, idempotently.)
    crate::utils::process_runtime::set_process_runtime_handle(runtime.handle().clone());
    // Both argv/stdin-text and SDK stdin sessions can emit stream-json.
    // Other output formats have no event stream in this transport.
    let _rate_limit_listener = (format == "stream-json")
        .then(|| SdkRateLimitListener::new(std::sync::Arc::new(json_line)));
    if input_format == "stream-json" {
        if config.prompt.is_some() {
            return fail("--input-format=stream-json reads user messages from stdin");
        }
        return runtime.block_on(run_stream_json(config, schema));
    }

    let prompt = match read_prompt(config) {
        Ok(prompt) if !prompt.is_empty() => prompt,
        Ok(_) => return fail("print mode requires a prompt on argv or stdin"),
        Err(error) => return fail(&error),
    };
    let started = std::time::Instant::now();
    let outcome = match runtime.block_on(async {
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let mcp_state = resolve_headless_mcp(config, &cwd).await?;
        let mut engine = QueryEngine::new(QueryEngineConfig {
            cli_config: config.clone(),
            schema: schema.clone(),
            resume_seed,
            mcp_state: Some(mcp_state),
            output_sink: None,
            permission_resolver: None,
            handle_elicitation: crate::tool::HandleElicitationCallback::default(),
        });
        crate::query_engine::ask(&mut engine, prompt, None).await
    }) {
        Ok(outcome) => outcome,
        Err(error) => return fail(&error),
    };
    finish_outcome(&format, schema.is_some(), &outcome, started.elapsed())
}

fn load_headless_resume(
    config: &CliConfig,
    cwd: &std::path::Path,
) -> Result<Option<QueryEngineResumeSeed>, String> {
    if config.from_pr.is_some() {
        return Err("--from-pr is not yet supported in headless mode".to_string());
    }
    let project_path = cwd.to_string_lossy();
    let target = if config.continue_session {
        Some(
            crate::commands::resume::load_for_continue(&project_path)
                .map_err(|error| error.to_string())?,
        )
    } else if let Some(resume) = config.resume.as_deref() {
        Some(if resume.trim().is_empty() {
            crate::commands::resume::load_for_continue(&project_path)
                .map_err(|error| error.to_string())?
        } else {
            crate::commands::resume::load_for_cli_resume(&project_path, resume)
                .map_err(|error| error.to_string())?
        })
    } else {
        None
    };

    let Some(target) = target else {
        if let Some(session_id) = &config.session_id {
            crate::bootstrap::state::switch_session(session_id.clone(), None);
        }
        return Ok(None);
    };
    let loaded = crate::utils::session_restore::ResumeLoadResult::try_from(&target)?;
    if config.fork_session {
        crate::bootstrap::state::set_session_id(target.session_id.clone());
        let generated = crate::bootstrap::state::regenerate_session_id(true);
        if let Some(session_id) = &config.session_id {
            crate::bootstrap::state::switch_session(session_id.clone(), None);
        } else {
            crate::bootstrap::state::switch_session(generated, None);
        }
        return Ok(Some(QueryEngineResumeSeed::new(
            loaded.messages.as_ref().clone(),
            None,
            true,
        )));
    }
    let processed = crate::utils::session_restore::process_resumed_conversation(
        loaded,
        None,
        std::sync::Arc::new(
            crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides_readonly(
                cwd,
            ),
        ),
    )?;
    Ok(Some(QueryEngineResumeSeed::new(
        processed.messages.as_ref().clone(),
        Some(processed),
        false,
    )))
}

async fn resolve_headless_mcp(
    config: &CliConfig,
    cwd: &std::path::Path,
) -> Result<crate::state::app_state_store::McpState, String> {
    let (startup, warnings) = crate::main::resolve_mcp_startup_config(config)?;
    for warning in warnings {
        crate::utils::debug::log_for_debugging(&warning);
    }
    let global = crate::utils::config::load_global_config();
    let project = crate::interactive_helpers::project_config_for_cwd(&global, cwd);
    let mut configs = if startup.strict || startup.bare {
        indexmap::IndexMap::new()
    } else {
        crate::services::mcp::config::get_all_mcp_configs(&global, &project)
            .await
            .map_err(|error| error.to_string())?
            .servers
    };
    configs.extend(startup.dynamic);
    if configs.is_empty() {
        return Ok(crate::state::app_state_store::McpState::default());
    }
    // Maps to main.tsx#connectMcpBatch: seed pending clients in configuration
    // order before any callback; print awaits completion before exposing state
    // to the existing native QueryEngine startup boundary.
    let state = std::sync::Mutex::new(crate::state::app_state_store::McpState {
        clients: crate::utils::process_env::ecmascript_object_entries(&configs)
            .into_iter()
            .map(|(name, config)| {
                crate::services::mcp::client::McpConnectionDiscovery::pending_with_config(
                    name, config,
                )
                .server
            })
            .collect(),
        ..Default::default()
    });
    crate::services::mcp::client::get_mcp_tools_commands_and_resources(
        |discovery| apply_headless_mcp_connection_attempt(&mut state.lock().unwrap(), discovery),
        &configs,
    )
    .await;
    Ok(state.into_inner().unwrap())
}

/// Native extraction of `main.tsx:3790-3809#connectMcpBatch`'s callback at
/// the existing print startup bridge: preserve client positions and first
/// tool/command names, rather than sorting a completed discovery array.
fn apply_headless_mcp_connection_attempt(
    state: &mut crate::state::app_state_store::McpState,
    discovery: impl Into<crate::services::mcp::client::McpConnectionDiscovery>,
) {
    let crate::services::mcp::client::McpConnectionDiscovery {
        server,
        tools,
        commands,
        ..
    } = discovery.into();
    if let Some(existing) = state
        .clients
        .iter_mut()
        .find(|existing| existing.client.name == server.client.name)
    {
        *existing = server;
    } else {
        state.clients.push(server);
    }
    state.tools.extend(tools);
    let mut tool_names = std::collections::HashSet::new();
    state
        .tools
        .retain(|tool| tool_names.insert(tool.name.clone()));
    state.commands.extend(commands);
    let mut command_names = std::collections::HashSet::new();
    state
        .commands
        .retain(|command| command_names.insert(command.name.clone()));
}

async fn run_stream_json(config: &CliConfig, schema: Option<serde_json::Value>) -> i32 {
    let launch_cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => return fail(&error.to_string()),
    };
    // A4: the source headless restore is awaited. Its retained synchronous
    // Rust readers may await plugins on this process executor, so do not block
    // the current-thread print loop while those readers run.
    let resume_config = config.clone();
    let resume_cwd = launch_cwd.clone();
    let resume_seed = match tokio::task::spawn_blocking(move || {
        load_headless_resume(&resume_config, &resume_cwd)
    })
    .await
    {
        Ok(Ok(seed)) => seed,
        Ok(Err(error)) => return fail(&error),
        Err(error) => return fail(&error.to_string()),
    };
    let cwd = std::env::current_dir().unwrap_or(launch_cwd);
    let workspace_trusted = crate::utils::config::check_has_trust_dialog_accepted();
    crate::main::initialize_lsp_if_trusted(workspace_trusted);
    let initial_mcp = match resolve_headless_mcp(config, &cwd).await {
        Ok(mcp) => mcp,
        Err(error) => return fail(&error),
    };

    let sdk_control = SdkControlBridge::default();
    let permission_resolver = if config.permission_prompt_tool.as_deref() == Some("stdio") {
        let bridge = sdk_control.clone();
        Some(std::sync::Arc::new(
            move |request, abort, agent_id: Option<String>, app_store: crate::tool::AppStoreRef| {
                let bridge = bridge.clone();
                Box::pin(async move {
                    request_sdk_tool_permission(
                        &bridge,
                        &request,
                        &abort,
                        agent_id.as_deref(),
                        &app_store,
                    )
                    .await
                })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = crate::types::permissions::PermissionPromptResponse,
                                > + Send,
                        >,
                    >
            },
        ) as QueryEnginePermissionResolver)
    } else {
        None
    };
    // CC print.ts:2189-2198 — with a stream-json SDK consumer on the other
    // end, MCP elicitations forward as `elicitation` control requests instead
    // of queuing for a dialog that headless mode never renders.
    let handle_elicitation = if config.input_format.as_deref() == Some("stream-json") {
        let bridge = sdk_control.clone();
        crate::tool::HandleElicitationCallback::new(move |server_name, params| {
            let bridge = bridge.clone();
            Box::pin(async move { request_sdk_elicitation(&bridge, &server_name, params).await })
        })
    } else {
        crate::tool::HandleElicitationCallback::default()
    };
    let mut engine = QueryEngine::new(QueryEngineConfig {
        cli_config: config.clone(),
        schema,
        resume_seed,
        mcp_state: Some(initial_mcp),
        output_sink: Some(QueryEngine::output_sink(json_line)),
        permission_resolver,
        handle_elicitation,
    });
    let engine_control = engine.control_handle();

    let (input_tx, input_rx) = async_channel::unbounded::<Result<StructuredInput, String>>();
    let reader_engine_control = engine_control.clone();
    let reader_sdk_control = sdk_control.clone();
    let replay_control_responses = config.replay_user_messages;
    std::thread::Builder::new()
        .name("sdk-stdin-reader".to_string())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut line_number = 0usize;
            while let Some(line) = next_stream_json_line(&mut stdin) {
                line_number += 1;
                let parsed = match line {
                    Ok(line) => parse_stream_json_line(&line, line_number),
                    Err(error) => Err(StreamInputError::Reported(format!(
                        "failed to read stream-json line {line_number}: {error}"
                    ))),
                };
                let parsed = match parsed {
                    Ok(parsed) => Ok(parsed),
                    // CC `cli/structuredIO.ts:457-462` — `processLine`'s catch
                    // prints the offending line and `process.exit(1)`s, so a
                    // malformed stdin line ends the session rather than being
                    // reported as one bad turn. `exit_process` is this port's
                    // established `process.exit` (`main.rs`,
                    // `entrypoints/cli.rs`): it drains the cleanup registry,
                    // which Node's exit handlers do here too.
                    Err(StreamInputError::Fatal(message)) => {
                        eprintln!("{message}");
                        crate::utils::cleanup_registry::exit_process(1);
                    }
                    Err(StreamInputError::Reported(message)) => Err(message),
                };
                if let Ok(StructuredInput::KeepAlive) = &parsed {
                    continue;
                }
                if let Ok(StructuredInput::EnvironmentUpdate(variables)) = &parsed {
                    crate::cli::structured_io::apply_environment_update(variables);
                    continue;
                }
                if let Ok(StructuredInput::ControlCancel(request_id)) = &parsed {
                    let sender = reader_sdk_control
                        .pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(request_id);
                    if let Some(sender) = sender {
                        let _ = sender.send_blocking(ControlResponseInput {
                            request_id: request_id.clone(),
                            subtype: "error".to_string(),
                            response: None,
                            error: Some("Control request cancelled by SDK host".to_string()),
                        });
                    }
                    continue;
                }
                if let Ok(StructuredInput::ControlResponse(response)) = &parsed {
                    let sender = reader_sdk_control
                        .pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&response.request_id);
                    if let Some(sender) = sender {
                        let _ = sender.send_blocking(response.clone());
                        if replay_control_responses {
                            let mut payload = serde_json::Map::from_iter([
                                ("subtype".to_string(), serde_json::json!(response.subtype)),
                                (
                                    "request_id".to_string(),
                                    serde_json::json!(response.request_id),
                                ),
                            ]);
                            if let Some(value) = &response.response {
                                payload.insert("response".to_string(), value.clone());
                            }
                            if let Some(error) = &response.error {
                                payload.insert("error".to_string(), error.clone().into());
                            }
                            json_line(serde_json::json!({
                                "type": "control_response",
                                "response": payload,
                            }));
                        }
                    }
                    continue;
                }
                if let Ok(StructuredInput::User(user)) = &parsed {
                    if !user.text.is_empty() {
                        reader_engine_control.queue_user();
                    }
                }
                if let Ok(StructuredInput::Control(control)) = &parsed {
                    let subtype = control
                        .request
                        .get("subtype")
                        .and_then(serde_json::Value::as_str);
                    if matches!(subtype, Some("interrupt" | "end_session")) {
                        reader_engine_control.interrupt();
                    }
                    if subtype == Some("interrupt") {
                        control_success(&control.request_id, None);
                        continue;
                    }
                }
                if input_tx.send_blocking(parsed).is_err() {
                    break;
                }
            }
            // CC `cli/structuredIO.ts:254-260` — `read()`'s close tail. Once
            // the input is exhausted CC sets `inputClosed` and rejects every
            // entry of `pendingRequests`, so an SDK host that closes stdin
            // while a `can_use_tool` / `hook_callback` / `elicitation` request
            // is in flight terminates it instead of leaving it hanging. This
            // thread IS that stream: `next_stream_json_line` returning `None`
            // is the `for await` completing (`:244-247`).
            //
            // The other way out of the loop — `send_blocking` failing because
            // `run_stream_json` has stopped receiving — is not CC's EOF, but it
            // ends the same producer, so a still-parked request would hang for
            // the same reason. Both exits close.
            reader_sdk_control.close_input();
        })
        .expect("failed to spawn SDK stdin reader");

    let mut initialized = false;
    let mut received_user_uuids = std::collections::HashSet::new();
    let mut exit_code = 0;
    while let Ok(input) = input_rx.recv().await {
        let (prompt, replay_input) = match input {
            Ok(StructuredInput::User(user)) if !user.text.is_empty() => {
                if let Some(uuid) = user.uuid.as_ref() {
                    if !received_user_uuids.insert(uuid.clone()) {
                        engine_control.cancel_queued_user();
                        if config.replay_user_messages {
                            json_line(crate::query_engine::replayed_user_message_value(
                                &user.text,
                                Some(&QueryEngineReplayInput {
                                    uuid: user.uuid.clone(),
                                    timestamp: user.timestamp.clone(),
                                }),
                            ));
                        }
                        continue;
                    }
                }
                let replay = QueryEngineReplayInput {
                    uuid: user.uuid.clone(),
                    timestamp: user.timestamp.clone(),
                };
                (user.text, Some(replay))
            }
            Ok(StructuredInput::User(_)) => continue,
            Ok(StructuredInput::History(message)) => {
                if config.replay_user_messages {
                    if let Some(value) = crate::query_engine::stream_message_value(&message) {
                        json_line(value);
                    }
                }
                engine.push_history(message);
                continue;
            }
            Ok(StructuredInput::Control(control)) => {
                if handle_control_request(control, &mut initialized, &mut engine, &sdk_control)
                    .await
                {
                    break;
                }
                continue;
            }
            Ok(
                StructuredInput::ControlResponse(_)
                | StructuredInput::ControlCancel(_)
                | StructuredInput::KeepAlive
                | StructuredInput::EnvironmentUpdate(_),
            ) => continue,
            Err(error) => {
                let mut outcome = QueryEngineOutcome::default();
                outcome.reason = "input_error".to_string();
                outcome.errors.push(error);
                exit_code = exit_code.max(finish_outcome(
                    "stream-json",
                    engine.schema().is_some(),
                    &outcome,
                    std::time::Duration::ZERO,
                ));
                continue;
            }
        };
        let started = std::time::Instant::now();
        match crate::query_engine::ask(&mut engine, prompt, replay_input).await {
            Ok(outcome) => {
                exit_code = exit_code.max(finish_outcome(
                    "stream-json",
                    engine.schema().is_some(),
                    &outcome,
                    started.elapsed(),
                ));
            }
            Err(error) => {
                let mut outcome = QueryEngineOutcome::default();
                outcome.reason = "error_during_execution".to_string();
                outcome.errors.push(error);
                exit_code = exit_code.max(finish_outcome(
                    "stream-json",
                    engine.schema().is_some(),
                    &outcome,
                    started.elapsed(),
                ));
            }
        }
    }
    exit_code
}

async fn handle_control_request(
    control: ControlInput,
    initialized: &mut bool,
    engine: &mut QueryEngine,
    sdk_control: &SdkControlBridge,
) -> bool {
    let subtype = control
        .request
        .get("subtype")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match subtype {
        "initialize" => {
            if *initialized {
                control_error(&control.request_id, "Already initialized");
                return false;
            }
            if let Some(prompt) = control
                .request
                .get("systemPrompt")
                .and_then(serde_json::Value::as_str)
            {
                engine.set_system_prompt(prompt.to_string());
            }
            if let Some(prompt) = control
                .request
                .get("appendSystemPrompt")
                .and_then(serde_json::Value::as_str)
            {
                engine.set_append_system_prompt(prompt.to_string());
            }
            if let Some(agents) = control
                .request
                .get("agents")
                .and_then(serde_json::Value::as_object)
            {
                let mut merged = crate::bootstrap::state::get_cli_agents_json()
                    .and_then(|agents| agents.as_object().cloned())
                    .unwrap_or_default();
                merged.extend(agents.clone());
                crate::bootstrap::state::set_cli_agents_json(Some(merged.into()));
            }
            // CC print.ts:4433-4448 — the initialize request registers SDK
            // callback hooks: each hookCallbackId becomes a HookCallback that
            // calls back into the consumer via a hook_callback control request.
            if let Some(hooks) = control
                .request
                .get("hooks")
                .and_then(serde_json::Value::as_object)
            {
                let mut registered = std::collections::HashMap::new();
                for (event, matchers) in hooks {
                    let Some(matchers) = matchers.as_array() else {
                        continue;
                    };
                    let matchers = matchers
                        .iter()
                        .filter_map(|matcher| {
                            let callback_ids = matcher.get("hookCallbackIds")?.as_array()?;
                            let timeout =
                                matcher.get("timeout").and_then(serde_json::Value::as_u64);
                            let hooks = callback_ids
                                .iter()
                                .filter_map(|id| id.as_str())
                                .map(|callback_id| {
                                    crate::schemas::hooks::RegisteredHook::Callback(
                                        create_hook_callback(
                                            sdk_control,
                                            callback_id.to_string(),
                                            timeout,
                                        ),
                                    )
                                })
                                .collect();
                            Some(crate::schemas::hooks::RegisteredHookMatcher {
                                matcher: matcher
                                    .get("matcher")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string),
                                hooks,
                                ..Default::default()
                            })
                        })
                        .collect();
                    registered.insert(event.clone(), matchers);
                }
                crate::bootstrap::state::register_hook_callbacks(registered);
            }
            if let Some(requested_schema) = control.request.get("jsonSchema") {
                // CC print.ts:4450-4452 stores the init-control schema
                // unconditionally and unvalidated; an invalid schema only
                // shows up later as a silently absent StructuredOutput tool
                // (main.tsx:2781-2801). The argv schema still wins for tool
                // creation (print.ts:1493) — see QueryEngine's two slots.
                engine.set_init_schema(requested_schema.clone());
            }
            // Maps to print.ts:2880 await handleInitializeRequest, followed
            // by initialized=true: the response completes before that flag.
            match initialize_control_response().await {
                Ok(response) => {
                    control_success(&control.request_id, Some(response));
                    *initialized = true;
                }
                Err(error) => control_error(&control.request_id, &error.to_string()),
            }
        }
        "set_model" => {
            let requested = control
                .request
                .get("model")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("default");
            engine.set_model(if requested == "default" {
                crate::utils::model::model::get_default_main_loop_model()
            } else {
                requested.to_string()
            });
            control_success(&control.request_id, None);
        }
        "set_max_thinking_tokens" => {
            match control.request.get("max_thinking_tokens") {
                Some(serde_json::Value::Null) | None => engine.set_thinking(None),
                Some(value) if value.as_i64() == Some(0) => {
                    engine.set_thinking(Some(crate::utils::thinking::ThinkingConfig::Disabled));
                }
                Some(value) if value.as_i64().is_some_and(|tokens| tokens > 0) => {
                    engine.set_thinking(Some(crate::utils::thinking::ThinkingConfig::Enabled {
                        budget_tokens: value.as_i64(),
                    }));
                }
                _ => {
                    control_error(
                        &control.request_id,
                        "max_thinking_tokens must be null or a non-negative integer",
                    );
                    return false;
                }
            }
            control_success(&control.request_id, None);
        }
        "set_permission_mode" => {
            let Some(mode) = control
                .request
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .and_then(crate::utils::permissions::permission_mode::external_permission_mode_from_string)
            else {
                control_error(&control.request_id, "Invalid permission mode");
                return false;
            };
            engine.set_permission_mode(mode);
            control_success(&control.request_id, None);
            json_line(serde_json::json!({
                "type": "system",
                "subtype": "status",
                "status": null,
                "permissionMode": crate::utils::permissions::permission_mode::to_external_permission_mode(mode),
                "uuid": uuid::Uuid::new_v4().to_string(),
                "session_id": crate::bootstrap::state::get_session_id(),
            }));
        }
        "mcp_status" => {
            // CC reads `connection.capabilities` straight off the client
            // records it is already filtering; Rust keeps them on the live
            // peers, so they are fetched here and handed to the projection.
            let experimental =
                crate::services::mcp::client::experimental_capabilities_by_server().await;
            let servers = engine
                .mcp_state()
                .map(|state| build_mcp_server_statuses(state, &experimental))
                .unwrap_or_default();
            control_success(
                &control.request_id,
                Some(serde_json::json!({"mcpServers": servers})),
            );
        }
        "mcp_reconnect" => {
            let Some(server_name) = control
                .request
                .get("serverName")
                .and_then(serde_json::Value::as_str)
            else {
                control_error(&control.request_id, "serverName is required");
                return false;
            };
            let config = engine
                .mcp_state()
                .and_then(|state| {
                    state
                        .clients
                        .iter()
                        .find(|server| server.client.name == server_name)
                })
                .and_then(|server| server.config.clone());
            let Some(config) = config else {
                control_error(
                    &control.request_id,
                    &format!("Server not found: {server_name}"),
                );
                return false;
            };
            let server =
                crate::services::mcp::use_manage_mcp_connections::reconnect_mcp_server_once(
                    server_name,
                    &config,
                )
                .await;
            let connected = server.server.client.status
                == crate::services::mcp::types::McpServerConnectionType::Connected;
            let error = server.server.client.error.clone();
            crate::services::mcp::use_manage_mcp_connections::apply_mcp_server_update(
                engine.mcp_state_mut(),
                server,
            );
            if connected {
                control_success(&control.request_id, None);
            } else {
                control_error(
                    &control.request_id,
                    error.as_deref().unwrap_or("Connection failed"),
                );
            }
        }
        "mcp_toggle" => {
            let Some(server_name) = control
                .request
                .get("serverName")
                .and_then(serde_json::Value::as_str)
            else {
                control_error(&control.request_id, "serverName is required");
                return false;
            };
            let Some(enabled) = control
                .request
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
            else {
                control_error(&control.request_id, "enabled is required");
                return false;
            };
            let state = engine.mcp_state_mut();
            let config = state
                .clients
                .iter()
                .find(|server| server.client.name == server_name)
                .and_then(|server| server.config.clone());
            let Some(config) = config else {
                control_error(
                    &control.request_id,
                    &format!("Server not found: {server_name}"),
                );
                return false;
            };
            let currently_enabled = state
                .clients
                .iter()
                .find(|server| server.client.name == server_name)
                .is_some_and(|server| {
                    server.client.status
                        != crate::services::mcp::types::McpServerConnectionType::Disabled
                });
            if currently_enabled == enabled {
                control_success(&control.request_id, None);
                return false;
            }
            match crate::services::mcp::use_manage_mcp_connections::toggle_mcp_server_once(
                server_name,
                state,
                &config,
            )
            .await
            {
                Ok(server) => {
                    crate::services::mcp::use_manage_mcp_connections::apply_mcp_server_update(
                        state, server,
                    );
                    control_success(&control.request_id, None);
                }
                Err(error) => control_error(&control.request_id, &error.to_string()),
            }
        }
        "cancel_async_message" => {
            control_success(
                &control.request_id,
                Some(serde_json::json!({"cancelled": false})),
            );
        }
        "seed_read_state" => {
            if let Some(seed) = read_state_seed_from_control(&control.request) {
                engine.seed_read_state(seed);
            }
            control_success(&control.request_id, None);
        }
        "apply_flag_settings" => {
            let Some(incoming) = control
                .request
                .get("settings")
                .and_then(serde_json::Value::as_object)
            else {
                control_error(&control.request_id, "settings must be an object");
                return false;
            };
            let mut merged = crate::utils::settings::get_flag_settings_inline()
                .and_then(|settings| settings.as_object().cloned())
                .unwrap_or_default();
            for (key, value) in incoming {
                if value.is_null() {
                    merged.remove(key);
                } else {
                    merged.insert(key.clone(), value.clone());
                }
            }
            crate::utils::settings::set_flag_settings_inline(Some(merged.into()));
            if incoming.contains_key("model") {
                engine.set_model_override(
                    incoming
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                );
            }
            control_success(&control.request_id, None);
        }
        "get_settings" => {
            let settings = crate::utils::settings::get_initial_settings();
            let model = engine.current_model();
            control_success(
                &control.request_id,
                Some(serde_json::json!({
                    "effective": settings,
                    "sources": [],
                    "applied": {"model": model, "effort": null}
                })),
            );
        }
        // Maps to CC `cli/print.ts:3783-3812`: fire-and-forget so the Haiku
        // call does not block the stdin loop; the control response is sent
        // when the title resolves. CC reuses the live query abortController
        // when it is not yet aborted; the engine does not expose that
        // controller, so this always takes CC's fallback branch (a fresh
        // controller) — an L1 timing-only difference on interrupt.
        "generate_session_title" => {
            let description = control
                .request
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let persist = control
                .request
                .get("persist")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let request_id = control.request_id.clone();
            tokio::spawn(async move {
                let abort = crate::tool::AbortController::default();
                let title =
                    crate::utils::session_title::generate_session_title(&description, &abort).await;
                if let Some(title) = title.as_deref().filter(|_| persist) {
                    if let Err(error) = crate::utils::session_storage::save_ai_generated_title(
                        &crate::bootstrap::state::get_session_id(),
                        title,
                    ) {
                        // CC wraps this write in try/logError; Rust's error
                        // log channel here is the debug log.
                        crate::utils::debug::log_for_debugging(&format!(
                            "saveAiGeneratedTitle failed: {error}"
                        ));
                    }
                }
                control_success(&request_id, Some(serde_json::json!({ "title": title })));
            });
        }
        // Maps to CC `cli/print.ts:3772-3781`: SDK and TaskStopTool share
        // `tasks/stopTask.ts` rather than maintaining separate kill logic.
        "stop_task" => {
            let Some(task_id) = control
                .request
                .get("task_id")
                .and_then(serde_json::Value::as_str)
            else {
                control_error(&control.request_id, "task_id is required");
                return false;
            };
            let app_store = engine.context().and_then(|context| {
                context
                    .app_store
                    .tasks_store
                    .clone()
                    .or_else(|| context.app_store.store.clone())
            });
            match crate::tasks::stop_task::stop_task(task_id, app_store.as_ref()).await {
                Ok(_) => control_success(
                    &control.request_id,
                    Some(serde_json::Value::Object(Default::default())),
                ),
                Err(error) => control_error(&control.request_id, &error.to_string()),
            }
        }
        "interrupt" => control_success(&control.request_id, None),
        "end_session" => {
            control_success(&control.request_id, None);
            return true;
        }
        "get_context_usage" => match engine.context() {
            Some(context) => {
                // Maps to print.ts's awaited collectContextData. Its sync
                // adapter can rediscover commands after cache invalidation.
                let context = context.clone();
                match tokio::task::spawn_blocking(move || context_usage_response(&context)).await {
                    Ok(response) => control_success(&control.request_id, Some(response)),
                    Err(error) => control_error(&control.request_id, &error.to_string()),
                }
            }
            None => control_error(
                &control.request_id,
                "Context usage is unavailable before the first query",
            ),
        },
        other => control_error(
            &control.request_id,
            &format!("Unsupported control request subtype: {other}"),
        ),
    }
    false
}

fn read_state_seed_from_control(
    request: &serde_json::Value,
) -> Option<crate::utils::query_helpers::ReadFileStateEntry> {
    let raw_path = request.get("path")?.as_str()?;
    let observed_mtime = request.get("mtime")?.as_f64()?.floor() as u128;
    let mut path = crate::utils::plugins::plugin_directories::expand_tilde_path(raw_path);
    if path.is_relative() {
        path = std::env::current_dir().ok()?.join(path);
    }
    let path = std::fs::canonicalize(path).ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    let disk_mtime = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    if disk_mtime > observed_mtime {
        return None;
    }
    let mut content = std::fs::read_to_string(&path).ok()?;
    if content.starts_with('\u{feff}') {
        content.remove(0);
    }
    content = content.replace("\r\n", "\n");
    Some(crate::utils::query_helpers::ReadFileStateEntry {
        path: path.to_string_lossy().to_string(),
        content: Some(content),
        timestamp_ms: i64::try_from(disk_mtime).ok(),
        offset: None,
        limit: None,
        is_partial_view: false,
        source: crate::utils::query_helpers::ReadFileStateSource::Read,
    })
}

/// Maps to: CC `cli/print.ts:1474-1500` `buildAllTools(appState)`.
///
/// "Shared tool assembly for ask() and the get_context_usage control request."
/// The headless pool is NOT `getTools()` plus appended MCP tools — CC runs the
/// same `assembleToolPool` → `mergeAndFilterTools` partition-sort the REPL
/// uses (`hooks/useMergedTools.ts:30`), so built-ins are a name-sorted
/// contiguous prefix and MCP tools a name-sorted suffix. That order is what
/// the server's `claude_code_system_cache_policy` prefix-matches against
/// (`tools.ts:354-359`); an insertion-ordered pool defeats it in `-p` mode.
///
/// * `launch_tools` — CC `[...tools, ...sdkTools, ...dynamicMcpState.tools]`
///   (`:1481`): the `main.tsx:2755-2785` `tools` argument, produced by
///   [`crate::main::build_headless_tools`]. The port's `McpState.tools`
///   already carries SDK/dynamic servers, so they enter via `mcp_tools` and
///   the deny filter inside `assemble_tool_pool` instead.
/// * `permission_context` / `mcp_tools` — CC `appState.toolPermissionContext`
///   / `appState.mcp.tools` (`:1475-1478`).
/// * `init_schema_tool` — CC `:1492-1498`: the SDK `initialize` jsonSchema
///   tool, created only when argv had no `--json-schema`, appended AFTER the
///   sorted pool.
///
/// CC `:1487-1491` drops `options.permissionPromptToolName` from the pool;
/// this port only accepts `--permission-prompt-tool=stdio` ([`run`]), which
/// names no tool, so that filter has nothing to remove here.
///
/// Called from `query_engine.rs#run_query` because the Rust engine builds its
/// own pool from `CliConfig` (CC's `print.ts` calls this before handing
/// `tools` to `QueryEngine.ask`); the body stays here with its source.
pub(crate) fn build_all_tools(
    launch_tools: &[crate::types::tools::Tool],
    permission_context: &crate::tool::ToolPermissionContext,
    mcp_tools: &[crate::types::tools::Tool],
    init_schema_tool: Option<crate::types::tools::Tool>,
) -> Vec<crate::types::tools::Tool> {
    // `:1475-1478` `assembleToolPool(appState.toolPermissionContext, appState.mcp.tools)`.
    let assembled = crate::tools::assemble_tool_pool(permission_context, mcp_tools);
    // `:1479-1486` `uniqBy(mergeAndFilterTools(initialTools, assembled, mode), 'name')`
    // — `merge_and_filter_tools` already dedupes by name, so the outer `uniqBy`
    // is a no-op the port does not repeat.
    let mut all_tools = crate::utils::tool_pool::merge_and_filter_tools(
        launch_tools,
        assembled,
        permission_context.mode,
    );
    // `:1492-1498` `if (initJsonSchema && !options.jsonSchema) allTools = [...allTools, tool]`.
    // No dedupe needed: `get_tools` strips StructuredOutput as a special tool.
    all_tools.extend(init_schema_tool);
    all_tools
}

fn context_usage_response(context: &crate::tool::ToolUseContext) -> serde_json::Value {
    let data = crate::commands::context::context::collect_context_data(
        &crate::commands::context::ContextCommandRequest::new(context),
    );
    let categories = data
        .categories
        .iter()
        .map(|category| {
            serde_json::json!({
                "name": category.name,
                "tokens": category.tokens,
                "color": format!("{:?}", category.color),
                "isDeferred": category.is_deferred,
            })
        })
        .collect::<Vec<_>>();
    let grid_rows = data
        .grid_rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|square| {
                    serde_json::json!({
                        "color": format!("{:?}", square.color),
                        "isFilled": square.square_fullness > 0.0,
                        "categoryName": square.category_name,
                        "tokens": 0,
                        "percentage": square.square_fullness * 100.0,
                        "squareFullness": square.square_fullness,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut response = serde_json::Map::new();
    response.insert("categories".to_string(), categories.into());
    response.insert("totalTokens".to_string(), data.total_tokens.into());
    response.insert("maxTokens".to_string(), data.max_tokens.into());
    response.insert("rawMaxTokens".to_string(), data.raw_max_tokens.into());
    response.insert("percentage".to_string(), data.percentage.into());
    response.insert("gridRows".to_string(), grid_rows.into());
    response.insert("model".to_string(), data.model.into());
    response.insert(
        "memoryFiles".to_string(),
        data.memory_files
            .into_iter()
            .map(|file| {
                serde_json::json!({
                    "path": file.path, "type": file.file_type, "tokens": file.tokens
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    response.insert(
        "mcpTools".to_string(),
        data.mcp_tools
            .into_iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name, "serverName": tool.server_name,
                    "tokens": tool.tokens, "isLoaded": tool.is_loaded
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    response.insert(
        "deferredBuiltinTools".to_string(),
        data.deferred_builtin_tools
            .into_iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name, "tokens": tool.tokens, "isLoaded": tool.is_loaded
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    response.insert(
        "systemTools".to_string(),
        data.system_tools
            .into_iter()
            .map(|tool| serde_json::json!({"name": tool.name, "tokens": tool.tokens}))
            .collect::<Vec<_>>()
            .into(),
    );
    response.insert(
        "systemPromptSections".to_string(),
        data.system_prompt_sections
            .into_iter()
            .map(|section| {
                serde_json::json!({
                    "name": section.name, "tokens": section.tokens
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    response.insert(
        "agents".to_string(),
        data.agents
            .into_iter()
            .map(|agent| {
                serde_json::json!({
                    "agentType": agent.agent_type,
                    "source": agent.source.display_name(),
                    "tokens": agent.tokens,
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    if let Some(commands) = data.slash_commands {
        response.insert(
            "slashCommands".to_string(),
            serde_json::json!({
                "totalCommands": commands.total_commands,
                "includedCommands": commands.included_commands,
                "tokens": commands.tokens,
            }),
        );
    }
    if let Some(skills) = data.skills {
        response.insert(
            "skills".to_string(),
            serde_json::json!({
                "totalSkills": skills.total_skills,
                "includedSkills": skills.included_skills,
                "tokens": skills.tokens,
                "skillFrontmatter": skills.skill_frontmatter.into_iter().map(|skill| {
                    serde_json::json!({
                        "name": skill.name,
                        "source": skill.source.display_name(),
                        "tokens": skill.tokens,
                    })
                }).collect::<Vec<_>>(),
            }),
        );
    }
    if let Some(threshold) = data.auto_compact_threshold {
        response.insert("autoCompactThreshold".to_string(), threshold.into());
    }
    response.insert(
        "isAutoCompactEnabled".to_string(),
        data.is_auto_compact_enabled.into(),
    );
    if let Some(breakdown) = data.message_breakdown {
        response.insert(
            "messageBreakdown".to_string(),
            serde_json::json!({
                "toolCallTokens": breakdown.tool_call_tokens,
                "toolResultTokens": breakdown.tool_result_tokens,
                "attachmentTokens": breakdown.attachment_tokens,
                "assistantMessageTokens": breakdown.assistant_message_tokens,
                "userMessageTokens": breakdown.user_message_tokens,
                "toolCallsByType": breakdown.tool_calls_by_type.into_iter().map(|tool| {
                    serde_json::json!({
                        "name": tool.name, "callTokens": tool.call_tokens,
                        "resultTokens": tool.result_tokens
                    })
                }).collect::<Vec<_>>(),
                "attachmentsByType": breakdown.attachments_by_type.into_iter().map(|attachment| {
                    serde_json::json!({"name": attachment.name, "tokens": attachment.tokens})
                }).collect::<Vec<_>>(),
            }),
        );
    }
    response.insert(
        "apiUsage".to_string(),
        data.api_usage
            .and_then(|usage| serde_json::to_value(usage).ok())
            .unwrap_or(serde_json::Value::Null),
    );
    response.into()
}

/// Maps to: CC `cli/print.ts:1612 buildMcpServerStatuses` — the
/// `McpServerStatus[]` payload for control responses, shared there by the
/// `mcp_status` and `reload_plugins` handlers.
///
/// NOT the same function as `utils/messages/system_init.rs`'s namesake: that
/// one maps to `systemInit.ts:63`, which emits only `{name, status}` from a
/// single source. The two looked like duplicates in Rust because this one had
/// been written as a copy of that one, emitting 2 of the 8 fields the source
/// returns (`print.ts:1689-1698`).
///
/// Two source behaviours are deliberately absent, both because their
/// subsystems are unported rather than by choice:
/// - CC merges THREE client sources (`:1620-1626`): `appState.mcp.clients`
///   plus `sdkClients` plus `dynamicMcpState.clients`, deduped by name.
///   Neither extra source exists here yet.
/// - `reload_plugins` is the second caller upstream; not ported, so this has
///   one caller.
///
/// `experimental_by_server` supplies what CC reads straight off the connection
/// record (`connection.capabilities.experimental`, `:1673`). Rust keeps
/// capabilities on the live peer rather than on `McpServerSnapshot`, so the
/// caller fetches them from the registry and passes them in; this stays a pure
/// projection, as the source's `.map()` is.
///
/// SEAM (`serverInfo.name`): CC returns `{name, version}`;
/// `McpClientSnapshot` only carries `server_version`, so only the version is
/// emitted.
///
/// SEAM (`KAIROS_CHANNELS`): CC's gate is
/// `feature('KAIROS') || feature('KAIROS_CHANNELS')`. Rust carries only the
/// `Kairos` flag — `KAIROS_CHANNELS` has no counterpart, the same gap already
/// recorded at `utils/messages.rs:370-371`. The gate is therefore narrower
/// than the source's: a build with only KAIROS_CHANNELS would echo
/// capabilities upstream and does not here.
fn build_mcp_server_statuses(
    state: &crate::state::app_state_store::McpState,
    experimental_by_server: &std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, serde_json::Value>,
    >,
) -> Vec<serde_json::Value> {
    use crate::services::mcp::types::{McpServerConnectionType, Transport};

    // Only the feature flag is hoisted: CC's `feature('KAIROS')` is a build
    // constant. `isChannelsEnabled()` and the global config are NOT hoisted —
    // CC reaches them only inside `if (exp['claude/channel'] && ...)`
    // (`:1677-1680`), so a server that never declared the channel capability
    // must not cause them to be read at all. Both have process-level side
    // effects here (config file load + cache, growthbook gate), so hoisting
    // them would run work the source never runs.
    let kairos = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::Kairos,
    );

    state
        .clients
        .iter()
        .map(|server| {
            let connected = server.client.status == McpServerConnectionType::Connected;

            // CC `:1628-1651` — three shapes, and anything else (ws/sdk) leaves
            // `config` undefined.
            let config = server
                .config
                .as_ref()
                .and_then(|config| match config.transport {
                    Transport::Sse | Transport::Http => Some(serde_json::json!({
                        "type": config.transport.as_str(),
                        "url": config.url,
                        "headers": config.headers,
                        "oauth": config.oauth,
                    })),
                    Transport::ClaudeAiProxy => Some(serde_json::json!({
                        "type": Transport::ClaudeAiProxy.as_str(),
                        "url": config.url,
                        "id": config.id,
                    })),
                    Transport::Stdio => Some(serde_json::json!({
                        "type": Transport::Stdio.as_str(),
                        "command": config.command,
                        "args": config.args,
                    })),
                    Transport::SseIde | Transport::Ws | Transport::WsIde | Transport::Sdk => None,
                });

            // CC `:1653-1663` — tools only for connected servers, each with the
            // three annotations. The `|| undefined` there means a false hint is
            // ABSENT, not `false`.
            let tools = connected.then(|| {
                server
                    .tools
                    .iter()
                    .map(|tool| {
                        let mut annotations = serde_json::Map::new();
                        if tool.read_only_hint {
                            annotations.insert("readOnly".to_string(), true.into());
                        }
                        if tool.destructive_hint {
                            annotations.insert("destructive".to_string(), true.into());
                        }
                        if tool.open_world_hint {
                            annotations.insert("openWorld".to_string(), true.into());
                        }
                        serde_json::json!({
                            "name": tool.name,
                            "annotations": serde_json::Value::Object(annotations),
                        })
                    })
                    .collect::<Vec<_>>()
            });

            let mut status = serde_json::Map::new();
            status.insert("name".to_string(), server.client.name.clone().into());
            status.insert(
                "status".to_string(),
                server.client.status.as_str().to_string().into(),
            );
            // CC `:1691-1692` — connected only.
            if let (true, Some(version)) = (connected, server.client.server_version.as_ref()) {
                status.insert(
                    "serverInfo".to_string(),
                    serde_json::json!({ "version": version }),
                );
            }
            // CC `:1693` — failed only.
            if server.client.status == McpServerConnectionType::Failed {
                if let Some(error) = server.client.error.as_ref() {
                    status.insert("error".to_string(), error.clone().into());
                }
            }
            if let Some(config) = config {
                status.insert("config".to_string(), config);
            }
            if let Some(scope) = server.config.as_ref().map(|config| config.scope.as_str()) {
                status.insert("scope".to_string(), scope.to_string().into());
            }
            if let Some(tools) = tools {
                status.insert("tools".to_string(), tools.into());
            }

            // CC `:1665-1688` — capabilities passthrough with an allowlist
            // pre-filter. The source's comment states the intent: the IDE reads
            // `experimental['claude/channel']` to decide whether to show the
            // Enable-channel prompt, so it is only echoed when `channel_enable`
            // would actually pass the allowlist. Not a security boundary — the
            // handler re-runs the full gate — just avoids dead buttons.
            if kairos && connected {
                if let Some(experimental) = experimental_by_server.get(&server.client.name) {
                    let mut experimental = experimental.clone();
                    let channel_key =
                        crate::services::mcp::channel_notification::CHANNEL_EXPERIMENTAL_CAPABILITY;
                    if experimental.contains_key(channel_key) {
                        // CC `:1678-1680` — both reads happen here, guarded by
                        // the key's presence.
                        let would_pass =
                            crate::services::mcp::channel_allowlist::is_channels_enabled()
                                && crate::services::mcp::channel_allowlist::is_channel_allowlisted(
                                    server
                                        .config
                                        .as_ref()
                                        .and_then(|config| config.plugin_source.as_deref()),
                                );
                        if !would_pass {
                            experimental.remove(channel_key);
                        }
                    }
                    // CC `:1685-1687` — an empty map leaves `capabilities`
                    // undefined rather than emitting `{experimental: {}}`.
                    if !experimental.is_empty() {
                        status.insert(
                            "capabilities".to_string(),
                            serde_json::json!({
                                "experimental": experimental
                                    .into_iter()
                                    .collect::<serde_json::Map<String, serde_json::Value>>(),
                            }),
                        );
                    }
                }
            }

            serde_json::Value::Object(status)
        })
        .collect()
}

fn sdk_model_infos() -> Vec<serde_json::Value> {
    crate::components::model_picker::model_picker_options(usize::MAX)
        .into_iter()
        .map(|option| {
            let value = if option.value == crate::components::model_picker::MODEL_NO_PREFERENCE {
                "default".to_string()
            } else {
                option.value
            };
            let resolved = if value == "default" {
                crate::utils::model::model::get_default_main_loop_model()
            } else {
                crate::utils::model::model::parse_user_specified_model(&value)
            };
            let mut info = serde_json::Map::from_iter([
                ("value".to_string(), serde_json::json!(value)),
                ("displayName".to_string(), serde_json::json!(option.label)),
                (
                    "description".to_string(),
                    serde_json::json!(option.description.unwrap_or_default()),
                ),
            ]);
            if crate::utils::effort::model_supports_effort(&resolved) {
                info.insert("supportsEffort".to_string(), true.into());
                let mut levels = vec!["low", "medium", "high"];
                if crate::utils::effort::model_supports_xhigh_effort(&resolved) {
                    levels.push("xhigh");
                }
                if crate::utils::effort::model_supports_max_effort(&resolved) {
                    levels.push("max");
                }
                info.insert(
                    "supportedEffortLevels".to_string(),
                    serde_json::json!(levels),
                );
            }
            if crate::utils::thinking::model_supports_adaptive_thinking(&resolved) {
                info.insert("supportsAdaptiveThinking".to_string(), true.into());
            }
            if crate::utils::fast_mode::is_fast_mode_supported_by_model(Some(&value)) {
                info.insert("supportsFastMode".to_string(), true.into());
            }
            info.into()
        })
        .collect()
}

/// Maps to print.ts#handleInitializeRequest's awaited response construction.
/// A4: keep the print executor running while existing sync command/agent
/// readers await plugin promises scheduled on that same executor.
async fn initialize_control_response() -> Result<serde_json::Value, tokio::task::JoinError> {
    tokio::task::spawn_blocking(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        let commands = crate::commands::get_commands(&cwd)
            .into_iter()
            .filter(|command| command.user_invocable)
            .map(|command| {
                serde_json::json!({
                    "name": crate::commands::get_command_name(&command),
                    "description": crate::commands::format_description_with_source(&command),
                    "argumentHint": command.argument_hint.as_deref().unwrap_or(""),
                })
            })
            .collect::<Vec<_>>();
        let agents =
        crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides_readonly(
            &cwd,
        )
        .active_agents
        .into_iter()
        .map(|agent| {
            serde_json::json!({
                "name": agent.agent_type,
                "description": agent.when_to_use,
                "model": agent.model.filter(|model| model != "inherit"),
            })
        })
        .collect::<Vec<_>>();
        let settings = crate::utils::settings::get_initial_settings();
        let available_output_styles =
            crate::constants::output_styles::get_all_output_styles_ordered(&cwd)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
        serde_json::json!({
            "commands": commands,
            "agents": agents,
            "output_style": settings.output_style.as_deref().unwrap_or(
                crate::constants::output_styles::DEFAULT_OUTPUT_STYLE_NAME
            ),
            "available_output_styles": available_output_styles,
            "models": sdk_model_infos(),
            "account": {
                "apiProvider": crate::utils::model::providers::get_api_provider_for_statsig()
            },
            "pid": std::process::id(),
        })
    })
    .await
}

fn control_success(request_id: &str, response: Option<serde_json::Value>) {
    let mut payload = serde_json::Map::from_iter([
        (
            "subtype".to_string(),
            serde_json::Value::String("success".to_string()),
        ),
        (
            "request_id".to_string(),
            serde_json::Value::String(request_id.to_string()),
        ),
    ]);
    if let Some(response) = response {
        payload.insert("response".to_string(), response);
    }
    json_line(serde_json::json!({
        "type": "control_response",
        "response": payload,
    }));
}

fn control_error(request_id: &str, error: &str) {
    json_line(serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": error,
        }
    }));
}

fn finish_outcome(
    format: &str,
    structured_required: bool,
    outcome: &QueryEngineOutcome,
    duration: std::time::Duration,
) -> i32 {
    let missing_structured = structured_required && outcome.structured.is_none();
    let is_error = missing_structured
        || !outcome.errors.is_empty()
        || matches!(
            outcome.reason.as_str(),
            "model_error"
                | "aborted_streaming"
                | "aborted_tools"
                | "prompt_too_long"
                | "input_error"
                | "max_turns"
                | "max_budget_usd"
        );
    if format == "text" {
        if is_error {
            for error in &outcome.errors {
                eprintln!("Error: {error}");
            }
            if missing_structured {
                eprintln!("Error: The model did not provide StructuredOutput");
            }
        } else if let Some(value) = &outcome.structured {
            println!("{}", serde_json::to_string(value).unwrap_or_default());
        } else {
            println!("{}", outcome.text);
        }
    } else {
        let mut projected_errors = outcome.errors.clone();
        if missing_structured {
            projected_errors.push("The model did not provide StructuredOutput".to_string());
        }
        emit_result(
            outcome,
            duration.as_millis() as u64,
            is_error,
            missing_structured,
            &projected_errors,
        );
    }
    i32::from(is_error)
}

fn read_prompt(config: &CliConfig) -> Result<String, String> {
    if let Some(prompt) = &config.prompt {
        return Ok(prompt.clone());
    }
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("failed to read stdin: {error}"))?;
    Ok(input)
}

/// How a rejected stream-json line ends.
///
/// CC `processLine` has no error return: the `jsonParse` throw reaches its
/// catch, which prints and `process.exit(1)`s (`cli/structuredIO.ts:457-462`),
/// and the two explicit branches take the same destiny through
/// `exitWithMessage` (`:444`, `:452`); an unknown `type`, by contrast, is
/// logged and dropped (`:436-441`). The port's parser stays a pure function so
/// both destinies are testable, so the destiny travels with the error instead
/// of being taken inside the parser.
///
/// Only the `jsonParse` branch is classified so far. Everything else keeps the
/// pre-existing [`StreamInputError::Reported`] destiny — that is NOT a claim
/// that CC reports them; it is the port's current behaviour, left alone
/// because each of the other branches lands on a different CC line and they
/// were not audited in this cut.
#[derive(Clone, Debug, PartialEq)]
enum StreamInputError {
    /// CC `cli/structuredIO.ts:457-462` — `console.error(...)` then
    /// `process.exit(1)`. The session does not survive the line.
    Fatal(String),
    /// The port's own arm: an `input_error` result, and reading continues
    /// (`run_stream_json`'s `Err` branch).
    Reported(String),
}

impl From<StreamInputError> for String {
    fn from(error: StreamInputError) -> Self {
        match error {
            StreamInputError::Fatal(message) | StreamInputError::Reported(message) => message,
        }
    }
}

/// Maps to: CC `cli/structuredIO.ts:222-253` — the SPLITTING half of `read()`,
/// which sits with the `processLine` port rather than in
/// [`crate::cli::structured_io`] for the same reason
/// [`parse_stream_json_line`] does.
///
/// CC accumulates arrivals into `content` and cuts at `content.indexOf('\n')`,
/// taking `content.slice(0, newline)` (`:228-231`). The byte BEFORE the
/// newline is never examined, so on a CRLF stream the line handed to
/// `processLine` still ends in `\r`; an unterminated remainder left when the
/// stream closes is processed as one final line (`:248-253`).
///
/// `BufRead::lines()`, which this replaced, cannot express that — it pops a
/// trailing `\r` along with the `\n`. The difference is load-bearing at
/// exactly one place, `processLine`'s falsiness short-circuit (`:337-339`):
/// under CRLF a blank line reaches CC as `"\r"`, which is TRUTHY, so it goes
/// on to `jsonParse`, throws, and the catch exits 1 (`:457-462`). Stripped to
/// `""` it was silently skipped instead, so #190's `""`-only short-circuit
/// held for LF and could not hold for CRLF until the splitter matched. Every
/// other line was also missing a trailing `\r`, but there the divergence is
/// invisible: `\r` is JSON whitespace, so `jsonParse` accepts it either way.
///
/// Reachability is not hypothetical. CC hands `process.stdin` to `StructuredIO`
/// raw for `--input-format stream-json` (`main.tsx:1192-1193`), and neither
/// Node nor Rust translates line endings on a pipe, so whatever the writer
/// emits arrives verbatim. Text-mode writers emit CRLF by default on Windows:
/// .NET/PowerShell `WriteLine` uses `Environment.NewLine`, and Python's
/// `subprocess` text-mode stdin translates `\n` to `os.linesep`. A CRLF file
/// piped in (`type input.jsonl | …`, or a `core.autocrlf` checkout `cat`ted on
/// any platform) is the same stream.
fn next_stream_json_line(reader: &mut impl std::io::BufRead) -> Option<std::io::Result<String>> {
    let mut buffer = Vec::new();
    match reader.read_until(b'\n', &mut buffer) {
        // Nothing buffered at EOF: CC's `for await` completes and the trailing
        // `if (content)` is false (`:248`), so there is no final line.
        Ok(0) => None,
        Ok(_) => {
            // CC `:230-231` — `slice(0, newline)` drops the `\n` and NOTHING
            // else; `:248` hands an unterminated tail over untouched.
            if buffer.last() == Some(&b'\n') {
                buffer.pop();
            }
            // Undecodable input keeps the destiny it already had (an
            // `io::Error` the caller reports as one bad line, reading
            // continues). CC differs here and is not followed: `content +=
            // block` stringifies each Buffer chunk lossily, so it would splice
            // in U+FFFD and let `jsonParse` decide. That is part of `read()`'s
            // buffering, not its splitting, and is not ported.
            Some(
                String::from_utf8(buffer)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        }
        Err(error) => Some(Err(error)),
    }
}

/// Maps to: CC `cli/structuredIO.ts:333-463#processLine`, blended with
/// `cli/print.ts:4893#loadInitialMessages`' message→history conversion (see
/// MODULE_MAP row 64 — the blend is why this stayed in `print.rs` through the
/// #180 relocation).
fn parse_stream_json_line(
    line: &str,
    line_number: usize,
) -> Result<StructuredInput, StreamInputError> {
    // CC `:336-339` — `if (!line) return undefined`. JS falsiness here is
    // EXACTLY the empty string: `"   "` is truthy, so it goes on to `jsonParse`
    // and throws. `line.trim().is_empty()` widened that to every all-whitespace
    // line and coerced it into a manufactured user turn, so a malformed line
    // was swallowed at the SDK trust boundary instead of rejected (#157).
    //
    // The empty-text user turn IS CC's `undefined` for both consumers: the
    // stdin reader only queues a user for `!user.text.is_empty()`, and
    // `run_stream_json`'s loop `continue`s on it. Nothing is manufactured
    // downstream; `StructuredInput` simply has no `Skip` variant for it to be.
    if line.is_empty() {
        return Ok(StructuredInput::User(UserInput {
            text: String::new(),
            uuid: None,
            timestamp: None,
        }));
    }
    // CC `:341` `jsonParse(line)` — the one failure inside `processLine`'s
    // `try` that this port classifies, and the one the whitespace line lands on.
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
        // CC `:459` prints the offending LINE, not its index.
        StreamInputError::Fatal(format!(
            "Error parsing streaming input line: {line}: {error}"
        ))
    })?;
    parse_stream_json_value(&value, line_number).map_err(StreamInputError::Reported)
}

/// `processLine`'s post-`jsonParse` type dispatch (CC `:344-456`).
fn parse_stream_json_value(
    value: &serde_json::Value,
    line_number: usize,
) -> Result<StructuredInput, String> {
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("keep_alive") => Ok(StructuredInput::KeepAlive),
        Some("update_environment_variables") => {
            let variables = value
                .get("variables")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| {
                    format!("stream-json line {line_number} has no environment variables")
                })?
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.clone(), value.to_string()))
                        .ok_or_else(|| {
                            format!(
                                "stream-json line {line_number} environment variable `{key}` is not a string"
                            )
                        })
                })
                .collect::<Result<indexmap::IndexMap<_, _>, _>>()?;
            Ok(StructuredInput::EnvironmentUpdate(variables))
        }
        Some("control_cancel_request") => {
            let request_id = value
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .filter(|request_id| !request_id.is_empty())
                .ok_or_else(|| {
                    format!("stream-json line {line_number} has no cancellation request_id")
                })?;
            Ok(StructuredInput::ControlCancel(request_id.to_string()))
        }
        Some("control_request") => {
            let request_id = value
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .filter(|request_id| !request_id.is_empty())
                .ok_or_else(|| {
                    format!("stream-json line {line_number} has no control request_id")
                })?;
            let request = value
                .get("request")
                .filter(|request| request.is_object())
                .ok_or_else(|| format!("stream-json line {line_number} has no control request"))?;
            if request
                .get("subtype")
                .and_then(serde_json::Value::as_str)
                .is_none()
            {
                return Err(format!(
                    "stream-json line {line_number} control request has no subtype"
                ));
            }
            Ok(StructuredInput::Control(ControlInput {
                request_id: request_id.to_string(),
                request: request.clone(),
            }))
        }
        Some("control_response") => {
            let payload = value
                .get("response")
                .filter(|response| response.is_object())
                .ok_or_else(|| format!("stream-json line {line_number} has no control response"))?;
            let request_id = payload
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .filter(|request_id| !request_id.is_empty())
                .ok_or_else(|| {
                    format!("stream-json line {line_number} control response has no request_id")
                })?;
            let subtype = payload
                .get("subtype")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    format!("stream-json line {line_number} control response has no subtype")
                })?;
            Ok(StructuredInput::ControlResponse(ControlResponseInput {
                request_id: request_id.to_string(),
                subtype: subtype.to_string(),
                response: payload.get("response").cloned(),
                error: payload
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }))
        }
        Some("assistant") => {
            parse_stream_assistant_history(value, line_number).map(StructuredInput::History)
        }
        Some("system") => Ok(crate::utils::conversation::into_typed_messages(
            crate::utils::messages::mappers::to_internal_messages(std::slice::from_ref(value)),
        )
        .into_iter()
        .next()
        .map(StructuredInput::History)
        .unwrap_or(StructuredInput::KeepAlive)),
        Some("user") | None if stream_user_has_tool_result(value) => {
            parse_stream_user_history(value, line_number).map(StructuredInput::History)
        }
        Some("user") | None => parse_stream_user_content(value, line_number).map(|text| {
            StructuredInput::User(UserInput {
                text,
                uuid: value
                    .get("uuid")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                timestamp: value.get("timestamp").cloned(),
            })
        }),
        Some(kind) => Err(format!(
            "stream-json line {line_number} has unsupported message type `{kind}`"
        )),
    }
}

fn parse_stream_user_content(
    value: &serde_json::Value,
    line_number: usize,
) -> Result<String, String> {
    let content = value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .ok_or_else(|| format!("stream-json line {line_number} has no message content"))?;
    match content {
        serde_json::Value::String(value) => Ok(value.clone()),
        serde_json::Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|block| block.get("type").and_then(serde_json::Value::as_str) != Some("text"))
            {
                return Err(format!(
                    "stream-json line {line_number} contains unsupported non-text user content"
                ));
            }
            let text = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>();
            Ok(text.join("\n"))
        }
        _ => Err(format!(
            "stream-json line {line_number} has invalid content"
        )),
    }
}

fn stream_user_has_tool_result(value: &serde_json::Value) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
            })
        })
}

fn parse_stream_user_history(
    value: &serde_json::Value,
    line_number: usize,
) -> Result<Message, String> {
    let blocks = value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("stream-json line {line_number} has no user history content"))?;
    // CC's adapter restores the envelope-level `tool_use_result` onto the
    // recreated user message (`remote/sdkMessageAdapter.ts:190,205`
    // `createUserMessage({ toolUseResult: msg.tool_use_result })`); the Rust
    // rows carry it on the ToolResult block.
    let envelope_tool_use_result = value.get("tool_use_result").cloned();
    let mut content = Vec::new();
    for block in blocks {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                    content.push(crate::types::message::UserContent::Text(text.to_string()));
                }
            }
            Some("tool_result") => {
                let Some(tool_use_id) =
                    block.get("tool_use_id").and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                let raw_content = block
                    .get("content")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!(""));
                let (text, content_blocks) = match raw_content {
                    serde_json::Value::String(text) => (text, Vec::new()),
                    serde_json::Value::Array(blocks) => {
                        let converted = blocks
                            .iter()
                            .map(crate::types::message::ToolResultContentBlock::from_structured_value)
                            .collect::<Vec<_>>();
                        let text = blocks
                            .iter()
                            .filter_map(|block| {
                                block.get("text").and_then(serde_json::Value::as_str)
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        (text, converted)
                    }
                    value => (value.to_string(), Vec::new()),
                };
                content.push(crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id: crate::types::ids::ToolUseId(tool_use_id.to_string()),
                        content: text,
                        is_error: block
                            .get("is_error")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        content_blocks,
                        tool_use_result: envelope_tool_use_result.clone(),
                    },
                ));
            }
            _ => {}
        }
    }
    Ok(Message::User(crate::types::message::UserMessage {
        // Stream-json envelopes carry the real message uuid; mint only when
        // the line predates it.
        uuid: value
            .get("uuid")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        timestamp: value
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
            .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now),
        content,
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    }))
}

/// Maps to CC `cli/print.ts:4042-4052`: history replay delegates to the
/// canonical SDK mapper. Validation stays at the structured input boundary;
/// raw-to-typed field conversion reuses the existing conversation adapter.
fn parse_stream_assistant_history(
    value: &serde_json::Value,
    line_number: usize,
) -> Result<Message, String> {
    let envelope = value
        .get("message")
        .filter(|message| message.is_object())
        .ok_or_else(|| format!("stream-json line {line_number} has no assistant message"))?;
    envelope
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("stream-json line {line_number} has no assistant content"))?;
    crate::utils::conversation::into_typed_messages(
        crate::utils::messages::mappers::to_internal_messages(std::slice::from_ref(value)),
    )
    .into_iter()
    .next()
    .ok_or_else(|| format!("stream-json line {line_number} has no assistant message"))
}

fn parse_stream_json_input(input: &str) -> Result<String, String> {
    input
        .lines()
        .enumerate()
        .map(
            |(index, line)| match parse_stream_json_line(line, index + 1)? {
                StructuredInput::User(user) => Ok(user.text),
                StructuredInput::Control(_)
                | StructuredInput::ControlResponse(_)
                | StructuredInput::ControlCancel(_)
                | StructuredInput::KeepAlive
                | StructuredInput::EnvironmentUpdate(_)
                | StructuredInput::History(_) => Err(format!(
                    "stream-json line {} is a control message, not user input",
                    index + 1
                )),
            },
        )
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| {
            parts
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
}

/// CC parses `options.jsonSchema` with a bare `jsonParse` (main.tsx:2776) —
/// a JSON syntax error throws (our `fail` here is the equivalent), but no
/// shape or schema validation happens at the CLI boundary: an invalid schema
/// only manifests as a silently absent StructuredOutput tool at creation
/// time (main.tsx:2781-2801, telemetry-only failure branch).
fn parse_schema(raw: Option<&str>) -> Result<Option<serde_json::Value>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let schema: serde_json::Value = serde_json::from_str(raw)
        .map_err(|error| format!("invalid --json-schema JSON: {error}"))?;
    Ok(Some(schema))
}

fn validate_headless_options(
    config: &CliConfig,
    output_format: &str,
    input_format: &str,
) -> Result<(), String> {
    if config.continue_session && config.resume.is_some() {
        return Err("--continue and --resume cannot be used together".to_string());
    }
    if config.fork_session && !config.continue_session && config.resume.is_none() {
        return Err("--fork-session requires --continue or --resume".to_string());
    }
    if let Some(session_id) = config.session_id.as_deref() {
        uuid::Uuid::parse_str(session_id)
            .map_err(|_| "Invalid session ID. Must be a valid UUID.".to_string())?;
        if (config.continue_session || config.resume.is_some()) && !config.fork_session {
            return Err(
                "--session-id can only be used with --continue or --resume if --fork-session is also specified."
                    .to_string(),
            );
        }
        if let Ok(cwd) = std::env::current_dir() {
            let project = cwd.to_string_lossy();
            if crate::utils::session_storage::get_session_file_path(&project, session_id).exists() {
                return Err(format!("Session ID {session_id} is already in use."));
            }
        }
    }
    if config.include_partial_messages && (!config.print || output_format != "stream-json") {
        return Err(
            "--include-partial-messages requires --print and --output-format=stream-json"
                .to_string(),
        );
    }
    if config.replay_user_messages
        && (input_format != "stream-json" || output_format != "stream-json")
    {
        return Err(
            "--replay-user-messages requires both --input-format=stream-json and --output-format=stream-json"
                .to_string(),
        );
    }
    if let Some(value) = config.max_turns.as_deref() {
        value
            .parse::<u32>()
            .ok()
            .filter(|turns| *turns > 0)
            .ok_or_else(|| "--max-turns must be a positive integer".to_string())?;
    }
    if let Some(value) = config.max_budget_usd.as_deref() {
        value
            .parse::<f64>()
            .ok()
            .filter(|amount| amount.is_finite() && *amount > 0.0)
            .ok_or_else(|| {
                "--max-budget-usd must be a positive number greater than 0".to_string()
            })?;
    }
    if let Some(value) = config.task_budget.as_deref() {
        value
            .parse::<u64>()
            .ok()
            .filter(|tokens| *tokens > 0)
            .ok_or_else(|| "--task-budget must be a positive integer".to_string())?;
    }
    if let Some(fallback) = config.fallback_model.as_deref() {
        let fallback = if fallback == "default" {
            crate::utils::model::model::get_default_main_loop_model()
        } else {
            crate::utils::model::model::parse_user_specified_model(fallback)
        };
        if fallback == crate::query_engine::resolve_model(config) {
            return Err(
                "Fallback model cannot be the same as the main model. Please specify a different model for --fallback-model."
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn effective_output_format(config: &CliConfig) -> String {
    if config.stream_json {
        "stream-json".to_string()
    } else {
        config
            .output_format
            .clone()
            .unwrap_or_else(|| "text".to_string())
    }
}

fn emit_result(
    outcome: &QueryEngineOutcome,
    duration_ms: u64,
    is_error: bool,
    missing_structured: bool,
    errors: &[String],
) {
    let subtype = if outcome.reason == "max_budget_usd" {
        "error_max_budget_usd"
    } else if outcome.reason == "max_turns" {
        "error_max_turns"
    } else if missing_structured {
        "error_max_structured_output_retries"
    } else if is_error {
        "error_during_execution"
    } else {
        "success"
    };
    let model_usage = crate::cost_tracker::get_model_usage()
        .into_iter()
        .map(|(model, usage)| {
            let value = serde_json::json!({
                "inputTokens": usage.input_tokens,
                "outputTokens": usage.output_tokens,
                "cacheReadInputTokens": usage.cache_read_input_tokens,
                "cacheCreationInputTokens": usage.cache_creation_input_tokens,
                "webSearchRequests": usage.web_search_requests,
                "costUSD": usage.cost_usd,
                "contextWindow": crate::utils::context::get_context_window_for_model(&model, &[]),
                "maxOutputTokens": crate::services::api::claude::get_max_output_tokens_for_model(&model),
            });
            (model, value)
        })
        .collect::<serde_json::Map<_, _>>();
    let mut result = serde_json::Map::from_iter([
        ("type".to_string(), serde_json::json!("result")),
        ("subtype".to_string(), serde_json::json!(subtype)),
        ("is_error".to_string(), serde_json::json!(is_error)),
        ("duration_ms".to_string(), serde_json::json!(duration_ms)),
        (
            "duration_api_ms".to_string(),
            serde_json::json!(crate::cost_tracker::get_total_api_duration()),
        ),
        ("num_turns".to_string(), serde_json::json!(outcome.turns)),
        (
            "stop_reason".to_string(),
            serde_json::json!(outcome.stop_reason),
        ),
        (
            "session_id".to_string(),
            serde_json::json!(crate::bootstrap::state::get_session_id()),
        ),
        (
            "total_cost_usd".to_string(),
            serde_json::json!(crate::cost_tracker::get_total_cost()),
        ),
        ("usage".to_string(), serde_json::json!(outcome.usage)),
        ("modelUsage".to_string(), model_usage.into()),
        (
            "permission_denials".to_string(),
            serde_json::json!(outcome.permission_denials),
        ),
        (
            "uuid".to_string(),
            serde_json::json!(uuid::Uuid::new_v4().to_string()),
        ),
    ]);
    if is_error {
        result.insert("errors".to_string(), serde_json::json!(errors));
    } else {
        result.insert("result".to_string(), serde_json::json!(outcome.text));
        if let Some(structured) = &outcome.structured {
            result.insert("structured_output".to_string(), structured.clone());
        }
    }
    json_line(result.into());
}

/// One NDJSON line to stdout. CC has no single owner for this: `cli/print.ts`
/// calls `writeToStdout(jsonStringify(…) + '\n')` directly (`:923`/`:926`) and
/// `StructuredIO.write` (`cli/structuredIO.ts:465-467`) wraps the same pair as
/// `ndjsonSafeStringify`. Both declaring files are third-party to this split
/// (`utils/process.ts`, `utils/slowOperations.ts`, `cli/ndjsonSafeStringify.ts`),
/// so it stays here and [`crate::cli::structured_io`] borrows it — including as
/// the production `SdkControlBridge::emit`.
pub(super) fn json_line(value: serde_json::Value) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(
        stdout,
        "{}",
        serde_json::to_string(&value).unwrap_or_default()
    );
    let _ = stdout.flush();
}

fn fail(message: &str) -> i32 {
    eprintln!("Error: {message}");
    1
}

#[cfg(test)]
mod mcp_server_status_tests {
    use super::*;
    use crate::services::mcp::types::{
        ConfigScope, McpClientSnapshot, McpServerConnectionType, McpServerSnapshot,
        McpToolSnapshot, ScopedMcpServerConfig, Transport,
    };

    fn config(transport: Transport) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            name: None,
            scope: ConfigScope::Project,
            transport,
            command: Some("run-docs".to_string()),
            args: vec!["--verbose".to_string()],
            env: Default::default(),
            url: Some("https://example.test/mcp".to_string()),
            headers: Default::default(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: Some("proxy-1".to_string()),
            plugin_source: None,
        }
    }

    fn server(
        name: &str,
        status: McpServerConnectionType,
        transport: Transport,
    ) -> McpServerSnapshot {
        McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name: name.to_string(),
                status,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: None,
                server_version: Some("1.2.3".to_string()),
                error: Some("boom".to_string()),
            },
            config: Some(config(transport)),
            supports_resources: false,
            tools: vec![McpToolSnapshot {
                name: "search".to_string(),
                display_name: None,
                description: None,
                input_schema: serde_json::json!({"type":"object"}),
                read_only_hint: true,
                destructive_hint: false,
                open_world_hint: false,
            }],
            prompts: Vec::new(),
            resources: Vec::new(),
        }
    }

    #[test]
    fn headless_connection_callbacks_match_official_pending_order_and_first_capabilities() {
        let mut state = crate::state::app_state_store::McpState {
            clients: vec![
                server("z-slow", McpServerConnectionType::Pending, Transport::Stdio),
                server("a-fast", McpServerConnectionType::Pending, Transport::Stdio),
            ],
            ..Default::default()
        };
        let mut fast = server(
            "a-fast",
            McpServerConnectionType::Connected,
            Transport::Stdio,
        );
        fast.tools[0].description = Some("first".into());
        apply_headless_mcp_connection_attempt(&mut state, fast.clone());
        assert_eq!(
            state.clients[0].client.status,
            McpServerConnectionType::Pending
        );
        assert_eq!(
            state.clients[1].client.status,
            McpServerConnectionType::Connected
        );
        let fast_name = state.tools[0].name.clone();
        apply_headless_mcp_connection_attempt(
            &mut state,
            server(
                "z-slow",
                McpServerConnectionType::Connected,
                Transport::Stdio,
            ),
        );
        assert_eq!(
            state
                .clients
                .iter()
                .map(|server| server.client.name.as_str())
                .collect::<Vec<_>>(),
            ["z-slow", "a-fast"]
        );
        assert_eq!(state.tools.len(), 2);
        assert_eq!(
            state.tools[0].name, fast_name,
            "tools follow completion callbacks, clients retain pending positions"
        );
        fast.tools[0].description = Some("later".into());
        apply_headless_mcp_connection_attempt(&mut state, fast);
        assert_eq!(state.tools.len(), 2);
        assert_eq!(
            state.tools[0].description, "first",
            "source uniqBy keeps the first tool with each name"
        );
    }

    #[test]
    fn headless_callback_consumes_supplied_tools_and_ignores_resource_map() {
        let mut state = crate::state::app_state_store::McpState::default();
        let mut callback = crate::services::mcp::client::McpConnectionDiscovery::from(server(
            "a",
            McpServerConnectionType::Connected,
            Transport::Stdio,
        ));
        let mut helper = crate::tools::list_mcp_resources_tool::list_mcp_resources_tool_schema();
        helper.description = "first helper".into();
        callback.tools = vec![helper.clone()];
        callback.resources = Some(vec![crate::services::mcp::types::ServerResource {
            server: "a".into(),
            uri: "a://resource".into(),
            name: "resource".into(),
            description: None,
            mime_type: None,
        }]);
        apply_headless_mcp_connection_attempt(&mut state, callback.clone());
        helper.description = "later helper".into();
        callback.tools = vec![helper];
        apply_headless_mcp_connection_attempt(&mut state, callback);
        assert_eq!(state.tools.len(), 1);
        assert_eq!(state.tools[0].description, "first helper");
        assert!(
            state.resources.is_empty(),
            "main.tsx connectMcpBatch does not consume resources"
        );
    }

    fn statuses(servers: Vec<McpServerSnapshot>) -> Vec<serde_json::Value> {
        let state = crate::state::app_state_store::McpState {
            clients: servers,
            ..Default::default()
        };
        build_mcp_server_statuses(&state, &Default::default())
    }

    /// Maps to: CC `print.ts:1689-1698` — connected servers carry `serverInfo`
    /// and `tools`, and `:1653-1663`'s `|| undefined` means a FALSE annotation
    /// is absent rather than `false`.
    #[test]
    fn connected_server_carries_server_info_and_only_true_annotations() {
        let out = statuses(vec![server(
            "docs",
            McpServerConnectionType::Connected,
            Transport::Stdio,
        )]);

        assert_eq!(out[0]["name"], "docs");
        assert_eq!(out[0]["status"], "connected");
        assert_eq!(out[0]["serverInfo"]["version"], "1.2.3");
        let annotations = &out[0]["tools"][0]["annotations"];
        assert_eq!(annotations["readOnly"], true);
        assert!(
            annotations.get("destructive").is_none(),
            "a false hint must be absent, not false (CC `|| undefined`)"
        );
        assert!(annotations.get("openWorld").is_none());
        // CC `:1693` — error is failed-only.
        assert!(out[0].get("error").is_none());
    }

    /// Maps to: CC `:1691-1693` — `serverInfo` is connected-only and `error`
    /// is failed-only; a failed server also reports no tools (`:1653`).
    #[test]
    fn failed_server_reports_error_and_no_tools_or_server_info() {
        let out = statuses(vec![server(
            "docs",
            McpServerConnectionType::Failed,
            Transport::Stdio,
        )]);

        assert_eq!(out[0]["status"], "failed");
        assert_eq!(out[0]["error"], "boom");
        assert!(out[0].get("serverInfo").is_none());
        assert!(out[0].get("tools").is_none());
    }

    /// Maps to: CC `:1628-1651` — three config shapes, and anything else
    /// leaves `config` undefined.
    #[test]
    fn config_projection_matches_the_official_three_shapes() {
        let http = statuses(vec![server(
            "a",
            McpServerConnectionType::Connected,
            Transport::Http,
        )]);
        assert_eq!(http[0]["config"]["type"], "http");
        assert_eq!(http[0]["config"]["url"], "https://example.test/mcp");
        assert!(http[0]["config"].get("command").is_none());

        let proxy = statuses(vec![server(
            "b",
            McpServerConnectionType::Connected,
            Transport::ClaudeAiProxy,
        )]);
        assert_eq!(proxy[0]["config"]["type"], "claudeai-proxy");
        assert_eq!(proxy[0]["config"]["id"], "proxy-1");

        let stdio = statuses(vec![server(
            "c",
            McpServerConnectionType::Connected,
            Transport::Stdio,
        )]);
        assert_eq!(stdio[0]["config"]["type"], "stdio");
        assert_eq!(stdio[0]["config"]["command"], "run-docs");
        assert_eq!(stdio[0]["config"]["args"][0], "--verbose");

        // ws/sdk have no branch upstream, so `config` stays absent.
        let ws = statuses(vec![server(
            "d",
            McpServerConnectionType::Connected,
            Transport::Ws,
        )]);
        assert!(ws[0].get("config").is_none());
        // `scope` comes off the config regardless of transport (CC `:1694`).
        assert_eq!(ws[0]["scope"], "project");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_stream_json(input: &str) -> Vec<String> {
        let mut reader = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut lines = Vec::new();
        while let Some(line) = next_stream_json_line(&mut reader) {
            lines.push(line.expect("fixture is valid UTF-8"));
        }
        lines
    }

    /// Maps to: CC `cli/structuredIO.ts:228-231` — `indexOf('\n')` +
    /// `slice(0, newline)`. The cut is at the newline and nowhere else, so a
    /// CRLF stream keeps its `\r`; `:248-253` then processes an unterminated
    /// tail as one more line.
    ///
    /// Old shape (`BufRead::lines()`): every `\r` here is stripped, so the
    /// CRLF and LF columns collapse into each other. Fails on the first CRLF
    /// assertion — an assertion failure, not a hang.
    #[test]
    fn stream_json_stdin_splits_on_newline_and_keeps_the_carriage_return() {
        // LF: unchanged by the port of the splitter.
        assert_eq!(
            split_stream_json("{\"a\":1}\n\n{\"b\":2}\n"),
            vec![
                "{\"a\":1}".to_string(),
                String::new(),
                "{\"b\":2}".to_string()
            ],
        );
        // CRLF: the blank line is `"\r"`, not `""`, and every other line keeps
        // its `\r` too.
        assert_eq!(
            split_stream_json("{\"a\":1}\r\n\r\n{\"b\":2}\r\n"),
            vec![
                "{\"a\":1}\r".to_string(),
                "\r".to_string(),
                "{\"b\":2}\r".to_string()
            ],
        );
        // A terminated stream has no trailing empty line: CC's `content` is
        // `""` when the loop ends, and `if (content)` is false (`:248`).
        assert_eq!(
            split_stream_json("{\"a\":1}\n"),
            vec!["{\"a\":1}".to_string()]
        );
        assert!(split_stream_json("").is_empty());
        // An unterminated tail IS processed, verbatim (`:248-253`).
        assert_eq!(
            split_stream_json("{\"a\":1}"),
            vec!["{\"a\":1}".to_string()]
        );
        assert_eq!(
            split_stream_json("{\"a\":1}\r"),
            vec!["{\"a\":1}\r".to_string()]
        );
        // A lone `\r` is not a separator on either side — CC splits on `\n`
        // only, so this is ONE line.
        assert_eq!(split_stream_json("a\rb\n"), vec!["a\rb".to_string()]);

        // The replaced shape, kept as executable evidence rather than a
        // claim: `BufRead::lines()` pops a trailing `\r` along with the `\n`,
        // collapsing the CRLF column above onto the LF one. Restoring it
        // makes this test fail outright; it does not hang.
        {
            use std::io::BufRead;
            let stripped = std::io::Cursor::new(b"{\"a\":1}\r\n\r\n{\"b\":2}\r\n".as_slice())
                .lines()
                .map(|line| line.expect("fixture is valid UTF-8"))
                .collect::<Vec<_>>();
            assert_eq!(
                stripped,
                vec![
                    "{\"a\":1}".to_string(),
                    String::new(),
                    "{\"b\":2}".to_string()
                ],
                "std strips the `\\r` CC keeps — that is the whole divergence"
            );
        }
    }

    /// The consequence of the splitter, end to end through `processLine`'s
    /// falsiness short-circuit (CC `:337-339`).
    ///
    /// Old shape: the CRLF blank line arrived as `""` and was skipped, so
    /// #190's `""`-only short-circuit was correct for LF and unreachable for
    /// CRLF. Fails on that shape at the `Fatal` assertion (not a hang).
    #[test]
    fn stream_json_crlf_blank_line_is_fatal_while_lf_blank_line_is_skipped() {
        let lf = split_stream_json("{\"type\":\"keep_alive\"}\n\n");
        assert_eq!(
            parse_stream_json_line(&lf[1], 2).unwrap(),
            StructuredInput::User(UserInput {
                text: String::new(),
                uuid: None,
                timestamp: None,
            }),
            "`!line` is true only for the empty string, which LF produces"
        );

        let crlf = split_stream_json("{\"type\":\"keep_alive\"}\r\n\r\n");
        assert_eq!(crlf[1], "\r");
        let error = parse_stream_json_line(&crlf[1], 2).unwrap_err();
        assert!(
            matches!(error, StreamInputError::Fatal(_)),
            "`\"\\r\"` is truthy, so CC runs jsonParse on it and the catch \
             exits 1 (`:457-462`); got {error:?}"
        );

        // And the reason the rest of a CRLF stream is unaffected: `\r` is JSON
        // whitespace, so the trailing byte CC's parser has always seen is one
        // `jsonParse` accepts. Pinned so the splitter fix cannot be read as
        // breaking ordinary CRLF input.
        assert_eq!(
            parse_stream_json_line(&crlf[0], 1).unwrap(),
            StructuredInput::KeepAlive
        );
        assert!(matches!(
            parse_stream_json_line(
                "{\"type\":\"control_request\",\"request_id\":\"1\",\"request\":{\"subtype\":\"interrupt\"}}\r",
                1,
            )
            .unwrap(),
            StructuredInput::Control(ControlInput { request_id, .. }) if request_id == "1"
        ));
        // Whitespace-only stays fatal on both, as it already was for LF.
        assert!(matches!(
            parse_stream_json_line("   \r", 1).unwrap_err(),
            StreamInputError::Fatal(_)
        ));
    }

    #[test]
    fn stream_json_input_extracts_sdk_text_blocks() {
        let input = r#"{"type":"user","uuid":"user-1","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"hello"}]}}"#;
        assert_eq!(parse_stream_json_input(input).unwrap(), "hello");
        assert!(matches!(
            parse_stream_json_line(input, 1).unwrap(),
            StructuredInput::User(UserInput { text, uuid: Some(uuid), timestamp: Some(_) })
                if text == "hello" && uuid == "user-1"
        ));
    }

    /// Maps to: CC `cli/structuredIO.ts:336-339` — `if (!line) return
    /// undefined`. JS falsiness admits the empty string and nothing else, so
    /// the short-circuit is exactly `""`.
    ///
    /// Old shape: `line.trim().is_empty()`, so BOTH cases below returned the
    /// same manufactured empty user turn. Fails (does not hang) on that shape:
    /// the second assertion gets an `Ok` where it expects an `Err`.
    #[test]
    fn stream_json_short_circuits_the_empty_line_only() {
        assert_eq!(
            parse_stream_json_line("", 1).unwrap(),
            StructuredInput::User(UserInput {
                text: String::new(),
                uuid: None,
                timestamp: None,
            }),
            "`!line` is true for the empty string: CC yields no message, and an \
             empty-text user turn is what both consumers here skip"
        );

        // `"   "` is truthy in JS, so CC runs `jsonParse` on it (`:341`), the
        // SyntaxError reaches the catch, and the catch exits (`:457-462`) —
        // the SDK trust boundary rejects malformed input rather than coercing
        // it (#157). Whitespace-only was the case that used to be coerced.
        let error = parse_stream_json_line("   ", 1).unwrap_err();
        assert!(
            matches!(error, StreamInputError::Fatal(_)),
            "a whitespace-only line must reach the catch, got {error:?}"
        );
        // CC `:459` frames the stderr message with the offending LINE (not its
        // index) and the thrown error; only the thrown error's own wording is
        // Rust's rather than V8's.
        let message = String::from(error);
        assert!(
            message.starts_with("Error parsing streaming input line:    : "),
            "unexpected frame: {message}"
        );

        // The same destiny for any other malformed line — the classification
        // is on `jsonParse` failing, not on the line being blank.
        assert!(matches!(
            parse_stream_json_line("{not json", 1).unwrap_err(),
            StreamInputError::Fatal(_)
        ));
        // A tab is whitespace too, and equally truthy.
        assert!(matches!(
            parse_stream_json_line("\t", 1).unwrap_err(),
            StreamInputError::Fatal(_)
        ));
    }

    /// A well-formed line that this port declines still keeps the
    /// non-terminating destiny it had before: only `jsonParse` failures are
    /// classified `Fatal`. Pins the blast radius of that classification, since
    /// `Fatal` kills the process at the reader-thread call site.
    #[test]
    fn stream_json_rejections_that_are_not_parse_failures_stay_non_fatal() {
        assert!(matches!(
            parse_stream_json_line(r#"{"type":"diagnostic"}"#, 1).unwrap_err(),
            StreamInputError::Reported(_)
        ));
        assert!(matches!(
            parse_stream_json_line(
                r#"{"type":"user","message":{"content":[{"type":"image","source":{}}]}}"#,
                1,
            )
            .unwrap_err(),
            StreamInputError::Reported(_)
        ));
    }

    #[test]
    fn stream_json_rejects_mixed_user_media_until_typed_input_is_wired() {
        let error = parse_stream_json_line(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hello"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}]}}"#,
            1,
        )
        .unwrap_err();
        assert!(String::from(error).contains("unsupported non-text"));
    }

    #[test]
    fn stream_json_parses_typed_control_requests() {
        let parsed = parse_stream_json_line(
            r#"{"type":"control_request","request_id":"1","request":{"subtype":"set_model","model":"sonnet"}}"#,
            1,
        )
        .unwrap();
        assert!(matches!(
            parsed,
            StructuredInput::Control(ControlInput { request_id, request })
                if request_id == "1" && request["subtype"] == "set_model"
        ));
    }

    #[test]
    fn stream_json_parses_tool_result_history() {
        let parsed = parse_stream_json_line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"done","is_error":false}]}}"#,
            1,
        )
        .unwrap();
        assert!(matches!(
            parsed,
            StructuredInput::History(Message::User(user))
                if matches!(
                    user.content.as_slice(),
                    [crate::types::message::UserContent::ToolResult(result)]
                        if result.tool_use_id.0 == "tool-1" && result.content == "done"
                )
        ));
    }

    #[test]
    fn stream_json_parses_assistant_history_with_identity() {
        let parsed = parse_stream_json_line(
            r#"{"type":"assistant","uuid":"assistant-1","message":{"id":"msg_1","role":"assistant","model":"claude-test","content":[{"type":"text","text":"prior"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            1,
        )
        .unwrap();
        assert!(matches!(
            parsed,
            StructuredInput::History(Message::Assistant(assistant))
                if assistant.uuid == "assistant-1"
                    && assistant.api_message_id() == Some("msg_1")
                    && assistant.content.iter().any(|content| matches!(
                        content,
                        AssistantContent::Text(text) if text == "prior"
                    ))
        ));
    }

    #[test]
    fn stream_json_parses_keepalive_and_environment_updates() {
        assert_eq!(
            parse_stream_json_line(r#"{"type":"keep_alive"}"#, 1).unwrap(),
            StructuredInput::KeepAlive
        );
        let StructuredInput::EnvironmentUpdate(variables) = parse_stream_json_line(
            r#"{"type":"update_environment_variables","variables":{"1\u0000tail":"malformed","2":"two","SECOND":"2","1":"one","FIRST":"1"}}"#,
            1,
        )
        .unwrap() else {
            panic!("expected environment update");
        };
        assert_eq!(
            variables.keys().map(String::as_str).collect::<Vec<_>>(),
            ["1\0tail", "2", "SECOND", "1", "FIRST"]
        );
    }

    #[test]
    fn stream_json_parses_control_responses_for_pending_sdk_callbacks() {
        let parsed = parse_stream_json_line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"permission-1","response":{"behavior":"deny","message":"no"}}}"#,
            1,
        )
        .unwrap();
        assert!(matches!(
            parsed,
            StructuredInput::ControlResponse(ControlResponseInput {
                request_id,
                subtype,
                response: Some(response),
                ..
            }) if request_id == "permission-1"
                && subtype == "success"
                && response["behavior"] == "deny"
        ));
    }

    /// CC parses `--json-schema` with a bare `jsonParse` (main.tsx:2776): a
    /// JSON syntax error throws, but no shape/schema validation happens at
    /// the CLI boundary — an invalid schema only means a silently absent
    /// StructuredOutput tool at creation time (main.tsx:2781-2801).
    #[test]
    fn schema_parse_only_rejects_json_syntax_errors() {
        assert!(parse_schema(Some("{not json")).is_err());
        assert!(parse_schema(Some("[]")).unwrap().is_some());
        assert!(
            parse_schema(Some(r#"{"type":"object"}"#))
                .unwrap()
                .is_some()
        );
        assert!(parse_schema(None).unwrap().is_none());
    }

    /// Maps to: CC `main.tsx:2098-2106` / `print.ts` — `--fallback-model`
    /// cannot equal the resolved main model (including after `"default"`).
    #[test]
    fn headless_fallback_model_cannot_match_main_model() {
        let config = CliConfig {
            print: true,
            model: Some("claude-sonnet-4-20250514".to_string()),
            fallback_model: Some("claude-sonnet-4-20250514".to_string()),
            ..CliConfig::default()
        };
        assert!(
            validate_headless_options(&config, "json", "text")
                .unwrap_err()
                .contains("same as the main model")
        );

        let config = CliConfig {
            print: true,
            model: Some("claude-sonnet-4-20250514".to_string()),
            fallback_model: Some("claude-opus-4-20250514".to_string()),
            ..CliConfig::default()
        };
        assert!(validate_headless_options(&config, "json", "text").is_ok());
    }

    #[test]
    fn headless_limit_validation_rejects_non_positive_values() {
        let config = CliConfig {
            print: true,
            max_turns: Some("0".to_string()),
            ..CliConfig::default()
        };
        assert!(
            validate_headless_options(&config, "json", "text")
                .unwrap_err()
                .contains("positive integer")
        );

        let config = CliConfig {
            print: true,
            max_budget_usd: Some("NaN".to_string()),
            ..CliConfig::default()
        };
        assert!(
            validate_headless_options(&config, "json", "text")
                .unwrap_err()
                .contains("positive number")
        );
    }

    #[test]
    fn sdk_model_info_response_is_non_empty() {
        let models = sdk_model_infos();
        assert!(models.iter().any(|model| model["value"] == "default"));
        assert!(models.iter().any(|model| model["value"] == "sonnet"));
    }

    #[test]
    fn seed_read_state_requires_client_mtime_to_be_fresh() {
        let root = std::env::temp_dir().join(format!("cometix-seed-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("seed.txt");
        std::fs::write(&path, "one\r\ntwo").unwrap();
        let mtime = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let seed = read_state_seed_from_control(&serde_json::json!({
            "path": path,
            "mtime": mtime,
        }))
        .unwrap();
        assert_eq!(seed.content.as_deref(), Some("one\ntwo"));
        assert!(
            read_state_seed_from_control(&serde_json::json!({
                "path": seed.path,
                "mtime": 0,
            }))
            .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod sdk_mapper_tests {
    use super::*;

    #[test]
    fn stream_compact_history_matches_official_sdk_mapper_injection() {
        let parsed = parse_stream_json_line(
            r#"{"type":"system","subtype":"compact_boundary","uuid":"compact-uuid","compact_metadata":{"trigger":"manual","pre_tokens":42,"preserved_segment":{"head_uuid":"h","anchor_uuid":"a","tail_uuid":"t"}}}"#,
            1,
        ).unwrap();
        let StructuredInput::History(Message::System(message)) = parsed else {
            panic!("SDK compact boundaries must enter headless history");
        };
        let metadata = message.compact_metadata().unwrap();
        assert_eq!(metadata.trigger.as_deref(), Some("manual"));
        assert_eq!(metadata.pre_tokens, Some(42));
        assert_eq!(
            metadata.preserved_segment.as_ref().unwrap()["anchorUuid"],
            "a"
        );
        assert_eq!(Message::System(message).uuid(), "compact-uuid");
        assert!(matches!(
            parse_stream_json_line(r#"{"type":"system","subtype":"init"}"#, 1).unwrap(),
            StructuredInput::KeepAlive
        ));
    }

    #[test]
    fn assistant_history_matches_official_fresh_timestamp_and_absent_request_id() {
        let input = serde_json::json!({
            "type":"assistant","uuid":"assistant-replay","timestamp":"2000-01-01T00:00:00Z",
            "request_id":"do-not-restore","error":"do-not-restore",
            "message":{"id":"api-id","model":"test","content":[{"type":"text","text":"prior"}]},
        });
        let Message::Assistant(message) = parse_stream_assistant_history(&input, 1).unwrap() else {
            panic!("must restore an assistant message");
        };
        assert_eq!(message.uuid, "assistant-replay");
        assert_eq!(message.api_message_id(), Some("api-id"));
        assert!(message.identity().unwrap().request_id.is_none());
        assert_ne!(message.timestamp.to_rfc3339(), "2000-01-01T00:00:00+00:00");
    }

    #[test]
    fn sdk_rate_limit_listener_matches_official_status_recovery_dedup_and_cleanup() {
        use crate::services::claude_ai_limits as limits;
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        limits::reset_for_test();
        let values = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = values.clone();
        let listener = SdkRateLimitListener::new(std::sync::Arc::new(move |value| {
            captured.lock().unwrap().push(value);
        }));
        limits::extract_quota_status_from_error_with(Some(429), None, true, 1.0);
        limits::extract_quota_status_from_error_with(Some(429), None, true, 1.0);
        limits::extract_quota_status_from_headers_with(
            &std::collections::HashMap::new(),
            false,
            1.0,
        );
        let snapshot = values.lock().unwrap().clone();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0]["type"], "rate_limit_event");
        assert_eq!(snapshot[0]["rate_limit_info"]["status"], "rejected");
        assert_eq!(snapshot[1]["rate_limit_info"]["status"], "allowed");
        assert!(
            snapshot[0]["rate_limit_info"]
                .get("unifiedRateLimitFallbackAvailable")
                .is_none()
        );
        assert_ne!(snapshot[0]["uuid"], snapshot[1]["uuid"]);
        drop(listener);
        limits::extract_quota_status_from_error_with(Some(429), None, true, 1.0);
        assert_eq!(values.lock().unwrap().len(), 2);
        limits::reset_for_test();
    }
    #[test]
    fn sdk_initialize_cold_plugins_keeps_published_current_thread_executor_live() {
        const CHILD: &str = "COMETIX_SDK_CURRENT_THREAD_PLUGIN_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            // Isolated test process: do not use initialize_test_process_runtime,
            // whose multithread executor would conceal the production deadlock.
            assert!(crate::utils::process_runtime::process_runtime_handle().is_none());
            let root = std::path::PathBuf::from(root);
            crate::utils::process_env::set("CLAUDE_CONFIG_DIR", root.join("config"));
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
            let response = runtime.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    initialize_control_response(),
                )
                .await
                .expect("SDK initialize kept its event loop running")
                .expect("response worker")
            });
            assert!(
                response["commands"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|command| command["name"] == "sdk-fixture:probe"),
                "{response}"
            );
            assert!(
                response["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|agent| agent["name"] == "sdk-fixture:reviewer"),
                "{response}"
            );
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "sdk-current-thread-plugin-{}",
            uuid::Uuid::new_v4()
        ));
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
            r#"{"name":"sdk-fixture"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("plugin/commands/probe.md"),
            "---\ndescription: Local SDK command fixture\n---\nNever execute",
        )
        .unwrap();
        std::fs::write(
            root.join("plugin/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Local SDK agent fixture\n---\nNever execute",
        )
        .unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::print::sdk_mapper_tests::sdk_initialize_cold_plugins_keeps_published_current_thread_executor_live", "--nocapture"])
            .env(CHILD, &root)
            .current_dir(&root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        // An in-runtime timeout cannot interrupt a synchronously blocked loop.
        // The external watchdog also bounds runtime shutdown if a worker stalls.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "SDK initialize deadlocked: {}",
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
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child must run the exact inline regression"
        );
    }
}
