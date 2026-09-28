//! Maps to: CC `components/CustomSelect/SelectMulti.tsx` and
//! `components/CustomSelect/use-multi-select-state.ts`.
//!
//! This retained-mode Rust boundary implements the visible multi-select state
//! used by MCP dialogs: arrow navigation, Space toggles, Enter submits, Escape
//! cancels, optional numeric toggles, hidden indexes, default values, and
//! scroll indicators. Rendering goes through the shared `SelectOption`
//! (SelectMulti.tsx:165-187) and `SelectInputOption` (SelectMulti.tsx:124-163)
//! components, with the `[✓]` checkbox injected as children. The retained
//! owner also implements the official submit-button focus, wrapping/page
//! navigation, selection/focus callbacks, input-option text state, and the
//! shared attachment-selection controller required by SelectInputOption.
//! Image paste and external-editor launching remain explicit optional seams.

use super::select::SelectOptionData;
use super::select_input_option::{SelectInputOption, image_attachment_ids};
use super::select_option::SelectOption;
use super::use_multi_select_state::{
    initial_focus_index, initial_input_values, update_input_value_selection,
};
use crate::components::prompt_input::input_paste::PastedContent;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

const TICK: &str = "✓";

#[derive(Default, Props)]
pub struct SelectMultiProps<'a> {
    pub is_disabled: bool,
    pub visible_option_count: Option<usize>,
    pub options: Vec<SelectOptionData>,
    pub default_value: Vec<String>,
    pub hide_indexes: bool,
    pub handle_escape: Option<bool>,
    pub focus_value: Option<String>,
    pub initial_focus_last: bool,
    pub submit_button_text: Option<String>,
    pub on_submit: HandlerMut<'a, Vec<String>>,
    pub on_change: HandlerMut<'a, Vec<String>>,
    pub on_focus: HandlerMut<'a, String>,
    pub on_input_change: HandlerMut<'a, (String, String)>,
    pub on_down_from_last_item: HandlerMut<'a, ()>,
    pub on_up_from_first_item: HandlerMut<'a, ()>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub pasted_contents: BTreeMap<usize, PastedContent>,
    pub on_remove_image: HandlerMut<'a, usize>,
    /// CC `onOpenEditor(currentValue, setValue)` (SelectMulti.tsx:53-56):
    /// `setValue` writes this list's input value, as typing does.
    pub on_open_editor: Handler<(String, Handler<String>)>,
    /// CC `onImagePaste` (SelectMulti.tsx:57-63).
    pub on_image_paste: Handler<crate::utils::image_paste::ClipboardImage>,
    /// Deterministic adapter seam for component tests, as on `Select`.
    pub clipboard_image_override: Option<crate::utils::image_paste::ClipboardImage>,
}

pub fn select_multi_toggle(mut selected: Vec<String>, value: &str) -> Vec<String> {
    if let Some(index) = selected.iter().position(|candidate| candidate == value) {
        selected.remove(index);
    } else {
        selected.push(value.to_string());
    }
    selected
}

