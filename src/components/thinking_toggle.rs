//! Maps to: CC `components/ThinkingToggle.tsx`.
//!
//! The toggle owns its keys: its Select picks a mode, a mid-conversation
//! change first asks for confirmation (`confirmationPending`), Confirmation
//! context's confirm:yes / confirm:no answer it or go back, and Ctrl+C/D run
//! its exit hook. PromptInput only opens it and handles the result.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::keyboard_shortcut_hint::{
    KeyboardShortcutHint, KeyboardShortcutHintStyleContext,
};
use crate::components::design_system::pane::Pane;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Maps to: CC `components/ThinkingToggle.tsx:30-41` `options`.
///
/// Official `dimDescription` defaults to dimmed (`dimDescription !== false`);
/// omit explicit `false` so Compact two-column descriptions stay dim.
pub fn thinking_toggle_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            value: "true".to_string(),
            label: "Enabled".to_string(),
            description: Some("Claude will think before responding".to_string()),
            dim_description: true,
            ..SelectOptionData::default()
        },
        SelectOptionData {
            value: "false".to_string(),
            label: "Disabled".to_string(),
            description: Some("Claude will respond without extended thinking".to_string()),
            dim_description: true,
            ..SelectOptionData::default()
        },
    ]
}

/// Maps to: CC `components/ThinkingToggle.tsx:67-74#handleSelectChange` branch.
fn thinking_toggle_requires_confirmation(
    current_value: bool,
    selected: bool,
    is_mid_conversation: bool,
) -> bool {
    is_mid_conversation && selected != current_value
}

#[derive(Default, Props)]
pub struct ThinkingToggleProps<'a> {
    /// CC `currentValue`.
    pub current_value: bool,
    /// CC `onSelect(enabled)`.
    pub on_select: HandlerMut<'a, bool>,
    /// CC `onCancel` (optional in CC; PromptInput always passes one).
    pub on_cancel: HandlerMut<'a, ()>,
    /// CC `isMidConversation`.
    pub is_mid_conversation: bool,
}

/// What a keybinding or the Select decided, for the next render to hand to
/// the `HandlerMut` props, which those handlers cannot hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Select(bool),
    Cancel,
}

/// The first outcome before the next render stands. CC calls onSelect and
/// onCancel as each key arrives, so a choice followed by Esc in one input
/// chunk still applies the choice; a later outcome must not overwrite it.
fn decide(mut outcome: State<Option<Outcome>>, decided: Outcome) {
    if outcome.get().is_none() {
        outcome.set(Some(decided));
    }
}

