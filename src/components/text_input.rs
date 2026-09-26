//! Maps to: CC `components/TextInput.tsx`.
//!
//! Official `TextInput` is the wrapper that composes `useTextInput` with
//! `BaseTextInput`, terminal-focus/accessibility cursor gating, clipboard-image
//! hints, and optional voice-mode cursor rendering. Cometix ports that boundary
//! for the normal terminal text path. Current safe divergences:
//! - `useClipboardImageHint(...)`: not executed here; Cometix has no clipboard
//!   image polling permission in this component boundary yet.
//! - `VOICE_MODE` waveform cursor: represented only by `hide_placeholder_text`
//!   when `voice_recording` is true; animated RGB waveform cursor needs the
//!   future voice runtime and a cursor-rendering extension.

use crate::components::base_text_input::{BaseInputState, BaseTextInput};
use crate::hooks::use_text_input::{HistoryDirection, UseTextInputOptions, use_text_input};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct TextInputProps<'a> {
    pub value: Option<State<String>>,
    pub cursor_offset: Option<State<usize>>,
    /// Maps to: CC `types/textInputTypes.ts#BaseTextInputProps.focus`.
    /// Omission enables input events but does not focus the placeholder cursor.
    pub focus: Option<bool>,
    pub multiline: bool,
    pub columns: usize,
    pub max_visible_lines: Option<usize>,
    pub disable_cursor_movement_for_up_down_keys: bool,
    pub disable_escape_double_press: bool,
    /// Native transport for a source parent cancel listener: BaseTextInput's
    /// source onInput does not stop Escape before parent useKeybinding runs.
    /// This is not a CC TextInput prop and leaves other input/navigation alone.
    pub escape_event_passthrough: bool,
    pub select_navigation_passthrough: bool,
    pub show_cursor: bool,
    pub placeholder: Option<String>,
    pub argument_hint: Option<String>,
    pub dim_color: bool,
    pub voice_recording: bool,
    pub reduced_motion: bool,
    pub on_change: Handler<String>,
    pub on_submit: HandlerMut<'a, String>,
    pub on_exit: HandlerMut<'a, ()>,
    pub on_history_up: HandlerMut<'a, ()>,
    pub on_history_down: HandlerMut<'a, ()>,
    pub on_clear_input: Handler<()>,
    pub on_history_reset: Handler<()>,
}

/// Maps to: CC `TextInput.tsx` `canShowCursor` accessibility gate.
pub fn text_input_can_show_cursor(terminal_focused: bool, accessibility_enabled: bool) -> bool {
    terminal_focused && !accessibility_enabled
}

/// Maps to: CC `TextInput.tsx` reduced-motion/voice branch deciding whether
/// placeholder text is hidden while recording.
pub fn text_input_hide_placeholder_text(voice_recording: bool) -> bool {
    voice_recording
}

fn accessibility_enabled_from_env() -> bool {
    crate::utils::env_utils::is_env_truthy(
        std::env::var("CLAUDE_CODE_ACCESSIBILITY").ok().as_deref(),
    )
}