pub fn select_multi_window_start(
    focused_index: usize,
    visible_count: usize,
    total: usize,
) -> usize {
    if total <= visible_count {
        return 0;
    }
    focused_index
        .saturating_sub(visible_count.saturating_sub(1))
        .min(total - visible_count)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectMultiAction {
    Submit(Vec<String>),
    Change(Vec<String>),
    Focus(String),
    InputChange(String, String),
    DownFromLast,
    UpFromFirst,
    Cancel,
    RemoveImage(usize),
}

#[component]
pub fn SelectMulti<'a>(
    props: &mut SelectMultiProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let initial_focus = initial_focus_index(
        &props.options,
        props.focus_value.as_deref(),
        props.initial_focus_last,
    );
    let mut focused_index = hooks.use_state(|| initial_focus);
    let mut selected_values = hooks.use_state(|| props.default_value.clone());
    let mut input_values = hooks.use_state(|| initial_input_values(&props.options));
    let mut is_submit_focused = hooks.use_state(|| false);
    let mut images_selected = hooks.use_state(|| false);
    let mut selected_image_index = hooks.use_state(|| 0usize);
    // CC use-select-navigation.ts:614-618: onFocus also reports the focus
    // the list mounts with.
    let initial_focus_value = props
        .options
        .get(initial_focus)
        .map(|option| option.value.clone());
    let mut pending_actions = hooks.use_state(|| {
        initial_focus_value
            .map(SelectMultiAction::Focus)
            .into_iter()
            .collect::<Vec<_>>()
    });
    // CC use-multi-select-state.ts:215 `useRegisterOverlay('multi-select')`,
    // for the whole mount, so the cancel-request handler leaves Esc and
    // app:interrupt to this list.
    let app_store = hooks.try_use_context::<crate::state::store::AppStore>();
    let mut overlay =
        hooks.use_state(|| Option::<crate::context::overlay_context::OverlayRegistration>::None);
    if overlay.read().is_none() {
        if let Some(store) = app_store.as_deref() {
            overlay.set(Some(crate::context::overlay_context::OverlayRegistration::register(
                store.clone(),
                "multi-select",
            )));
        }
    }

    let pending = pending_actions.read().clone();
    if !pending.is_empty() {
        pending_actions.set(Vec::new());
        for action in pending {
            match action {
                SelectMultiAction::Submit(values) => (props.on_submit)(values),
                SelectMultiAction::Change(values) => (props.on_change)(values),
                SelectMultiAction::Focus(value) => (props.on_focus)(value),
                SelectMultiAction::InputChange(value, input) => {
                    (props.on_input_change)((value, input))
                }
                SelectMultiAction::DownFromLast => (props.on_down_from_last_item)(()),
                SelectMultiAction::UpFromFirst => (props.on_up_from_first_item)(()),
                SelectMultiAction::Cancel => (props.on_cancel)(()),
                SelectMultiAction::RemoveImage(id) => (props.on_remove_image)(id),
            }
        }
    }

    let handle_escape = props.handle_escape.unwrap_or(true);
    let total = props.options.len();
    let visible_count = props
        .visible_option_count
        .unwrap_or(5)
        .max(1)
        .min(total.max(1));
    let focused = focused_index.get().min(total.saturating_sub(1));
    let image_count = image_attachment_ids(&props.pasted_contents).len();
    let focused_has_input = props
        .options
        .get(focused)
        .is_some_and(|option| option.input.is_some());
    if images_selected.get() && (image_count == 0 || !focused_has_input) {
        images_selected.set(false);
    }
    if image_count > 0 && selected_image_index.get() >= image_count {
        selected_image_index.set(image_count - 1);
    }
    let images_selected_value = images_selected.get();
    let selected_image_index_value = selected_image_index.get();
    let selected = selected_values.read().clone();
    let inputs = input_values.read().clone();
    let submit_focused = is_submit_focused.get();
    let has_submit_button = props.submit_button_text.is_some() && !props.on_submit.is_default();
    let start = select_multi_window_start(focused, visible_count, total);
    let end = (start + visible_count).min(total);
    let max_index_width = total.max(1).to_string().len();

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut selected_values = selected_values;
        let mut input_values = input_values;
        let mut is_submit_focused = is_submit_focused;
        let mut images_selected = images_selected;
        let mut selected_image_index = selected_image_index;
        let mut pending_actions = pending_actions;
        let options = props.options.clone();
        let hide_indexes = props.hide_indexes;
        let is_disabled = props.is_disabled;
        let handle_escape = handle_escape;
        let has_down_handler = !props.on_down_from_last_item.is_default();
        let has_up_handler = !props.on_up_from_first_item.is_default();
        move |event| {
            if is_disabled {
                return;
            }
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) = event
            else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            let total = options.len();
            if total == 0 {
                if handle_escape && matches!(code, KeyCode::Esc) {
                    pending_actions.set(vec![SelectMultiAction::Cancel]);
                }
                return;
            }

            let current = focused_index.get().min(total - 1);
            let focused_option = options.get(current);
            let in_input = focused_option.is_some_and(|option| option.input.is_some());
            // A selected attachment owns every key until an Attachments action
            // (or the source-matched raw Up escape) clears the mode. Capture
            // the render snapshot so the same event cannot also navigate after
            // a child handler updates retained state.
            if images_selected_value || images_selected.get() {
                return;
            }
            if in_input && matches!(code, KeyCode::Down) && image_count > 0 {
                images_selected.set(true);
                selected_image_index.set(image_count - 1);
                return;
            }
            let queue = |pending: &mut State<Vec<SelectMultiAction>>, action| {
                let mut actions = pending.read().clone();
                actions.push(action);
                pending.set(actions);
            };
            let focus = |focused_index: &mut State<usize>,
                         pending: &mut State<Vec<SelectMultiAction>>,
                         index: usize| {
                focused_index.set(index);
                if let Some(option) = options.get(index) {
                    queue(pending, SelectMultiAction::Focus(option.value.clone()));
                }
            };
            let change_selection = |selected_values: &mut State<Vec<String>>,
                                    pending: &mut State<Vec<SelectMultiAction>>,
                                    value: &str| {
                let next = select_multi_toggle(selected_values.read().clone(), value);
                selected_values.set(next.clone());
                queue(pending, SelectMultiAction::Change(next));
            };

            match code {
                KeyCode::Esc if handle_escape => {
                    queue(&mut pending_actions, SelectMultiAction::Cancel)
                }
                KeyCode::Up | KeyCode::Char('k') if !in_input || matches!(code, KeyCode::Up) => {
                    if is_submit_focused.get() {
                        is_submit_focused.set(false);
                        focus(&mut focused_index, &mut pending_actions, total - 1);
                    } else if current == 0 && has_up_handler {
                        queue(&mut pending_actions, SelectMultiAction::UpFromFirst);
                    } else {
                        focus(
                            &mut focused_index,
                            &mut pending_actions,
                            if current == 0 { total - 1 } else { current - 1 },
                        );
                    }
                }
                KeyCode::Down | KeyCode::Char('j')
                    if !in_input || matches!(code, KeyCode::Down) =>
                {
                    if is_submit_focused.get() {
                        if has_down_handler {
                            queue(&mut pending_actions, SelectMultiAction::DownFromLast);
                        }
                    } else if has_submit_button && current + 1 == total {
                        is_submit_focused.set(true);
                    } else if !has_submit_button && current + 1 == total && has_down_handler {
                        queue(&mut pending_actions, SelectMultiAction::DownFromLast);
                    } else {
                        focus(
                            &mut focused_index,
                            &mut pending_actions,
                            (current + 1) % total,
                        );
                    }
                }
                KeyCode::Tab => {
                    if modifiers.contains(KeyModifiers::SHIFT) {
                        if is_submit_focused.get() {
                            is_submit_focused.set(false);
                            focus(&mut focused_index, &mut pending_actions, total - 1);
                        } else {
                            focus(
                                &mut focused_index,
                                &mut pending_actions,
                                if current == 0 { total - 1 } else { current - 1 },
                            );
                        }
                    } else if has_submit_button && current + 1 == total && !is_submit_focused.get()
                    {
                        is_submit_focused.set(true);
                    } else if !is_submit_focused.get() {
                        focus(
                            &mut focused_index,
                            &mut pending_actions,
                            (current + 1) % total,
                        );
                    }
                }
                KeyCode::PageDown => focus(
                    &mut focused_index,
                    &mut pending_actions,
                    (current + visible_count).min(total - 1),
                ),
                KeyCode::PageUp => focus(
                    &mut focused_index,
                    &mut pending_actions,
                    current.saturating_sub(visible_count),
                ),
                KeyCode::Char(' ') if in_input => {
                    if let Some(option) = focused_option {
                        let mut values = input_values.read().clone();
                        let input = values.entry(option.value.clone()).or_default();
                        input.push(' ');
                        let next_input = input.clone();
                        input_values.set(values);
                        let next_selected = update_input_value_selection(
                            &selected_values.read(),
                            &option.value,
                            &next_input,
                        );
                        selected_values.set(next_selected.clone());
                        queue(
                            &mut pending_actions,
                            SelectMultiAction::InputChange(option.value.clone(), next_input),
                        );
                        queue(
                            &mut pending_actions,
                            SelectMultiAction::Change(next_selected),
                        );
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(option) = focused_option {
                        change_selection(&mut selected_values, &mut pending_actions, &option.value);
                    }
                }
                KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) && in_input => {
                    queue(
                        &mut pending_actions,
                        SelectMultiAction::Submit(selected_values.read().clone()),
                    );
                }
                KeyCode::Enter if is_submit_focused.get() || !has_submit_button => {
                    queue(
                        &mut pending_actions,
                        SelectMultiAction::Submit(selected_values.read().clone()),
                    );
                }
                KeyCode::Enter => {
                    if let Some(option) = focused_option {
                        change_selection(&mut selected_values, &mut pending_actions, &option.value);
                    }
                }
                KeyCode::Backspace if in_input => {
                    if let Some(option) = focused_option {
                        let mut values = input_values.read().clone();
                        let input = values.entry(option.value.clone()).or_default();
                        input.pop();
                        let next_input = input.clone();
                        input_values.set(values);
                        let next_selected = update_input_value_selection(
                            &selected_values.read(),
                            &option.value,
                            &next_input,
                        );
                        selected_values.set(next_selected.clone());
                        queue(
                            &mut pending_actions,
                            SelectMultiAction::InputChange(option.value.clone(), next_input),
                        );
                        queue(
                            &mut pending_actions,
                            SelectMultiAction::Change(next_selected),
                        );
                    }
                }
                KeyCode::Char(c)
                    if in_input
                        && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(option) = focused_option {
                        let mut values = input_values.read().clone();
                        let input = values.entry(option.value.clone()).or_default();
                        input.push(c);
                        let next_input = input.clone();
                        input_values.set(values);
                        let next_selected = update_input_value_selection(
                            &selected_values.read(),
                            &option.value,
                            &next_input,
                        );
                        selected_values.set(next_selected.clone());
                        queue(
                            &mut pending_actions,
                            SelectMultiAction::InputChange(option.value.clone(), next_input),
                        );
                        queue(
                            &mut pending_actions,
                            SelectMultiAction::Change(next_selected),
                        );
                    }
                }
                KeyCode::Char(c)
                    if !hide_indexes
                        && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    let Some(digit) = c.to_digit(10) else {
                        return;
                    };
                    if digit == 0 {
                        return;
                    }
                    let index = digit as usize - 1;
                    if let Some(option) = options.get(index) {
                        change_selection(&mut selected_values, &mut pending_actions, &option.value);
                    }
                }
                _ => {}
            }
        }
    });

    let on_remove_image: Handler<usize> = if props.on_remove_image.is_default() {
        Handler::default()
    } else {
        let pending = pending_actions;
        Handler::from(move |id| {
            let mut pending = pending;
            let mut actions = pending.read().clone();
            actions.push(SelectMultiAction::RemoveImage(id));
            pending.set(actions);
        })
    };
    let on_images_selected_change = Handler::from(move |selected| {
        let mut state = images_selected;
        state.set(selected);
    });
    let on_selected_image_index_change = Handler::from(move |index| {
        let mut state = selected_image_index;
        state.set(index);
    });
    let pasted_contents = props.pasted_contents.clone();
    // The editor's `setValue` for one input option: the same state update
    // and callbacks as typing into it.
    let open_editor_for = |value: String| -> Handler<(String, Handler<String>)> {
        if props.on_open_editor.is_default() {
            return Handler::default();
        }
        let open_editor = props.on_open_editor.clone();
        Handler::from(move |(current, _): (String, Handler<String>)| {
            let value = value.clone();
            let set_value = Handler::from(move |text: String| {
                let (mut input_values, mut selected_values, mut pending) =
                    (input_values, selected_values, pending_actions);
                let mut values = input_values.read().clone();
                values.insert(value.clone(), text.clone());
                input_values.set(values);
                let next_selected =
                    update_input_value_selection(&selected_values.read(), &value, &text);
                selected_values.set(next_selected.clone());
                let mut actions = pending.read().clone();
                actions.push(SelectMultiAction::InputChange(value.clone(), text));
                actions.push(SelectMultiAction::Change(next_selected));
                pending.set(actions);
            });
            open_editor((current, set_value));
        })
    };
    let on_image_paste = props.on_image_paste.clone();
    let clipboard_image_override = props.clipboard_image_override.clone();

    // Maps to: CC SelectMulti.tsx:107-207 — options plus optional submit row.
    element! {
        View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Column) {
                #(props.options[start..end].iter().enumerate().map(|(visible_idx, option)| {
                    let option_index = start + visible_idx;
                    // CC :111-114.
                    let is_option_focused = !props.is_disabled && option_index == focused && !submit_focused;
                    let is_selected = selected.iter().any(|value| value == &option.value);
                    let is_first_visible_option = option_index == start;
                    let is_last_visible_option = option_index + 1 == end;
                    let are_more_options_below = end < total;
                    let are_more_options_above = start > 0;
                    // CC :122 — 1-based display index.
                    let i = option_index + 1;
                    let checkbox_color = is_selected.then_some(theme.success);

                    // Maps to: CC SelectMulti.tsx:124-163 — input options render
                    // via SelectInputOption with the checkbox as children.
                    if let Some(input) = option.input.clone() {
                        return element! {
                            View(key: option.value.clone(), flex_direction: FlexDirection::Row, flex_shrink: 0.0f32, column_gap: 1u32) {
                                SelectInputOption(
                                    label: option.label.clone(),
                                    index: i,
                                    max_index_width: max_index_width,
                                    is_focused: is_option_focused,
                                    // CC :132 — selection state is shown via the checkbox instead.
                                    is_selected: false,
                                    show_scroll_down: are_more_options_below && is_last_visible_option,
                                    show_scroll_up: are_more_options_above && is_first_visible_option,
                                    // CC :125 reads retained `state.inputValues`.
                                    input_value: inputs.get(&option.value).cloned().unwrap_or(input.value),
                                    placeholder: input.placeholder,
                                    show_label_with_value: input.show_label_with_value,
                                    label_value_separator: input.label_value_separator,
                                    description: option.description.clone(),
                                    dim_description: option.disabled || option.dim_description,
                                    // CC :151 — layout="compact".
                                    layout_expanded: false,
                                    pasted_contents: pasted_contents.clone(),
                                    on_remove_image: on_remove_image.clone(),
                                    images_selected: images_selected_value,
                                    selected_image_index: selected_image_index_value,
                                    on_images_selected_change: on_images_selected_change.clone(),
                                    on_selected_image_index_change: on_selected_image_index_change.clone(),
                                    on_open_editor: open_editor_for(option.value.clone()),
                                    on_image_paste: on_image_paste.clone(),
                                    clipboard_image_override: clipboard_image_override.clone(),
                                ) {
                                    // CC :157-159 — `[✓] ` with trailing space, success when selected.
                                    Text(content: format!("[{}] ", if is_selected { TICK } else { " " }), color: checkbox_color, wrap: TextWrap::NoWrap)
                                }
                            }
                        }.into_any();
                    }

                    // Maps to: CC SelectMulti.tsx:165-187 — non-input options render
                    // via SelectOption (→ ListItem: pointer/scroll indicator + row).
                    // CC :177 — `${i}.`.padEnd(maxIndexWidth).
                    let padded_index = format!(
                        "{:<width$}",
                        format!("{i}."),
                        width = max_index_width,
                    );
                    element! {
                        View(key: option.value.clone(), flex_direction: FlexDirection::Row, flex_shrink: 0.0f32, column_gap: 1u32) {
                            SelectOption(
                                is_focused: is_option_focused,
                                // CC :169 — selection state is shown via the checkbox instead.
                                is_selected: false,
                                show_scroll_down: are_more_options_below && is_last_visible_option,
                                show_scroll_up: are_more_options_above && is_first_visible_option,
                                description: option.description.clone(),
                            ) {
                                // CC ListItem row is gap={1} between all children; Cometix
                                // ListItem only separates the indicator with an explicit
                                // space, so this Row carries the inter-child gap.
                                View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32, column_gap: 1u32) {
                                    #(if !props.hide_indexes {
                                        // CC :176-178.
                                        Some(element! { Text(content: padded_index.clone(), dim: true, wrap: TextWrap::NoWrap) })
                                    } else { None })
                                    // CC :179-181 — checkbox carries the success color.
                                    Text(content: format!("[{}]", if is_selected { TICK } else { " " }), color: checkbox_color, wrap: TextWrap::NoWrap)
                                    // CC :182-184 — label colors on focus only (not selection).
                                    Text(content: option.label.clone(), color: is_option_focused.then_some(theme.suggestion), wrap: TextWrap::NoWrap)
                                }
                            }
                        }
                    }.into_any()
                }))
            }
            #(if has_submit_button {
                props.submit_button_text.clone().map(|label| element! {
                    View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
                        Text(content: if submit_focused { "❯".to_string() } else { " ".to_string() }, color: submit_focused.then_some(theme.suggestion), wrap: TextWrap::NoWrap)
                        View(margin_left: 3u32) {
                            Text(content: label, color: submit_focused.then_some(theme.suggestion), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        }
                    }
                })
            } else { None })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn option(value: &str) -> SelectOptionData {
        SelectOptionData {
            label: value.to_string(),
            value: value.to_string(),
            ..SelectOptionData::default()
        }
    }

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, code);
        event.modifiers = modifiers;
        TerminalEvent::Key(event)
    }

    #[component]
    fn AttachmentKeybindingHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = hooks.use_const(|| {
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            bindings.push(crate::keybindings::types::ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("right"),
                action: None,
                context: crate::keybindings::types::ContextName::Attachments,
            });
            bindings.push(crate::keybindings::types::ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("f4"),
                action: Some("attachments:next".to_string()),
                context: crate::keybindings::types::ContextName::Attachments,
            });
            crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings)
        });
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks, runtime,
        );
        let mut pasted_contents = hooks.use_state(|| {
            BTreeMap::from([
                (
                    1,
                    PastedContent::Image {
                        id: 1,
                        media_type: Some("image/png".to_string()),
                        data: None,
                        filename: None,
                        dimensions: None,
                        source_path: None,
                    },
                ),
                (
                    2,
                    PastedContent::Image {
                        id: 2,
                        media_type: Some("image/png".to_string()),
                        data: None,
                        filename: None,
                        dimensions: None,
                        source_path: None,
                    },
                ),
            ])
        });
        let mut removed = hooks.use_state(Vec::<usize>::new);
        let contents = pasted_contents.read().clone();
        let removed_marker = format!("removed={:?}", removed.read().as_slice());
        let input = SelectOptionData {
            label: "Extra".to_string(),
            value: "extra".to_string(),
            input: Some(crate::components::custom_select::SelectInputOptionData {
                placeholder: Some("details".to_string()),
                show_label_with_value: true,
                label_value_separator: Some(": ".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        element! {
            ContextProvider(value: Context::owned(runtime)) {

                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        SelectMulti(
                            options: vec![input],
                            pasted_contents: contents,
                            on_remove_image: move |id| {
                                let mut next = pasted_contents.read().clone();
                                next.retain(|_, content| content.id() != id);
                                pasted_contents.set(next);
                                let mut ids = removed.read().clone();
                                ids.push(id);
                                removed.set(ids);
                            },
                        )
                        Text(content: removed_marker)
                    }
                }
            }
        }
    }

    fn canvas_lines(canvas: &Canvas) -> Vec<String> {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn find_text_cell(canvas: &Canvas, needle: &str) -> Option<(usize, usize)> {
        canvas_lines(canvas)
            .iter()
            .enumerate()
            .find_map(|(row, line)| line.find(needle).map(|column| (column, row)))
    }

    #[test]
    fn select_multi_helpers_toggle_and_window_like_official_state() {
        assert_eq!(
            select_multi_toggle(vec!["a".to_string()], "a"),
            Vec::<String>::new()
        );
        assert_eq!(select_multi_toggle(Vec::new(), "a"), vec!["a".to_string()]);
        assert_eq!(select_multi_window_start(4, 3, 10), 2);
        assert_eq!(select_multi_window_start(1, 3, 2), 0);
    }

    #[test]
    fn select_multi_renders_selected_rows_and_hidden_indexes() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SelectMulti(
                    options: vec![option("alpha"), option("beta")],
                    default_value: vec!["beta".to_string()],
                    hide_indexes: true,
                )
            }
        }
        .render(Some(80))
        .to_string();

        // CC SelectMulti.tsx:165-187 — the focused row gets the ListItem
        // pointer with a single gap before the checkbox (hideIndexes).
        assert!(text.contains("❯ [ ] alpha"), "canvas=\n{text}");
        assert!(text.contains("[✓] beta"), "canvas=\n{text}");
        assert!(!text.contains("1."), "canvas=\n{text}");
    }

    #[test]
    fn select_multi_renders_official_indexes_and_colors() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                SelectMulti(
                    options: vec![option("alpha"), option("beta")],
                    default_value: vec!["beta".to_string()],
                )
            }
        }
        .render(Some(80));
        let text = canvas_lines(&canvas).join("\n");

        // CC SelectMulti.tsx:165-187 — pointer, dim index, checkbox, label,
        // each separated by ListItem's gap of 1.
        assert!(text.contains("❯ 1. [ ] alpha"), "canvas=\n{text}");
        assert!(text.contains("2. [✓] beta"), "canvas=\n{text}");

        // CC :182-184 — focused label uses suggestion.
        let (alpha_column, alpha_row) = find_text_cell(&canvas, "alpha").expect("alpha cell");
        let alpha_style = canvas
            .cell(alpha_column, alpha_row)
            .and_then(|cell| cell.text_style())
            .expect("alpha style");
        assert_eq!(
            alpha_style.color,
            Some(current_theme.suggestion),
            "canvas=\n{text}"
        );

        // CC :179-181 — the checkbox carries the success color, while the
        // selected label stays uncolored (selection shows on the checkbox only).
        let (tick_column, tick_row) = find_text_cell(&canvas, "✓").expect("tick cell");
        let tick_style = canvas
            .cell(tick_column, tick_row)
            .and_then(|cell| cell.text_style())
            .expect("tick style");
        assert_eq!(
            tick_style.color,
            Some(current_theme.success),
            "canvas=\n{text}"
        );
        let (beta_column, beta_row) = find_text_cell(&canvas, "beta").expect("beta cell");
        let beta_style = canvas
            .cell(beta_column, beta_row)
            .and_then(|cell| cell.text_style())
            .expect("beta style");
        assert_eq!(beta_style.color, None, "canvas=\n{text}");
    }

    #[test]
    fn select_multi_input_option_renders_checkbox_prefix_via_select_input_option() {
        let extra = SelectOptionData {
            label: "Extra".to_string(),
            value: "extra".to_string(),
            input: Some(crate::components::custom_select::SelectInputOptionData {
                placeholder: Some("add detail".to_string()),
                value: String::new(),
                show_label_with_value: true,
                label_value_separator: Some(": ".to_string()),
            }),
            ..SelectOptionData::default()
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SelectMulti(
                    options: vec![option("alpha"), extra],
                    default_value: vec!["extra".to_string()],
                    hide_indexes: true,
                )
            }
        }
        .render(Some(80))
        .to_string();

        // CC SelectMulti.tsx:124-163 — the input option renders through
        // SelectInputOption: the index shows even with hideIndexes
        // (select-input-option.tsx:261), the `[✓] ` checkbox children follow
        // it, then label + separator + placeholder (blurred: value/placeholder).
        assert!(text.contains("2. [✓] Extra"), "canvas=\n{text}");
        // Non-input sibling still hides its index.
        assert!(text.contains("❯ [ ] alpha"), "canvas=\n{text}");
    }

    #[test]
    fn select_multi_space_toggles_and_enter_submits() {
        let submitted = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let submitted_for_handler = Arc::clone(&submitted);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    SelectMulti(
                        options: vec![option("alpha"), option("beta")],
                        default_value: vec!["alpha".to_string(), "beta".to_string()],
                        on_submit: move |values| submitted_for_handler.lock().expect("submitted mutex").push(values),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Char(' ')),
                        key(KeyCode::Enter),
                    ]))
                    .with_size(100, 20),
                ),
            );
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            submitted.lock().expect("submitted mutex").as_slice(),
            &[vec!["beta".to_string()]]
        );
    }

    #[test]
    fn select_multi_submit_button_focuses_after_last_option() {
        let submitted = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let submitted_for_handler = Arc::clone(&submitted);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    SelectMulti(
                        options: vec![option("alpha"), option("beta")],
                        default_value: vec!["alpha".to_string()],
                        submit_button_text: Some("Confirm".to_string()),
                        on_submit: move |values| submitted_for_handler.lock().expect("submitted mutex").push(values),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Down),
                        key(KeyCode::Down),
                        key(KeyCode::Enter),
                    ]))
                    .with_size(100, 20),
                ),
            );
            for _ in 0..10 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            submitted.lock().expect("submitted mutex").as_slice(),
            &[vec!["alpha".to_string()]]
        );
    }

    #[test]
    fn select_multi_input_state_selects_nonempty_value_and_ctrl_enter_submits() {
        let submitted = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let changes = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let submitted_for_handler = Arc::clone(&submitted);
        let changes_for_handler = Arc::clone(&changes);
        let input = SelectOptionData {
            label: "Extra".to_string(),
            value: "extra".to_string(),
            input: Some(crate::components::custom_select::SelectInputOptionData {
                placeholder: Some("details".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    SelectMulti(
                        options: vec![input],
                        submit_button_text: Some("Confirm".to_string()),
                        on_input_change: move |change| changes_for_handler.lock().expect("changes mutex").push(change),
                        on_submit: move |values| submitted_for_handler.lock().expect("submitted mutex").push(values),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Char('x')),
                        key(KeyCode::Char('y')),
                        key(KeyCode::Backspace),
                        modified_key(KeyCode::Enter, KeyModifiers::CONTROL),
                    ]))
                    .with_size(100, 20),
                ),
            );
            for _ in 0..12 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            changes.lock().expect("changes mutex").as_slice(),
            &[
                ("extra".to_string(), "x".to_string()),
                ("extra".to_string(), "xy".to_string()),
                ("extra".to_string(), "x".to_string()),
            ]
        );
        assert_eq!(
            submitted.lock().expect("submitted mutex").as_slice(),
            &[vec!["extra".to_string()]]
        );
    }

    #[test]
    fn attachment_actions_honor_null_unbind_and_live_remap() {
        let text = futures::executor::block_on(async move {
            let events = vec![
                key(KeyCode::Down),
                key(KeyCode::Right),
                key(KeyCode::F(4)),
                key(KeyCode::Backspace),
                key(KeyCode::Esc),
                key(KeyCode::Char('x')),
            ];
            let paced = stream::unfold(events.into_iter(), |mut events| async move {
                let event = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(25)).await;
                Some((event, events))
            });
            let mut app = element!(AttachmentKeybindingHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(paced).with_size(120, 20),
            ));
            let mut last = String::new();
            for _ in 0..64 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(120)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("removed=[1]") && last.contains("Extra: x") {
                    break;
                }
            }
            last
        });

        // Entry starts on the last image. Right is explicitly unbound, while
        // F4 remaps attachments:next and wraps to image #1, so Backspace must
        // remove #1 rather than #2. Escape then returns focus to text input.
        assert!(text.contains("removed=[1]"), "canvas=\n{text}");
        assert!(!text.contains("[Image #1]"), "canvas=\n{text}");
        assert!(text.contains("[Image #2]"), "canvas=\n{text}");
        assert!(text.contains("Extra: x"), "canvas=\n{text}");
    }
}
