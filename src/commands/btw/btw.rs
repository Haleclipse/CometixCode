//! Maps to: CC `commands/btw/btw.tsx`.

use crate::commands::Command;
use crate::components::markdown::Markdown;
use crate::components::spinner::SpinnerGlyph;
use crate::constants::figures::{DOWN_ARROW, UP_ARROW};
use crate::constants::query_source::QuerySource;
use crate::types::message::Message;
use crate::utils::forked_agent::{
    CacheSafeParams, CacheSafeParamsContext, create_cache_safe_params, get_last_cache_safe_params,
};
use crate::utils::messages::get_messages_after_compact_boundary;
use crate::utils::process_user_input::ProcessUserInputBaseResult;
use crate::utils::process_user_input::process_slash_command::{
    LocalCommandUi, SlashCommandAction, SlashCommandInvocation, system_display_local_command_result,
};
use crate::utils::side_question::run_side_question;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::sync::Arc;
use std::time::Duration;

const CHROME_ROWS: u16 = 5;
const OUTER_CHROME_ROWS: u16 = 6;
const SCROLL_LINES: i32 = 3;
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// Maps to: CC `stripInProgressAssistantMessage` (`btw.tsx:160-171`).
pub fn strip_in_progress_assistant_message(messages: &[Message]) -> Vec<Message> {
    let Some(last) = messages.last() else {
        return messages.to_vec();
    };
    if let Message::Assistant(assistant) = last {
        if assistant.stop_reason.is_none() {
            return messages[..messages.len().saturating_sub(1)].to_vec();
        }
    }
    messages.to_vec()
}

