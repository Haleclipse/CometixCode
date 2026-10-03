//! Stop hook query seam.
//! Maps to official `query/stopHooks.ts`.
//!
//! The upstream function performs several side-effecting actions at turn end:
//! stop hooks, task/teammate hooks, prompt suggestions, memory extraction,
//! auto-dream, and cache-safe parameter writes. Cometix executes the official
//! Stop hook seam when configured, while keeping unrelated side-effecting
//! background systems as explicit TODO seams.

use crate::constants::query_source::QuerySource;
use crate::services::hooks::RegisteredHooks;
use crate::tool::ToolUseContext;
use crate::types::message::{AssistantContent, AssistantMessage, Message};
use crate::types::message::{
    RenderableMessage, RenderableMessageKind, StopHookInfo, SystemMessage,
};
use crate::utils::forked_agent::{
    CacheSafeParams, CacheSafeParamsContext, create_cache_safe_params, save_cache_safe_params_arc,
};

#[cfg(test)]
static TEST_STOP_HOOK_RESULTS: std::sync::LazyLock<
    std::sync::Mutex<
        std::collections::HashMap<
            String,
            std::collections::VecDeque<Vec<crate::services::hooks::HookResult>>,
        >,
    >,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
pub(crate) fn push_test_stop_hook_results(
    test_id: impl Into<String>,
    results: Vec<crate::services::hooks::HookResult>,
) {
    TEST_STOP_HOOK_RESULTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(test_id.into())
        .or_default()
        .push_back(results);
}

