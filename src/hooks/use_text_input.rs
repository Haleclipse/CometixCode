//! Maps to: CC `hooks/useTextInput.ts`.
//! This hook owns terminal input handling for controlled prompt text:
//! readline-style cursor movement, kill/yank, multiline submit/newline,
//! paste normalization, double Ctrl-C/D exit, and double-Esc clear.

use crate::hooks::use_double_press::{DoublePressState, use_double_press};
use crate::hooks::use_exit::{ExitState, use_exit};
use crate::keybindings::types::{ChordResolveResult, ContextName};
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::cursor::{Cursor, RenderedLine, clamp_cursor};
use crate::utils::kill_ring::{reset_kill_accumulation, reset_yank_state};
use iocraft::prelude::*;
use std::collections::HashSet;

#[derive(Clone)]
pub struct UseTextInputOptions {
    pub value: State<String>,
    /// Source callbacks retained for the input event, before ancestor dispatch.
    pub on_change: Handler<String>,
    pub on_clear_input: Handler<()>,
    pub on_history_reset: Handler<()>,
    pub cursor_offset: State<usize>,
    /// Maps to CC `useTextInput`'s `inlineGhostText` option. This is a
    /// render-only carrier; accepting it remains owned by useTypeahead.
    pub inline_ghost_text: Option<crate::hooks::use_typeahead::InlineGhostText>,
    pub focus: bool,
    pub multiline: bool,
    pub columns: usize,
    pub max_visible_lines: Option<usize>,
    pub disable_cursor_movement_for_up_down_keys: bool,
    pub disable_escape_double_press: bool,
    /// Maps to: CC event-order parity for `useCancelRequest`. Ink delivers
    /// events parents-first, so CC's CancelRequestHandler wins Escape and
    /// active-task Ctrl+C. iocraft bubbles children-first, so when set,
    /// `chat:cancel` / `app:interrupt` are left unconsumed for the ancestor
    /// cancel handler; idle Ctrl+C still uses text-level double-press exit.
    pub cancel_passthrough: bool,
    /// Native BaseTextInput event transport: retain text-level Escape processing
    /// but let the same event continue to a source parent cancel listener.
    pub escape_event_passthrough: bool,
    /// L1 Ink parent-before-input dispatch for CustomSelect navigation.
    pub select_navigation_passthrough: bool,
}

pub struct TextInputState {
    pub value: State<String>,
    pub cursor_offset: State<usize>,
    pub pending_submit: State<Option<String>>,
    pub pending_history: State<Option<HistoryDirection>>,
    pub exit: ExitState,
    pub escape: DoublePressState,
    pub rendered_lines: Vec<RenderedLine>,
    pub cursor_line: usize,
    pub cursor_column: usize,
    pub viewport_char_offset: usize,
    pub viewport_char_end: usize,
    pub inline_ghost_text: Option<crate::hooks::use_typeahead::InlineGhostText>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryDirection {
    Up,
    Down,
}

impl TextInputState {
    pub fn take_pending_submit(&mut self) -> Option<String> {
        let value = self.pending_submit.read().clone();
        if value.is_some() {
            self.pending_submit.set(None);
        }
        value
    }

    pub fn take_pending_history(&mut self) -> Option<HistoryDirection> {
        let value = self.pending_history.get();
        if value.is_some() {
            self.pending_history.set(None);
        }
        value
    }

    pub fn exit_hint(&self) -> Option<&'static str> {
        if self.exit.ctrl_c.is_pending() {
            Some("Press Ctrl-C again to exit")
        } else if self.exit.ctrl_d.is_pending() {
            Some("Press Ctrl-D again to exit")
        } else {
            None
        }
    }
}

