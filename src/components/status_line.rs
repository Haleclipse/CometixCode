//! Maps to: CC `components/StatusLine.tsx`.
//!
//! Owns:
//! - `statusLineShouldDisplay`
//! - `buildStatusLineCommandInput` (stdin JSON organization / export)
//! - `getLastAssistantMessageId`
//! - rendered status-line row (`StatusLine` component)
//!
//! Command execution stays in [`crate::services::hooks::statusline`]
//! (`executeStatusLineCommand`). Debounced refresh is driven by callers
//! (REPL / main) via [`run_status_line_update`], matching the official
//! `StatusLineInner#doUpdate` body without React hooks.

use crate::constants::output_styles::DEFAULT_OUTPUT_STYLE_NAME;
use crate::cost_tracker::{
    get_total_api_duration, get_total_cost, get_total_duration, get_total_input_tokens,
    get_total_lines_added, get_total_lines_removed, get_total_output_tokens,
};
use crate::types::message::Message;
use crate::types::permissions::PermissionMode;
use crate::types::status_line::{
    StatusLineAgent, StatusLineCommandInput, StatusLineContextWindow, StatusLineCost,
    StatusLineModel, StatusLineOutputStyle, StatusLineRemote, StatusLineVim, StatusLineWorkspace,
    StatusLineWorktree,
};
use crate::utils::context::{calculate_context_percentages, get_context_window_for_model};
use crate::utils::model::model::{get_runtime_main_loop_model, render_model_name};
use crate::utils::settings::types::SettingsJson;
use crate::utils::tokens::{does_most_recent_assistant_message_exceed_200k, get_current_usage};
use iocraft::prelude::*;

/// Maps to: CC `components/StatusLine.tsx#statusLineShouldDisplay`.
pub fn status_line_should_display(settings: &SettingsJson) -> bool {
    settings.status_line.is_some()
}

/// Maps to the `statusLineText ? ... : fullscreen ? ' ' : null` render gate in
/// CC `components/StatusLine.tsx#StatusLineInner`. Cometix currently uses the
/// normal main-screen/native-scrollback branch, so only non-empty text renders.
/// Maps to: CC `PromptInputFooter.tsx:176-178` — the mount condition is
/// `!exitMessage.show && !isPasting && statusLineShouldDisplay(settings)`.
///
/// It deliberately does NOT look at the text. The component owns the update
/// loop, so gating its mount on the text it produces is a deadlock: no mount
/// means no loop, no loop means no text, and the status line never appears.
/// That is exactly what happened when the loop moved here from `main.rs`, where
/// it had run outside the tree and could seed the text before the footer
/// mounted.
///
/// An empty text renders a zero-height view (see `StatusLine`), which is what
/// CC gets from rendering a `StatusLineInner` whose `statusLineText` is still
/// undefined.
///
/// Seam: CC also gates on `isPasting`; Cometix's footer does not track that
/// yet.
pub fn status_line_should_render(configured: bool, exit_hint_visible: bool) -> bool {
    !exit_hint_visible && configured
}

/// Maps to: CC `components/StatusLine.tsx#getLastAssistantMessageId`.
pub fn get_last_assistant_message_id(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => Some(format!(
            "{}:{}",
            assistant.timestamp.timestamp_millis(),
            assistant.model.as_deref().unwrap_or("assistant")
        )),
        _ => None,
    })
}

fn permission_mode_api_name(mode: PermissionMode) -> String {
    serde_json::to_value(mode)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "default".to_string())
}

fn transcript_path_for_session(session_id: &str) -> String {
    let cwd = crate::bootstrap::state::get_original_cwd();
    let project_path = cwd.to_string_lossy();
    crate::utils::session_storage::get_session_file_path(&project_path, session_id)
        .display()
        .to_string()
}

fn current_cwd_string() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| {
            crate::bootstrap::state::get_original_cwd()
                .display()
                .to_string()
        })
}

