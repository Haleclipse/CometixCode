//! Maps to: CC `components/CustomSelect/select-input-option.tsx`.
//!
//! Attachment selection and input-adjacent actions are owned here like the
//! source component: clipboard reads run off-frame, external-editor requests
//! are delegated to the caller, an empty focused input can remove the last
//! image, selected images consume the four `attachments:*` actions, Up exits
//! the mode, and losing focus clears it. The caller remains the value/image
//! storage owner.

use crate::components::clickable_image_ref::ClickableImageRef;
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::select_option::SelectOption;
use crate::components::design_system::byline::Byline;
use crate::components::prompt_input::input_paste::PastedContent;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybinding, use_keybindings};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

#[derive(Default, Props)]
pub struct SelectInputOptionProps {
    pub label: String,
    /// 1-based display index. CC renders the index unconditionally for input
    /// options (select-input-option.tsx:261), even when hideIndexes is set.
    pub index: usize,
    pub max_index_width: usize,
    pub is_focused: bool,
    pub is_selected: bool,
    pub show_scroll_down: bool,
    pub show_scroll_up: bool,
    /// Maps to: CC Select's per-option `inputValues` entry.
    pub input_value: String,
    pub on_input_change: Handler<String>,
    pub on_submit: Handler<String>,
    pub on_exit: Handler<()>,
    pub placeholder: Option<String>,
    /// Maps to: CC `showLabelWithValue` (merged with Select's showLabel).
    pub show_label_with_value: bool,
    /// Maps to: CC `labelValueSeparator`, default ", ".
    pub label_value_separator: Option<String>,
    pub description: Option<String>,
    pub dim_description: bool,
    /// Maps to: CC `layout` — only the description padding differs
    /// (expanded: maxIndexWidth+3, compact: maxIndexWidth+4).
    pub layout_expanded: bool,
    /// Maps to: CC `children` (select-input-option.tsx:262) — rendered right
    /// after the index Text, before the label/input content. SelectMulti uses
    /// this to inject its `[✓] ` checkbox prefix.
    pub children: Vec<AnyElement<'static>>,
    /// Maps to: CC `pastedContents`; only image entries render in this row.
    pub pasted_contents: BTreeMap<usize, PastedContent>,
    /// Maps to: CC `onRemoveImage`.
    pub on_remove_image: Handler<usize>,
    /// Maps to: CC's Select-owned image-selection state and setters.
    pub images_selected: bool,
    pub selected_image_index: usize,
    pub on_images_selected_change: Handler<bool>,
    pub on_selected_image_index_change: Handler<usize>,
    /// Maps to: CC `components/CustomSelect/select-input-option.tsx:141-148`
    /// `onOpenEditor(currentValue, setValue)`: `setValue` is this option's
    /// `onInputChange`, so the editor's text lands in the input.
    pub on_open_editor: Handler<(String, Handler<String>)>,
    /// Maps to: CC `components/CustomSelect/select-input-option.tsx:150-167`.
    pub on_image_paste: Handler<crate::utils::image_paste::ClipboardImage>,
    /// Deterministic adapter seam for component tests. Production reads the
    /// platform clipboard only after the configured action is dispatched.
    pub clipboard_image_override: Option<crate::utils::image_paste::ClipboardImage>,
}

/// Maps to: CC `Object.values(pastedContents).filter(c => c.type === 'image')`.
pub fn image_attachment_ids(contents: &BTreeMap<usize, PastedContent>) -> Vec<usize> {
    contents
        .values()
        .filter_map(|content| match content {
            PastedContent::Image { id, .. } => Some(*id),
            PastedContent::Text { .. } => None,
        })
        .collect()
}

fn wrapped_image_index(current: usize, count: usize, delta: isize) -> usize {
    if count <= 1 {
        return current.min(count.saturating_sub(1));
    }
    if delta < 0 {
        (current + count - 1) % count
    } else {
        (current + 1) % count
    }
}