/// Maps to: `useTextInput({...})`.
pub fn use_text_input(hooks: &mut Hooks, mut options: UseTextInputOptions) -> TextInputState {
    let mut notifications = crate::context::notifications::use_notifications(hooks);
    let exit = use_exit(hooks);
    let escape = use_double_press(hooks);
    let keybinding_runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let mut pending_submit = hooks.use_state(|| Option::<String>::None);
    let mut pending_history = hooks.use_state(|| Option::<HistoryDirection>::None);

    // Native controlled props expose byte offsets. Keep this hook's canonical
    // Cursor across their lossy projection: source UTF-16 can lie inside a
    // surrogate pair after a valid NFC insertion. External text/byte changes
    // invalidate the carrier; all editing remains in utils/Cursor's owner.
    let retained_cursor = hooks.use_ref(|| None::<(String, usize, Cursor)>);
    let _ = cursor_from_options(options.clone(), retained_cursor);
    let current_text = options.value.read().clone();

    // Maps to: CC useExitOnCtrlCD's `app:exit` handler for additional user
    // bindings while preserving useTextInput's hardcoded Ctrl-D editing rule.
    // Non-empty input keeps Ctrl-D as delete-forward; idle/empty input routes
    // both the default key and any remap through the same double-press owner.
    let app_exit_active = options.focus && current_text.is_empty();
    use_keybinding(
        hooks,
        keybinding_runtime.clone(),
        "app:exit",
        ContextName::Global,
        move || app_exit_active,
        move || {
            exit.ctrl_d.press();
            true
        },
    );

    // CC useTextInput.ts:126-153 owns only clear-input double Escape.
    // Preserve the render's originalValue in the event callback, as Ink does.
    // Do not classify a pending double press using a later render's text:
    // following keys in the same input batch may already have changed it.
    // PromptInput owns an independent counter for empty-input rewind.
    hooks.use_propagated_terminal_events({
        let mut options = options.clone();
        move |event| {
            if !options.focus {
                return;
            }

            match event.event() {
                TerminalEvent::Paste(text) => {
                    exit.clear();
                    reset_kill_accumulation();
                    reset_yank_state();

                    let inserted = normalize_paste(text, options.multiline);
                    if inserted.is_empty() {
                        return;
                    }
                    let cursor = cursor_from_options(options.clone(), retained_cursor).insert(&inserted);
                    set_from_cursor(options.clone(), retained_cursor, cursor);
                    event.stop_propagation();
                }
                TerminalEvent::Key(
                    key_event @ KeyEvent {
                        code,
                        kind,
                        modifiers,
                        ..
                    },
                ) if *kind != KeyEventKind::Release => {
                    if options.select_navigation_passthrough
                        && (matches!(code, KeyCode::Up | KeyCode::Down)
                            || modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(code, KeyCode::Char('p' | 'n')))
                    {
                        return;
                    }
                    // Configurable actions mounted above TextInput must win
                    // over readline editing keys (notably dynamic command:*).
                    // Maps to: CC REPL handler mount order, where
                    // CommandKeybindingHandlers precedes PromptInput.
                    if let Some(runtime) = keybinding_runtime.as_ref().filter(|_| !matches!(code, KeyCode::Esc) || !options.escape_event_passthrough) {
                        if runtime.chord_pending() {
                            event.stop_propagation();
                            return;
                        }
                        if let Some(keystroke) =
                            crate::keybindings::matcher::key_event_to_keystroke(key_event)
                        {
                            let mut contexts: HashSet<ContextName> = runtime.active_contexts();
                            contexts.insert(ContextName::Chat);
                            contexts.insert(ContextName::Global);
                            match crate::keybindings::resolver::resolve_key_with_chord_state(
                                Some(&keystroke),
                                keystroke.key == "escape",
                                &contexts,
                                runtime.bindings().as_slice(),
                                None,
                            ) {
                                ChordResolveResult::Match { action }
                                    if runtime.has_registered_handler(&action, &contexts) =>
                                {
                                    return;
                                }
                                ChordResolveResult::Unbound => {
                                    event.stop_propagation();
                                    return;
                                }
                                _ => {}
                            }
                        }
                    }

                    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
                    let alt = modifiers.contains(KeyModifiers::ALT);
                    let shift = modifiers.contains(KeyModifiers::SHIFT);
                    let is_plain_ctrl = *modifiers == KeyModifiers::CONTROL;

                    // Ctrl-C — active tasks first resolve current CC's
                    // `app:interrupt`; when idle, useTextInput retains the
                    // text-level clear/double-press exit behavior.
                    if is_plain_ctrl && matches!(code, KeyCode::Char('c') | KeyCode::Char('C')) {
                        if options.cancel_passthrough {
                            return;
                        }
                        reset_kill_accumulation();
                        reset_yank_state();
                        if !options.value.read().is_empty() {
                            options.value.set(String::new());
                            (options.on_change)(String::new());
                            options.cursor_offset.set(0);
                        }
                        exit.ctrl_c.press();
                        event.stop_propagation();
                        return;
                    }

                    // Ctrl-D — maps to useTextInput handleCtrlD: empty input
                    // uses double-press exit; non-empty deletes forward below.
                    if is_plain_ctrl
                        && matches!(code, KeyCode::Char('d') | KeyCode::Char('D'))
                        && options.value.read().is_empty()
                    {
                        reset_kill_accumulation();
                        reset_yank_state();
                        exit.ctrl_d.press();
                        event.stop_propagation();
                        return;
                    }

                    let cursor = cursor_from_options(options.clone(), retained_cursor);
                    let mut next = None;
                    let mut submit = None;
                    let mut history = None;

                    update_ring_state_for_key(code, modifiers);

                    match code {
                        KeyCode::Esc => {
                            if options.cancel_passthrough {
                                // Leave the event for the ancestor cancel
                                // handler; do not count a double-press.
                                exit.clear();
                                return;
                            }
                            exit.clear();
                            if !options.disable_escape_double_press {
                                escape.press();
                                if escape.take_triggered() {
                                    // CC :138-150: all source effects run in this
                                    // event before a later ancestor can cancel.
                                    notifications.remove_notification("escape-again-to-clear");
                                    (options.on_clear_input)(());
                                    if !current_text.is_empty() {
                                        if !current_text.trim_matches(|c| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')).is_empty() {
                                            crate::utils::prompt_history::add_to_history(&current_text);
                                        }
                                        options.value.set(String::new());
                                        (options.on_change)(String::new());
                                        options.cursor_offset.set(0);
                                        (options.on_history_reset)(());
                                    }
                                } else if !current_text.is_empty() {
                                    notifications.add_notification(crate::context::notifications::Notification::text(
                                        "escape-again-to-clear", "Esc again to clear", crate::context::notifications::NotificationPriority::Immediate,
                                    ).with_timeout_ms(1000));
                                }
                            }
                            if !options.escape_event_passthrough {
                                event.stop_propagation();
                            }
                            return;
                        }
                        KeyCode::Enter => {
                            exit.clear();
                            if options.multiline
                                && options.cursor_offset.get() > 0
                                && options
                                    .value
                                    .read()
                                    .as_bytes()
                                    .get(options.cursor_offset.get().saturating_sub(1))
                                    == Some(&b'\\')
                            {
                                next = Some(cursor.backspace().insert("\n"));
                            } else if modified_enter_inserts_newline(modifiers) {
                                next = Some(cursor.insert("\n"));
                            } else {
                                submit = Some(options.value.read().clone());
                            }
                        }
                        KeyCode::Backspace => {
                            exit.clear();
                            next = Some(if ctrl || alt {
                                cursor.kill_word_before()
                            } else {
                                cursor
                                    .delete_token_before()
                                    .unwrap_or_else(|| cursor.backspace())
                            });
                        }
                        KeyCode::Delete => {
                            exit.clear();
                            next = Some(if alt {
                                cursor.kill_to_line_end()
                            } else {
                                cursor.del()
                            });
                        }
                        KeyCode::Home => {
                            exit.clear();
                            next = Some(cursor.start_of_line());
                        }
                        KeyCode::End => {
                            exit.clear();
                            next = Some(cursor.end_of_line());
                        }
                        KeyCode::PageUp => {
                            exit.clear();
                            next = Some(cursor.start_of_line());
                        }
                        KeyCode::PageDown => {
                            exit.clear();
                            next = Some(cursor.end_of_line());
                        }
                        KeyCode::Left if ctrl || alt => {
                            exit.clear();
                            next = Some(cursor.prev_word());
                        }
                        KeyCode::Right if ctrl || alt => {
                            exit.clear();
                            next = Some(cursor.next_word());
                        }
                        KeyCode::Left => {
                            exit.clear();
                            next = Some(cursor.left());
                        }
                        KeyCode::Right => {
                            exit.clear();
                            next = Some(cursor.right());
                        }
                        KeyCode::Up if !shift => {
                            exit.clear();
                            if options.disable_cursor_movement_for_up_down_keys {
                                history = Some(HistoryDirection::Up);
                            } else {
                                let moved = cursor.up();
                                if moved.offset() == cursor.offset() && options.multiline {
                                    let logical = cursor.up_logical_line();
                                    if logical.offset() == cursor.offset() {
                                        history = Some(HistoryDirection::Up);
                                    } else {
                                        next = Some(logical);
                                    }
                                } else if moved.offset() == cursor.offset() {
                                    history = Some(HistoryDirection::Up);
                                } else {
                                    next = Some(moved);
                                }
                            }
                        }
                        KeyCode::Down if !shift => {
                            exit.clear();
                            if options.disable_cursor_movement_for_up_down_keys {
                                history = Some(HistoryDirection::Down);
                            } else {
                                let moved = cursor.down();
                                if moved.offset() == cursor.offset() && options.multiline {
                                    let logical = cursor.down_logical_line();
                                    if logical.offset() == cursor.offset() {
                                        history = Some(HistoryDirection::Down);
                                    } else {
                                        next = Some(logical);
                                    }
                                } else if moved.offset() == cursor.offset() {
                                    history = Some(HistoryDirection::Down);
                                } else {
                                    next = Some(moved);
                                }
                            }
                        }
                        KeyCode::Tab => {
                            exit.clear();
                            // Tab is owned by autocomplete/typeahead. If it reaches
                            // here, keep parity with CC: no text edit.
                        }
                        KeyCode::Char(c)
                            if ctrl_j_inserts_newline(*c, modifiers, options.multiline) =>
                        {
                            exit.clear();
                            next = Some(cursor.insert("\n"));
                        }
                        KeyCode::Char(c) if ctrl && matches!(c.to_ascii_lowercase(), 'n' | 'p') => {
                            exit.clear();
                            let is_down = c.to_ascii_lowercase() == 'n';
                            if options.disable_cursor_movement_for_up_down_keys {
                                history = Some(if is_down {
                                    HistoryDirection::Down
                                } else {
                                    HistoryDirection::Up
                                });
                            } else {
                                let moved = if is_down { cursor.down() } else { cursor.up() };
                                if moved.offset() == cursor.offset() && options.multiline {
                                    let logical = if is_down {
                                        cursor.down_logical_line()
                                    } else {
                                        cursor.up_logical_line()
                                    };
                                    if logical.offset() == cursor.offset() {
                                        history = Some(if is_down {
                                            HistoryDirection::Down
                                        } else {
                                            HistoryDirection::Up
                                        });
                                    } else {
                                        next = Some(logical);
                                    }
                                } else if moved.offset() == cursor.offset() {
                                    history = Some(if is_down {
                                        HistoryDirection::Down
                                    } else {
                                        HistoryDirection::Up
                                    });
                                } else {
                                    next = Some(moved);
                                }
                            }
                        }
                        KeyCode::Char(c) if ctrl => {
                            exit.clear();
                            next = ctrl_action(cursor, *c, options.clone());
                        }
                        KeyCode::Char(c) if alt => {
                            exit.clear();
                            next = meta_action(cursor, *c);
                        }
                        KeyCode::Char(c) => {
                            exit.clear();
                            next = Some(cursor.insert(&c.to_string()));
                        }
                        _ => {
                            exit.clear();
                            reset_kill_accumulation();
                            reset_yank_state();
                        }
                    }

                    if let Some(text) = submit {
                        pending_submit.set(Some(text));
                        event.stop_propagation();
                        return;
                    }
                    if let Some(direction) = history {
                        pending_history.set(Some(direction));
                        event.stop_propagation();
                        return;
                    }
                    if let Some(cursor) = next {
                        set_from_cursor(options.clone(), retained_cursor, cursor);
                        // CC useTextInput.ts:477-485 edits without consuming
                        // the event. Later listeners (including the prompt's
                        // SessionBackgroundHint) must still receive Ctrl+B.
                    }
                }
                _ => {}
            }
        }
    });

    let cursor = cursor_from_options(options.clone(), retained_cursor);
    let pos = cursor.get_position();
    let viewport_start_line = cursor.get_viewport_start_line(options.max_visible_lines);
    let rendered_lines = cursor.render_lines(options.max_visible_lines);
    let viewport_char_offset = cursor.get_viewport_char_offset(options.max_visible_lines);
    let viewport_char_end = cursor.get_viewport_char_end(options.max_visible_lines);

    TextInputState {
        value: options.value,
        cursor_offset: options.cursor_offset,
        pending_submit,
        pending_history,
        exit,
        escape,
        rendered_lines,
        cursor_line: pos.line.saturating_sub(viewport_start_line),
        cursor_column: pos.column,
        viewport_char_offset,
        viewport_char_end,
        inline_ghost_text: options.inline_ghost_text,
    }
}

