//! Maps to: CC `utils/hooks/execAgentHook.ts`.
//! The query actor is the established PORTING.md A2 async-generator carrier.

use super::hook_helpers::{HookResponse, add_arguments_to_prompt, create_structured_output_tool};
use super::session_hooks::clear_session_hooks;
use crate::query::deps::production_deps;
use crate::query::{QueryEvent, QueryParams};
use crate::schemas::hooks::AgentHook;
use crate::services::hooks::{HookBlockingError, HookEvent, HookOutcome, HookResult};
use crate::tool::{AbortController, GetAppStateCallback, SetInProgressToolUseIds, ToolUseContext};
use crate::types::message::{Attachment, AttachmentMessage, Message};
use crate::types::permissions::{PermissionMode, PermissionRuleSource};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maps to: CC `execAgentHook` (:39-339). `_messages` deliberately does not
/// seed the child conversation; the transcript path is supplied instead.
#[allow(clippy::too_many_arguments)]
pub async fn exec_agent_hook(
    hook: &AgentHook,
    hook_name: &str,
    hook_event: HookEvent,
    json_input: &str,
    signal: &AbortController,
    tool_use_context: &ToolUseContext,
    tool_use_id: Option<&str>,
    messages: &[Message],
    agent_name: Option<&str>,
) -> HookResult<AgentHook> {
    exec_agent_hook_with_query(
        hook,
        hook_name,
        hook_event,
        json_input,
        signal,
        tool_use_context,
        tool_use_id,
        messages,
        agent_name,
        |params| crate::query::spawn_query_generator(params, production_deps()),
    )
    .await
}