/// Maps to: CC `SelectInputOption` render (:248-347): SelectOption/ListItem
/// row (cursor declaration off — the TextInput owns it in CC) containing the
/// dim index plus one of three content shapes:
/// - showLabelWithValue: label, then when focused a suggestion-colored
///   separator + input (placeholder while empty); when blurred with a value,
///   separator + value.
/// - focused without label: the input itself (placeholder falls back to the
///   label).
/// - blurred without label: value, else placeholder/label in inactive.
#[component]
pub fn SelectInputOption(
    props: &mut SelectInputOptionProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks
        .try_use_context::<Theme>()
        .map(|theme| *theme)
        .unwrap_or_else(|| *crate::utils::theme::current());
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // Maps to CC SelectInputOption's controlled TextInput and local cursor.
    let mut input_value = hooks.use_state(|| props.input_value.clone());
    let mut last_external_value = hooks.use_state(|| props.input_value.clone());
    let mut cursor = hooks.use_state(|| props.input_value.len());
    if *last_external_value.read() != props.input_value {
        last_external_value.set(props.input_value.clone());
        if *input_value.read() != props.input_value {
            input_value.set(props.input_value.clone());
            cursor.set(cursor.get().min(props.input_value.len()));
        }
    }
    let image_ids = image_attachment_ids(&props.pasted_contents);
    let image_paste_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<
            Option<crate::utils::image_paste::ClipboardImage>,
        >())
    });
    let mut image_paste_result =
        hooks.use_state(|| Option::<Option<crate::utils::image_paste::ClipboardImage>>::None);
    let image_paste_receiver = image_paste_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(image) = image_paste_receiver.recv().await {
            image_paste_result.set(Some(image));
        }
    });
    let completed_image_paste = image_paste_result.read().clone();
    if let Some(image) = completed_image_paste {
        image_paste_result.set(None);
        if let Some(image) = image {
            (props.on_image_paste)(image);
        }
    }

    // Maps to: CC `components/CustomSelect/select-input-option.tsx:141-148`
    // `onOpenEditor?.(inputValue, onInputChange)`.
    let open_editor = props.on_open_editor.clone();
    let editor_value = props.input_value.clone();
    let set_value = props.on_input_change.clone();
    let editor_active = props.is_focused && !props.on_open_editor.is_default();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "chat:externalEditor",
        ContextName::Chat,
        move || editor_active,
        move || {
            open_editor((editor_value.clone(), set_value.clone()));
            true
        },
    );

    // Maps to: CC `components/CustomSelect/select-input-option.tsx:150-167`.
    let image_paste_sender = image_paste_channel.0.clone();
    let clipboard_image_override = props.clipboard_image_override.clone();
    let image_paste_active = props.is_focused && !props.on_image_paste.is_default();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "chat:imagePaste",
        ContextName::Chat,
        move || image_paste_active,
        move || {
            let sender = image_paste_sender.clone();
            let override_image = clipboard_image_override.clone();
            std::thread::spawn(move || {
                let image =
                    override_image.or_else(crate::utils::image_paste::get_image_from_clipboard);
                let _ = sender.send_blocking(image);
            });
            true
        },
    );

    // Maps to: CC select-input-option.tsx `attachments:remove` outside image
    // selection mode. This intentionally shares the action with the selected
    // image handler; their active predicates are mutually exclusive.
    let remove_last_image = props.on_remove_image.clone();
    let last_image_id = image_ids.last().copied();
    let remove_last_active = props.is_focused
        && !props.images_selected
        && props.input_value.is_empty()
        && last_image_id.is_some()
        && !props.on_remove_image.is_default();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "attachments:remove",
        ContextName::Attachments,
        move || remove_last_active,
        move || {
            let Some(id) = last_image_id else {
                return false;
            };
            remove_last_image(id);
            true
        },
    );

    // Maps to: CC select-input-option.tsx image-selection `useKeybindings`.
    let next_ids = image_ids.clone();
    let next_index = props.selected_image_index;
    let set_next_index = props.on_selected_image_index_change.clone();
    let previous_ids = image_ids.clone();
    let previous_index = props.selected_image_index;
    let set_previous_index = props.on_selected_image_index_change.clone();
    let remove_ids = image_ids.clone();
    let remove_index = props.selected_image_index;
    let remove_selected_image = props.on_remove_image.clone();
    let set_selected_after_remove = props.on_selected_image_index_change.clone();
    let set_images_after_remove = props.on_images_selected_change.clone();
    let exit_images = props.on_images_selected_change.clone();
    let selected_handlers: KeybindingHandlers = vec![
        (
            "attachments:next".to_string(),
            Box::new(move || {
                if next_ids.len() > 1 {
                    set_next_index(wrapped_image_index(next_index, next_ids.len(), 1));
                }
                true
            }),
        ),
        (
            "attachments:previous".to_string(),
            Box::new(move || {
                if previous_ids.len() > 1 {
                    set_previous_index(wrapped_image_index(previous_index, previous_ids.len(), -1));
                }
                true
            }),
        ),
        (
            "attachments:remove".to_string(),
            Box::new(move || {
                if let Some(id) = remove_ids.get(remove_index).copied() {
                    // CC keeps the image mode unchanged when no removal owner
                    // was supplied; index/exit updates live inside the same
                    // `img && onRemoveImage` branch.
                    if !remove_selected_image.is_default() {
                        remove_selected_image(id);
                        if remove_ids.len() <= 1 {
                            set_images_after_remove(false);
                        } else {
                            set_selected_after_remove(remove_index.min(remove_ids.len() - 2));
                        }
                    }
                }
                true
            }),
        ),
        (
            "attachments:exit".to_string(),
            Box::new(move || {
                exit_images(false);
                true
            }),
        ),
    ];
    let selection_active = props.is_focused && props.images_selected;
    use_keybindings(
        &mut hooks,
        runtime,
        selected_handlers,
        ContextName::Attachments,
        move || selection_active,
    );

    // Maps to: CC's intentionally raw Up-arrow escape. Up has no Attachments
    // action in defaultBindings; consume it here so an ancestor Select does not
    // also move focus on the same retained event.
    let exit_images_on_up = props.on_images_selected_change.clone();
    let raw_exit_active = props.is_focused && props.images_selected;
    hooks.use_propagated_terminal_events(move |event| {
        if !raw_exit_active {
            return;
        }
        let TerminalEvent::Key(KeyEvent {
            code: KeyCode::Up,
            kind,
            ..
        }) = event.event()
        else {
            return;
        };
        if *kind == KeyEventKind::Release {
            return;
        }
        exit_images_on_up(false);
        event.stop_propagation();
    });

    // Maps to: CC's focus-loss effect.
    let clear_images_on_blur = props.on_images_selected_change.clone();
    let focused = props.is_focused;
    let images_selected = props.images_selected;
    hooks.use_effect(
        move || {
            if !focused && images_selected {
                clear_images_on_blur(false);
            }
        },
        (focused, images_selected),
    );

    // CC :262 — children render between the index and the content.
    let children: Vec<AnyElement<'static>> = props.children.drain(..).collect();
    let padded_index = format!(
        "{:<width$}",
        format!("{}.", props.index),
        width = props.max_index_width + 2,
    );
    let separator = props
        .label_value_separator
        .clone()
        .unwrap_or_else(|| ", ".to_string());
    let label_color = props.is_focused.then_some(theme.suggestion);

    // Partial migration boundary: legacy permission/SelectMulti callers still
    // own editing in their parent key handler. Keep their controlled display
    // until they provide CC's input callbacks; never create a second editor.
    let editable = !props.on_input_change.is_default() || !props.on_submit.is_default();
    let content: AnyElement<'static> = if props.show_label_with_value {
        // CC :263-308.
        let mut row: Vec<AnyElement<'static>> = vec![
            element! {
                Text(content: props.label.clone(), color: label_color, wrap: TextWrap::NoWrap)
            }
            .into_any(),
        ];
        if props.is_focused {
            row.push(
                element! {
                    Text(content: separator.clone(), color: theme.suggestion, wrap: TextWrap::NoWrap)
                }
                .into_any(),
            );
            row.push(if editable { element! {
                crate::components::text_input::TextInput(
                    value: Some(input_value), cursor_offset: Some(cursor),
                    focus: props.is_focused && !props.images_selected,
                    multiline: true, columns: 80usize, show_cursor: true,
                    disable_escape_double_press: true,
                    select_navigation_passthrough: true,
                    disable_cursor_movement_for_up_down_keys: true,
                    placeholder: props.placeholder.clone().or_else(|| Some(props.label.clone())),
                    on_change: { let change = props.on_input_change.clone(); move |value| change(value) },
                    on_submit: { let submit = props.on_submit.clone(); move |value| submit(value) },
                    on_exit: { let exit = props.on_exit.clone(); move |_| exit(()) },
                )
            }.into_any() } else { element! {
                Text(content: if props.input_value.is_empty() { props.placeholder.clone().unwrap_or_default() } else { props.input_value.clone() }, dim: props.input_value.is_empty(), wrap: TextWrap::NoWrap)
            }.into_any() });
        } else if !props.input_value.is_empty() {
            row.push(
                element! {
                    Text(content: format!("{separator}{}", props.input_value), wrap: TextWrap::NoWrap)
                }
                .into_any(),
            );
        }
        element! {
            View(flex_direction: FlexDirection::Row) { #(row) }
        }
        .into_any()
    } else if props.is_focused && editable {
        element! {
                crate::components::text_input::TextInput(
                    value: Some(input_value), cursor_offset: Some(cursor),
                    focus: props.is_focused && !props.images_selected,
                    multiline: true, columns: 80usize, show_cursor: true,
                    disable_escape_double_press: true,
                    select_navigation_passthrough: true,
                    disable_cursor_movement_for_up_down_keys: true,
                    placeholder: props.placeholder.clone().or_else(|| Some(props.label.clone())),
                    on_change: { let change = props.on_input_change.clone(); move |value| change(value) },
                    on_submit: { let submit = props.on_submit.clone(); move |value| submit(value) },
                    on_exit: { let exit = props.on_exit.clone(); move |_| exit(()) },
                )
            }.into_any()
    } else {
        // CC :341-345 — value, else placeholder/label in inactive.
        let has_value = !props.input_value.is_empty();
        let display = if has_value {
            props.input_value.clone()
        } else {
            props
                .placeholder
                .clone()
                .unwrap_or_else(|| props.label.clone())
        };
        element! {
            Text(
                content: display,
                color: (!has_value).then_some(theme.inactive),
                wrap: TextWrap::NoWrap,
            )
        }
        .into_any()
    };

    // CC :245-246.
    let desc_padding = if props.layout_expanded {
        props.max_index_width + 3
    } else {
        props.max_index_width + 4
    } as u32;
    let description = props.description.clone().filter(|s| !s.is_empty());
    let desc_color = if props.is_selected {
        Some(theme.success)
    } else if props.is_focused {
        Some(theme.suggestion)
    } else {
        None
    };
    let dim_description = props.dim_description;
    let mut image_hints = Vec::<AnyElement<'static>>::new();
    if image_ids.len() > 1 {
        image_hints.push(
            element! {
                ConfigurableShortcutHint(
                    action: "attachments:next".to_string(),
                    context: "Attachments".to_string(),
                    fallback: "→".to_string(),
                    description: "next".to_string(),
                    dim: true,
                )
            }
            .into_any(),
        );
        image_hints.push(
            element! {
                ConfigurableShortcutHint(
                    action: "attachments:previous".to_string(),
                    context: "Attachments".to_string(),
                    fallback: "←".to_string(),
                    description: "prev".to_string(),
                    dim: true,
                )
            }
            .into_any(),
        );
    }
    image_hints.push(
        element! {
            ConfigurableShortcutHint(
                action: "attachments:remove".to_string(),
                context: "Attachments".to_string(),
                fallback: "backspace".to_string(),
                description: "remove".to_string(),
                dim: true,
            )
        }
        .into_any(),
    );
    image_hints.push(
        element! {
            ConfigurableShortcutHint(
                action: "attachments:exit".to_string(),
                context: "Attachments".to_string(),
                fallback: "esc".to_string(),
                description: "cancel".to_string(),
                dim: true,
            )
        }
        .into_any(),
    );
    let selected_image_index = props.selected_image_index;
    let images_selected = props.images_selected;
    let is_focused = props.is_focused;

    element! {
        View(flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
            SelectOption(
                is_focused: props.is_focused,
                is_selected: props.is_selected,
                show_scroll_down: props.show_scroll_down,
                show_scroll_up: props.show_scroll_up,
                declare_cursor: Some(false),
            ) {
                View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
                    Text(content: padded_index, dim: true, wrap: TextWrap::NoWrap)
                    #(children)
                    #(content)
                }
            }
            #(description.map(|desc| element! {
                View(padding_left: desc_padding) {
                    Text(content: desc, color: desc_color, dim: dim_description, wrap: TextWrap::NoWrap)
                }
            }))
            #(if image_ids.is_empty() { None } else { Some(element! {
                View(flex_direction: FlexDirection::Row, column_gap: 1u32, padding_left: desc_padding) {
                    #(image_ids.into_iter().enumerate().map(|(index, id)| element! {
                        ClickableImageRef(
                            image_id: id as u64,
                            is_selected: images_selected && index == selected_image_index,
                        )
                    }))
                    View(flex_grow: 1.0f32, justify_content: JustifyContent::FLEX_START, flex_direction: FlexDirection::Row) {
                        #(if images_selected {
                            Some(element! { Byline { #(image_hints) } }.into_any())
                        } else if is_focused {
                            Some(element! { Text(content: "(↓ to select)".to_string(), dim: true, wrap: TextWrap::NoWrap) }.into_any())
                        } else {
                            None
                        })
                    }
                }
            })})
            #(if props.layout_expanded {
                Some(element! { Text(content: " ".to_string(), wrap: TextWrap::NoWrap) })
            } else {
                None
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn focused_input_option_shows_separator_and_dim_placeholder() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                SelectInputOption(
                    label: "Summarize from here".to_string(),
                    index: 4usize,
                    max_index_width: 1usize,
                    is_focused: true,
                    input_value: String::new(),
                    placeholder: Some("add context (optional)".to_string()),
                    show_label_with_value: true,
                    label_value_separator: Some(": ".to_string()),
                )
            }
        }
        .render(Some(80))
        .to_string();
        assert!(
            text.contains("❯ 4. Summarize from here: add context (optional)"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn blurred_input_option_shows_label_and_committed_value() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                SelectInputOption(
                    label: "Summarize from here".to_string(),
                    index: 4usize,
                    max_index_width: 1usize,
                    input_value: "focus on tests".to_string(),
                    placeholder: Some("add context (optional)".to_string()),
                    show_label_with_value: true,
                    label_value_separator: Some(": ".to_string()),
                )
            }
        }
        .render(Some(80))
        .to_string();
        assert!(
            text.contains("4. Summarize from here: focus on tests"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("add context"), "canvas=\n{text}");
    }

    #[test]
    fn input_option_renders_only_image_attachments_and_selection_byline() {
        let mut pasted_contents = BTreeMap::new();
        pasted_contents.insert(
            1,
            PastedContent::Text {
                id: 1,
                content: "not an image".to_string(),
            },
        );
        pasted_contents.insert(
            2,
            PastedContent::Image {
                id: 7,
                media_type: Some("image/png".to_string()),
                data: None,
                filename: None,
                dimensions: None,
                source_path: None,
            },
        );
        pasted_contents.insert(
            3,
            PastedContent::Image {
                id: 9,
                media_type: Some("image/jpeg".to_string()),
                data: None,
                filename: None,
                dimensions: None,
                source_path: None,
            },
        );

        assert_eq!(image_attachment_ids(&pasted_contents), vec![7, 9]);
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SelectInputOption(
                    label: "Other".to_string(),
                    index: 1usize,
                    max_index_width: 1usize,
                    is_focused: true,
                    input_value: String::new(),
                    pasted_contents: pasted_contents,
                    images_selected: true,
                    selected_image_index: 1usize,
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("[Image #7]"), "canvas=\n{text}");
        assert!(text.contains("[Image #9]"), "canvas=\n{text}");
        assert!(!text.contains("not an image"), "canvas=\n{text}");
        assert!(text.contains("next"), "canvas=\n{text}");
        assert!(text.contains("prev"), "canvas=\n{text}");
        assert!(text.contains("remove"), "canvas=\n{text}");
        assert!(text.contains("cancel"), "canvas=\n{text}");
    }
    #[test]
    fn editable_input_matches_official_non_ascii_initial_cursor() {
        use futures::StreamExt;
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let submitted = result.clone();
        // CC useTextInput.ts:105-106 requires the notification/store context;
        // supply the same input runtime/focus environment as the application.
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(store)) {
                    ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                        FocusScope(handle_keys: false) {
                            ContextProvider(value: Context::owned(*theme::current())) {
                                SelectInputOption(label: "Context".to_string(), input_value: "你好".to_string(), is_focused: true,
                                    on_submit: Handler::from(move |text| { *submitted.lock().unwrap() = Some(text); }))
                            }
                        }
                    }
                }
            };
            let events = futures::stream::unfold(
                vec![KeyCode::Char('X'), KeyCode::Enter].into_iter(),
                |mut events| async move {
                    let code = events.next()?;
                    futures_timer::Delay::new(std::time::Duration::from_millis(50)).await;
                    Some((
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)),
                        events,
                    ))
                },
            );
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(80, 10),
            ));
            while crate::utils::race(frames.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_millis(200)).await;
                None
            })
            .await
            .is_some()
            {}
        });
        // CC select-input-option.tsx:120: initial cursor is the end of the value.
        assert_eq!(*result.lock().unwrap(), Some("你好X".to_string()));
    }
}