/// Maps to: CC `components/ThinkingToggle.tsx:19-135`.
#[component]
pub fn ThinkingToggle<'a>(
    props: &mut ThinkingToggleProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    // CC :25.
    let exit_state = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    // CC :26-28.
    let confirmation_pending = hooks.use_state(|| None::<bool>);
    let outcome = hooks.use_state(|| None::<Outcome>);
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // CC :44-54: Esc (or n) steps back out of the confirmation, else cancels.
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        || true,
        move || {
            let mut pending = confirmation_pending;
            if pending.get().is_some() {
                pending.set(None);
            } else {
                decide(outcome, Outcome::Cancel);
            }
            true
        },
    );
    // CC :57-65: Enter (or y) confirms, only while a confirmation is shown.
    let is_confirming = confirmation_pending.get().is_some();
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "confirm:yes",
        ContextName::Confirmation,
        move || is_confirming,
        move || {
            if let Some(selected) = confirmation_pending.get() {
                decide(outcome, Outcome::Select(selected));
            }
            true
        },
    );
    if let Some(decided) = outcome.get() {
        let mut outcome = outcome;
        outcome.set(None);
        match decided {
            Outcome::Select(enabled) => (props.on_select)(enabled),
            Outcome::Cancel => (props.on_cancel)(()),
        }
    }

    // CC :67-74 `handleSelectChange`.
    let current_value = props.current_value;
    let is_mid_conversation = props.is_mid_conversation;
    let handle_select_change = move |value: String| {
        let selected = value == "true";
        if thinking_toggle_requires_confirmation(current_value, selected, is_mid_conversation) {
            let mut pending = confirmation_pending;
            pending.set(Some(selected));
        } else {
            decide(outcome, Outcome::Select(selected));
        }
    };

    // Maps to CC footer: `<Text dimColor italic>{…Byline…}</Text>`.
    let footer_style = KeyboardShortcutHintStyleContext {
        dim: true,
        italic: true,
    };

    // CC :76-134.
    element! {
        Pane(color: Some(theme.permission)) {
            View(flex_direction: FlexDirection::Column) {
                View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                    Text(content: "Toggle thinking mode".to_string(), color: theme.remember, weight: Weight::Bold)
                    Text(content: "Enable or disable thinking for this session.".to_string(), color: theme.inactive)
                }
                #(if is_confirming {
                    element! {
                        View(flex_direction: FlexDirection::Column, margin_bottom: 1u32, row_gap: 1u32) {
                            Text(content: "Changing thinking mode mid-conversation will increase latency and may reduce quality. For best results, set this at the start of a session.".to_string(), color: theme.warning)
                            Text(content: "Do you want to proceed?".to_string(), color: theme.warning)
                        }
                    }.into_any()
                } else {
                    element! {
                        View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                            ThinkingToggleSelect(
                                current_value: current_value,
                                on_change: handle_select_change,
                                on_cancel: move |_| decide(outcome, Outcome::Cancel),
                            )
                        }
                    }.into_any()
                })
            }
            ContextProvider(value: Context::owned(footer_style)) {
                #(if exit_state.pending {
                    element! {
                        Text(
                            content: format!("Press {} again to exit", exit_state.key_name.unwrap_or("Ctrl-C")),
                            color: theme.inactive,
                            italic: true,
                        )
                    }.into_any()
                } else if is_confirming {
                    element! {
                        Byline {
                            KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                            ConfigurableShortcutHint(
                                action: "confirm:no".to_string(),
                                context: "Confirmation".to_string(),
                                fallback: "Esc".to_string(),
                                description: "cancel".to_string(),
                            )
                        }
                    }.into_any()
                } else {
                    element! {
                        Byline {
                            KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                            ConfigurableShortcutHint(
                                action: "confirm:no".to_string(),
                                context: "Confirmation".to_string(),
                                fallback: "Esc".to_string(),
                                description: "exit".to_string(),
                            )
                        }
                    }.into_any()
                })
            }
        }
    }
}

#[derive(Default, Props)]
struct ThinkingToggleSelectProps<'a> {
    current_value: bool,
    /// CC `onChange={handleSelectChange}`.
    on_change: HandlerMut<'a, String>,
    /// CC `onCancel={onCancel ?? (() => {})}`.
    on_cancel: HandlerMut<'a, ()>,
}