/// Rust-only I/O seam for the imported `query` generator. Production and
/// actor regressions use the same query loop; scripted yields test exception
/// and boundary paths without model/network nondeterminism.
#[allow(clippy::too_many_arguments)]
async fn exec_agent_hook_with_query<F>(
    hook: &AgentHook,
    hook_name: &str,
    hook_event: HookEvent,
    json_input: &str,
    signal: &AbortController,
    parent: &ToolUseContext,
    tool_use_id: Option<&str>,
    _messages: &[Message],
    agent_name: Option<&str>,
    query: F,
) -> HookResult<AgentHook>
where
    F: FnOnce(QueryParams) -> crate::query::QueryHandle,
{
    let effective_id = tool_use_id
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("hook-{}", uuid::Uuid::new_v4()));
    let transcript_path = match parent.agent_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => crate::utils::session_storage::get_agent_transcript_path(id),
        None => crate::utils::session_storage::get_transcript_path(None),
    };
    let start = Instant::now();
    let processed_prompt = add_arguments_to_prompt(&hook.prompt, json_input);
    crate::utils::debug::log_for_debugging(&format!(
        "Hooks: Processing agent hook with prompt: {processed_prompt}"
    ));
    let user_message = Message::User(crate::utils::messages::create_user_message(
        processed_prompt.clone(),
    ));
    crate::utils::debug::log_for_debugging("Hooks: Starting agent query with 1 messages");
    let timeout_ms = hook
        .timeout
        .filter(|t| *t != 0.0 && !t.is_nan())
        .map(|t| t * 1000.0)
        .unwrap_or(60_000.0);
    // Node setTimeout truncates fractions and clamps invalid/out-of-range delays to 1ms.
    let timeout = Duration::from_millis(if (1.0..=2_147_483_647.0).contains(&timeout_ms) {
        timeout_ms as u64
    } else {
        1
    });
    // CC registers after createCombinedAbortSignal's already-aborted return:
    // no event is replayed and that path has no timer (source quirk).
    let listen_for_parent = !signal.is_aborted();
    let timeout_at = tokio::time::Instant::now() + timeout;
    let abort = AbortController::default();
    let mut parent_signal = signal.signal();
    let hook_agent_id = format!("hook-agent-{}", uuid::Uuid::new_v4());
    let mut context = parent.clone();
    context.agent_id = Some(hook_agent_id.clone());
    context.abort_controller = abort.clone();
    context.tools.retain(|tool| {
        !crate::types::tools::tool_matches_name(
            tool,
            crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME,
        ) && !crate::constants::tools::ALL_AGENT_DISALLOWED_TOOLS.contains(tool.name.as_str())
    });
    context.tools.push(create_structured_output_tool());
    context.main_loop_model = Some(
        hook.model
            .clone()
            .unwrap_or_else(crate::utils::model::model::get_small_fast_model),
    );
    context.is_non_interactive_session = true;
    context.thinking_config = Some(crate::utils::thinking::ThinkingConfig::Disabled);
    context.set_in_progress_tool_use_ids = SetInProgressToolUseIds::default();
    context.set_has_interruptible_tool_in_progress =
        crate::tool::SetHasInterruptibleToolInProgress::default();
    context.interruptible_tool_use_ids.clear();
    context.can_use_tool =
        crate::utils::permissions::permissions::has_permissions_to_use_tool_callback();
    let parent_state = parent.clone();
    let read_rule =
        crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(
            &format!("Read(/{})", transcript_path.display()),
        );
    context = context.with_get_app_state_override(GetAppStateCallback::new(move || {
        let mut state = parent_state
            .get_app_state()
            .map(|s| (*s).clone())
            .unwrap_or_else(|| {
                let mut state = crate::state::app_state_store::AppState::default();
                state.tool_permission_context =
                    Arc::new(parent_state.tool_permission_context.clone());
                state
            });
        let mut permissions = (*state.tool_permission_context).clone();
        permissions.mode = PermissionMode::DontAsk;
        permissions
            .always_allow_rules
            .entry(PermissionRuleSource::Session)
            .or_default()
            .push(read_rule.clone());
        state.tool_permission_context = Arc::new(permissions);
        Some(Arc::new(state))
    }));
    let system_prompt = vec![format!(
        "You are verifying a stop condition in Claude Code. Your task is to verify that the agent completed the given plan. The conversation transcript is available at: {}\nYou can read this file to analyze the conversation history if needed.\n\nUse the available tools to inspect the codebase and verify the condition.\nUse as few steps as possible - be efficient and direct.\n\nWhen done, return your result using the StructuredOutput tool with:\n- ok: true if the condition is met\n- ok: false with reason if the condition is not met",
        transcript_path.display()
    )];
    super::hook_helpers::register_structured_output_enforcement(&hook_agent_id);
    let query_params = QueryParams {
        turn_id: uuid::Uuid::new_v4().to_string(),
        input: processed_prompt,
        messages: Vec::new(),
        model_messages: vec![user_message],
        system_prompt,
        user_context: Default::default(),
        system_context: Default::default(),
        query_source: crate::constants::query_source::QuerySource::HookAgent,
        token_budget: None,
        task_budget: None,
        max_turns: None,
        tool_use_context: context,
    };
    let mut turn_count = 0;
    let mut hit_max_turns = false;
    let mut structured_output: Option<HookResponse> = None;
    let query_result: anyhow::Result<()> = async {
        let handle = query(query_params);
        let _generator_guard = handle.resume.clone().map(crate::query::QueryGeneratorGuard);
        let deadline = tokio::time::sleep_until(timeout_at);
        tokio::pin!(deadline);
        let mut listening = listen_for_parent;
        let result = loop {
            let event = tokio::select! {
                event = handle.events.recv() => match event {
                    Ok(event) => event,
                    Err(error) => break Err(anyhow::anyhow!(error)),
                },
                _ = parent_signal.aborted(), if listening => { abort.abort(); listening = false; continue; },
                _ = &mut deadline, if listening => { abort.abort(); listening = false; continue; },
            };
            match event {
                QueryEvent::StreamRequestStart => parent.stream_mode_sink.set("requesting"),
                QueryEvent::Stream(crate::types::message::StreamEvent::ApiEvent {event, ..}) => {
                    crate::utils::messages::handle_message_from_stream(&event, &parent.response_length_sink, &parent.stream_mode_sink);
                }
                QueryEvent::Message(Message::Assistant(_)) => {
                    turn_count += 1;
                    if turn_count >= 50 { hit_max_turns = true; crate::utils::debug::log_for_debugging(&format!("Hooks: Agent turn {turn_count} hit max turns, aborting")); abort.abort(); break Ok(()); }
                }
                QueryEvent::Message(Message::Attachment(AttachmentMessage {attachment: Attachment::StructuredOutput {data}, ..}))
                | QueryEvent::ModelMessage(Message::Attachment(AttachmentMessage {attachment: Attachment::StructuredOutput {data}, ..})) => {
                    if let Ok(data) = crate::utils::zod::safe_parse(super::hook_helpers::hook_response_schema(), &data) {
                        crate::utils::debug::log_for_debugging(&format!("Hooks: Got structured output: {data}"));
                        structured_output = serde_json::from_value(data).ok();
                        abort.abort(); break Ok(());
                    }
                }
                QueryEvent::Terminal(terminal) => break match terminal.exception {
                    Some(error) => Err(anyhow::anyhow!(error)), None => Ok(()),
                },
                _ => {},
            }
            // AsyncGenerator.next(): only now may the actor continue past yield.
            if let Some(resume) = &handle.resume { resume.advance(); }
        };
        if let Some(resume) = &handle.resume { resume.close(); }
        handle.events.close();
        result
    }.await;
    // Dropping the select's local futures is cleanupCombinedSignal; no detached timer/listener.
    if let Err(error) = query_result {
        if abort.is_aborted() {
            return HookResult {
                hook: Some(hook.clone()),
                outcome: HookOutcome::Cancelled,
                ..Default::default()
            };
        }
        let error_message = error.to_string();
        crate::utils::debug::log_for_debugging(&format!(
            "Hooks: Agent hook error: {error_message}"
        ));
        crate::services::analytics::log_event(
            "tengu_agent_stop_hook_error",
            serde_json::json!({
                "durationMs": start.elapsed().as_millis() as u64, "errorType": 2, "agentName": agent_name,
            }),
        );
        return HookResult {
            hook: Some(hook.clone()),
            outcome: HookOutcome::NonBlockingError,
            message: Some(AttachmentMessage::new(Attachment::HookNonBlockingError {
                hook_name: hook_name.into(),
                tool_use_id: effective_id,
                hook_event: hook_event.as_str().into(),
                stderr: format!("Error executing agent hook: {error_message}"),
                stdout: String::new(),
                exit_code: 1,
                command: None,
                duration_ms: None,
            })),
            ..Default::default()
        };
    }
    // Deliberately not a finally guard: the source catch leaves session hooks registered.
    clear_session_hooks(&hook_agent_id);
    let mut result = HookResult {
        hook: Some(hook.clone()),
        ..Default::default()
    };
    let metadata = serde_json::json!({"durationMs": start.elapsed().as_millis() as u64, "turnCount": turn_count, "agentName": agent_name});
    match structured_output {
        None => {
            crate::utils::debug::log_for_debugging(if hit_max_turns {
                "Hooks: Agent hook did not complete within 50 turns"
            } else {
                "Hooks: Agent hook did not return structured output"
            });
            let event = if hit_max_turns {
                "tengu_agent_stop_hook_max_turns"
            } else {
                "tengu_agent_stop_hook_error"
            };
            let mut metadata = metadata;
            if !hit_max_turns {
                metadata["errorType"] = 1.into();
            }
            crate::services::analytics::log_event(event, metadata);
            result.outcome = HookOutcome::Cancelled;
        }
        Some(response) if !response.ok => {
            crate::utils::debug::log_for_debugging(&format!(
                "Hooks: Agent hook condition was not met: {}",
                response.reason.as_deref().unwrap_or("undefined")
            ));
            result.outcome = HookOutcome::Blocking;
            result.blocking_error = Some(HookBlockingError {
                blocking_error: format!(
                    "Agent hook condition was not met: {}",
                    response.reason.as_deref().unwrap_or("undefined")
                ),
                command: hook.prompt.clone(),
            });
        }
        Some(_) => {
            crate::utils::debug::log_for_debugging("Hooks: Agent hook condition was met");
            crate::services::analytics::log_event("tengu_agent_stop_hook_success", metadata);
            result.message = Some(AttachmentMessage::new(Attachment::HookSuccess {
                content: String::new(),
                hook_name: hook_name.into(),
                tool_use_id: effective_id,
                hook_event: hook_event.as_str().into(),
                stdout: None,
                stderr: None,
                exit_code: None,
                command: None,
                duration_ms: None,
            }));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{QueryHandle, transitions::Terminal};
    use crate::types::message::{AssistantContent, AssistantMessage, StopReason, ToolUseBlock};
    use serde_json::json;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use crate::types::message::UserContent;

    fn hook() -> AgentHook {
        AgentHook {
            prompt: "Verify $ARGUMENTS".into(),
            timeout: Some(1.0),
            model: None,
            condition: None,
            status_message: None,
            once: None,
        }
    }
    fn assistant() -> AssistantMessage {
        AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("checking".into())],
            model: None,
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        }
    }
    fn output(data: serde_json::Value) -> QueryEvent {
        QueryEvent::Message(Message::Attachment(AttachmentMessage::new(
            Attachment::StructuredOutput { data },
        )))
    }
    fn scripted(params: QueryParams, events: Vec<QueryEvent>) -> QueryHandle {
        let (tx, rx) = async_channel::unbounded();
        let (commands, _) = async_channel::unbounded();
        for event in events {
            tx.try_send(event).unwrap();
        }
        tx.try_send(QueryEvent::Terminal(Terminal::new("completed")))
            .unwrap();
        QueryHandle {
            id: params.turn_id,
            events: Arc::new(rx),
            commands: Arc::new(commands),
            abort_controller: params.tool_use_context.abort_controller,
            resume: None,
        }
    }
    async fn run(events: Vec<QueryEvent>) -> HookResult<AgentHook> {
        exec_agent_hook_with_query(
            &hook(),
            "Stop",
            HookEvent::Stop,
            "{}",
            &AbortController::default(),
            &ToolUseContext::default(),
            Some("tool-1"),
            &[],
            None,
            |params| scripted(params, events),
        )
        .await
    }

    #[tokio::test]
    async fn outcomes_and_attachments_matches_official_branches() {
        // CC execAgentHook.ts:264-298; hookResponseSchema strips unknown keys.
        let result = run(vec![output(json!({"ok":true,"extra":1}))]).await;
        assert_eq!(result.outcome, HookOutcome::Success);
        assert_eq!(result.hook.unwrap().prompt, hook().prompt);
        assert!(
            matches!(result.message.unwrap().attachment, Attachment::HookSuccess {
            hook_name, tool_use_id, hook_event, content, command: None, duration_ms: None, ..
        } if hook_name == "Stop" && tool_use_id == "tool-1" && hook_event == "Stop" && content.is_empty())
        );
        for (data, reason) in [
            (json!({"ok":false}), "undefined"),
            (json!({"ok":false,"reason":""}), ""),
            (json!({"ok":false,"reason":"tests failed"}), "tests failed"),
        ] {
            let result = run(vec![output(data)]).await;
            assert_eq!(result.outcome, HookOutcome::Blocking);
            let error = result.blocking_error.unwrap();
            assert_eq!(
                error.blocking_error,
                format!("Agent hook condition was not met: {reason}")
            );
            assert_eq!(error.command, hook().prompt);
            assert!(result.message.is_none());
        }
    }

    #[tokio::test]
    async fn validation_and_assistant_limit_matches_official_yields() {
        // CC :192-228: only real assistant yields count; max check precedes output.
        let mut events = vec![
            output(json!({"ok":true,"reason":null})),
            output(json!({"ok":"true"})),
        ];
        events.extend((0..49).map(|_| QueryEvent::Message(Message::Assistant(assistant()))));
        events.push(QueryEvent::ModelMessage(Message::Assistant(assistant())));
        events.push(output(json!({"ok":true})));
        assert_eq!(run(events).await.outcome, HookOutcome::Success);
        let mut events: Vec<_> = (0..50)
            .map(|_| QueryEvent::Message(Message::Assistant(assistant())))
            .collect();
        events.push(output(json!({"ok":true})));
        let result = run(events).await;
        assert_eq!(result.outcome, HookOutcome::Cancelled);
        assert!(result.message.is_none());
        assert_eq!(
            run(vec![output(json!({"ok":true,"reason":null}))])
                .await
                .outcome,
            HookOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn context_and_live_permission_overlay_matches_official_state() {
        // CC :54-57,63-70,93-156,181-189. No parent conversation is copied.
        let store = crate::state::store::AppStore::new(Default::default(), None);
        let mut parent = ToolUseContext::default().with_app_store(store.clone());
        parent.agent_id = Some("parent-agent".into());
        let mut alias = crate::types::tools::Tool {
            name: "other".into(),
            aliases: vec!["StructuredOutput".into()],
            ..Default::default()
        };
        parent.tools = vec![alias.clone()];
        alias.name = "Read".into();
        alias.aliases.clear();
        parent.tools.push(alias);
        let mut definition = hook();
        definition.model = Some(String::new());
        let seen_id = Arc::new(Mutex::new(String::new()));
        let seen = seen_id.clone();
        let result = exec_agent_hook_with_query(&definition, "Stop", HookEvent::Stop, "{\"x\":1}",
            &AbortController::default(), &parent, Some(""), &[Message::Assistant(assistant())], None,
            |params| {
                assert_eq!(params.model_messages.len(), 1);
                assert!(matches!(&params.model_messages[0], Message::User(m) if m.content == vec![UserContent::Text("Verify {\"x\":1}".into())]));
                assert_eq!(params.query_source, crate::constants::query_source::QuerySource::HookAgent);
                assert!(params.max_turns.is_none());
                assert!(params.user_context.is_empty() && params.system_context.is_empty());
                let context = &params.tool_use_context;
                assert_eq!(context.main_loop_model.as_deref(), Some(""));
                assert_eq!(context.thinking_config, Some(crate::utils::thinking::ThinkingConfig::Disabled));
                assert!(context.is_non_interactive_session);
                assert!(context.set_in_progress_tool_use_ids.0.is_none());
                assert_eq!(context.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Read", "StructuredOutput"]);
                let id = context.agent_id.as_ref().unwrap();
                assert!(id.starts_with("hook-agent-"));
                *seen.lock().unwrap() = id.clone();
                assert!(!super::super::session_hooks::get_session_function_hooks(id, None).is_empty());
                let state = context.get_app_state().unwrap();
                assert_eq!(state.tool_permission_context.mode, PermissionMode::DontAsk);
                let path = crate::utils::session_storage::get_agent_transcript_path("parent-agent");
                assert!(params.system_prompt[0].contains(&path.display().to_string()));
                assert_eq!(state.tool_permission_context.always_allow_rules[&PermissionRuleSource::Session],
                    vec![crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(&format!("Read(/{})", path.display()))]);
                parent.set_app_state(|state| state.verbose = true);
                assert!(context.get_app_state().unwrap().verbose);
                assert_ne!(parent.get_app_state().unwrap().tool_permission_context.mode, PermissionMode::DontAsk);
                scripted(params, vec![output(json!({"ok":true}))])
            }).await;
        assert!(
            matches!(result.message.unwrap().attachment, Attachment::HookSuccess {tool_use_id,..} if tool_use_id.starts_with("hook-"))
        );
        assert!(
            super::super::session_hooks::get_session_function_hooks(&seen_id.lock().unwrap(), None)
                .is_empty()
        );
        assert!(!parent.abort_controller.is_aborted());
    }

    #[tokio::test]
    async fn exception_cleanup_matches_official_catch() {
        // CC :302-337: general exception is non-blocking and does NOT clear session hooks.
        let seen_id = Arc::new(Mutex::new(String::new()));
        let result = exec_agent_hook_with_query(
            &hook(),
            "Stop",
            HookEvent::Stop,
            "{}",
            &AbortController::default(),
            &ToolUseContext::default(),
            Some("t"),
            &[],
            None,
            |params| {
                *seen_id.lock().unwrap() = params.tool_use_context.agent_id.clone().unwrap();
                let mut terminal = Terminal::new("model_error");
                terminal.exception = Some("query failure".into());
                scripted(params, vec![QueryEvent::Terminal(terminal)])
            },
        )
        .await;
        assert_eq!(result.outcome, HookOutcome::NonBlockingError);
        assert!(
            matches!(result.message.unwrap().attachment, Attachment::HookNonBlockingError {stderr, stdout, exit_code, ..}
            if stderr == "Error executing agent hook: query failure" && stdout.is_empty() && exit_code == 1)
        );
        let id = seen_id.lock().unwrap().clone();
        assert!(!super::super::session_hooks::get_session_function_hooks(&id, None).is_empty());
        clear_session_hooks(&id);
        // A model error which query itself caught is normal exhaustion, not a throw.
        assert_eq!(
            run(vec![QueryEvent::Terminal(Terminal::new("model_error"))])
                .await
                .outcome,
            HookOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn fractional_timeout_and_preaborted_signal_matches_official_order() {
        // CC :75-85 and combinedAbortSignal.ts:22-24: fractional seconds;
        // an already-aborted combined signal is not replayed to the late listener.
        let mut definition = hook();
        definition.timeout = Some(0.005);
        let start = Instant::now();
        let result = exec_agent_hook_with_query(
            &definition,
            "Stop",
            HookEvent::Stop,
            "{}",
            &AbortController::default(),
            &ToolUseContext::default(),
            None,
            &[],
            None,
            |params| {
                let (tx, rx) = async_channel::unbounded();
                let (commands, _) = async_channel::unbounded();
                let abort = params.tool_use_context.abort_controller;
                let mut signal = abort.signal();
                tokio::spawn(async move {
                    signal.aborted().await;
                    tx.send(QueryEvent::Terminal(Terminal::new("aborted_streaming")))
                        .await
                        .unwrap();
                });
                QueryHandle {
                    id: params.turn_id,
                    events: Arc::new(rx),
                    commands: Arc::new(commands),
                    abort_controller: abort,
                    resume: None,
                }
            },
        )
        .await;
        assert_eq!(result.outcome, HookOutcome::Cancelled);
        assert!(start.elapsed() < Duration::from_millis(500));
        let signal = AbortController::default();
        signal.abort();
        let result = exec_agent_hook_with_query(
            &definition,
            "Stop",
            HookEvent::Stop,
            "{}",
            &signal,
            &ToolUseContext::default(),
            None,
            &[],
            None,
            |params| {
                let (tx, rx) = async_channel::unbounded();
                let (commands, _) = async_channel::unbounded();
                let abort = params.tool_use_context.abort_controller;
                let probe = abort.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    assert!(!probe.is_aborted());
                    tx.send(output(json!({"ok":true}))).await.unwrap();
                });
                QueryHandle {
                    id: params.turn_id,
                    events: Arc::new(rx),
                    commands: Arc::new(commands),
                    abort_controller: abort,
                    resume: None,
                }
            },
        )
        .await;
        assert_eq!(result.outcome, HookOutcome::Success);
    }

    #[tokio::test]
    async fn stream_callbacks_matches_official_utf16_and_modes() {
        let lengths = Arc::new(AtomicUsize::new(0));
        let modes = Arc::new(Mutex::new(Vec::new()));
        let mut parent = ToolUseContext::default();
        let captured = lengths.clone();
        parent.response_length_sink = crate::tool::ResponseLengthSink::new(move |n| {
            captured.fetch_add(n, Ordering::SeqCst);
        });
        let captured = modes.clone();
        parent.stream_mode_sink = crate::tool::StreamModeSink(Some(Arc::new(move |mode| {
            captured.lock().unwrap().push(mode.to_owned())
        })));
        let events = vec![
            json!({"type":"message_start"}),
            json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"😀"}}),
            json!({"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{}"}}),
            json!({"type":"content_block_delta","delta":{"type":"signature_delta","signature":"ignored"}}),
            json!({"type":"message_stop"}),
        ];
        exec_agent_hook_with_query(
            &hook(),
            "Stop",
            HookEvent::Stop,
            "{}",
            &AbortController::default(),
            &parent,
            None,
            &[],
            None,
            |params| {
                scripted(
                    params,
                    std::iter::once(QueryEvent::StreamRequestStart)
                        .chain(events.into_iter().map(|event| {
                            QueryEvent::Stream(crate::types::message::StreamEvent::ApiEvent {
                                event,
                                ttft_ms: None,
                            })
                        }))
                        .collect(),
                )
            },
        )
        .await;
        assert_eq!(lengths.load(Ordering::SeqCst), 4);
        assert_eq!(
            *modes.lock().unwrap(),
            ["requesting", "responding", "tool-use"]
        );
    }

    #[derive(Clone)]
    struct ModelDeps(Arc<AtomicUsize>, bool);
    impl crate::query::deps::QueryDeps for ModelDeps {
        fn call_model(
            &self,
            request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            let calls = self.0.clone();
            let fail = self.1;
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if fail {
                    anyhow::bail!("model open failure");
                }
                assert_eq!(
                    request.query_source,
                    crate::constants::query_source::QuerySource::HookAgent
                );
                assert_eq!(request.permission_context.mode, PermissionMode::DontAsk);
                let (tx, rx) = tokio::sync::mpsc::channel(2);
                let mut message = assistant();
                message.stop_reason = Some(StopReason::ToolUse);
                message.content = vec![AssistantContent::ToolUse(ToolUseBlock {
                    id: crate::types::ids::ToolUseId("structured".into()),
                    name: "StructuredOutput".into(),
                    input: json!({"ok":true,"extra":"stripped by Zod"}),
                })];
                tx.send(crate::services::api::claude::QueryModelStreamItem::Assistant(message))
                    .await
                    .unwrap();
                Ok(rx)
            })
        }
    }
    #[tokio::test]
    async fn real_query_actor_and_structured_tool_matches_official_no_extra_turn() {
        for fail in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let deps = ModelDeps(calls.clone(), fail);
            let result = exec_agent_hook_with_query(
                &hook(),
                "Stop",
                HookEvent::Stop,
                "{}",
                &AbortController::default(),
                &ToolUseContext::default(),
                None,
                &[],
                None,
                |params| crate::query::spawn_query_generator(params, deps),
            )
            .await;
            // CC query.ts:955-1002 catches model failures; execAgentHook sees
            // normal exhaustion, hence Cancelled rather than NonBlockingError.
            assert_eq!(
                result.outcome,
                if fail {
                    HookOutcome::Cancelled
                } else {
                    HookOutcome::Success
                },
                "{result:?}"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "no model request after StructuredOutput yield"
            );
        }
    }
    /// Opt-in live-provider test. Run through Kitty with the user's real
    /// config/workdir; --settings is parsed by the production startup owner.
    #[tokio::test]
    #[ignore = "requires authorized real provider settings and Kitty"]
    async fn real_provider_matches_official_agent_hook_outcomes() {
        assert_eq!(
            crate::utils::process_env::var("COMETIX_RUN_REAL_AGENT_HOOK").as_deref(),
            Some("1")
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workdir = root.join(".test/kitty-workdir");
        assert_eq!(
            crate::utils::process_env::var("CLAUDE_CONFIG_DIR").unwrap(),
            root.join(".test/kitty-config").display().to_string()
        );
        std::env::set_current_dir(&workdir).unwrap();
        crate::bootstrap::state::set_original_cwd(&workdir);
        let settings = crate::utils::node_os::homedir().join(".claude/settings.grok.json");
        crate::main::apply_live_startup_flags(&[
            "cometix-code".into(),
            "--settings".into(),
            settings.display().to_string(),
        ])
        .unwrap();
        crate::utils::managed_env::apply_safe_config_environment_variables();
        #[cfg(feature = "mcp_runtime")]
        crate::utils::tls_provider::install_crypto_provider();
        let parent = ToolUseContext::default();
        for ok in [true, false] {
            let mut definition = hook();
            definition.timeout = Some(60.0);
            definition.model = Some(crate::utils::model::model::get_default_opus_model());
            definition.prompt = format!(
                "This is a deterministic hook integration test. Immediately call StructuredOutput exactly once with {{\"ok\":{ok},\"reason\":\"agent-hook-live-check\"}}. Do not inspect files or produce a prose answer."
            );
            let result = exec_agent_hook(
                &definition,
                "Stop",
                HookEvent::Stop,
                "{}",
                &AbortController::default(),
                &parent,
                None,
                &[],
                None,
            )
            .await;
            assert_eq!(
                result.outcome,
                if ok {
                    HookOutcome::Success
                } else {
                    HookOutcome::Blocking
                },
                "{result:?}"
            );
            if !ok {
                assert_eq!(
                    result.blocking_error.unwrap().blocking_error,
                    "Agent hook condition was not met: agent-hook-live-check"
                );
            }
            println!("AGENT_HOOK_LIVE_OK ok={ok}");
        }
    }
}