/// Maps to: CC `components/TextInput.tsx#TextInput`.
#[component]
pub fn TextInput<'a>(
    props: &mut TextInputProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let terminal_focused = hooks.use_terminal_focus();
    let accessibility_enabled = accessibility_enabled_from_env();
    let terminal_focus_for_cursor =
        text_input_can_show_cursor(terminal_focused, accessibility_enabled);
    let Some(value) = props.value else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };
    let Some(cursor_offset) = props.cursor_offset else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };

    let mut state = use_text_input(
        &mut hooks,
        UseTextInputOptions {
            on_change: props.on_change.clone(),
            on_clear_input: props.on_clear_input.clone(),
            on_history_reset: props.on_history_reset.clone(),
            cancel_passthrough: props.select_navigation_passthrough,
            escape_event_passthrough: props.escape_event_passthrough,
            select_navigation_passthrough: props.select_navigation_passthrough,
            value,
            cursor_offset,
            inline_ghost_text: None,
            // CC BaseTextInput -> useInput disables only explicit false.
            focus: props.focus.unwrap_or(true),
            multiline: props.multiline,
            columns: props.columns.max(1),
            max_visible_lines: props.max_visible_lines,
            disable_cursor_movement_for_up_down_keys: props
                .disable_cursor_movement_for_up_down_keys,
            disable_escape_double_press: props.disable_escape_double_press,
        },
    );

    if let Some(text) = state.take_pending_submit() {
        (props.on_submit)(text);
    }
    if let Some(direction) = state.take_pending_history() {
        match direction {
            HistoryDirection::Up => (props.on_history_up)(()),
            HistoryDirection::Down => (props.on_history_down)(()),
        }
    }
    if state.exit.should_exit() {
        (props.on_exit)(());
    }

    element! {
        View(flex_direction: FlexDirection::Row) {
            BaseTextInput(
                input_state: BaseInputState::from(&state),
                value: value.to_string(),
                placeholder: props.placeholder.clone(),
                // CC renderPlaceholder/useDeclaredCursor require truthy focus.
                focus: props.focus.unwrap_or(false),
                show_cursor: props.show_cursor,
                terminal_focus: terminal_focus_for_cursor,
                cursor_offset: cursor_offset.get(),
                argument_hint: props.argument_hint.clone(),
                dim_color: props.dim_color,
                hide_placeholder_text: text_input_hide_placeholder_text(props.voice_recording),
            )
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn text_input_cursor_accessibility_gate_matches_official() {
        assert!(text_input_can_show_cursor(true, false));
        assert!(!text_input_can_show_cursor(false, false));
        assert!(!text_input_can_show_cursor(true, true));
    }

    #[test]
    fn text_input_voice_recording_hides_placeholder_text() {
        assert!(text_input_hide_placeholder_text(true));
        assert!(!text_input_hide_placeholder_text(false));
    }

    #[component]
    fn TextInputHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(|| String::new());
        let cursor_offset = hooks.use_state(|| 0usize);
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TextInput(
                    value: value,
                    cursor_offset: cursor_offset,
                    focus: true,
                    multiline: true,
                    columns: 40usize,
                    show_cursor: true,
                    placeholder: Some("Ask Claude".to_string()),
                )
            }
        }
    }

    #[test]
    fn text_input_component_composes_base_text_input_placeholder() {
        let text = element! { ContextProvider(value: Context::owned(crate::state::store::AppStore::new(crate::state::app_state_store::AppState::default(), None))) { TextInputHarness } }.render(Some(80)).to_string();
        assert_eq!(text, "Ask Claude\n");
    }

    #[component]
    fn EscapePassthroughCtrlCProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(|| "nonempty".to_string());
        let cursor_offset = hooks.use_state(|| 8usize);
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                View(flex_direction: FlexDirection::Column) {
                    TextInput(value: Some(value), cursor_offset: Some(cursor_offset), focus: true,
                        columns: 40usize, escape_event_passthrough: true)
                    Text(content: format!("value={:?};offset={}", value.read().as_str(), cursor_offset.get()))
                }
            }
        }
    }

    #[test]
    fn text_input_escape_passthrough_preserves_nonempty_ctrl_c_editing() {
        // CC useTextInput.ts:108-120 handles Ctrl+C independently of the
        // BaseTextInput → parent Escape propagation restored for rule input.
        // The real runtime has no parent app:interrupt handler here, so the
        // text-level branch runs within the normal provider/focus environment.
        use futures::StreamExt;
        futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element! {
                ContextProvider(value: Context::owned(crate::state::store::AppStore::new(crate::state::app_state_store::AppState::default(), None))) {
                    ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                        FocusScope(handle_keys: false) { EscapePassthroughCtrlCProbe }
                    }
                }
            };
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(80, 5),
            ));
            let mut last = String::new();
            let drive = async {
                let mut sent = false;
                while let Some(canvas) = frames.next().await {
                    last = canvas.to_string();
                    if !sent && last.contains("value=\"nonempty\";offset=8") {
                        let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
                        key.modifiers = KeyModifiers::CONTROL;
                        keys.send(TerminalEvent::Key(key)).await.unwrap();
                        sent = true;
                    } else if sent && last.contains("value=\"\";offset=0") {
                        return true;
                    }
                }
                false
            };
            let completed = crate::utils::race(drive, async {
                futures_timer::Delay::new(std::time::Duration::from_secs(3)).await;
                false
            })
            .await;
            assert!(completed, "Escape-only transport changed Ctrl+C: {last}");
        });
    }

    #[derive(Default, Props)]
    struct EscapeSourceProbeProps {
        trace: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
        parent_cancel: bool,
    }

    #[component]
    fn EscapeSourceProbe(
        props: &EscapeSourceProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(|| "scout".to_string());
        let cursor = hooks.use_state(|| 5usize);
        let mut mounted = hooks.use_state(|| true);
        let store = hooks.use_context::<crate::state::store::AppStore>().clone();
        // Match the production footer subscription: writes alone need not
        // produce a distinct Canvas frame for this input-only fixture.
        let notification = crate::state::app_state::use_app_state(&mut hooks, |state| {
            state
                .notifications
                .current
                .as_ref()
                .map(|item| item.key.clone())
        });
        let parent_store = store.clone();
        let parent_trace = props.trace.clone();
        let parent_cancel = props.parent_cancel;
        hooks.use_propagated_terminal_events(move |event| {
            if parent_cancel && !event.is_propagation_stopped() && matches!(event.event(), TerminalEvent::Key(key) if key.code == KeyCode::Esc && key.kind == KeyEventKind::Press) {
                if let Some(trace) = &parent_trace { trace.lock().unwrap().push(format!("parent:{}", parent_store.get().notifications.current.as_ref().map(|n| n.key.as_str()).unwrap_or("none"))); }
                mounted.set(false);
                event.stop_propagation();
            }
        });
        let clear_trace = props.trace.clone();
        let change_trace = props.trace.clone();
        let reset_trace = props.trace.clone();
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("mounted={} value={:?} notification={}", mounted.get(), value.read().as_str(), notification.as_deref().unwrap_or("none")))
                #(mounted.get().then(|| element! {
                    TextInput(value: Some(value), cursor_offset: Some(cursor), focus: true, columns: 60usize,
                        escape_event_passthrough: parent_cancel,
                        on_clear_input: move |_| {
                            if let Some(trace) = &clear_trace { trace.lock().unwrap().push(format!("clear:{}:{}", value.read().as_str(), store.get().notifications.current.as_ref().map(|n| n.key.as_str()).unwrap_or("none"))); }
                        },
                        on_change: move |text: String| {
                            let saved = std::fs::read_to_string(crate::utils::prompt_history::history_file_path()).unwrap_or_default().contains("scout");
                            if let Some(trace) = &change_trace { trace.lock().unwrap().push(format!("change:{text}:history={saved}")); }
                        },
                        on_history_reset: move |_| { if let Some(trace) = &reset_trace { trace.lock().unwrap().push(format!("reset:{}", cursor.get())); } },
                    )
                }))
            }
        }
    }

    #[component]
    fn EscapeSourceHarness(
        props: &EscapeSourceProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                FocusScope(handle_keys: false) {
                    EscapeSourceProbe(trace: props.trace.clone(), parent_cancel: props.parent_cancel)
                }
            }
        }
    }

    #[test]
    fn text_input_escape_matches_official_notification_order_history_and_parent_unmount() {
        use crate::utils::env_utils::{EnvVarGuard, TEST_ENV_LOCK};
        use futures::StreamExt;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".test/shared-escape")
            .join(format!("unit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &directory);
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::utils::process_runtime::initialize_test_process_runtime();
        for parent_cancel in [false, true] {
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            futures::executor::block_on(async {
                let (keys, events) = async_channel::unbounded();
                let mut app = element! {
                    ContextProvider(value: Context::owned(store.clone())) {
                        ContextProvider(value: Context::owned(*theme::current())) {
                            EscapeSourceHarness(trace: Some(trace.clone()), parent_cancel)
                        }
                    }
                };
                let mut frames = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(events).with_size(80, 8),
                ));
                let mut last = String::new();
                let mut stage = 0;
                let drive = async {
                    while let Some(canvas) = frames.next().await {
                        last = canvas.to_string();
                        if stage == 0 && last.contains("mounted=true value=\"scout\"") {
                            keys.send(TerminalEvent::Key(KeyEvent::new(
                                KeyEventKind::Press,
                                KeyCode::Esc,
                            )))
                            .await
                            .unwrap();
                            stage = 1;
                        } else if stage == 1 && store.get().notifications.current.is_some() {
                            let notification = store.get().notifications.current.clone().unwrap();
                            assert_eq!(notification.text, "Esc again to clear");
                            assert_eq!(notification.timeout_ms, Some(1000));
                            assert_eq!(
                                notification.priority,
                                crate::context::notifications::NotificationPriority::Immediate
                            );
                            if parent_cancel {
                                if last.contains("mounted=false") {
                                    return true;
                                }
                            } else {
                                keys.send(TerminalEvent::Key(KeyEvent::new(
                                    KeyEventKind::Press,
                                    KeyCode::Esc,
                                )))
                                .await
                                .unwrap();
                                stage = 2;
                            }
                        } else if stage == 2 && trace.lock().unwrap().len() == 3 {
                            return true;
                        }
                    }
                    false
                };
                let completed = crate::utils::race(drive, async {
                    futures_timer::Delay::new(std::time::Duration::from_secs(4)).await;
                    false
                })
                .await;
                assert!(
                    completed,
                    "parent_cancel={parent_cancel} stage={stage}: {last}"
                );
                drop(frames);
                if parent_cancel {
                    assert_eq!(*trace.lock().unwrap(), vec!["parent:escape-again-to-clear"]);
                    assert!(
                        store.get().notifications.current.is_some(),
                        "input unmount must not cancel provider notification timer"
                    );
                    futures_timer::Delay::new(std::time::Duration::from_millis(1100)).await;
                    assert!(
                        store.get().notifications.current.is_none(),
                        "provider timer must survive input unmount"
                    );
                } else {
                    assert_eq!(
                        *trace.lock().unwrap(),
                        vec!["clear:scout:none", "change::history=true", "reset:0"]
                    );
                    assert!(store.get().notifications.current.is_none());
                    let entries =
                        std::fs::read_to_string(crate::utils::prompt_history::history_file_path())
                            .unwrap();
                    assert_eq!(
                        entries.lines().count(),
                        1,
                        "one canonical source history append"
                    );
                }
            });
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[component]
    fn SourceOffsetProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut value = hooks.use_state(|| "e🙂".to_string());
        let mut offset = hooks.use_state(|| 1usize);
        let mut last_key = hooks.use_state(String::new);
        hooks.use_propagated_terminal_events(move |event| {
            if let TerminalEvent::Key(key) = event.event() {
                if key.kind == KeyEventKind::Release {
                    return;
                }
                match key.code {
                    KeyCode::F(2) => {
                        value.set("history".to_string());
                        offset.set(3);
                    }
                    KeyCode::F(3) => offset.set(0),
                    KeyCode::F(4) => {
                        value.set(String::new());
                        offset.set(0);
                    }
                    _ => {}
                }
                // The source numeric position may change while its native byte
                // projection remains equal. Observe event completion separately
                // rather than requiring an unrelated text mutation for a frame.
                last_key.set(format!("{:?}", key.code));
            }
        });
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("value={:?} byte={} key={}", value.read().as_str(), offset.get(), last_key.read().as_str()))
                TextInput(value: Some(value), cursor_offset: Some(offset), focus: true, columns: 60usize)
            }
        }
    }

    #[component]
    fn SourceOffsetHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let store =
            hooks.use_const(|| crate::state::store::AppStore::new(Default::default(), None));
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                ContextProvider(value: Context::owned(store.clone())) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        FocusScope(handle_keys: false) { SourceOffsetProbe }
                    }
                }
            }
        }
    }

    #[test]
    fn text_input_retains_source_offset_after_nfc_and_reconciles_external_updates() {
        // Complete original useTextInput onInput + actual Cursor executed in
        // text-input-offset-peer-oracle.json. Lone source surrogate results are
        // projected through Rust String's documented U+FFFD boundary.
        use futures::StreamExt;
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        for (key, expected) in [
            (KeyCode::Left, "éX🙂"),
            (KeyCode::Right, "é🙂X"),
            (KeyCode::Delete, "é�X"),
            (KeyCode::Backspace, "éX�"),
            (KeyCode::F(2), "hisXtory"),
            (KeyCode::F(3), "Xé🙂"),
            (KeyCode::F(4), "X"),
        ] {
            futures::executor::block_on(async {
                let (keys, events) = async_channel::unbounded();
                let mut app = element!(SourceOffsetHarness);
                let mut frames = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(events).with_size(80, 8),
                ));
                let mut stage = 0;
                let mut last = String::new();
                let key_label = format!("key={key:?}");
                let expected_label = format!("value={expected:?}");
                let drive = async {
                    while let Some(canvas) = frames.next().await {
                        last = canvas.to_string();
                        if stage == 0 && last.contains("value=\"e🙂\" byte=1") {
                            keys.send(TerminalEvent::Key(KeyEvent::new(
                                KeyEventKind::Press,
                                KeyCode::Char('\u{0301}'),
                            )))
                            .await
                            .unwrap();
                            stage = 1;
                        } else if stage == 1 && last.contains("value=\"é🙂\" byte=2") {
                            keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, key)))
                                .await
                                .unwrap();
                            stage = 2;
                        } else if stage == 2 && last.contains(&key_label) {
                            keys.send(TerminalEvent::Paste("X".to_string()))
                                .await
                                .unwrap();
                            stage = 3;
                        } else if stage == 3 && last.contains(&expected_label) {
                            return true;
                        }
                    }
                    false
                };
                let completed = crate::utils::race(drive, async {
                    futures_timer::Delay::new(std::time::Duration::from_secs(4)).await;
                    false
                })
                .await;
                assert!(
                    completed,
                    "source offset key={key:?} stage={stage} expected={expected:?}: {last}"
                );
            });
        }
    }

    #[derive(Default, Props)]
    struct OptionalFocusProbeProps {
        focus: Option<bool>,
    }

    #[component]
    fn OptionalFocusProbe(
        props: &OptionalFocusProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(String::new);
        let cursor = hooks.use_state(|| 0usize);
        let mut received = hooks.use_state(|| 0usize);
        let mut submits = hooks.use_state(|| 0usize);
        // A transport receipt proves disabled input received the same event;
        // no assertion relies on waiting for an unchanged value by timeout.
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                received.set(received.get() + 1);
            }
        });
        element! {
            View(flex_direction: FlexDirection::Column) {
                TextInput(value: Some(value), cursor_offset: Some(cursor),
                    focus: props.focus, show_cursor: true, columns: 40usize,
                    placeholder: Some("Enter rule".to_string()),
                    on_submit: move |_| submits.set(submits.get() + 1))
                Text(content: format!("value={:?};offset={};received={};submits={}", value.read().as_str(), cursor.get(), received.get(), submits.get()))
            }
        }
    }

    #[component]
    fn OptionalFocusHarness(
        props: &OptionalFocusProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                FocusScope(handle_keys: false) { OptionalFocusProbe(focus: props.focus) }
            }
        }
    }

    #[test]
    fn text_input_optional_focus_matches_official_listener_and_placeholder() {
        // CC BaseTextInput: useInput(isActive: focus) versus renderPlaceholder.
        // Omission accepts editing; only explicit true inverts placeholder E.
        use futures::StreamExt;
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _accessibility =
            crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CODE_ACCESSIBILITY", "0");
        crate::utils::process_runtime::initialize_test_process_runtime();
        assert_eq!(TextInputProps::default().focus, None);
        for focus in [None, Some(true), Some(false)] {
            futures::executor::block_on(async {
                let (keys, events) = async_channel::unbounded();
                let mut app = element! {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        ContextProvider(value: Context::owned(crate::state::store::AppStore::new(Default::default(), None))) {
                            OptionalFocusHarness(focus: focus)
                        }
                    }
                };
                let mut frames = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(events).with_size(80, 5),
                ));
                let active = focus != Some(false);
                let expected = [
                    "value=\"\";offset=0;received=0;submits=0".to_string(),
                    format!(
                        "value={:?};offset={};received=1;submits=0",
                        if active { "x" } else { "" },
                        usize::from(active)
                    ),
                    format!(
                        "value={:?};offset={};received=2;submits={}",
                        if active { "x" } else { "" },
                        usize::from(active),
                        usize::from(active)
                    ),
                    format!(
                        "value=\"\";offset=0;received=3;submits={}",
                        usize::from(active)
                    ),
                ];
                let codes = [KeyCode::Char('x'), KeyCode::Enter, KeyCode::Backspace];
                let mut stage = 0usize;
                let mut last = String::new();
                let exercise = async {
                    while let Some(canvas) = frames.next().await {
                        last = canvas.to_string();
                        if !last.contains(&expected[stage]) {
                            continue;
                        }
                        if stage == 0 || stage == 3 || !active {
                            assert!(
                                last.starts_with("Enter rule"),
                                "focus={focus:?} stage={stage}: {last}"
                            );
                            let style = canvas.resolved_text_style(0, 0).unwrap();
                            assert_eq!(
                                style.invert,
                                focus == Some(true),
                                "focus={focus:?} stage={stage}"
                            );
                            assert_eq!(
                                style.dim,
                                focus != Some(true),
                                "focus={focus:?} stage={stage}"
                            );
                        }
                        if stage == 3 {
                            return true;
                        }
                        keys.send(TerminalEvent::Key(KeyEvent::new(
                            KeyEventKind::Press,
                            codes[stage],
                        )))
                        .await
                        .unwrap();
                        stage += 1;
                    }
                    false
                };
                let done = crate::utils::race(exercise, async {
                    futures_timer::Delay::new(std::time::Duration::from_secs(3)).await;
                    false
                })
                .await;
                assert!(done, "focus={focus:?} stage={stage} last={last}");
            });
        }
    }
}
