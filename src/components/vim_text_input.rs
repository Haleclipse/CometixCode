//! Maps to: CC `components/VimTextInput.tsx`.
//!
//! Official `VimTextInput` composes `useVimInput` with `BaseTextInput`,
//! clipboard-image hint polling, terminal focus, and initial-mode syncing.
//! Cometix currently lacks the `useVimInput` state machine; this boundary
//! delegates to the normal `TextInput` path and exposes the official mode
//! synchronization helpers so the future Vim hook can attach here without
//! moving render responsibilities into prompt input.

use crate::components::base_text_input::{BaseInputState, BaseTextInput};
use crate::components::prompt_input::shimmered_input::TextHighlight;
use crate::hooks::use_text_input::{HistoryDirection, UseTextInputOptions, use_text_input};
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VimMode {
    #[default]
    Insert,
    Normal,
}

impl VimMode {
    /// Maps to: CC `types/textInputTypes.ts#VimMode` string values.
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Normal => "NORMAL",
        }
    }
}

/// Maps to: CC `VimTextInput.tsx` effect syncing `props.initialMode` into
/// `useVimInput`'s `setMode` when it differs from current mode.
pub fn vim_text_input_pending_initial_mode_sync(
    current_mode: VimMode,
    initial_mode: Option<VimMode>,
) -> Option<VimMode> {
    initial_mode.filter(|mode| *mode != current_mode)
}

#[derive(Default, Props)]
pub struct VimTextInputProps<'a> {
    /// When supplied, PromptInput owns the editing hook and VimTextInput keeps
    /// the official rendering boundary without registering a second reader.
    pub managed_input_state: Option<BaseInputState>,
    pub value: Option<State<String>>,
    pub cursor_offset: Option<State<usize>>,
    pub focus: bool,
    pub multiline: bool,
    pub columns: usize,
    pub max_visible_lines: Option<usize>,
    pub disable_cursor_movement_for_up_down_keys: bool,
    pub disable_escape_double_press: bool,
    pub show_cursor: bool,
    pub placeholder: Option<String>,
    pub highlights: Vec<TextHighlight>,
    pub argument_hint: Option<String>,
    pub dim_color: bool,
    pub voice_recording: bool,
    pub reduced_motion: bool,
    pub initial_mode: Option<VimMode>,
    pub current_mode: VimMode,
    pub on_change: Handler<String>,
    pub on_submit: HandlerMut<'a, String>,
    pub on_exit: HandlerMut<'a, ()>,
    pub on_history_up: HandlerMut<'a, ()>,
    pub on_history_down: HandlerMut<'a, ()>,
    pub on_clear_input: Handler<()>,
    pub on_history_reset: Handler<()>,
}

/// Maps to: CC `components/VimTextInput.tsx#VimTextInput`.
#[component]
pub fn VimTextInput<'a>(
    props: &mut VimTextInputProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let _pending_mode_sync =
        vim_text_input_pending_initial_mode_sync(props.current_mode, props.initial_mode);
    let terminal_focused = hooks.use_terminal_focus();
    let Some(value) = props.value else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };
    let Some(cursor_offset) = props.cursor_offset else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };

    if let Some(input_state) = props.managed_input_state.clone() {
        return element! {
            View(flex_direction: FlexDirection::Column) {
                BaseTextInput(
                    input_state: input_state,
                    value: value.to_string(),
                    placeholder: props.placeholder.clone(),
                    focus: props.focus,
                    show_cursor: props.show_cursor,
                    terminal_focus: terminal_focused,
                    cursor_offset: cursor_offset.get(),
                    argument_hint: props.argument_hint.clone(),
                    dim_color: props.dim_color,
                    hide_placeholder_text: props.voice_recording,
                    highlights: props.highlights.clone(),
                )
            }
        }
        .into_any();
    }

    let mut state = use_text_input(
        &mut hooks,
        UseTextInputOptions {
            on_change: props.on_change.clone(),
            on_clear_input: props.on_clear_input.clone(),
            on_history_reset: props.on_history_reset.clone(),
            escape_event_passthrough: false,
            select_navigation_passthrough: false,
            preceding_keybinding_contexts: Vec::new(),
            cancel_passthrough: false,
            value,
            cursor_offset,
            inline_ghost_text: None,
            focus: props.focus,
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
    if state.exit.take_should_exit() {
        (props.on_exit)(());
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            BaseTextInput(
                input_state: BaseInputState::from(&state),
                value: value.to_string(),
                placeholder: props.placeholder.clone(),
                focus: props.focus,
                show_cursor: props.show_cursor,
                terminal_focus: terminal_focused,
                cursor_offset: cursor_offset.get(),
                argument_hint: props.argument_hint.clone(),
                dim_color: props.dim_color,
                hide_placeholder_text: props.voice_recording,
                highlights: props.highlights.clone(),
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
    fn vim_mode_strings_match_official_values() {
        assert_eq!(VimMode::Insert.as_official_str(), "INSERT");
        assert_eq!(VimMode::Normal.as_official_str(), "NORMAL");
    }

    #[test]
    fn vim_text_input_initial_mode_sync_matches_official_effect_gate() {
        assert_eq!(
            vim_text_input_pending_initial_mode_sync(VimMode::Insert, Some(VimMode::Normal)),
            Some(VimMode::Normal)
        );
        assert_eq!(
            vim_text_input_pending_initial_mode_sync(VimMode::Normal, Some(VimMode::Normal)),
            None
        );
        assert_eq!(
            vim_text_input_pending_initial_mode_sync(VimMode::Insert, None),
            None
        );
    }

    #[component]
    fn VimTextInputHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(|| String::new());
        let cursor_offset = hooks.use_state(|| 0usize);
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                VimTextInput(
                    value: value,
                    cursor_offset: cursor_offset,
                    focus: true,
                    multiline: true,
                    columns: 40usize,
                    show_cursor: true,
                    placeholder: Some("Ask Claude".to_string()),
                    initial_mode: Some(VimMode::Insert),
                    current_mode: VimMode::Insert,
                )
            }
        }
    }

    #[test]
    fn vim_text_input_composes_text_input_render_path() {
        let text = element! {
            ContextProvider(value: Context::owned(crate::state::store::AppStore::new(Default::default(), None))) {
                VimTextInputHarness
            }
        }.render(Some(80)).to_string();
        assert_eq!(text, "Ask Claude\n");
    }
}