/// Maps to: CC `buildCacheSafeParams` (`btw.tsx:174-207`).
///
/// The saved prompt bytes come from the last completed main-thread request,
/// while model/tool/thinking state and messages always come from the exact
/// context captured for this `/btw` invocation.
pub async fn build_cache_safe_params(context: &crate::tool::ToolUseContext) -> CacheSafeParams {
    let fork_context_messages = get_messages_after_compact_boundary(
        &strip_in_progress_assistant_message(&context.messages),
    );
    if let Some(saved) = get_last_cache_safe_params() {
        return CacheSafeParams {
            system_prompt: saved.system_prompt.clone(),
            user_context: saved.user_context.clone(),
            system_context: saved.system_context.clone(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(fork_context_messages),
        };
    }

    let model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_default_main_loop_model);
    // Maps to CC `btw.tsx:189-196` `getSystemPrompt(context.options.tools,
    // context.options.mainLoopModel, [], context.options.mcpClients)` — the
    // fallback rebuild passes an EMPTY additionalWorkingDirectories list.
    let raw_system_prompt = crate::constants::prompts::get_system_prompt(
        &context.tools,
        &model,
        &[],
        &context.mcp_state.clients,
    );
    create_cache_safe_params(CacheSafeParamsContext {
        messages: fork_context_messages,
        system_prompt: raw_system_prompt,
        user_context: crate::context::get_user_context(),
        system_context: crate::context::get_system_context(),
        tool_use_context: context.clone(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BtwFetchOutcome {
    Response(String),
    Error(String),
}

fn fetch_error_message(error: anyhow::Error) -> String {
    let message = error.to_string();
    if message.trim().is_empty() {
        "Failed to get response".to_string()
    } else {
        message
    }
}

fn spawn_fetch_response(
    question: String,
    context: Arc<crate::tool::ToolUseContext>,
) -> async_channel::Receiver<BtwFetchOutcome> {
    let (outcome_tx, outcome_rx) = async_channel::bounded(1);
    let failure_tx = outcome_tx.clone();
    let spawn = std::thread::Builder::new()
        .name("btw-side-question".to_string())
        .spawn(move || {
            let result = futures::executor::block_on(async {
                let cache_safe_params = build_cache_safe_params(context.as_ref()).await;
                run_side_question(&question, cache_safe_params).await
            });
            let outcome = match result {
                Ok(side) => match side.response {
                    Some(text) => BtwFetchOutcome::Response(text),
                    None => BtwFetchOutcome::Error("No response received".to_string()),
                },
                Err(error) => BtwFetchOutcome::Error(fetch_error_message(error)),
            };
            let _ = outcome_tx.send_blocking(outcome);
        });
    if let Err(error) = spawn {
        let _ = failure_tx.send_blocking(BtwFetchOutcome::Error(error.to_string()));
    }
    outcome_rx
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BtwKeyAction {
    Dismiss,
    Scroll(i32),
}

/// Maps to: CC `BtwSideQuestion.handleKeyDown` (`btw.tsx:58-77`).
fn btw_key_action(key: &str, ctrl: bool) -> Option<BtwKeyAction> {
    if matches!(key, "escape" | "return" | " ") || (ctrl && matches!(key, "c" | "d")) {
        return Some(BtwKeyAction::Dismiss);
    }
    if key == "up" || (ctrl && key == "p") {
        return Some(BtwKeyAction::Scroll(-SCROLL_LINES));
    }
    if key == "down" || (ctrl && key == "n") {
        return Some(BtwKeyAction::Scroll(SCROLL_LINES));
    }
    None
}

fn spinner_interval(finished: bool) -> Option<Duration> {
    (!finished).then_some(SPINNER_INTERVAL)
}

fn content_viewport_height(
    response: Option<&str>,
    error: Option<&str>,
    columns: u16,
    max_height: u16,
) -> u16 {
    use unicode_width::UnicodeWidthStr;

    let width = usize::from(columns.saturating_sub(4).max(1));
    let rendered_rows = if let Some(error) = error {
        error
            .lines()
            .map(|line| UnicodeWidthStr::width(line).max(1).div_ceil(width))
            .sum::<usize>()
    } else if let Some(response) = response {
        // The child is `Markdown`, not raw `Text`: tables, block gaps, code,
        // and ANSI styling can produce a different row count than `str::lines`.
        // Use the same formatter before applying the parent viewport width.
        crate::components::markdown::markdown_to_lines_with_width(
            response,
            usize::from(columns.max(1)),
        )
        .iter()
        .map(|line| {
            crate::components::markdown_table::display_width_ansi(&line.text)
                .max(1)
                .div_ceil(width)
        })
        .sum::<usize>()
    } else {
        1
    };
    rendered_rows.clamp(1, usize::from(max_height)) as u16
}

#[derive(Default, Props)]
struct BtwSideQuestionViewProps<'a> {
    question: String,
    response: Option<String>,
    error: Option<String>,
    frame: usize,
    on_done: HandlerMut<'a, ()>,
}

/// Retained rendering half of CC `BtwSideQuestion`.
///
/// `FocusScope + View(tab_index, auto_focus, on_key_down)` is the iocraft L1
/// equivalent of CC's focusable Ink `Box`. The nested `ScrollBox` remains a
/// main-screen local-command viewport; it is unrelated to deferred fullscreen
/// REPL scrolling.
#[component]
fn BtwSideQuestionView<'a>(
    props: &mut BtwSideQuestionViewProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut pending_done = hooks.use_state(|| false);
    let scroll_handle = hooks.use_ref_default::<ScrollBoxHandle>();

    let mut pending_done_for_key = pending_done;
    let mut scroll_handle_for_key = scroll_handle;
    let on_key_down = move |event: ViewKeyboardEvent| {
        let Some(action) = btw_key_action(&event.key, event.ctrl) else {
            return;
        };
        event.prevent_default();
        // CC's focused Box only targets this panel. Stop iocraft's global
        // propagation so PromptInput/app handlers cannot also consume it.
        event.stop_propagation();
        match action {
            BtwKeyAction::Dismiss => pending_done_for_key.set(true),
            BtwKeyAction::Scroll(lines) => scroll_handle_for_key.write().scroll_by(lines),
        }
    };

    if pending_done.get() {
        pending_done.set(false);
        (props.on_done)(());
    }

    let finished = props.response.is_some() || props.error.is_some();
    let (columns, rows) = hooks.use_terminal_size();
    let max_content_height = rows.saturating_sub(CHROME_ROWS + OUTER_CHROME_ROWS).max(5);
    let content_height = content_viewport_height(
        props.response.as_deref(),
        props.error.as_deref(),
        columns,
        max_content_height,
    );
    let question = props.question.clone();
    let response = props.response.clone();
    let error = props.error.clone();
    let frame = props.frame;

    element! {
        FocusScope(trap_keys: Some(true)) {
            View(
                flex_direction: FlexDirection::Column,
                padding_left: 2u32,
                margin_top: 1u32,
                tab_index: Some(0),
                auto_focus: true,
                on_key_down: on_key_down,
            ) {
                View(flex_direction: FlexDirection::Row) {
                    Text(
                        content: "/btw ".to_string(),
                        color: theme.warning,
                        weight: Weight::Bold,
                        wrap: TextWrap::NoWrap,
                    )
                    Text(content: question, dim: true, wrap: TextWrap::Wrap)
                }
                View(
                    margin_top: 1u32,
                    margin_left: 2u32,
                    height: content_height as u32,
                    max_height: max_content_height as u32,
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::Hidden,
                ) {
                    ScrollBox(
                        handle: Some(scroll_handle),
                        sticky_scroll: false,
                        scroll_step: Some(SCROLL_LINES as u16),
                        keyboard_scroll: Some(false),
                    ) {
                        #(if let Some(error) = error {
                            Some(element! {
                                Text(content: error, color: theme.error, wrap: TextWrap::Wrap)
                            }.into_any())
                        } else if let Some(response) = response {
                            Some(element! {
                                Markdown(content: response)
                            }.into_any())
                        } else {
                            Some(element! {
                                View(flex_direction: FlexDirection::Row) {
                                    SpinnerGlyph(frame: frame, color: Some(theme.warning))
                                    Text(
                                        content: "Answering...".to_string(),
                                        color: theme.warning,
                                        wrap: TextWrap::NoWrap,
                                    )
                                }
                            }.into_any())
                        })
                    }
                }
                #(finished.then(|| element! {
                    View(margin_top: 1u32) {
                        Text(
                            content: format!(
                                "{UP_ARROW}/{DOWN_ARROW} to scroll · Space, Enter, or Escape to dismiss"
                            ),
                            dim: true,
                            wrap: TextWrap::Wrap,
                        )
                    }
                }))
            }
        }
    }
}