/// Maps to: CC `components/StatusLine.tsx#buildStatusLineCommandInput`.
///
/// `main_loop_model` must already be API-resolved (CC `useMainLoopModel()`).
pub fn build_status_line_command_input(
    permission_mode: PermissionMode,
    exceeds_200k_tokens: bool,
    settings: &SettingsJson,
    messages: &[Message],
    added_dirs: &[String],
    main_loop_model: &str,
    vim_mode: Option<&str>,
) -> StatusLineCommandInput {
    let runtime_model = get_runtime_main_loop_model(
        permission_mode,
        main_loop_model.to_string(),
        exceeds_200k_tokens,
    );
    let output_style_name = settings
        .output_style
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_OUTPUT_STYLE_NAME)
        .to_string();

    let current_usage = get_current_usage(messages);
    let context_window_size = get_context_window_for_model(&runtime_model, &[]);
    let (used_percentage, remaining_percentage) =
        calculate_context_percentages(current_usage.as_ref(), context_window_size);

    let session_id = crate::bootstrap::state::get_session_id();
    let session_name = crate::utils::session_storage::get_current_session_metadata()
        .custom_title
        .filter(|title| !title.trim().is_empty());
    let cwd = current_cwd_string();
    let project_dir = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();

    // Official `createBaseHookInput()` is called with no args from StatusLine,
    // so top-level `permission_mode` is usually omitted. Keep the field optional
    // for forward-compat with hook payloads that do pass it.
    let mut input = StatusLineCommandInput {
        session_id: session_id.clone(),
        session_name,
        transcript_path: transcript_path_for_session(&session_id),
        cwd: cwd.clone(),
        permission_mode: None,
        model: StatusLineModel {
            id: runtime_model.clone(),
            display_name: render_model_name(&runtime_model),
        },
        workspace: StatusLineWorkspace {
            current_dir: cwd,
            project_dir,
            added_dirs: added_dirs.to_vec(),
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
        output_style: StatusLineOutputStyle {
            name: output_style_name,
        },
        cost: StatusLineCost {
            total_cost_usd: get_total_cost(),
            total_duration_ms: get_total_duration(),
            total_api_duration_ms: get_total_api_duration(),
            total_lines_added: get_total_lines_added(),
            total_lines_removed: get_total_lines_removed(),
        },
        context_window: StatusLineContextWindow {
            total_input_tokens: get_total_input_tokens(),
            total_output_tokens: get_total_output_tokens(),
            context_window_size,
            current_usage,
            used_percentage,
            remaining_percentage,
        },
        exceeds_200k_tokens,
        rate_limits: None,
        vim: None,
        agent: None,
        remote: None,
        worktree: None,
    };

    // Optional blocks — same gates as CC StatusLine.tsx.
    if crate::components::prompt_input::utils::is_vim_mode_enabled() {
        input.vim = Some(StatusLineVim {
            mode: vim_mode.unwrap_or("INSERT").to_string(),
        });
    }

    if let Some(agent) = crate::utils::session_storage::get_current_session_metadata().agent_setting
    {
        if !agent.trim().is_empty() {
            input.agent = Some(StatusLineAgent {
                name: agent,
                r#type: None,
            });
        }
    }

    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref()) {
        input.remote = Some(StatusLineRemote {
            session_id: session_id.clone(),
        });
    }

    if let Some(worktree) = crate::utils::worktree::get_current_worktree_session() {
        input.worktree = Some(StatusLineWorktree {
            name: worktree.worktree_name,
            path: worktree.worktree_path,
            branch: worktree.worktree_branch,
            original_cwd: worktree.original_cwd,
            original_branch: worktree.original_branch,
        });
    }

    // Keep permission mode available for scripts that already read it; official
    // StatusLine omits it when `createBaseHookInput()` is called with no args.
    // Callers that want the live mode can pass it via a dedicated helper below.
    let _ = permission_mode_api_name(permission_mode);

    input
}

