//! Maps to: CC `components/CustomSelect/use-select-input.ts`.
//!
//! Two input layers, matching CC:
//! - `select:next` / `select:previous` / `select:accept` / `select:cancel`
//!   through the keybinding runtime in the Select context (inactive while
//!   the focused option is an input — CC :115 `if (!isInInput)`).
//! - Raw terminal events for number-key selection (full-width digits
//!   normalized, CC :175/:255-282), pageUp/pageDown (:233-239), and the
//!   multi-select space toggle (:241-253).
//!
//! Registers the `select` overlay while cancellable (CC :99-101
//! useRegisterOverlay) so the cancel-request handler leaves Escape alone.
//!
//! Not yet wired (their consumers don't exist): Tab input-mode toggling
//! and image selection mode (CC :182-231).
//!
//! Keybinding handlers must be Send + 'static, so accept/cancel surface as
//! take-able events the consumer maps onto its callbacks in the component
//! body (the same seam as PromptInput's take_pending_* pattern).

use super::use_select_state::SelectState;
use crate::context::overlay_context::OverlayRegistration;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use iocraft::prelude::*;

/// Maps to: CC `disableSelection?: boolean | 'numeric'`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisableSelection {
    #[default]
    No,
    Yes,
    Numeric,
}

/// Per-option facts the input layer needs (CC reads them off the options
/// array and the inputValues map).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectInputOptionMeta {
    pub value: String,
    pub disabled: bool,
    pub is_input: bool,
    /// CC: `inputValues.get(value)?.trim()` non-empty (pre-filled input).
    pub input_has_value: bool,
    pub allow_empty_submit_to_cancel: bool,
}

pub struct UseSelectInputOptions {
    pub is_disabled: bool,
    pub disable_selection: DisableSelection,
    pub is_multi_select: bool,
    /// CC: `!!state.onCancel` — gates select:cancel and overlay registration.
    pub has_on_cancel: bool,
    /// CC: `onUpFromFirstItem` provided — pressing up on the first item
    /// (with the viewport at the top) emits the edge event instead of
    /// wrapping (:126-135).
    pub has_on_up_from_first_item: bool,
    /// CC: `onDownFromLastItem` provided — pressing down on the last item
    /// emits the edge event instead of wrapping (:116-125).
    pub has_on_down_from_last_item: bool,
    /// CC: `onInputModeToggle` provided — Tab reports the focused value
    /// (:181-184).
    pub has_on_input_mode_toggle: bool,
    pub option_metas: Vec<SelectInputOptionMeta>,
}

impl Default for UseSelectInputOptions {
    fn default() -> Self {
        Self {
            is_disabled: false,
            disable_selection: DisableSelection::No,
            is_multi_select: false,
            has_on_cancel: false,
            has_on_up_from_first_item: false,
            has_on_down_from_last_item: false,
            has_on_input_mode_toggle: false,
            option_metas: Vec::new(),
        }
    }
}

/// Accept/cancel/toggle/edge events for the component body to map onto its
/// callbacks (CC calls onChange/onCancel/onUpFromFirstItem directly inside
/// the handlers).
#[derive(Clone, Copy)]
pub struct SelectInputEvents {
    accepted: State<Option<String>>,
    cancelled: State<bool>,
    toggled: State<Option<String>>,
    up_from_first: State<bool>,
    down_from_last: State<bool>,
    input_mode_toggled: State<Option<String>>,
}

impl SelectInputEvents {
    /// Tab on the focused option (CC onInputModeToggle, :181-184).
    pub fn take_input_mode_toggle(&self) -> Option<String> {
        let toggled = self.input_mode_toggled.read().clone();
        if toggled.is_some() {
            let mut state = self.input_mode_toggled;
            state.set(None);
        }
        toggled
    }

    /// The value committed via Enter or a number key (CC onChange).
    pub fn take_accepted(&self) -> Option<String> {
        let accepted = self.accepted.read().clone();
        if accepted.is_some() {
            let mut state = self.accepted;
            state.set(None);
        }
        accepted
    }

    /// Escape while cancellable (CC onCancel).
    pub fn take_cancelled(&self) -> bool {
        let cancelled = self.cancelled.get();
        if cancelled {
            let mut state = self.cancelled;
            state.set(false);
        }
        cancelled
    }

    /// Space toggle on the focused option (multi-select, CC :241-253).
    pub fn take_toggled(&self) -> Option<String> {
        let toggled = self.toggled.read().clone();
        if toggled.is_some() {
            let mut state = self.toggled;
            state.set(None);
        }
        toggled
    }

    /// Up pressed on the first item with the viewport at the top
    /// (CC onUpFromFirstItem).
    pub fn take_up_from_first_item(&self) -> bool {
        let fired = self.up_from_first.get();
        if fired {
            let mut state = self.up_from_first;
            state.set(false);
        }
        fired
    }