#[derive(Default, Props)]
pub struct BtwSideQuestionProps<'a> {
    pub question: String,
    pub context: Arc<crate::tool::ToolUseContext>,
    pub on_done: HandlerMut<'a, ()>,
}

/// Maps to: CC `BtwSideQuestion` (`btw.tsx:43-151`).
#[component]
pub fn BtwSideQuestion<'a>(
    props: &mut BtwSideQuestionProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let response = hooks.use_state(|| None::<String>);
    let error = hooks.use_state(|| None::<String>);
    let frame = hooks.use_state(|| 0usize);
    let mut pending_done = hooks.use_state(|| false);

    // `use_future` itself is polled by the retained render loop. Spawn all
    // cache-context construction, config/filesystem reads, and query work onto
    // a worker before awaiting the typed outcome channel.
    let outcome_rx = hooks.use_const({
        let question = props.question.clone();
        let context = Arc::clone(&props.context);
        move || Arc::new(spawn_fetch_response(question, context))
    });
    hooks.use_future({
        let mut response = response;
        let mut error = error;
        async move {
            if let Ok(outcome) = outcome_rx.recv().await {
                match outcome {
                    BtwFetchOutcome::Response(text) => response.set(Some(text)),
                    BtwFetchOutcome::Error(text) => error.set(Some(text)),
                }
            }
        }
    });

    let finished = response.read().is_some() || error.read().is_some();
    let mut frame_for_interval = frame;
    hooks.use_interval(
        move || frame_for_interval.set(frame_for_interval.get().wrapping_add(1)),
        spinner_interval(finished),
    );

    if pending_done.get() {
        pending_done.set(false);
        (props.on_done)(());
    }
    let mut pending_done_for_view = pending_done;
    element! {
        BtwSideQuestionView(
            question: props.question.clone(),
            response: response.read().clone(),
            error: error.read().clone(),
            frame: frame.get(),
            on_done: move |_| pending_done_for_view.set(true),
        )
    }
}

fn increment_btw_use_count() -> anyhow::Result<()> {
    crate::utils::config::save_global_config(|config| {
        config.btw_use_count = Some(config.btw_use_count.unwrap_or(0).saturating_add(1));
    })
}

fn increment_btw_use_count_in_background() {
    // Preserve the explicit no-write seam without creating a detached worker
    // that could outlive a test/environment guard.
    if !crate::utils::config::is_config_write_enabled() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("btw-config-write".to_string())
        .spawn(|| {
            if let Err(error) = increment_btw_use_count() {
                tracing::warn!(error = %error, "failed to persist btwUseCount");
            }
        });
}