/// Like [`build_status_line_command_input`], but also sets top-level
/// `permission_mode` from the live AppState mode (useful when scripts expect it).
pub fn build_status_line_command_input_with_permission_mode(
    permission_mode: PermissionMode,
    exceeds_200k_tokens: bool,
    settings: &SettingsJson,
    messages: &[Message],
    added_dirs: &[String],
    main_loop_model: &str,
    vim_mode: Option<&str>,
) -> StatusLineCommandInput {
    let mut input = build_status_line_command_input(
        permission_mode,
        exceeds_200k_tokens,
        settings,
        messages,
        added_dirs,
        main_loop_model,
        vim_mode,
    );
    input.permission_mode = Some(permission_mode_api_name(permission_mode));
    input
}

/// Maps to: CC `StatusLineInner#doUpdate` body (without React debounce/refs).
pub async fn run_status_line_update(
    permission_mode: PermissionMode,
    settings: &SettingsJson,
    messages: &[Message],
    added_dirs: &[String],
    main_loop_model: &str,
    vim_mode: Option<&str>,
    workspace_trusted: bool,
    policy_settings: Option<&SettingsJson>,
    log_result: bool,
) -> Option<String> {
    let exceeds_200k_tokens = does_most_recent_assistant_message_exceed_200k(messages);
    let status_input = build_status_line_command_input(
        permission_mode,
        exceeds_200k_tokens,
        settings,
        messages,
        added_dirs,
        main_loop_model,
        vim_mode,
    );
    crate::services::hooks::statusline::execute_status_line_command(
        &status_input,
        workspace_trusted,
        policy_settings,
        log_result,
    )
    .await
}

#[derive(Default, Props)]
pub struct StatusLineProps {
    /// Maps to: CC `AppState.statusLineText` read (StatusLine.tsx:195),
    /// threaded through the footer mount as a plain text prop.
    pub text: Option<String>,
    /// Maps to: CC `Props.messagesRef` (StatusLine.tsx:176). The source keeps
    /// messages behind a ref so they are read only inside the debounced
    /// callback and never re-render the component; an `Arc` snapshot has the
    /// same effect here — cloning it is a refcount bump, and the component does
    /// not key any render on its contents.
    pub messages: Option<std::sync::Arc<Vec<crate::types::message::Message>>>,
    /// Maps to: CC `Props.lastAssistantMessageId` (`:177`) — "the actual
    /// re-render trigger", per the source's own comment.
    pub last_assistant_message_id: Option<String>,
    /// Maps to: CC `Props.vimMode` (`:178`).
    pub vim_mode: Option<String>,
}

/// Maps to: CC `StatusLine.tsx:291-304` `scheduleUpdate` debounce window.
const STATUS_LINE_DEBOUNCE_MS: u64 = 300;

/// Inputs for [`use_status_line_update`], bundled so the hook keeps one
/// parameter per concern rather than six positional ones.
struct StatusLineUpdateInputs {
    store: Option<crate::state::store::AppStore>,
    messages: Option<std::sync::Arc<Vec<Message>>>,
    last_assistant_message_id: Option<String>,
    vim_mode: Option<String>,
}

/// What CC's `previousStateRef` (`StatusLine.tsx:307-318`) holds.
#[derive(Clone, Default, PartialEq)]
struct StatusLineTriggerState {
    message_id: Option<String>,
    permission_mode: Option<PermissionMode>,
    vim_mode: Option<String>,
    main_loop_model: Option<String>,
}