/// L1 (inline Select state carrier, PORTING.md): CC :97-104 renders this
/// Select only while no confirmation is pending, so stepping back out of
/// the confirmation remounts it, focused on the current value again.
#[component]
fn ThinkingToggleSelect<'a>(
    props: &mut ThinkingToggleSelectProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let options = thinking_toggle_options();
    let current = if props.current_value { "true" } else { "false" }.to_string();
    // CC :97-104: `defaultValue` and `defaultFocusValue` are the current
    // value; two visible options; compact layout by default.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(2),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: Some(current.clone()),
            focus_value: Some(current),
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            has_on_cancel: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    if let Some(value) = events.take_accepted() {
        (props.on_change)(value);
    }
    if events.take_cancelled() {
        (props.on_cancel)(());
    }
    let navigation = state.navigation.snapshot();
    element! {
        Select(
            options: options,
            focused_index: navigation.focused_index().unwrap_or(0),
            selected_value: state.committed_value(),
            visible_option_count: navigation.visible_option_count,
            visible_from_index: navigation.visible_from_index,
            layout: SelectLayout::Compact,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    type Results = Arc<Mutex<Vec<String>>>;

    #[derive(Default, Props)]
    struct ToggleHarnessProps {
        current_value: bool,
        is_mid_conversation: bool,
        results: Option<Results>,
    }

    /// The toggle under the keybinding runtime and theme it mounts with, and
    /// a line counting key presses (so each key produces a frame) and
    /// listing its callbacks.
    #[component]
    fn ToggleHarness(props: &ToggleHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            KeybindingRuntime::with_default_bindings(),
        );
        let results = props.results.clone().unwrap_or_default();
        let selected = results.clone();
        let cancelled = results.clone();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        ThinkingToggle(
                            current_value: props.current_value,
                            is_mid_conversation: props.is_mid_conversation,
                            on_select: move |enabled: bool| {
                                selected.lock().unwrap().push(format!("select {enabled}"));
                            },
                            on_cancel: move |_| cancelled.lock().unwrap().push("cancel".to_string()),
                        )
                        ResultsEcho(results: Some(results))
                    }
                }
            }
        }
    }

    #[component]
    fn ResultsEcho(props: &ToggleHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let results = props.results.clone().unwrap_or_default();
        let results = results.lock().unwrap().join(",");
        element! { Text(content: format!("keys={} results=[{results}]", keys.get())) }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(KeyEventKind::Press, code)
    }

    /// Sends one key per frame; returns the mount frame and the frame after
    /// each key.
    fn drive(current_value: bool, is_mid_conversation: bool, keys_in_order: Vec<KeyEvent>) -> Vec<String> {
        drive_batches(
            current_value,
            is_mid_conversation,
            keys_in_order.into_iter().map(|key| vec![key]).collect(),
        )
    }

    /// Sends each batch back to back, so it reaches the toggle as one input
    /// chunk; returns the mount frame and the frame after each batch.
    fn drive_batches(
        current_value: bool,
        is_mid_conversation: bool,
        batches: Vec<Vec<KeyEvent>>,
    ) -> Vec<String> {
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(ToggleHarness(
                current_value: current_value,
                is_mid_conversation: is_mid_conversation,
                results: Some(Arc::new(Mutex::new(Vec::new()))),
            ));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
            let mut frames = Vec::new();
            let mut sent = 0;
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                if !text.contains(&format!("keys={sent} ")) {
                    continue;
                }
                frames.push(text);
                let Some(batch) = batches.get(frames.len() - 1) else {
                    break;
                };
                for event in batch {
                    keys.send(TerminalEvent::Key(event.clone())).await.unwrap();
                }
                sent += batch.len();
            }
            frames
        })
    }

    fn focused_row(frame: &str) -> String {
        frame
            .lines()
            .find(|line| line.contains('❯'))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    #[test]
    fn thinking_toggle_options_match_official_copy() {
        let options = thinking_toggle_options();
        assert_eq!(options[0].value, "true");
        assert_eq!(options[0].label, "Enabled");
        assert_eq!(
            options[0].description.as_deref(),
            Some("Claude will think before responding")
        );
        assert_eq!(options[1].value, "false");
        assert_eq!(options[1].label, "Disabled");
    }

    #[test]
    fn thinking_toggle_confirmation_gate_matches_official_branch() {
        assert!(thinking_toggle_requires_confirmation(true, false, true));
        assert!(!thinking_toggle_requires_confirmation(true, true, true));
        assert!(!thinking_toggle_requires_confirmation(true, false, false));
    }

    #[test]
    fn thinking_toggle_renders_select_and_exit_footer() {
        let canvas = element!(ToggleHarness(current_value: true)).render(Some(100));
        let text = canvas.to_string();
        assert!(text.contains("Toggle thinking mode"), "canvas=\n{text}");
        // Compact default: numbered indexes + description on the same row family.
        assert!(text.contains("1. Enabled"), "canvas=\n{text}");
        assert!(text.contains("2. Disabled"), "canvas=\n{text}");
        assert!(
            text.contains("Claude will think before responding"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to exit"),
            "canvas=\n{text}"
        );
        // Footer inherits CC `<Text dimColor italic>`.
        let footer_y = text
            .lines()
            .position(|line| line.contains("Enter to confirm"))
            .unwrap();
        let enter_col = text.lines().nth(footer_y).unwrap().find("Enter to confirm").unwrap();
        let style = canvas
            .resolved_text_style(enter_col, footer_y)
            .expect("footer style");
        assert!(style.italic, "canvas=\n{text}");
        assert!(style.dim, "canvas=\n{text}");
    }

    #[test]
    fn thinking_toggle_opens_focused_on_the_current_value() {
        // CC :99 `defaultFocusValue={currentValue ? 'true' : 'false'}`.
        let frames = drive(false, false, Vec::new());
        assert!(focused_row(&frames[0]).contains("Disabled"), "{}", frames[0]);
    }

    #[test]
    fn thinking_toggle_selects_with_j_and_enter_outside_a_conversation() {
        // Select context j/↓ move; Enter selects (:67-74 without a
        // confirmation outside a conversation).
        let frames = drive(true, false, vec![key(KeyCode::Char('j')), key(KeyCode::Enter), key(KeyCode::F(12))]);
        assert!(frames[3].contains("results=[select false]"), "{}", frames[3]);
    }

    // F12 is a key nothing binds: the frame after it shows what the key
    // before it settled into, whatever the renderer's settle rounds.

    #[test]
    fn thinking_toggle_mid_conversation_asks_and_y_confirms() {
        // CC :69-70 then :57-65: a changed mode asks first; y (confirm:yes)
        // selects it.
        let frames = drive(
            true,
            true,
            vec![
                key(KeyCode::Down),
                key(KeyCode::Enter),
                key(KeyCode::F(12)),
                key(KeyCode::Char('y')),
                key(KeyCode::F(12)),
            ],
        );
        assert!(frames[3].contains("Do you want to proceed?"), "{}", frames[3]);
        assert!(frames[5].contains("results=[select false]"), "{}", frames[5]);
    }

    #[test]
    fn thinking_toggle_mid_conversation_enter_confirms() {
        // Confirmation binds Enter to confirm:yes (the footer's "Enter to
        // confirm"); the Select is unmounted, so its select:accept is not
        // there to reselect.
        let frames = drive(
            true,
            true,
            vec![
                key(KeyCode::Down),
                key(KeyCode::Enter),
                key(KeyCode::F(12)),
                key(KeyCode::Enter),
                key(KeyCode::F(12)),
            ],
        );
        assert!(frames[3].contains("Do you want to proceed?"), "{}", frames[3]);
        assert!(frames[5].contains("results=[select false]"), "{}", frames[5]);
    }

    #[test]
    fn thinking_toggle_escape_leaves_the_confirmation_then_cancels() {
        // CC :44-54: Esc first steps back to the Select — remounted, so
        // focused on the current value (Disabled) again rather than where ↑
        // left it — then cancels.
        let frames = drive(
            false,
            true,
            vec![
                key(KeyCode::Up),
                key(KeyCode::Enter),
                key(KeyCode::F(12)),
                key(KeyCode::Esc),
                key(KeyCode::F(12)),
                key(KeyCode::Esc),
                key(KeyCode::F(12)),
            ],
        );
        assert!(focused_row(&frames[1]).contains("Enabled"), "{}", frames[1]);
        assert!(frames[3].contains("Do you want to proceed?"), "{}", frames[3]);
        assert!(!frames[5].contains("Do you want to proceed?"), "{}", frames[5]);
        assert!(focused_row(&frames[5]).contains("Disabled"), "{}", frames[5]);
        assert!(frames[7].contains("results=[cancel]"), "{}", frames[7]);
    }

    #[test]
    fn thinking_toggle_keeps_a_choice_followed_by_escape_in_one_chunk() {
        // CC calls onSelect as Enter arrives, before the Esc that follows in
        // the same chunk: the choice stands.
        let frames = drive_batches(
            true,
            false,
            vec![
                vec![key(KeyCode::Down)],
                vec![key(KeyCode::Enter), key(KeyCode::Esc)],
                vec![key(KeyCode::F(12))],
            ],
        );
        assert!(frames[3].contains("results=[select false]"), "{}", frames[3]);
    }

    #[test]
    fn thinking_toggle_n_cancels_from_the_select() {
        // Confirmation context binds n to confirm:no as well as Esc.
        let frames = drive(true, false, vec![key(KeyCode::Char('n')), key(KeyCode::F(12))]);
        assert!(frames[2].contains("results=[cancel]"), "{}", frames[2]);
    }

    #[test]
    fn thinking_toggle_shows_the_ctrl_c_exit_hint() {
        // CC :25, :109-110: the toggle's own exit hook arms the hint.
        let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
        ctrl_c.modifiers = KeyModifiers::CONTROL;
        let frames = drive(true, false, vec![ctrl_c]);
        assert!(frames[1].contains("Press Ctrl-C again to exit"), "{}", frames[1]);
    }
}