/// Maps to: CC `commands/btw/btw.tsx#call` (`btw.tsx:209-231`).
pub fn call(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    let question = args.trim();
    if question.is_empty() {
        return system_display_local_command_result(
            uuid,
            command.name.as_ref(),
            args,
            "Usage: /btw <your question>",
        );
    }

    increment_btw_use_count_in_background();

    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::OpenLocalCommandUi {
            command: LocalCommandUi::Btw {
                question: question.to_string(),
                context: Arc::new(context.clone()),
            },
            invocation: SlashCommandInvocation::new(command.name.as_ref(), args),
        }),
        query_source: QuerySource::Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantContent, AssistantMessage};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use futures::{StreamExt, stream};

    struct CacheSafeParamsRestore(Option<CacheSafeParams>);

    impl Drop for CacheSafeParamsRestore {
        fn drop(&mut self) {
            crate::utils::forked_agent::save_cache_safe_params(self.0.take());
        }
    }

    #[test]
    fn strip_in_progress_assistant_message_matches_official_null_stop_reason_tail() {
        let messages = vec![
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text("hi".into())],
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
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("partial".into())],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ];
        assert_eq!(strip_in_progress_assistant_message(&messages).len(), 1);
    }

    #[test]
    fn build_cache_safe_params_matches_official_saved_bytes_and_current_context() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let previous = get_last_cache_safe_params().map(|params| params.as_ref().clone());
        let _restore = CacheSafeParamsRestore(previous);

        let saved = CacheSafeParams {
            system_prompt: vec!["saved-system".to_string()],
            user_context: [("saved-user".to_string(), "yes".to_string())]
                .into_iter()
                .collect(),
            system_context: [("saved-system-context".to_string(), "yes".to_string())]
                .into_iter()
                .collect(),
            tool_use_context: crate::tool::ToolUseContext::default()
                .with_main_loop_model("saved-model"),
            fork_context_messages: Arc::new(Vec::new()),
        };
        crate::utils::forked_agent::save_cache_safe_params(Some(saved));

        let current_messages = vec![
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text("current".into())],
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
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("partial".into())],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ];
        let current = crate::tool::ToolUseContext::default()
            .with_main_loop_model("current-model")
            .with_messages(current_messages);
        let built = futures::executor::block_on(build_cache_safe_params(&current));

        assert_eq!(built.system_prompt, vec!["saved-system"]);
        assert_eq!(
            built.user_context.get("saved-user"),
            Some(&"yes".to_string())
        );
        assert_eq!(
            built.tool_use_context.main_loop_model.as_deref(),
            Some("current-model")
        );
        assert_eq!(built.fork_context_messages.len(), 1);
    }

    #[test]
    fn btw_call_empty_args_matches_official_system_display_rows() {
        let command = Command::local_ui(super::super::NAME, super::super::DESCRIPTION);
        let result = call(
            &command,
            "  ",
            Some("btw-1".into()),
            &crate::tool::ToolUseContext::default(),
        );
        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(result.messages.len(), 2);
        assert!(matches!(
            &result.messages[0].kind,
            crate::types::message::RenderableMessageKind::System(
                crate::types::message::SystemMessage::LocalCommand { content: command, .. }
            ) if command.contains("<command-name>/btw</command-name>")
        ));
        assert!(matches!(
            &result.messages[1].kind,
            crate::types::message::RenderableMessageKind::System(
                crate::types::message::SystemMessage::LocalCommand { content: command, .. }
            ) if command == "<local-command-stdout>Usage: /btw <your question></local-command-stdout>"
        ));
    }

    #[test]
    fn btw_call_carries_the_exact_current_context_snapshot() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _no_write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let command = Command::local_ui(super::super::NAME, super::super::DESCRIPTION);
        let context = crate::tool::ToolUseContext::default()
            .with_main_loop_model("current-model")
            .with_messages(vec![Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text("context".into())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            })]);
        let result = call(&command, "what is caching?", None, &context);
        let Some(SlashCommandAction::OpenLocalCommandUi {
            command:
                LocalCommandUi::Btw {
                    question,
                    context: captured,
                },
            invocation,
        }) = result.local_action
        else {
            panic!("expected /btw local UI action");
        };
        assert_eq!(question, "what is caching?");
        assert_eq!(
            invocation,
            SlashCommandInvocation::new("btw", "what is caching?")
        );
        assert_eq!(captured.main_loop_model.as_deref(), Some("current-model"));
        assert_eq!(captured.messages, context.messages);
    }

    #[test]
    fn btw_key_actions_match_official_dismiss_and_three_line_scroll() {
        assert_eq!(btw_key_action("escape", false), Some(BtwKeyAction::Dismiss));
        assert_eq!(btw_key_action("return", false), Some(BtwKeyAction::Dismiss));
        assert_eq!(btw_key_action(" ", false), Some(BtwKeyAction::Dismiss));
        assert_eq!(btw_key_action("c", true), Some(BtwKeyAction::Dismiss));
        assert_eq!(btw_key_action("d", true), Some(BtwKeyAction::Dismiss));
        assert_eq!(btw_key_action("up", false), Some(BtwKeyAction::Scroll(-3)));
        assert_eq!(btw_key_action("p", true), Some(BtwKeyAction::Scroll(-3)));
        assert_eq!(btw_key_action("down", false), Some(BtwKeyAction::Scroll(3)));
        assert_eq!(btw_key_action("n", true), Some(BtwKeyAction::Scroll(3)));
        assert_eq!(btw_key_action("p", false), None);
    }

    #[test]
    fn btw_spinner_interval_stops_after_response_or_error_like_official() {
        assert_eq!(spinner_interval(false), Some(Duration::from_millis(80)));
        assert_eq!(spinner_interval(true), None);
    }

    #[test]
    fn btw_viewport_height_counts_rendered_markdown_rows_not_raw_source_lines() {
        let table = "| A | B |\n|---|---|\n| x | y |";
        let height = content_viewport_height(Some(table), None, 80, 20);
        assert!(
            height > table.lines().count() as u16,
            "Markdown table chrome must contribute viewport rows: height={height}"
        );
    }

    #[component]
    fn DismissHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let dismissed = hooks.use_state(|| false);
        if dismissed.get() {
            system.exit();
            return element!(Text(content: "dismissed".to_string())).into_any();
        }
        let mut dismissed_for_done = dismissed;
        element! {
            ContextProvider(value: Context::owned(crate::utils::theme::DARK)) {
                BtwSideQuestionView(
                    question: "question".to_string(),
                    response: Some("answer".to_string()),
                    on_done: move |_| dismissed_for_done.set(true),
                )
            }
        }
        .into_any()
    }

    fn render_dismiss_key(event: KeyEvent) -> Vec<String> {
        futures::executor::block_on(async {
            let mut app = element!(DismissHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(event)])),
            ));
            let mut outputs = Vec::new();
            while outputs.len() < 5 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                outputs.push(canvas.to_string());
            }
            outputs
        })
    }

    #[test]
    fn btw_focused_view_consumes_all_official_dismiss_keys_locally() {
        let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
        ctrl_c.modifiers = KeyModifiers::CONTROL;
        let mut ctrl_d = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('d'));
        ctrl_d.modifiers = KeyModifiers::CONTROL;
        let cases = vec![
            ("Ctrl-C", ctrl_c),
            ("Ctrl-D", ctrl_d),
            ("Escape", KeyEvent::new(KeyEventKind::Press, KeyCode::Esc)),
            ("Enter", KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
            (
                "Space",
                KeyEvent::new(KeyEventKind::Press, KeyCode::Char(' ')),
            ),
        ];

        for (label, event) in cases {
            let outputs = render_dismiss_key(event);
            assert!(
                outputs
                    .last()
                    .is_some_and(|canvas| canvas.contains("dismissed")),
                "{label} should dismiss /btw through its local owner: {outputs:#?}"
            );
        }
    }

    #[component]
    fn ScrollHarness(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let response = (0..20)
            .map(|index| format!("row-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        element! {
            ContextProvider(value: Context::owned(crate::utils::theme::DARK)) {
                BtwSideQuestionView(
                    question: "question".to_string(),
                    response: Some(response),
                )
            }
        }
    }

    #[test]
    fn btw_down_arrow_scrolls_the_inner_main_screen_viewport_three_lines() {
        let events = stream::once(async {
            futures_timer::Delay::new(Duration::from_millis(10)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down))
        });
        let canvases = futures::executor::block_on(async {
            let mut app = element!(ScrollHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(40, 18),
            ));
            let mut canvases = Vec::new();
            while canvases.len() < 5 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                canvases.push(canvas);
            }
            canvases
        });
        let before = canvases.first().map(Canvas::to_string).unwrap_or_default();
        let after = canvases.last().map(Canvas::to_string).unwrap_or_default();
        assert!(before.contains("row-00"), "before={before}");
        assert!(after.contains("row-03"), "after={after}");
        assert!(!after.contains("row-00"), "after={after}");
    }
}