    /// Down pressed on the last item (CC onDownFromLastItem).
    pub fn take_down_from_last_item(&self) -> bool {
        let fired = self.down_from_last.get();
        if fired {
            let mut state = self.down_from_last;
            state.set(false);
        }
        fired
    }
}

fn focused_meta(
    state: &SelectState,
    metas: &[SelectInputOptionMeta],
) -> Option<SelectInputOptionMeta> {
    let focused = state.focused_value()?;
    metas.iter().find(|meta| meta.value == focused).cloned()
}

/// Maps to: CC digit normalization (`normalizeFullWidthDigits`).
fn normalize_digit(character: char) -> Option<u32> {
    match character {
        '0'..='9' => character.to_digit(10),
        '０'..='９' => Some(character as u32 - '０' as u32),
        _ => None,
    }
}

/// Maps to: CC `useSelectInput`.
pub fn use_select_input(
    hooks: &mut Hooks,
    state: SelectState,
    options: UseSelectInputOptions,
) -> SelectInputEvents {
    let accepted = hooks.use_state(|| Option::<String>::None);
    let cancelled = hooks.use_state(|| false);
    let toggled = hooks.use_state(|| Option::<String>::None);
    let up_from_first = hooks.use_state(|| false);
    let down_from_last = hooks.use_state(|| false);
    let input_mode_toggled = hooks.use_state(|| Option::<String>::None);
    let events = SelectInputEvents {
        accepted,
        cancelled,
        toggled,
        up_from_first,
        down_from_last,
        input_mode_toggled,
    };
    let has_on_input_mode_toggle = options.has_on_input_mode_toggle;

    // CC :99-101 — register the 'select' overlay while cancellable so the
    // cancel-request handler won't intercept Escape.
    let mut overlay = hooks.use_state(|| Option::<OverlayRegistration>::None);
    let app_store = hooks.try_use_context::<crate::state::store::AppStore>();
    if options.has_on_cancel {
        if overlay.read().is_none() {
            if let Some(store) = app_store.as_deref() {
                let registration = OverlayRegistration::register(store.clone(), "select");
                overlay.set(Some(registration));
            }
        }
    } else if overlay.read().is_some() {
        overlay.set(None);
    }

    let keybinding_runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());

    let is_disabled = options.is_disabled;
    let disable_selection = options.disable_selection;
    let is_multi_select = options.is_multi_select;
    let has_on_cancel = options.has_on_cancel;
    let has_on_up_from_first_item = options.has_on_up_from_first_item;
    let has_on_down_from_last_item = options.has_on_down_from_last_item;
    let metas = options.option_metas;

    // CC :112-148 — navigation/accept only while not in an input option.
    let nav_active = {
        let metas = metas.clone();
        move || !is_disabled && !focused_meta(&state, &metas).is_some_and(|meta| meta.is_input)
    };
    {
        let metas_for_next = metas.clone();
        let mut down_from_last = down_from_last;
        use_keybinding(
            hooks,
            keybinding_runtime.clone(),
            "select:next",
            ContextName::Select,
            nav_active.clone(),
            move || {
                // CC :116-125 — with onDownFromLastItem, the last item emits
                // the edge event instead of wrapping.
                if has_on_down_from_last_item {
                    let last = metas_for_next.last().map(|meta| meta.value.clone());
                    if last.is_some() && state.focused_value() == last {
                        down_from_last.set(true);
                        return true;
                    }
                }
                state.navigation.focus_next_option();
                true
            },
        );
    }
    {
        let metas_for_previous = metas.clone();
        let mut up_from_first = up_from_first;
        use_keybinding(
            hooks,
            keybinding_runtime.clone(),
            "select:previous",
            ContextName::Select,
            nav_active.clone(),
            move || {
                // CC :126-135 — with onUpFromFirstItem and the viewport at
                // the top, the first item emits the edge event instead of
                // wrapping.
                if has_on_up_from_first_item && state.navigation.snapshot().visible_from_index == 0
                {
                    let first = metas_for_previous.first().map(|meta| meta.value.clone());
                    if first.is_some() && state.focused_value() == first {
                        up_from_first.set(true);
                        return true;
                    }
                }
                state.navigation.focus_previous_option();
                true
            },
        );
    }
    {
        let metas_for_accept = metas.clone();
        let mut accepted = accepted;
        use_keybinding(
            hooks,
            keybinding_runtime.clone(),
            "select:accept",
            ContextName::Select,
            nav_active,
            move || {
                // CC :136-147.
                if disable_selection == DisableSelection::Yes {
                    return true;
                }
                let Some(focused) = state.focused_value() else {
                    return true;
                };
                if metas_for_accept
                    .iter()
                    .any(|meta| meta.value == focused && meta.disabled)
                {
                    return true;
                }
                state.select_focused_option();
                accepted.set(Some(focused));
                true
            },
        );
    }
    {
        let mut cancelled = cancelled;
        use_keybinding(
            hooks,
            keybinding_runtime,
            "select:cancel",
            ContextName::Select,
            move || !is_disabled && has_on_cancel,
            move || {
                cancelled.set(true);
                true
            },
        );
    }

    // CC :173-286 — remaining raw keys: numbers, pageUp/pageDown, space.
    hooks.use_propagated_terminal_events({
        let metas = metas;
        let mut accepted = accepted;
        let mut toggled = toggled;
        move |event| {
            if is_disabled {
                return;
            }
            let TerminalEvent::Key(key_event) = event.event() else {
                return;
            };
            if key_event.kind == KeyEventKind::Release {
                return;
            }
            // CC :181-184: Tab (shift or not) toggles the focused option's
            // input mode; the key is not stopped.
            if has_on_input_mode_toggle
                && matches!(key_event.code, KeyCode::Tab | KeyCode::BackTab)
            {
                if let Some(value) = state.focused_value() {
                    let mut toggled = input_mode_toggled;
                    toggled.set(Some(value));
                    return;
                }
            }
            let in_input = focused_meta(&state, &metas).is_some_and(|meta| meta.is_input);
            // CC use-select-input.ts:197-225: raw arrows/Ctrl-N/P still
            // navigate while an input option has focus. Other keys, including
            // digits and j/k, belong to that option's text editor.
            if in_input {
                let down = key_event.code == KeyCode::Down
                    || (key_event.code == KeyCode::Char('n')
                        && key_event.modifiers.contains(KeyModifiers::CONTROL));
                let up = key_event.code == KeyCode::Up
                    || (key_event.code == KeyCode::Char('p')
                        && key_event.modifiers.contains(KeyModifiers::CONTROL));
                if down {
                    if has_on_down_from_last_item
                        && metas
                            .last()
                            .is_some_and(|meta| Some(meta.value.clone()) == state.focused_value())
                    {
                        let mut edge = down_from_last;
                        edge.set(true);
                    } else {
                        state.navigation.focus_next_option();
                    }
                    event.stop_propagation();
                    return;
                }
                if up {
                    if has_on_up_from_first_item
                        && state.navigation.snapshot().visible_from_index == 0
                        && metas
                            .first()
                            .is_some_and(|meta| Some(meta.value.clone()) == state.focused_value())
                    {
                        let mut edge = up_from_first;
                        edge.set(true);
                    } else {
                        state.navigation.focus_previous_option();
                    }
                    event.stop_propagation();
                    return;
                }
            }
            match key_event.code {
                KeyCode::PageDown if !in_input => {
                    state.navigation.focus_next_page();
                    event.stop_propagation();
                }
                KeyCode::PageUp if !in_input => {
                    state.navigation.focus_previous_page();
                    event.stop_propagation();
                }
                // CC :243-253 — space toggles the focused option in
                // multi-select (full-width space normalized).
                KeyCode::Char(' ') | KeyCode::Char('\u{3000}')
                    if !in_input
                        && is_multi_select
                        && disable_selection != DisableSelection::Yes =>
                {
                    if let Some(meta) = focused_meta(&state, &metas) {
                        if !meta.disabled {
                            state.select_focused_option();
                            toggled.set(Some(meta.value));
                        }
                    }
                    event.stop_propagation();
                }
                // CC :255-282 — number keys select 1-based options. Digits
                // pass through to the TextInput while in an input option
                // (CC :226-230).
                KeyCode::Char(character) if !in_input => {
                    let Some(digit) = normalize_digit(character) else {
                        return;
                    };
                    if key_event.modifiers != KeyModifiers::NONE
                        && key_event.modifiers != KeyModifiers::SHIFT
                    {
                        return;
                    }
                    if matches!(
                        disable_selection,
                        DisableSelection::Yes | DisableSelection::Numeric
                    ) {
                        return;
                    }
                    let Some(index) = (digit as usize).checked_sub(1) else {
                        return;
                    };
                    let Some(meta) = metas.get(index) else {
                        return;
                    };
                    if meta.disabled {
                        event.stop_propagation();
                        return;
                    }
                    if meta.is_input {
                        // CC :265-277 — pre-filled or allow-empty inputs
                        // auto-submit; empty ones just take focus.
                        if meta.input_has_value || meta.allow_empty_submit_to_cancel {
                            accepted.set(Some(meta.value.clone()));
                        } else {
                            state.navigation.focus_option(&meta.value);
                        }
                        event.stop_propagation();
                        return;
                    }
                    accepted.set(Some(meta.value.clone()));
                    event.stop_propagation();
                }
                _ => {}
            }
        }
    });

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_normalization_matches_official_full_width_handling() {
        assert_eq!(normalize_digit('3'), Some(3));
        assert_eq!(normalize_digit('３'), Some(3));
        assert_eq!(normalize_digit('a'), None);
    }
}