#[cfg(test)]
pub(crate) fn clear_test_stop_hook_results(test_id: &str) {
    TEST_STOP_HOOK_RESULTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(test_id);
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StopHookResult {
    /// Non-blocking transcript messages yielded while Stop hooks run.
    pub messages: Vec<RenderableMessage>,
    /// CC `blockingErrors` (query/stopHooks.ts:257-263).
    ///
    /// CC builds ONE message per blocking hook: an `isMeta` user message that
    /// it both yields to the transcript and collects here, so the model sees
    /// the feedback while the UI hides it ("shown in summary message instead"
    /// — the raw text rides `hookErrors` into the summary). Emitting a second,
    /// visible row for the same error would print it twice.
    pub blocking_model_messages: Vec<Message>,
    pub prevent_continuation: bool,
}

#[derive(Debug, Clone)]
pub struct StopHookParams {
    pub messages_for_query: Vec<Message>,
    pub assistant_messages: Vec<AssistantMessage>,
    pub system_prompt: Vec<String>,
    pub user_context: std::collections::BTreeMap<String, String>,
    pub system_context: std::collections::BTreeMap<String, String>,
    pub tool_use_context: ToolUseContext,
    pub query_source: QuerySource,
    pub stop_hook_active: Option<bool>,
    /// Optional actor event channel for live stop-hook progress. Maps to CC
    /// `executeStopHooks(...)` progress messages consumed by REPL spinnerSuffix.
    pub event_tx: Option<super::QueryEventSender>,
}

/// Maps to: CC `query/stopHooks.ts` `handleStopHooks(...)`.
///
/// Production loads the merged hooks settings and executes configured Stop
/// hooks through `services::hooks::lifecycle::execute_stop_hooks(...)`. The
/// remaining upstream side effects (prompt suggestions, extract memories,
/// auto-dream, and teammate/task hooks) intentionally stay TODO seams here
/// rather than being folded into `query.rs`.
/// Maps to: CC `handleStopHooks` saving `CacheSafeParams` for `/btw` and
/// side_question SDK control requests. Only main-session sources overwrite.
fn maybe_save_cache_safe_params(
    params: &StopHookParams,
) -> Option<std::sync::Arc<CacheSafeParams>> {
    if !matches!(params.query_source, QuerySource::Prompt | QuerySource::Sdk) {
        return None;
    }
    let mut messages = params.messages_for_query.clone();
    messages.extend(
        params
            .assistant_messages
            .iter()
            .cloned()
            .map(Message::Assistant),
    );
    let cache = std::sync::Arc::new(create_cache_safe_params(CacheSafeParamsContext {
        messages,
        system_prompt: params.system_prompt.clone(),
        user_context: params.user_context.clone(),
        system_context: params.system_context.clone(),
        tool_use_context: params.tool_use_context.clone(),
    }));
    save_cache_safe_params_arc(Some(cache.clone()));
    Some(cache)
}

/// The hook table `executeStopHooks` runs against.
///
/// Maps to: CC `utils/hooks.ts:2001-2010` — `executeHooks` resolves
/// `const sessionId = toolUseContext?.agentId ?? getSessionId()` and hands it to
/// `getMatchingHooks` → `getHooksConfig`, which merges that session's own hooks
/// (`:1541-1563`). `query/stopHooks.ts:180-189` is what supplies the context:
/// it passes `toolUseContext.agentId` as the subagent id AND `toolUseContext`
/// itself, so a subagent's Stop pass sees the hooks that agent registered while
/// the main thread's sees its own.
///
/// Old shape: `load_hooks_config()` alone, which owns only the settings and
/// registered/plugin channels — so every hook a run registered at runtime
/// through `register_agent_frontmatter_hooks_for_run` or `register_skill_hooks`
/// was silently absent from Stop. Not a hang: the table just came back without
/// them and `get_matching_hooks` matched nothing.
fn stop_hooks_config(tool_use_context: &ToolUseContext) -> RegisteredHooks {
    let session_id = tool_use_context
        .agent_id
        .clone()
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    crate::services::hooks::load_hooks_config_with_session_hooks(&session_id)
}

#[cfg(not(test))]
pub async fn handle_stop_hooks(params: StopHookParams) -> StopHookResult {
    let cache_safe_params = maybe_save_cache_safe_params(&params);
    let prompt_suggestion_env_disabled = crate::utils::env_utils::is_env_defined_falsy(
        crate::utils::process_env::var("CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION").as_deref(),
    );
    if matches!(params.query_source, QuerySource::Prompt)
        && !prompt_suggestion_env_disabled
        && !crate::utils::env_utils::is_bare_mode()
    {
        if let Some(cache_safe_params) = cache_safe_params {
            tokio::spawn(async move {
                crate::services::prompt_suggestion::prompt_suggestion::execute_prompt_suggestion(
                    std::sync::Arc::unwrap_or_clone(cache_safe_params),
                )
                .await;
            });
        }
    }
    if params.stop_hook_active.unwrap_or(false) {
        return StopHookResult::default();
    }

    let config = stop_hooks_config(&params.tool_use_context);
    // The emptiness guard has to read the MERGED table: CC has no early return
    // at all here (`getMatchingHooks` simply yields nothing), so a check against
    // the unmerged one would skip a session-only hook before it could run.
    if config.is_empty() {
        return StopHookResult::default();
    }

    handle_stop_hooks_with_config(params, &config, Vec::new()).await
}

/// Tests keep `handle_stop_hooks(...)` side-effect free so unit tests never run
/// user-configured shell hooks from a developer's real settings. Direct hook
/// execution parity is covered by `handle_stop_hooks_with_config(...)` tests.
#[cfg(test)]
pub async fn handle_stop_hooks(params: StopHookParams) -> StopHookResult {
    let _ = maybe_save_cache_safe_params(&params);
    if params.stop_hook_active.unwrap_or(false) {
        return StopHookResult::default();
    }
    let results = params
        .system_context
        .get("__test_stop_hook_id")
        .and_then(|test_id| {
            TEST_STOP_HOOK_RESULTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get_mut(test_id)
                .and_then(|queue| queue.pop_front())
        });
    results
        .map(stop_hook_result_from_hook_results)
        .unwrap_or_default()
}

/// Execute Stop hooks with an already-resolved hooks config.
/// Maps to the `executeStopHooks(...)` consumption inside CC
/// `query/stopHooks.ts`.
pub async fn handle_stop_hooks_with_config(
    params: StopHookParams,
    config: &RegisteredHooks,
    base_env: Vec<(String, String)>,
) -> StopHookResult {
    // Cache-safe params are saved by the public `handle_stop_hooks` owner.
    // This extracted executor must not duplicate the full-history snapshot.
    if params.stop_hook_active.unwrap_or(false) {
        return StopHookResult::default();
    }
    let last_assistant_message = last_assistant_message_text(&params.assistant_messages);
    let abort_controller = params.tool_use_context.abort_controller.clone();
    let event_tx = params.event_tx.clone();
    // CC `executeStopHooks(permissionMode, …, stopHookActive ?? false, …)`
    // (`query/stopHooks.ts:178-184`): the mode comes off the app state's
    // `toolPermissionContext`, which this port carries on the tool use context.
    let permission_mode = crate::utils::permissions::permission_mode::to_external_permission_mode(
        params.tool_use_context.tool_permission_context.mode,
    )
    .to_string();
    let stop_hook_active = params.stop_hook_active.unwrap_or(false);
    // Maps to: CC `query/stopHooks.ts:200-256` — the consumer of the ONE
    // `executeStopHooks` generator, which forwards the progress messages
    // `executeHooks` yields (`utils/hooks.ts:2094-2116`) to the transcript.
    // Old shape: a second, progress-emitting copy of the executor loop lived
    // here, which is how it drifted from `lifecycle::execute_event` in three
    // places — a null `tool_use_id` there, no abort check there, no progress
    // here — while CC has exactly one Stop executor.
    let progress_sink: Option<Box<crate::services::hooks::lifecycle::HookProgressSink>> =
        event_tx.clone().map(|event_tx| {
            Box::new(move |progress| {
                let event_tx = event_tx.clone();
                Box::pin(async move {
                    event_tx
                        .send(super::QueryEvent::StopHookProgress(
                            stop_hook_progress_event(progress),
                        ))
                        .await
                        .is_ok()
                }) as futures::future::BoxFuture<'static, bool>
            }) as Box<crate::services::hooks::lifecycle::HookProgressSink>
        });
    let results = crate::services::hooks::lifecycle::execute_stop_hooks(
        config,
        Some(permission_mode.as_str()),
        stop_hook_active,
        last_assistant_message.as_deref(),
        base_env,
        // CC `executeStopHooks(permissionMode, toolUseContext.abortController
        // .signal, …)` (`query/stopHooks.ts:180-189`).
        Some(&abort_controller),
        progress_sink.as_deref(),
        params.tool_use_context.agent_id.as_deref(),
        params.tool_use_context.agent_type.as_deref(),
    )
    .await;
    let mut result = stop_hook_result_from_hook_results(results);
    // Maps to: CC `query/stopHooks.ts:283-294` — an abort observed while Stop
    // hooks run yields the interruption marker and returns
    // `{ blockingErrors: [], preventContinuation: true }`. Note there is NO
    // `reason !== 'interrupt'` guard here, unlike query.ts's two sites.
    //
    // Dropping the blocking errors is the point: a blocked turn would feed
    // them back to the model and continue, which is exactly what the user
    // just interrupted. `preventContinuation` makes query return
    // `stop_hook_prevented` instead.
    if abort_controller.is_aborted() {
        result.blocking_model_messages.clear();
        result.prevent_continuation = true;
        if let Some(event_tx) = event_tx {
            let _ = event_tx
                .send(super::QueryEvent::Message(Message::User(
                    crate::utils::messages::create_user_interruption_message(false),
                )))
                .await;
        }
    }
    result
}

/// Project the executor's progress onto the REPL spinner's event.
///
/// Maps to: CC `query/stopHooks.ts:204-215` — the consumer reads `toolUseID`
/// and the `HookProgress` payload (`types/hooks.ts:234-241`) off the yielded
/// progress message. `Completed`/`Finished` are the port's counted stand-ins
/// for what CC derives from the resolved attachments and generator exhaustion.
fn stop_hook_progress_event(
    progress: crate::services::hooks::lifecycle::HookProgress,
) -> super::StopHookProgressEvent {
    use crate::services::hooks::lifecycle::HookProgress;
    match progress {
        HookProgress::Started {
            tool_use_id,
            hook_event,
            command,
            status_message,
            total,
        } => super::StopHookProgressEvent::Started {
            tool_use_id,
            hook_event,
            command,
            status_message,
            total,
        },
        HookProgress::Completed {
            tool_use_id,
            hook_event,
        } => super::StopHookProgressEvent::Completed {
            tool_use_id,
            hook_event,
        },
        HookProgress::Finished { tool_use_id } => {
            super::StopHookProgressEvent::Finished { tool_use_id }
        }
    }
}

fn last_assistant_message_text(assistant_messages: &[AssistantMessage]) -> Option<String> {
    assistant_messages.iter().rev().find_map(|message| {
        let text = message
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text(text) => Some(text.as_str()),
                AssistantContent::Thinking { text, .. } => Some(text.as_str()),
                AssistantContent::Advisor { content, .. } => content.text(),
                AssistantContent::RedactedThinking { .. }
                | AssistantContent::ToolUse(_)
                | AssistantContent::ServerToolUse(_)
                | AssistantContent::WebSearchToolResult { .. }
                | AssistantContent::MessageIdentity(_) => None,
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

/// Maps to the result normalization inside CC `query/stopHooks.ts` after
/// `executeStopHooks(...)` yields hook results. This helper is side-effect free:
/// shell hook execution remains in `services/hooks/*`, while this query seam
/// owns conversion into transcript and typed continuation messages.
pub fn stop_hook_result_from_hook_results(
    results: Vec<crate::services::hooks::HookResult>,
) -> StopHookResult {
    let mut output = StopHookResult::default();
    let mut hook_infos = Vec::new();
    let mut hook_errors = Vec::new();
    let mut has_output = false;
    let mut total_duration_ms = 0u64;
    let mut stop_reason = None;

    for (index, result) in results.into_iter().enumerate() {
        if result
            .stdout
            .as_deref()
            .is_some_and(|stdout| !stdout.trim().is_empty())
            || result
                .stderr
                .as_deref()
                .is_some_and(|stderr| !stderr.trim().is_empty())
        {
            has_output = true;
        }
        if let Some(duration_ms) = result.duration_ms {
            total_duration_ms = total_duration_ms.saturating_add(duration_ms);
        }
        if result.command.is_some() {
            hook_infos.push(StopHookInfo {
                command: result.command.clone(),
                prompt_text: None,
                duration_ms: result.duration_ms,
                output: result
                    .stdout
                    .as_ref()
                    .filter(|stdout| !stdout.trim().is_empty())
                    .cloned(),
                error: result
                    .stderr
                    .as_ref()
                    .filter(|stderr| !stderr.trim().is_empty())
                    .cloned(),
                prevented_continuation: result.prevent_continuation,
            });
        }
        if result.outcome == crate::services::hooks::HookOutcome::NonBlockingError {
            if let Some(stderr) = result
                .stderr
                .as_ref()
                .filter(|stderr| !stderr.trim().is_empty())
            {
                hook_errors.push(stderr.clone());
            }
        }
        if let Some(message) = result.system_message {
            has_output = true;
            output.messages.push(RenderableMessage::system(
                format!("stop-hook-message-{index}"),
                message,
            ));
        }
        if let Some(blocking_error) = result.blocking_error {
            let model_feedback = crate::services::hooks::get_stop_hook_message(&blocking_error);
            let raw_feedback = blocking_error.blocking_error;
            // The raw text reaches the UI through the summary's `hookErrors`,
            // exactly like CC (:266).
            hook_errors.push(raw_feedback);
            has_output = true;
            output.blocking_model_messages.push(Message::User(
                crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    // CC `createUserMessage({ …, isMeta: true })` — the model
                    // reads it, `shouldShowUserMessage` hides it.
                    content: vec![crate::types::message::UserContent::MetaText(model_feedback)],
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
            ));
        }
        if result.prevent_continuation {
            output.prevent_continuation = true;
            stop_reason = result
                .stop_reason
                .or_else(|| Some("Stop hook prevented continuation".to_string()));
        }
    }

    if !hook_infos.is_empty() {
        // Maps to: CC `createStopHookSummaryMessage(...)`
        // (utils/messages.ts:4398-4426); level 'info' like the query-loop
        // caller.
        output.messages.push(RenderableMessage {
            uuid: "stop-hook-summary".to_string(),
            kind: RenderableMessageKind::System(SystemMessage::StopHookSummary {
                base: crate::types::message::SystemBase::with_uuid("stop-hook-summary"),
                hook_label: Some("Stop".to_string()),
                hook_count: hook_infos.len(),
                hook_infos,
                hook_errors,
                prevented_continuation: output.prevent_continuation,
                stop_reason,
                has_output,
                level: crate::types::message::SystemMessageLevel::Info,
                tool_use_id: None,
                total_duration_ms: Some(total_duration_ms),
            }),
        });
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hooks::{HooksConfig, test_support::registered_config};
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn stop_hook_result_from_hook_results_keeps_nonblocking_messages_out_of_continuation() {
        let result = stop_hook_result_from_hook_results(vec![crate::services::hooks::HookResult {
            system_message: Some("Stop hook note".to_string()),
            ..Default::default()
        }]);

        assert_eq!(result.messages.len(), 1);
        assert!(result.blocking_model_messages.is_empty());
        assert!(!result.prevent_continuation);
    }

    #[test]
    fn stop_hook_result_from_hook_results_maps_blocking_errors_to_continuation_messages() {
        let result = stop_hook_result_from_hook_results(vec![crate::services::hooks::HookResult {
            blocking_error: Some(crate::services::hooks::HookBlockingError {
                blocking_error: "Stop hook blocked continuation".to_string(),
                command: "echo block".to_string(),
            }),
            ..Default::default()
        }]);

        assert_eq!(result.blocking_model_messages.len(), 1);
        assert!(matches!(
            &result.blocking_model_messages[0],
            Message::User(user) if user.content.iter().any(|content| matches!(
                content,
                // CC marks it `isMeta` so the render list hides it.
                crate::types::message::UserContent::MetaText(text)
                    if text == "Stop hook feedback:\nStop hook blocked continuation"
            ))
        ));
    }

    /// CC yields ONE message per blocking hook (stopHooks.ts:257-263): an
    /// `isMeta` user message the model reads and the UI hides, because the
    /// text is already shown by the summary's `hookErrors`. Emitting a visible
    /// row alongside it printed the same error twice.
    #[test]
    fn blocking_error_yields_one_model_message_hidden_from_the_render_list() {
        let result = stop_hook_result_from_hook_results(vec![crate::services::hooks::HookResult {
            command: Some("echo block".to_string()),
            blocking_error: Some(crate::services::hooks::HookBlockingError {
                blocking_error: "Stop hook blocked continuation".to_string(),
                command: "echo block".to_string(),
            }),
            ..Default::default()
        }]);

        assert_eq!(result.blocking_model_messages.len(), 1);
        let rows = crate::utils::messages::normalize_messages(&result.blocking_model_messages);
        assert_eq!(rows.len(), 1, "the message still enters the transcript");
        let visible = crate::components::messages_list::filter_non_rendering_messages(rows, false);
        assert!(
            visible.is_empty(),
            "shouldShowUserMessage drops the isMeta blocking feedback"
        );

        // The raw text is what the summary surfaces instead.
        let summary_errors = result
            .messages
            .iter()
            .find_map(|message| match &message.kind {
                RenderableMessageKind::System(SystemMessage::StopHookSummary {
                    hook_errors,
                    ..
                }) => Some(hook_errors.clone()),
                _ => None,
            });
        assert_eq!(
            summary_errors,
            Some(vec!["Stop hook blocked continuation".to_string()])
        );
    }

    fn stop_hook_params_for_tests() -> StopHookParams {
        StopHookParams {
            messages_for_query: Vec::new(),
            assistant_messages: vec![AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("last assistant".to_string())],
                model: None,
                stop_reason: None,
                usage: None,
            }],
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            tool_use_context: ToolUseContext::default(),
            query_source: QuerySource::Prompt,
            stop_hook_active: None,
            event_tx: None,
        }
    }

    #[tokio::test]
    async fn handle_stop_hooks_with_config_emits_spinner_progress_events() {
        let config: HooksConfig = serde_json::from_value(serde_json::json!({
            "Stop": [{
                "matcher": "*",
                "hooks": [
                    { "command": "printf first", "timeout": 5, "status": "Cleaning up" },
                    { "command": "printf second", "timeout": 5 }
                ]
            }]
        }))
        .unwrap();
        let config = registered_config(&config);
        let (event_tx, event_rx) = async_channel::unbounded();
        let mut params = stop_hook_params_for_tests();
        params.event_tx = Some(event_tx.into());

        let result = handle_stop_hooks_with_config(params, &config, Vec::new()).await;
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }

        assert!(result.messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(SystemMessage::StopHookSummary { hook_count: 2, .. })
        )));
        assert!(matches!(
            &events[0],
            crate::query::QueryEvent::StopHookProgress(
                crate::query::StopHookProgressEvent::Started {
                    hook_event,
                    status_message: Some(status_message),
                    total: 2,
                    ..
                }
            ) if hook_event == "Stop" && status_message == "Cleaning up"
        ));
        assert!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    crate::query::QueryEvent::StopHookProgress(
                        crate::query::StopHookProgressEvent::Completed { .. }
                    )
                ))
                .count()
                == 2
        );
        assert!(matches!(
            events.last(),
            Some(crate::query::QueryEvent::StopHookProgress(
                crate::query::StopHookProgressEvent::Finished { .. }
            ))
        ));
    }

    #[test]
    fn stop_hook_result_from_hook_results_builds_official_summary_metadata() {
        let result = stop_hook_result_from_hook_results(vec![crate::services::hooks::HookResult {
            command: Some("echo hook".to_string()),
            duration_ms: Some(42),
            stdout: Some("hook output".to_string()),
            ..Default::default()
        }]);

        assert!(result.messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(SystemMessage::StopHookSummary {
                hook_label: Some(label),
                hook_count: 1,
                hook_infos,
                has_output: true,
                total_duration_ms: Some(42),
                ..
            }) if label == "Stop"
                && hook_infos.len() == 1
                && hook_infos[0].command.as_deref() == Some("echo hook")
        )));
    }

    #[tokio::test]
    async fn handle_stop_hooks_with_config_skips_when_stop_hook_active() {
        let config: HooksConfig = serde_json::from_value(serde_json::json!({
            "Stop": [{
                "matcher": "*",
                "hooks": [{"command": "printf should-not-run", "timeout": 5}]
            }]
        }))
        .unwrap();
        let config = registered_config(&config);
        let mut params = stop_hook_params_for_tests();
        params.stop_hook_active = Some(true);

        let result = handle_stop_hooks_with_config(params, &config, vec![]).await;

        assert!(result.messages.is_empty());
        assert!(result.blocking_model_messages.is_empty());
        assert!(!result.prevent_continuation);
    }

    #[tokio::test]
    async fn handle_stop_hooks_with_config_executes_configured_stop_hook_blocking_result() {
        let config: HooksConfig = serde_json::from_value(serde_json::json!({
            "Stop": [{
                "matcher": "*",
                "hooks": [{
                    "command": "printf '%s' '{\"decision\":\"block\",\"reason\":\"Stop says no\"}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let config = registered_config(&config);

        let result =
            handle_stop_hooks_with_config(stop_hook_params_for_tests(), &config, vec![]).await;

        assert_eq!(result.blocking_model_messages.len(), 1);
        assert!(matches!(
            &result.blocking_model_messages[0],
            Message::User(user) if user.content.iter().any(|content| matches!(
                content,
                // CC marks it `isMeta` so the render list hides it.
                crate::types::message::UserContent::MetaText(text) if text == "Stop hook feedback:\nStop says no"
            ))
        ));
    }

    /// Maps to: CC `query/stopHooks.ts:283-294`. Same config as the blocking
    /// test above, which asserts one blocking message survives — under an
    /// abort CC drops them and forces `preventContinuation` instead, because
    /// feeding stop-hook feedback back to the model would continue exactly the
    /// turn the user just interrupted. Unlike query.ts's two sites, CC applies
    /// no `reason !== 'interrupt'` guard here.
    #[tokio::test]
    async fn abort_during_stop_hooks_drops_blocking_errors_and_marks_the_interrupt() {
        let config: HooksConfig = serde_json::from_value(serde_json::json!({
            "Stop": [{
                "matcher": "*",
                "hooks": [{
                    "command": "printf '%s' '{\"decision\":\"block\",\"reason\":\"Stop says no\"}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let config = registered_config(&config);
        let (event_tx, event_rx) = async_channel::unbounded();
        let mut params = stop_hook_params_for_tests();
        params.event_tx = Some(event_tx.into());
        // The context carries the actor's controller (query.rs
        // `loop_local_tool_use_context`), which is what the REPL aborts.
        params.tool_use_context.abort_controller.abort();

        let result = handle_stop_hooks_with_config(params, &config, Vec::new()).await;

        assert!(
            result.blocking_model_messages.is_empty(),
            "CC returns `blockingErrors: []` on abort"
        );
        assert!(result.prevent_continuation);

        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events.iter().any(|event| matches!(
                event,
                super::super::QueryEvent::Message(Message::User(user))
                    if user.content.iter().any(|content| matches!(
                        content,
                        crate::types::message::UserContent::Text(text)
                            if text == crate::utils::messages::INTERRUPT_MESSAGE
                    ))
            )),
            "the interruption marker must reach the transcript"
        );
    }

    /// Maps to: CC `utils/hooks.ts:2003` — `executeHooks` keys the config on
    /// `toolUseContext?.agentId ?? getSessionId()`, and `query/stopHooks.ts:186`
    /// is what hands it the context. Both arms matter: a subagent Stop pass must
    /// see the hooks that agent registered, and the main thread's must see the
    /// main session's.
    ///
    /// Old shape: `load_hooks_config()` on its own, which owns only the settings
    /// and registered channels — so the table came back WITHOUT either session's
    /// hooks and `get_matching_hooks` matched nothing. A silent skip, not a hang.
    #[test]
    fn stop_hooks_config_merges_the_agents_own_session_hooks_then_the_main_sessions() {
        use crate::services::hooks::{HookCommand, HookEvent};
        use crate::utils::hooks::session_hooks;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let hook = |command: &str| HookCommand {
            command: command.to_string(),
            shell: None,
            timeout: Some(5),
            condition: None,
            status: None,
            once: None,
            is_async: None,
            async_rewake: None,
        };
        let main_session = crate::bootstrap::state::get_session_id();

        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook("agent-stop", HookEvent::Stop, "", hook("echo agent stop"));
        session_hooks::add_session_hook(&main_session, HookEvent::Stop, "", hook("echo main stop"));

        let agent_context = ToolUseContext::default().with_agent_id(Some("agent-stop".to_string()));
        let agent_config = stop_hooks_config(&agent_context);
        let main_config = stop_hooks_config(&ToolUseContext::default());
        session_hooks::clear_all_session_hooks();

        let commands = |config: &RegisteredHooks| -> Vec<String> {
            config
                .get("Stop")
                .map(|entries| {
                    entries
                        .iter()
                        .flat_map(|entry| entry.hooks.iter())
                        .filter_map(|hook| match hook {
                            crate::schemas::hooks::RegisteredHook::Command(command) => {
                                Some(command.command.clone())
                            }
                            crate::schemas::hooks::RegisteredHook::Callback(_) => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        assert_eq!(commands(&agent_config), vec!["echo agent stop".to_string()]);
        assert_eq!(commands(&main_config), vec!["echo main stop".to_string()]);
    }

    #[tokio::test]
    async fn handle_stop_hooks_is_side_effect_free_noop_under_unit_tests() {
        let result = handle_stop_hooks(stop_hook_params_for_tests()).await;

        assert!(result.blocking_model_messages.is_empty());
        assert!(!result.prevent_continuation);
    }
}
