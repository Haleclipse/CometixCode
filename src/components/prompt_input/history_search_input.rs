//! Maps to: CC `components/PromptInput/HistorySearchInput.tsx`.

use crate::components::text_input::TextInput;
use crate::keybindings::types::ContextName;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use unicode_width::UnicodeWidthStr;

/// Maps to: CC `HistorySearchInput` props (`:6-10`). `value` carries both
/// `value` and `onChange`: the TextInput writes the query State it is given,
/// which is `useHistorySearch`'s `historyQuery` / `setHistoryQuery`.
#[derive(Default, Props)]
pub struct HistorySearchInputProps {
    pub value: Option<State<String>>,
    pub history_failed_match: bool,
}

/// Maps to: CC `HistorySearchInput` (`:12-35`).
#[component]
pub fn HistorySearchInput(
    props: &HistorySearchInputProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let internal_value = hooks.use_state(String::new);
    let mut cursor = hooks.use_state(|| 0usize);
    let value = props.value.unwrap_or(internal_value);
    // CC :26-28 `cursorOffset={value.length}` with a no-op
    // `onChangeCursorOffset`: "Force cursor to end of search input since
    // navigation should cancel search". Every key starts from the end, so
    // after the TextInput handles one — this component's listener runs after
    // its child's — the cursor goes back to the end before the next key in
    // the same batch. The TextInput's cursor is a byte offset.
    hooks.use_terminal_events(move |_| {
        let end = value.read().len();
        if cursor.get() != end {
            cursor.set(end);
        }
    });
    let end = value.read().len();
    if cursor.get() != end {
        cursor.set(end);
    }
    // CC :29 `columns={stringWidth(value) + 1}`.
    let width = UnicodeWidthStr::width(value.read().as_str()) + 1;
    element! {
        View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
            // CC :19 `<Text dimColor>` — ThemedText's dimColor is the
            // `inactive` foreground (PORTING.md dimColor contract).
            Text(
                content: if props.history_failed_match { "no matching prompt:" } else { "search prompts:" }.to_string(),
                color: theme.inactive,
                wrap: TextWrap::NoWrap,
            )
            TextInput(
                value: value,
                cursor_offset: cursor,
                columns: width,
                focus: true,
                show_cursor: true,
                multiline: false,
                dim_color: true,
                // useHistorySearch's `HistorySearch` bindings registered with
                // PromptInput, before this TextInput mounted, so in CC they
                // take Esc (accept), Ctrl+C (cancel), Enter (execute), Tab
                // and ctrl+r ahead of it (`useHistorySearch.ts:254-257`).
                preceding_keybinding_contexts: vec![ContextName::HistorySearch],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::keybinding_context::KeybindingRuntime;
    use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybindings};
    use futures::StreamExt;

    /// Stands in for PromptInput: its `HistorySearch` handlers register in
    /// its own render, before the HistorySearchInput below mounts — the
    /// order useHistorySearch and the footer have in CC.
    #[component]
    fn SearchKeysProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = hooks
            .try_use_context::<KeybindingRuntime>()
            .map(|runtime| runtime.clone());
        let query = hooks.use_state(String::new);
        let mut accepted = hooks.use_state(|| 0u32);
        let mut cancelled = hooks.use_state(|| 0u32);
        let handlers: KeybindingHandlers = vec![
            (
                "historySearch:accept".to_string(),
                Box::new(move || {
                    accepted.set(accepted.get() + 1);
                    true
                }),
            ),
            (
                "historySearch:cancel".to_string(),
                Box::new(move || {
                    cancelled.set(cancelled.get() + 1);
                    true
                }),
            ),
        ];
        use_keybindings(&mut hooks, runtime, handlers, ContextName::HistorySearch, || true);
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!(
                    "query={:?} accepted={} cancelled={}",
                    query.read().as_str(),
                    accepted.get(),
                    cancelled.get(),
                ))
                HistorySearchInput(value: Some(query))
            }
        }
    }

    #[component]
    fn SearchKeysHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            KeybindingRuntime::with_default_bindings(),
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                FocusScope(handle_keys: false) {
                    SearchKeysProbe
                }
            }
        }
    }

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[component]
    fn DimProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let query = hooks.use_state(|| "abc".to_string());
        element! { HistorySearchInput(value: Some(query)) }
    }

    #[test]
    fn history_search_input_dim_text_uses_the_inactive_foreground() {
        // CC `HistorySearchInput.tsx:19` and `BaseTextInput.tsx:144` are
        // ThemedText `dimColor`, i.e. `theme.inactive`, not SGR dim.
        let current_theme = *crate::utils::theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            ))) {
                ContextProvider(value: Context::owned(current_theme)) {
                    DimProbe
                }
            }
        }
        .render(Some(80));
        let row: String = (0..canvas.width())
            .map(|x| canvas.cell(x, 0).and_then(|cell| cell.text()).unwrap_or(" ").to_string())
            .collect();
        let label_x = row.find("search").expect("label");
        let query_x = row.find("abc").expect("query");
        for x in [label_x, query_x] {
            let style = canvas.resolved_text_style(x, 0).expect("styled cell");
            assert_eq!(style.color, Some(current_theme.inactive), "row={row:?} x={x}");
        }
    }

    #[test]
    fn history_search_bindings_take_escape_and_ctrl_c_ahead_of_the_input() {
        // CC `defaultBindings.ts:172-179`: in HistorySearch, Esc accepts and
        // Ctrl+C cancels. Those handlers registered before the input, so the
        // input never sees either key. Each is followed by a plain character
        // so every step yields a frame whichever way the key went.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let last = futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element! {
                ContextProvider(value: Context::owned(store.clone())) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        SearchKeysHarness
                    }
                }
            };
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(80, 6),
            ));
            let mut sent = false;
            let mut last = String::new();
            while let Some(canvas) = frames.next().await {
                last = canvas.to_string();
                if !sent && last.contains("query=\"\"") {
                    let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
                    ctrl_c.modifiers = KeyModifiers::CONTROL;
                    for event in [
                        key(KeyCode::Char('g')),
                        key(KeyCode::Char('i')),
                        key(KeyCode::Char('t')),
                        key(KeyCode::Esc),
                        key(KeyCode::Char('x')),
                        TerminalEvent::Key(ctrl_c),
                        key(KeyCode::Char('y')),
                    ] {
                        keys.send(event).await.unwrap();
                    }
                    sent = true;
                } else if sent && last.contains("y\" accepted=") {
                    break;
                }
            }
            last
        });
        assert!(
            last.contains("query=\"gitxy\" accepted=1 cancelled=1"),
            "canvas=\n{last}"
        );
    }
}