fn cursor_from_options(
    mut options: UseTextInputOptions,
    mut retained: Ref<Option<(String, usize, Cursor)>>,
) -> Cursor {
    let text = options.value.read().clone();
    let byte_offset = options.cursor_offset.get();
    let previous = retained.read().clone();
    let cursor = if let Some((_, _, cursor)) =
        previous.filter(|(external_text, external_offset, _)| {
            external_text == &text && *external_offset == byte_offset
        }) {
        Cursor::from_text_utf16(text.clone(), options.columns, cursor.offset_utf16())
    } else {
        // Preserve the existing external-byte API's clamp. It cannot represent
        // a surrogate-internal source position; internally produced positions
        // take the canonical branch above instead of being clamped again.
        let clamped = clamp_cursor(&text, byte_offset);
        if clamped != byte_offset {
            options.cursor_offset.set(clamped);
        }
        Cursor::from_text(text.clone(), options.columns, clamped)
    };
    *retained.write() = Some((text, options.cursor_offset.get(), cursor.clone()));
    cursor
}

fn set_from_cursor(
    mut options: UseTextInputOptions,
    mut retained: Ref<Option<(String, usize, Cursor)>>,
    cursor: Cursor,
) {
    // Cursor first: `on_change` observers report the edit's full state (text
    // and caret) to their owner, and must not see the previous caret.
    options.cursor_offset.set(cursor.offset());
    if options.value.read().as_str() != cursor.text() {
        options.value.set(cursor.text().to_string());
        (options.on_change)(cursor.text().to_string());
    }
    *retained.write() = Some((cursor.text().to_string(), cursor.offset(), cursor));
}