/// Maps to: CC `StatusLine.tsx:306-326` — the effect that schedules
/// `doUpdate` when any of the four trigger values changes, plus the 300ms
/// debounce at `:291-304` and the `doUpdate` body at `:240-288`.
///
/// Cometix previously ran `run_status_line_update` exactly once, from a
/// `tokio::spawn` in `main.rs` over a launch-time snapshot, so the status line
/// was computed per process and never refreshed — switching model, permission
/// mode or vim mode, and every new assistant message, left it stale. Owning
/// the loop here restores the source's behaviour and, as a side effect, moves
/// the write after Provider adoption (P5 G10).
///
/// Deviations from the source, both forced by iocraft and both bounded:
/// - React clears the pending timer on every reschedule (`:292-294`). iocraft
///   has no `clearTimeout`, so the future instead re-reads the trigger state
///   after sleeping and abandons the run if it changed — the same
///   "only the last edit within the window wins" outcome, reached by checking
///   at the end rather than cancelling at the start.
/// - CC keeps messages behind a ref so reading them cannot re-render; the
///   `Arc` snapshot here has the same property.
fn use_status_line_update(hooks: &mut Hooks, inputs: StatusLineUpdateInputs) {
    let StatusLineUpdateInputs {
        store,
        messages,
        last_assistant_message_id,
        vim_mode,
    } = inputs;

    let permission_mode =
        crate::state::app_state::use_app_state(hooks, |state| state.tool_permission_context.mode);
    let main_loop_model = crate::state::app_state::use_app_state(hooks, |state| {
        crate::hooks::use_main_loop_model::use_main_loop_model(
            state.main_loop_model.as_deref(),
            state.main_loop_model_for_session.as_deref(),
        )
    });

    // The render side only PUBLISHES the current trigger values; the long-lived
    // future below consumes them. `use_future` spawns its future exactly once
    // per mount ("After that, calling this function has no effect" —
    // iocraft `use_future.rs`), so a future created per change would never be
    // created at all after the first render. A shared slot is what lets one
    // future see every later change.
    let slot = hooks
        .use_state(|| std::sync::Arc::new(std::sync::Mutex::new(StatusLineTriggerState::default())))
        .read()
        .clone();
    let messages_slot = hooks
        .use_state(|| {
            std::sync::Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
                Vec::<Message>::new(),
            )))
        })
        .read()
        .clone();

    *slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = StatusLineTriggerState {
        message_id: last_assistant_message_id,
        // The trigger state's `Option`s mean "not published yet", which the
        // poll loop uses as its initial baseline — not "no provider".
        permission_mode: Some(permission_mode),
        vim_mode,
        main_loop_model: Some(main_loop_model),
    };
    if let Some(messages) = messages {
        *messages_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = messages;
    }

    let Some(store) = store else {
        // No provider: nothing to write into. The hook count is unchanged
        // because `use_future` below still runs — see the note there.
        hooks.use_future(async {});
        return;
    };

    hooks.use_future(async move {
        // CC's `previousStateRef` (`:307-318`), owned by the loop rather than
        // by a render, since this future outlives every individual render.
        let mut previous = StatusLineTriggerState::default();
        loop {
            // Deadline poll standing in for CC's resettable `setTimeout`
            // (`:291-304`) — the established L1 for this shape (PORTING.md).
            // Sleeping first is also the debounce: a burst of changes inside
            // one window collapses into the single read below.
            futures_timer::Delay::new(std::time::Duration::from_millis(STATUS_LINE_DEBOUNCE_MS))
                .await;

            let current = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            // CC `:307-312` — all four equal means do nothing.
            if current == previous {
                continue;
            }
            // CC `:313-317` advances permissionMode / vimMode / mainLoopModel
            // but DELIBERATELY not messageId, so the next `doUpdate` recomputes
            // `exceeds200kTokens` against the latest messages.
            previous = StatusLineTriggerState {
                message_id: previous.message_id.clone(),
                ..current.clone()
            };

            let snapshot = store.get();
            let settings = (*snapshot.settings).clone();
            let added_dirs: Vec<String> = snapshot
                .tool_permission_context
                .additional_working_directories
                .keys()
                .cloned()
                .collect();
            let messages = messages_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();

            let text = run_status_line_update(
                snapshot.tool_permission_context.mode,
                &settings,
                &messages,
                &added_dirs,
                current.main_loop_model.as_deref().unwrap_or_default(),
                current.vim_mode.as_deref(),
                // Same source `main.rs` uses for this argument
                // (`check_has_trust_dialog_accepted()`). Reading
                // `get_current_project_config().has_trust_dialog_accepted`
                // instead is not equivalent — it misses the other ways trust is
                // established, and a false here silently suppresses the
                // command, which is indistinguishable from "status line broken".
                crate::utils::config::check_has_trust_dialog_accepted(),
                None,
                true,
            )
            .await;

            // Maps to: CC `:280-283` — explicit compare-and-skip
            // (`prev.statusLineText === text ? prev : {...prev, statusLineText: text}`).
            // Assignment past the guard is unconditional, `None` included, so a
            // disabled or blocked executor clears stale text.
            store.set_state(|prev| {
                if prev.status_line_text == text {
                    return crate::state::store::UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.status_line_text = text;
                crate::state::store::UpdateDecision::Replace {
                    next: std::sync::Arc::new(next),
                    result: (),
                }
            });
        }
    });
}

/// Maps to: CC `components/StatusLine.tsx#StatusLine`.
#[component]
pub fn StatusLine(props: &StatusLineProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    // Maps to: CC StatusLine.tsx:391 paddingX — component-local read from
    // settings (`settings?.statusLine?.padding ?? 0`), not AppState.
    let padding_x = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state
            .settings
            .status_line
            .as_ref()
            .and_then(|status_line| status_line.padding)
            .unwrap_or(0)
    });

    let status_line_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    use_status_line_update(
        &mut hooks,
        StatusLineUpdateInputs {
            store: status_line_store,
            messages: props.messages.clone(),
            last_assistant_message_id: props.last_assistant_message_id.clone(),
            vim_mode: props.vim_mode.clone(),
        },
    );

    // Maps to: CC `StatusLine.tsx:393-406`, comment included —
    //
    //   StatusLine must have stable height […] a 0→1 row change when the
    //   command finishes steals a row from ScrollBox and shifts content.
    //   Reserve the row while loading.
    //
    // Cometix returned a 0-height view instead, so between "a status line is
    // configured" and "its command has produced text" the footer lost the row
    // it had already suppressed the hint for, and the whole prompt box
    // collapsed. Reserving it is what the source does.
    //
    // The source gates the placeholder on `isFullscreenEnvEnabled()` because
    // only its fullscreen shell has the `flexShrink:0` footer that the shift
    // damages. Cometix's inline main screen has the same problem for a
    // different reason — `footer_height_for` is a computed budget, so a
    // 0→1 change there is a visible reflow — so the placeholder is
    // unconditional here, and `footer_height_for` counts the row on the same
    // condition that mounts it.
    let text = props.text.as_deref().unwrap_or(" ");

    element! {
        View(
            flex_direction: FlexDirection::Row,
            width: 100pct,
            height: 1u32,
            padding_x: padding_x as u32,
            overflow: Overflow::Hidden,
        ) {
            Ansi(content: text.to_string(), color: Some(theme.inactive), wrap: TextWrap::Truncate)
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantContent, AssistantMessage, StopReason};
    use crate::utils::settings::types::StatusLineSettings;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use crate::utils::theme;
    use chrono::Utc;

    #[test]
    fn status_line_should_display_matches_official_settings_gate() {
        assert!(!status_line_should_display(&SettingsJson::default()));
        assert!(status_line_should_display(&SettingsJson {
            status_line: Some(StatusLineSettings {
                kind: Some("command".to_string()),
                command: "printf ok".to_string(),
                padding: None,
            }),
            ..Default::default()
        }));
    }

    /// Maps to: CC `PromptInputFooter.tsx:176-178` — the mount is gated on the
    /// SETTINGS, never on the text.
    ///
    /// This assertion is the inverse of what it used to be, and the change is
    /// the point: the component owns the update loop, so requiring text to
    /// mount it deadlocks (no mount, no loop, no text). The text still controls
    /// the ROW HEIGHT — see `footer_height_for`.
    #[test]
    fn status_line_mount_gate_follows_settings_not_text() {
        assert!(
            status_line_should_render(true, false),
            "configured status line must mount even before its first text, or \
             the loop that produces that text never runs"
        );
        assert!(!status_line_should_render(false, false));
        assert!(!status_line_should_render(true, true));
    }

    #[test]
    fn status_line_configured_derives_live_from_settings_matches_official() {
        // CC re-derives `statusLineShouldDisplay(settings)` per render
        // (StatusLine.tsx:59 via PromptInputFooter.tsx:136) — a settings
        // change must flip the gate immediately, with no startup snapshot.
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        assert!(!status_line_should_display(&store.get().settings));

        store.replace_with(|state| {
            state.settings = std::sync::Arc::new(SettingsJson {
                status_line: Some(StatusLineSettings {
                    kind: Some("command".to_string()),
                    command: "printf ok".to_string(),
                    padding: None,
                }),
                ..Default::default()
            });
        });
        assert!(status_line_should_display(&store.get().settings));

        store.replace_with(|state| {
            state.settings = std::sync::Arc::new(SettingsJson::default());
        });
        assert!(!status_line_should_display(&store.get().settings));
    }

    #[test]
    fn build_status_line_command_input_resolves_model_and_null_usage_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "grok-4.5");

        let settings = SettingsJson {
            model: Some("opus".to_string()),
            output_style: Some("explanatory".to_string()),
            ..Default::default()
        };
        let main_loop_model =
            crate::hooks::use_main_loop_model::use_main_loop_model(settings.model.as_deref(), None);
        let input = build_status_line_command_input(
            PermissionMode::Default,
            false,
            &settings,
            &[],
            &["/tmp/extra".to_string()],
            &main_loop_model,
            None,
        );

        assert_eq!(input.model.id, "grok-4.5");
        assert!(input.context_window.current_usage.is_none());
        assert!(input.context_window.used_percentage.is_none());
        assert!(input.context_window.remaining_percentage.is_none());
        assert_eq!(input.output_style.name, "explanatory");
        assert_eq!(input.workspace.added_dirs, vec!["/tmp/extra".to_string()]);
        assert!(!input.exceeds_200k_tokens);
        assert!(input.permission_mode.is_none());

        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
    }

    #[test]
    fn build_status_line_command_input_fills_usage_percentages_from_messages() {
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::Text("hi".to_string())],
            model: Some("claude-sonnet-4-6".to_string()),
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(crate::types::message::TokenUsage {
                input_tokens: 50_000,
                output_tokens: 10,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_deleted_input_tokens: 0,
            }),
        })];
        let input = build_status_line_command_input(
            PermissionMode::Default,
            false,
            &SettingsJson::default(),
            &messages,
            &[],
            "claude-sonnet-4-6",
            None,
        );
        assert_eq!(input.context_window.used_percentage, Some(25));
        assert_eq!(input.context_window.remaining_percentage, Some(75));
        assert!(input.context_window.current_usage.is_some());
    }

    #[test]
    fn status_line_component_renders_ansi_with_inactive_default_color() {
        let theme = *theme::current();
        // Padding comes from `AppState.settings.statusLine.padding`
        // (CC StatusLine.tsx:391), so seed a store with padding 1.
        let store = {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = std::sync::Arc::new(SettingsJson {
                status_line: Some(StatusLineSettings {
                    kind: Some("command".to_string()),
                    command: "printf ok".to_string(),
                    padding: Some(1),
                }),
                ..Default::default()
            });
            crate::state::store::AppStore::new(initial, None)
        };
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        StatusLine(text: Some("plain \u{1b}[32mgreen\u{1b}[0m".to_string()))
                    }.into_any()),
                )
            }
        }
        .render(Some(80));
        let text = canvas.to_string();

        assert!(text.contains("plain green"), "canvas=\n{text}");
        let plain_style = canvas.resolved_text_style(1, 0).expect("plain style");
        assert_eq!(plain_style.color, Some(theme.inactive));
        let green_style = canvas.resolved_text_style(7, 0).expect("green style");
        assert_eq!(green_style.color, Some(Color::DarkGreen));
    }
}