fn modified_enter_inserts_newline(modifiers: &KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
}

fn ctrl_j_inserts_newline(c: char, modifiers: &KeyModifiers, multiline: bool) -> bool {
    multiline
        && c.to_ascii_lowercase() == 'j'
        && modifiers.contains(KeyModifiers::CONTROL)
        && !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
}

fn ctrl_action(cursor: Cursor, c: char, mut options: UseTextInputOptions) -> Option<Cursor> {
    match c.to_ascii_lowercase() {
        'a' => Some(cursor.start_of_line()),
        'b' => Some(cursor.left()),
        'd' => {
            if cursor.text().is_empty() {
                options.cursor_offset.set(0);
                None
            } else {
                Some(cursor.del())
            }
        }
        'e' => Some(cursor.end_of_line()),
        'f' => Some(cursor.right()),
        'h' => Some(
            cursor
                .delete_token_before()
                .unwrap_or_else(|| cursor.backspace()),
        ),
        'k' => Some(cursor.kill_to_line_end()),
        'n' => {
            // Handled by Up/Down paths in terminals that expose arrows; keep
            // readline parity when Ctrl-N arrives as a char event.
            Some(cursor.down())
        }
        'p' => Some(cursor.up()),
        'u' => Some(cursor.kill_to_line_start()),
        'w' => Some(cursor.kill_word_before()),
        'y' => Some(cursor.yank()),
        _ => None,
    }
}

fn meta_action(cursor: Cursor, c: char) -> Option<Cursor> {
    match c.to_ascii_lowercase() {
        'b' => Some(cursor.prev_word()),
        'd' => Some(cursor.delete_word_after()),
        'f' => Some(cursor.next_word()),
        'y' => Some(cursor.yank_pop()),
        _ => None,
    }
}

fn normalize_paste(text: &str, multiline: bool) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        normalized
    } else {
        normalized.replace('\n', " ")
    }
}

fn update_ring_state_for_key(code: &KeyCode, modifiers: &KeyModifiers) {
    if !is_kill_key(code, modifiers) {
        reset_kill_accumulation();
    }
    if !is_yank_key(code, modifiers) {
        reset_yank_state();
    }
}

fn is_kill_key(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    match code {
        KeyCode::Char(c) if ctrl && matches!(c.to_ascii_lowercase(), 'k' | 'u' | 'w') => true,
        KeyCode::Backspace | KeyCode::Delete if alt => true,
        _ => false,
    }
}

fn is_yank_key(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    let ctrl_or_alt = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    matches!(code, KeyCode::Char(c) if ctrl_or_alt && c.to_ascii_lowercase() == 'y')
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    fn input_test_store() -> crate::state::store::AppStore {
        crate::utils::process_runtime::initialize_test_process_runtime();
        crate::state::store::AppStore::new(crate::state::app_state_store::AppState::default(), None)
    }

    #[derive(Default, Props)]
    struct EscapeEditingProbeProps {
        disabled: bool,
        escape_event_passthrough: bool,
    }

    #[component]
    fn EscapeEditingProbe(
        props: &EscapeEditingProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(|| "x".to_string());
        let cursor_offset = hooks.use_state(|| 1usize);
        let mut ready = hooks.use_state(|| false);
        hooks.use_propagated_terminal_events(move |event| {
            if matches!(event.event(), TerminalEvent::Key(key) if key.code == KeyCode::F(24)) {
                ready.set(true);
            }
        });
        let mut cleared = hooks.use_state(Vec::<String>::new);
        let original_value = value.read().clone();
        let _input = use_text_input(
            &mut hooks,
            UseTextInputOptions {
                on_change: Handler::default(),
                on_clear_input: Handler::from(move |_| {
                    let mut cleared = cleared;
                    let mut values = cleared.read().clone();
                    values.push(original_value.clone());
                    cleared.set(values);
                }),
                on_history_reset: Handler::default(),
                value,
                cursor_offset,
                inline_ghost_text: None,
                focus: true,
                multiline: true,
                columns: 80,
                max_visible_lines: None,
                disable_cursor_movement_for_up_down_keys: false,
                disable_escape_double_press: props.disabled,
                cancel_passthrough: false,
                escape_event_passthrough: props.escape_event_passthrough,
                select_navigation_passthrough: false,
            },
        );
        element! { Text(content: format!("ready={} value={:?} cleared={:?}", ready.get(), value.read().as_str(), cleared.read().as_slice())) }
    }

    #[test]
    fn escape_editing_matches_official_event_time_and_independent_timer() {
        // CC useTextInput.ts:126-153,320-330: clear in the callback, with
        // originalValue from this render. Other keys do not reset its timer.
        let press = |code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code));
        let esc = || press(KeyCode::Esc);
        for escape_event_passthrough in [false, true] {
            for (disabled, mut events, expected) in [
                (
                    false,
                    vec![esc(), press(KeyCode::Left), esc()],
                    "value=\"\" cleared=[\"x\"]",
                ),
                (
                    false,
                    vec![esc(), esc(), press(KeyCode::Backspace)],
                    "value=\"\" cleared=[\"x\"]",
                ),
                (
                    false,
                    vec![esc(), esc(), esc(), esc()],
                    "value=\"\" cleared=[\"x\", \"x\"]",
                ),
                (
                    false,
                    vec![
                        esc(),
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Esc)),
                    ],
                    "value=\"x\" cleared=[]",
                ),
                (true, vec![esc(), esc()], "value=\"x\" cleared=[]"),
            ] {
                events.push(press(KeyCode::F(24)));
                let text = futures::executor::block_on(async {
                    let mut app = element! {
                        ContextProvider(value: Context::owned(input_test_store())) {
                            ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                                FocusScope(handle_keys: false) { EscapeEditingProbe(disabled, escape_event_passthrough) }
                            }
                        }
                    };
                    let events = stream::iter(events).chain(stream::pending());
                    let mut frames = Box::pin(app.mock_terminal_render_loop(
                        MockTerminalConfig::with_events(events).with_size(90, 3),
                    ));
                    let mut last = String::new();
                    for _ in 0..10 {
                        let Some(frame) = crate::utils::race(frames.next(), async {
                            futures_timer::Delay::new(Duration::from_millis(200)).await;
                            None
                        })
                        .await
                        else {
                            break;
                        };
                        last = frame.to_string();
                        if last.contains("ready=true") {
                            break;
                        }
                    }
                    last
                });
                assert!(
                    text.contains(expected),
                    "passthrough={escape_event_passthrough}; expected {expected}; frame={text}"
                );
            }
        }
    }

    #[component]
    fn IdleExitActionChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(String::new);
        let cursor_offset = hooks.use_state(|| 0usize);
        let input = use_text_input(
            &mut hooks,
            UseTextInputOptions {
                on_change: Handler::default(),
                on_clear_input: Handler::default(),
                on_history_reset: Handler::default(),
                value,
                cursor_offset,
                inline_ghost_text: None,
                focus: true,
                multiline: true,
                columns: 80,
                max_visible_lines: None,
                disable_cursor_movement_for_up_down_keys: false,
                disable_escape_double_press: false,
                cancel_passthrough: false,
                escape_event_passthrough: false,
                select_navigation_passthrough: false,
            },
        );
        element! {
            Text(content: if input.exit.should_exit() {
                "exited".to_string()
            } else {
                input.exit_hint().unwrap_or("waiting").to_string()
            })
        }
    }

    #[component]
    fn IdleExitActionHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = hooks.use_const(|| {
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            bindings.push(crate::keybindings::types::ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("f6"),
                action: Some("app:exit".to_string()),
                context: ContextName::Global,
            });
            crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings)
        });
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks, runtime,
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                IdleExitActionChild
            }
        }
    }

    #[test]
    fn modified_enter_newline_matches_prompt_multiline_shortcuts() {
        assert!(modified_enter_inserts_newline(&KeyModifiers::SHIFT));
        assert!(modified_enter_inserts_newline(&KeyModifiers::ALT));
        assert!(!modified_enter_inserts_newline(&KeyModifiers::CONTROL));
        assert!(!modified_enter_inserts_newline(&KeyModifiers::empty()));
    }

    #[test]
    fn ctrl_j_newline_is_multiline_only() {
        assert!(ctrl_j_inserts_newline('j', &KeyModifiers::CONTROL, true));
        assert!(ctrl_j_inserts_newline('J', &KeyModifiers::CONTROL, true));
        assert!(!ctrl_j_inserts_newline('j', &KeyModifiers::CONTROL, false));
        assert!(!ctrl_j_inserts_newline('j', &KeyModifiers::SHIFT, true));
    }

    #[test]
    fn idle_text_input_additional_app_exit_binding_uses_double_press_owner() {
        let text = futures::executor::block_on(async move {
            let events = vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::F(6))),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::F(6))),
            ];
            let paced = stream::unfold(events.into_iter(), |mut events| async move {
                let event = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(25)).await;
                Some((event, events))
            });
            let mut app = element! { ContextProvider(value: Context::owned(input_test_store())) { FocusScope(handle_keys: false) { IdleExitActionHarness } } };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(paced).with_size(40, 5),
            ));
            let mut last = String::new();
            for _ in 0..24 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(120)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("exited") {
                    break;
                }
            }
            last
        });

        assert!(text.contains("exited"), "canvas=\n{text}");
    }
}
