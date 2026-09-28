//! Maps to: CC `components/CustomSelect/select.tsx` (rendering path).

use crate::components::custom_select::select_input_option::{
    SelectInputOption, image_attachment_ids,
};
use crate::components::custom_select::select_option::SelectOption;
use crate::components::prompt_input::input_paste::PastedContent;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;
use unicode_width::UnicodeWidthStr;

/// Maps to: CC `OptionWithDescription` `type: 'input'` display config plus
/// the per-option entry of Select's `inputValues` map (the committed value).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectInputOptionData {
    pub placeholder: Option<String>,
    pub value: String,
    pub show_label_with_value: bool,
    pub label_value_separator: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOptionData {
    pub label: String,
    pub description: Option<String>,
    pub dim_description: bool,
    pub value: String,
    pub disabled: bool,
    /// Maps to: CC `type: 'input'` options (rendered via SelectInputOption).
    pub input: Option<SelectInputOptionData>,
}

impl Default for SelectOptionData {
    fn default() -> Self {
        Self {
            label: String::new(),
            description: None,
            dim_description: true,
            value: String::new(),
            disabled: false,
            input: None,
        }
    }
}

/// Maps to: CC `select.tsx#OptionWithDescription.label:29` ReactNode.
/// The verified Text/Fragment label subset uses the existing structured text
/// flow. Select owns the enclosing Text and can prepend its index in the same
/// flow; callers supply child-local styles only. Static getTextContent remains
/// the separate `SelectOptionData.label` projection (function components are
/// not executed there). This does not claim arbitrary layout ReactNode support.
pub type SelectOptionLabel = Vec<StyledSegment>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectLayout {
    #[default]
    Compact,
    Expanded,
    CompactVertical,
}

#[derive(Props)]
pub struct SelectProps {
    pub is_disabled: bool,
    pub hide_indexes: bool,
    pub visible_option_count: usize,
    pub options: Vec<SelectOptionData>,
    /// ReactNode labels keyed by option value. Absent entries use the exact
    /// existing String label path; this does not change selection/navigation.
    pub option_labels: BTreeMap<String, SelectOptionLabel>,
    pub focused_index: usize,
    pub selected_value: Option<String>,
    pub visible_from_index: usize,
    pub layout: SelectLayout,
    /// Maps to: CC `inlineDescriptions` (select.tsx:152): descriptions follow
    /// the label in the compact layout, no two-column layout, and input
    /// options show their label beside the input.
    pub inline_descriptions: bool,
    /// Maps to: CC Select's attachment props. Select owns the selection mode;
    /// the caller remains the pasted-content/removal owner.
    pub pasted_contents: BTreeMap<usize, PastedContent>,
    pub on_remove_image: Handler<usize>,
    /// CC `onOpenEditor(currentValue, setValue)`.
    pub on_open_editor: Handler<(String, Handler<String>)>,
    pub on_image_paste: Handler<crate::utils::image_paste::ClipboardImage>,
    pub clipboard_image_override: Option<crate::utils::image_paste::ClipboardImage>,
    /// CC input option onChange/onSubmit, keyed by the option value.
    pub on_input_change: Handler<(String, String)>,
    pub on_input_submit: Handler<(String, String)>,
    pub on_cancel: Handler<()>,
}

impl Default for SelectProps {
    fn default() -> Self {
        Self {
            is_disabled: false,
            hide_indexes: false,
            visible_option_count: 5,
            options: Vec::new(),
            option_labels: BTreeMap::new(),
            focused_index: 0,
            selected_value: None,
            visible_from_index: 0,
            layout: SelectLayout::Compact,
            inline_descriptions: false,
            pasted_contents: BTreeMap::new(),
            on_remove_image: Handler::default(),
            on_open_editor: Handler::default(),
            on_image_paste: Handler::default(),
            clipboard_image_override: None,
            on_input_change: Handler::default(),
            on_input_submit: Handler::default(),
            on_cancel: Handler::default(),
        }
    }
}

#[component]
pub fn Select(props: &SelectProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let count = props.options.len();
    let editable_inputs =
        !props.on_input_change.is_default() || !props.on_input_submit.is_default();
    // Maps to CC select.tsx:229-240: this map belongs to the mounted Select,
    // separate from an option's caller-owned onChange feedback.
    let input_values = hooks.use_state(|| {
        props
            .options
            .iter()
            .filter_map(|option| {
                option
                    .input
                    .as_ref()
                    .map(|input| (option.value.clone(), input.value.clone()))
            })
            .collect::<BTreeMap<_, _>>()
    });
    // Maps to: CC select.tsx's shared image-selection state. It is shared by
    // input options and reset when the focused input disappears.
    let mut images_selected = hooks.use_state(|| false);
    let mut selected_image_index = hooks.use_state(|| 0usize);
    let image_count = image_attachment_ids(&props.pasted_contents).len();
    let focused_has_input = props
        .options
        .get(props.focused_index)
        .is_some_and(|option| option.input.is_some());
    if images_selected.get() && (image_count == 0 || !focused_has_input) {
        images_selected.set(false);
    }
    if image_count > 0 && selected_image_index.get() >= image_count {
        selected_image_index.set(image_count - 1);
    }
    let enter_image_selection =
        !props.is_disabled && focused_has_input && !images_selected.get() && image_count > 0;
    hooks.use_propagated_terminal_events(move |event| {
        if !enter_image_selection {
            return;
        }
        // If another queued event arrives before the retained rerender, honor
        // the mode entered by the preceding Down event and keep it away from
        // ancestor select/input owners.
        if images_selected.get() {
            event.stop_propagation();
            return;
        }
        let TerminalEvent::Key(KeyEvent {
            code: KeyCode::Down,
            kind,
            ..
        }) = event.event()
        else {
            return;
        };
        if *kind == KeyEventKind::Release {
            return;
        }
        images_selected.set(true);
        selected_image_index.set(image_count - 1);
        event.stop_propagation();
    });
    let images_selected_value = images_selected.get();
    let selected_image_index_value = selected_image_index.get();
    let on_images_selected_change = Handler::from(move |selected| {
        let mut state = images_selected;
        state.set(selected);
    });
    let on_selected_image_index_change = Handler::from(move |index| {
        let mut state = selected_image_index;
        state.set(index);
    });
    let pasted_contents = props.pasted_contents.clone();
    let on_remove_image = props.on_remove_image.clone();
    let on_open_editor = props.on_open_editor.clone();
    let on_image_paste = props.on_image_paste.clone();
    let clipboard_image_override = props.clipboard_image_override.clone();
    let visible_count = props.visible_option_count.max(1).min(count.max(1));
    let start = props
        .visible_from_index
        .min(count.saturating_sub(visible_count));
    let end = (start + visible_count).min(count);
    let max_index_width = if props.hide_indexes {
        0
    } else {
        count.max(1).to_string().len()
    };
    let layout = props.layout;
    // CC :607-611 — the two-column layout is skipped when any visible option
    // is an input option (not supported there, CC returns null for them).
    let has_input_options = props.options[start..end]
        .iter()
        .any(|option| option.input.is_some());
    let has_two_column_descriptions = layout == SelectLayout::Compact
        && !props.inline_descriptions
        && !has_input_options
        && props.options[start..end].iter().any(|option| {
            option
                .description
                .as_deref()
                .is_some_and(|desc| !desc.is_empty())
        });

    if has_two_column_descriptions {
        let max_label_width = props.options[start..end]
            .iter()
            .enumerate()
            .map(|(_, option)| {
                let is_selected = props.selected_value.as_ref() == Some(&option.value);
                let index_width = if props.hide_indexes {
                    0
                } else {
                    max_index_width + 2
                };
                // Indicator + gap + optional index + label + optional checkmark.
                2 + index_width
                    + UnicodeWidthStr::width(option.label.as_str())
                    + if is_selected { 2 } else { 0 }
            })
            .max()
            .unwrap_or(0);

        return element! {
            View(flex_direction: FlexDirection::Column) {
                #(props.options[start..end].iter().enumerate().map(|(visible_idx, option)| {
                    let option_index = start + visible_idx;
                    let is_first_visible_option = option_index == start;
                    let is_last_visible_option = option_index + 1 == end;
                    let are_more_options_below = end < count;
                    let are_more_options_above = start > 0;
                    let is_focused = !props.is_disabled && option_index == props.focused_index;
                    let is_selected = props.selected_value.as_ref() == Some(&option.value);
                    let option_color = if option.disabled {
                        Some(theme.inactive)
                    } else if is_selected {
                        Some(theme.success)
                    } else if is_focused {
                        Some(theme.suggestion)
                    } else {
                        None
                    };
                    let index = if props.hide_indexes {
                        String::new()
                    } else {
                        format!("{}.", option_index + 1)
                    };
                    let padded_index = if props.hide_indexes {
                        String::new()
                    } else {
                        format!("{index:<width$}", width = max_index_width + 2)
                    };
                    let current_label_width = 2
                        + if props.hide_indexes { 0 } else { max_index_width + 2 }
                        + UnicodeWidthStr::width(option.label.as_str())
                        + if is_selected { 2 } else { 0 };
                    let padding = max_label_width.saturating_sub(current_label_width);
                    let description = option.description.clone().unwrap_or_else(|| " ".to_string());
                    // CC ThemedText dimColor resolves to inactive, overriding
                    // the selected/focused color; it is not Ink ANSI dim.
                    let dim_description = option.disabled || option.dim_description;
                    let desc_color = if dim_description {
                        Some(theme.inactive)
                    } else {
                        option_color
                    };

                    let mut label_segments = props.option_labels.get(&option.value).cloned()
                        .unwrap_or_else(|| vec![StyledSegment::new(option.label.clone())]);
                    if !props.hide_indexes {
                        let mut index_segment = StyledSegment::new(padded_index.clone());
                        index_segment.styles.color = Some(theme.inactive);
                        label_segments.insert(0, index_segment);
                    }
                    element! {
                        View(key: option.value.clone(), flex_direction: FlexDirection::Row) {
                          View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
                            #(if is_focused {
                                Some(element! { Text(content: "❯".to_string(), color: theme.suggestion, wrap: TextWrap::Wrap) }.into_any())
                            } else if are_more_options_below && is_last_visible_option {
                                Some(element! { Text(content: "↓".to_string(), color: theme.inactive, wrap: TextWrap::Wrap) }.into_any())
                            } else if are_more_options_above && is_first_visible_option {
                                Some(element! { Text(content: "↑".to_string(), color: theme.inactive, wrap: TextWrap::Wrap) }.into_any())
                            } else {
                                Some(element! { Text(content: " ".to_string(), wrap: TextWrap::Wrap) }.into_any())
                            })
                            Text(content: " ".to_string(), wrap: TextWrap::Wrap)
                            Text(segments: Some(label_segments), color: option_color, wrap: TextWrap::Wrap)
                            #(if is_selected {
                                Some(element! { Text(content: " ✔".to_string(), color: theme.success, wrap: TextWrap::Wrap) })
                            } else { None })
                            #(if padding > 0 {
                                Some(element! { Text(content: " ".repeat(padding), wrap: TextWrap::Wrap) })
                            } else { None })
                          }
                            View(flex_grow: 1.0f32, margin_left: 2u32) {
                                Ansi(content: description, color: desc_color, wrap: TextWrap::Wrap)
                            }
                        }
                    }
                }))
            }
        };
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(props.options[start..end].iter().enumerate().map(|(visible_idx, option)| {
                let option_index = start + visible_idx;
                let is_first_visible_option = option_index == start;
                let is_last_visible_option = option_index + 1 == end;
                let are_more_options_below = end < count;
                let are_more_options_above = start > 0;
                let is_focused = !props.is_disabled && option_index == props.focused_index;
                let is_selected = props.selected_value.as_ref() == Some(&option.value);

                // CC :754-815 — input options render via SelectInputOption in
                // every layout (the two-column branch is already skipped when
                // inputs are present).
                if let Some(input) = option.input.clone() {
                    return element! {
                        View(key: option.value.clone(), flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
                            SelectInputOption(
                                label: option.label.clone(),
                                index: option_index + 1,
                                max_index_width: max_index_width,
                                is_focused: is_focused,
                                is_selected: is_selected,
                                show_scroll_down: are_more_options_below && is_last_visible_option,
                                show_scroll_up: are_more_options_above && is_first_visible_option,
                                input_value: if editable_inputs { input_values.read().get(&option.value).cloned().unwrap_or_default() } else { input.value },
                                on_input_change: if editable_inputs { let change = props.on_input_change.clone(); let key = option.value.clone(); Handler::from(move |text: String| { let mut values = input_values; values.write().insert(key.clone(), text.clone()); change((key.clone(), text)); }) } else { Handler::default() },
                                on_submit: if editable_inputs { let submit = props.on_input_submit.clone(); let key = option.value.clone(); Handler::from(move |text| submit((key.clone(), text))) } else { Handler::default() },
                                on_exit: props.on_cancel.clone(),
                                placeholder: input.placeholder,
                                // CC select-input-option.tsx:119 `showLabelProp
                                // || option.showLabelWithValue`; Select passes
                                // `inlineDescriptions` as showLabel.
                                show_label_with_value: input.show_label_with_value || props.inline_descriptions,
                                label_value_separator: input.label_value_separator,
                                description: option.description.clone(),
                                dim_description: option.disabled || option.dim_description,
                                layout_expanded: layout == SelectLayout::Expanded,
                                pasted_contents: pasted_contents.clone(),
                                on_remove_image: on_remove_image.clone(),
                                images_selected: images_selected_value,
                                selected_image_index: selected_image_index_value,
                                on_images_selected_change: on_images_selected_change.clone(),
                                on_selected_image_index_change: on_selected_image_index_change.clone(),
                                on_open_editor: on_open_editor.clone(),
                                on_image_paste: on_image_paste.clone(),
                                clipboard_image_override: clipboard_image_override.clone(),
                            )
                        }
                    }.into_any();
                }

                let option_color = if option.disabled {
                    Some(theme.inactive)
                } else if is_selected {
                    Some(theme.success)
                } else if is_focused {
                    Some(theme.suggestion)
                } else {
                    None
                };
                // CC :858-860 / :556-558 — the numeric index is its own dim
                // Text; only the label carries the focus/selected color.
                let padded_index = (!props.hide_indexes
                    && matches!(layout, SelectLayout::Compact | SelectLayout::CompactVertical))
                    .then(|| format!(
                        "{:<width$}",
                        format!("{}.", option_index + 1),
                        width = max_index_width + if layout == SelectLayout::CompactVertical { 1 } else { 2 },
                    ));
                let label_segments = props.option_labels.get(&option.value).cloned()
                    .unwrap_or_else(|| vec![StyledSegment::new(option.label.clone())]);
                let description = option.description.clone();
                let dim_description = option.disabled || option.dim_description;
                let desc_color = if dim_description {
                    Some(theme.inactive)
                } else {
                    option_color
                };

                match layout {
                    SelectLayout::Expanded => element! {
                        View(key: option.value.clone(), flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
                            SelectOption(
                                is_focused: is_focused,
                                is_selected: is_selected,
                                show_scroll_down: are_more_options_below && is_last_visible_option,
                                show_scroll_up: are_more_options_above && is_first_visible_option,
                            ) {
                                Text(segments: Some(label_segments.clone()), color: option_color, wrap: TextWrap::Wrap)
                            }
                            #(description.filter(|s| !s.is_empty()).map(|desc| element! {
                                View(padding_left: 2u32) {
                                    Ansi(content: desc, color: desc_color, wrap: TextWrap::Wrap)
                                }
                            }))
                            Text(content: " ", wrap: TextWrap::NoWrap)
                        }
                    }.into_any(),
                    // CC select.tsx:849-906: the ordinary Compact branch
                    // returns SelectOption directly; Expanded and Vertical
                    // retain their non-shrinking per-option columns. With
                    // `inlineDescriptions` the description follows the label
                    // inside its Text (:874-884).
                    SelectLayout::Compact => element! {
                        SelectOption(
                            key: option.value.clone(),
                            is_focused: is_focused,
                            is_selected: is_selected,
                            show_scroll_down: are_more_options_below && is_last_visible_option,
                            show_scroll_up: are_more_options_above && is_first_visible_option,
                        ) {
                            View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
                                #(padded_index.clone().map(|index| element! {
                                    Text(content: index, color: theme.inactive, wrap: TextWrap::Wrap)
                                }))
                                Text(
                                    segments: Some({
                                        let mut segments = label_segments.clone();
                                        if let Some(desc) = description.clone().filter(|desc| props.inline_descriptions && !desc.is_empty()) {
                                            let mut segment = StyledSegment::new(format!(" {desc}"));
                                            segment.styles.color = desc_color;
                                            segments.push(segment);
                                        }
                                        segments
                                    }),
                                    color: option_color,
                                    wrap: TextWrap::Wrap,
                                )
                            }
                        }
                    }.into_any(),
                    SelectLayout::CompactVertical => element! {
                        View(key: option.value.clone(), flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
                            SelectOption(
                                is_focused: is_focused,
                                is_selected: is_selected,
                                show_scroll_down: are_more_options_below && is_last_visible_option,
                                show_scroll_up: are_more_options_above && is_first_visible_option,
                            ) {
                                #(padded_index.clone().map(|index| element! {
                                    Text(content: index, color: theme.inactive, wrap: TextWrap::Wrap)
                                }))
                                Text(segments: Some(label_segments.clone()), color: option_color, wrap: TextWrap::Wrap)
                            }
                            #(description.filter(|s| !s.is_empty()).map(|desc| element! {
                                    View(padding_left: if props.hide_indexes { 4u32 } else { (max_index_width + 4) as u32 }) {
                                        Ansi(content: desc, color: desc_color, wrap: TextWrap::Wrap)
                                    }
                            }))
                        }
                    }.into_any(),
                }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

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
            .find_map(|(row, line)| {
                line.find(needle)
                    .map(|offset| (UnicodeWidthStr::width(&line[..offset]), row))
            })
    }

    // Narrow Expanded/CompactVertical/Compact geometry and overflow remain
    // different from Bun. User deferred layout research on 2026-09-13;
    // preserve every original oracle assertion for an explicit ignored run.
    #[test]
    #[ignore = "Deferred Bun/Yoga vs Taffy narrow-column layout; see research/layout-regressions-deferred-0913.md"]
    fn select_matches_original_kitty_narrow_label_flows() {
        // CC CustomSelect/select.tsx:412-441,540-594,653-755 (two-column
        // Compact enabled at :604-612), rendered by Bun 1.3.14
        // in Kitty: .test/failed11-recheck-0912/select-bun-fresh.txt.
        // Preserve the source's spacing/overflow. The former Compact DESC
        // vertical spelling came from Node and is not the Bun oracle.
        let current_theme = *theme::current();
        let mut differences = Vec::new();
        for (layout, expected) in [
            (
                SelectLayout::Expanded,
                vec![
                    "❯first-token second-token",
                    " third-token fourth-token",
                    "",
                    "  description-token",
                    "  another-token",
                    "  final-token",
                ],
            ),
            (
                SelectLayout::CompactVertical,
                vec![
                    "❯1. first-token",
                    "    second-token",
                    "    third-token",
                    "    fourth-token",
                    "     description-token",
                    "     another-token",
                    "     final-token",
                ],
            ),
            (
                SelectLayout::Compact,
                vec![
                    " 1. first-token             DESC",
                    " second-token third-token",
                    " fourth-token",
                ],
            ),
        ] {
            let canvas = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    View(width: 26u32, flex_direction: FlexDirection::Column) {
                        Select(
                            options: vec![SelectOptionData {
                                label: "first-token second-token third-token fourth-token".to_string(),
                                description: Some(if layout == SelectLayout::Compact { "DESC" } else { "description-token another-token final-token" }.to_string()),
                                dim_description: true,
                                value: "long".to_string(),
                                ..Default::default()
                            }],
                            visible_option_count: 1usize,
                            layout,
                        )
                    }
                }
            }.render(Some(80));
            let mut actual = canvas_lines(&canvas);
            while actual.last().is_some_and(String::is_empty) {
                actual.pop();
            }
            if actual != expected {
                differences.push(format!(
                    "{layout:?}: actual={actual:?}, Bun source={expected:?}"
                ));
            }
        }
        // Exercise every layout even when the first differs; retain the exact
        // source comparison for each rather than hiding the later cases.
        assert!(differences.is_empty(), "{}", differences.join("\n"));
    }

    #[test]
    fn select_matches_original_kitty_react_node_two_column_projection() {
        let current_theme = *theme::current();
        let mut icon = StyledSegment::new("✘ ");
        icon.styles.color = Some(current_theme.error);
        let mut suffix = StyledSegment::new(" (retry)");
        suffix.styles.color = Some(current_theme.inactive);
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Select(
                    options: vec![
                        SelectOptionData { label: "abc (retry)".to_string(), description: Some("D1".to_string()), dim_description: true, value: "node".to_string(), ..Default::default() },
                        SelectOptionData { label: "abcdefghijkl".to_string(), description: Some("D2".to_string()), dim_description: true, value: "plain".to_string(), ..Default::default() },
                    ],
                    option_labels: BTreeMap::from([("node".to_string(), vec![icon, StyledSegment::new("abc"), suffix])]),
                    visible_option_count: 2usize,
                )
            }
        }.render(Some(80));
        let lines = canvas_lines(&canvas);
        assert_eq!(lines[0], "❯ 1. ✘ abc (retry)   D1");
        assert_eq!(lines[1], "  2. abcdefghijkl  D2");
        let (x, y) = find_text_cell(&canvas, "abc (retry)").unwrap();
        assert_eq!(
            canvas.resolved_text_style(x, y).unwrap().color,
            Some(current_theme.suggestion)
        );
        let (x, y) = find_text_cell(&canvas, "(retry)").unwrap();
        let style = canvas.resolved_text_style(x, y).unwrap();
        assert_eq!(style.color, Some(current_theme.inactive));
        assert_eq!(style.weight, Weight::Normal);
        assert!(!style.dim);
    }

    #[test]
    fn select_matches_original_kitty_themed_inactive_and_expanded_plain_weight() {
        // Source oracle: .test/shared-select/original-80.ansi, captured from
        // actual select.tsx + ThemedText in Kitty with the real settings.
        // ThemedText dimColor resolves inactive RGB; it never sets Ink dim.
        let current_theme = *theme::current();
        for layout in [
            SelectLayout::Expanded,
            SelectLayout::CompactVertical,
            SelectLayout::Compact,
        ] {
            let canvas = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    Select(
                        options: vec![SelectOptionData {
                            label: "Focused plain label".to_string(),
                            description: Some("Description default dim".to_string()),
                            dim_description: true,
                            value: "one".to_string(),
                            ..Default::default()
                        }],
                        visible_option_count: 1usize,
                        layout,
                    )
                }
            }
            .render(Some(80));
            let (label_x, label_y) = find_text_cell(&canvas, "Focused").expect("focused label");
            let label_style = canvas.resolved_text_style(label_x, label_y).unwrap();
            assert_eq!(
                label_style.color,
                Some(current_theme.suggestion),
                "{layout:?}"
            );
            assert_eq!(
                label_style.weight,
                Weight::Normal,
                "{layout:?}: source does not bold focused labels"
            );
            assert!(!label_style.dim, "{layout:?}");
            let (desc_x, desc_y) = find_text_cell(&canvas, "Description").expect("description");
            let desc_style = canvas.resolved_text_style(desc_x, desc_y).unwrap();
            assert_eq!(
                desc_style.color,
                Some(current_theme.inactive),
                "{layout:?}: ThemedText color"
            );
            assert_eq!(desc_style.weight, Weight::Normal, "{layout:?}");
            assert!(!desc_style.dim, "{layout:?}: theme inactive is not SGR dim");
            if layout != SelectLayout::Expanded {
                let (index_x, index_y) = find_text_cell(&canvas, "1.").expect("index");
                let style = canvas.resolved_text_style(index_x, index_y).unwrap();
                assert_eq!(style.color, Some(current_theme.inactive), "{layout:?}");
                assert_eq!(style.weight, Weight::Normal, "{layout:?}");
                assert!(!style.dim, "{layout:?}");
            }
        }
    }

    // Description line breaking in the 26-column styled-text fixture remains
    // different from Bun. User deferred research on 2026-09-13; retain
    // the source text/style assertions, not a claim that rendering is fixed.
    #[test]
    #[ignore = "Deferred Bun vs iocraft description wrapping; see research/layout-regressions-deferred-0913.md"]
    fn select_matches_original_kitty_ansi_description_and_wrapping() {
        // Actual source Select oracle: original-extended.ansi includes an
        // inactive description with nested bold/red, and a 26-column wrap.
        let current_theme = *theme::current();
        for layout in [
            SelectLayout::Expanded,
            SelectLayout::CompactVertical,
            SelectLayout::Compact,
        ] {
            let canvas = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    Select(
                        options: vec![SelectOptionData {
                            label: "Ansi label".to_string(),
                            description: Some("normal \x1b[1mbold\x1b[22m \x1b[31mred\x1b[39m".to_string()),
                            dim_description: true,
                            value: "one".to_string(),
                            ..Default::default()
                        }],
                        visible_option_count: 1usize,
                        layout,
                    )
                }
            }.render(Some(80));
            let (x, y) = find_text_cell(&canvas, "bold").expect("bold span");
            let style = canvas.resolved_text_style(x, y).unwrap();
            assert_eq!(style.weight, Weight::Bold, "{layout:?}");
            assert_eq!(style.color, Some(current_theme.inactive), "{layout:?}");
            assert!(!style.dim, "{layout:?}");
            let (x, y) = find_text_cell(&canvas, "red").expect("red span");
            assert_eq!(
                canvas.resolved_text_style(x, y).unwrap().color,
                Some(Color::DarkRed),
                "{layout:?}"
            );
        }
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                View(width: 26u32, flex_direction: FlexDirection::Column) {
                    Select(
                        options: vec![SelectOptionData {
                            label: "Short".to_string(),
                            description: Some("description-token another-token final-token".to_string()),
                            dim_description: true,
                            value: "one".to_string(),
                            ..Default::default()
                        }],
                        visible_option_count: 1usize,
                        layout: SelectLayout::Expanded,
                    )
                }
            }
        }.render(Some(80));
        let lines = canvas_lines(&canvas);
        for token in ["description-token", "another-token", "final-token"] {
            assert!(
                lines.iter().any(|line| line.trim() == token),
                "source wraps each description token: {lines:?}"
            );
        }
    }

    #[test]
    fn compact_vertical_focus_uses_pointer_color_without_cursor_background() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Select(
                    options: vec![SelectOptionData {
                        label: "/clear".to_string(),
                        description: Some("Clear conversation history".to_string()),
                        dim_description: true,
                        value: "clear".to_string(),
                        disabled: false,
                        input: None,
                    }],
                    focused_index: 0usize,
                    visible_option_count: 1usize,
                    layout: SelectLayout::CompactVertical,
                    hide_indexes: true,
                )
            }
        }
        .render(Some(80));
        let text = canvas_lines(&canvas).join("\n");
        let pointer = canvas.cell(0, 0).expect("pointer cell");
        let style = pointer.text_style().expect("pointer style");

        assert_eq!(pointer.text(), Some("❯"), "canvas=\n{text}");
        assert_eq!(
            style.color,
            Some(current_theme.suggestion),
            "canvas=\n{text}"
        );
        assert_eq!(pointer.background_color, None, "canvas=\n{text}");
        assert_eq!(canvas.cursor_declaration(), None, "canvas=\n{text}");
    }

    #[test]
    fn compact_vertical_dim_description_does_not_inherit_focused_color() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Select(
                    options: vec![SelectOptionData {
                        label: "/clear".to_string(),
                        description: Some("Clear conversation history".to_string()),
                        dim_description: true,
                        value: "clear".to_string(),
                        disabled: false,
                        input: None,
                    }],
                    focused_index: 0usize,
                    visible_option_count: 1usize,
                    layout: SelectLayout::CompactVertical,
                    hide_indexes: true,
                )
            }
        }
        .render(Some(80));
        let text = canvas_lines(&canvas).join("\n");
        let (column, row) =
            find_text_cell(&canvas, "Clear conversation history").expect("description cell");
        let style = canvas
            .cell(column, row)
            .and_then(|cell| cell.text_style())
            .expect("description style");

        assert_eq!(style.color, Some(current_theme.inactive), "canvas=\n{text}");
        assert_eq!(style.weight, Weight::Normal, "canvas=\n{text}");
        assert!(!style.dim, "canvas=\n{text}");
    }

    #[test]
    fn compact_with_descriptions_uses_official_two_column_shape() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Select(
                    options: vec![
                        SelectOptionData {
                            label: "Default".to_string(),
                            description: Some("Claude completes coding tasks efficiently".to_string()),
                            dim_description: true,
                            value: "default".to_string(),
                            disabled: false,
                            input: None,
                        },
                        SelectOptionData {
                            label: "Learning".to_string(),
                            description: Some("Claude asks for hands-on practice".to_string()),
                            dim_description: true,
                            value: "Learning".to_string(),
                            disabled: false,
                            input: None,
                        },
                    ],
                    focused_index: 0usize,
                    selected_value: Some("default".to_string()),
                    visible_option_count: 2usize,
                    layout: SelectLayout::Compact,
                )
            }
        }
        .render(Some(100));
        let text = canvas_lines(&canvas).join("\n");
        let first_row = canvas_lines(&canvas)
            .into_iter()
            .find(|line| line.contains("Default"))
            .expect("Default row should render");
        let (pointer_column, pointer_row) = find_text_cell(&canvas, "❯").expect("pointer cell");
        let pointer = canvas
            .cell(pointer_column, pointer_row)
            .expect("pointer cell");
        let (desc_column, desc_row) = find_text_cell(&canvas, "Claude completes")
            .expect("description should render in the same two-column row");
        let desc_style = canvas
            .cell(desc_column, desc_row)
            .and_then(|cell| cell.text_style())
            .expect("description style");

        assert!(
            first_row.contains("Default") && first_row.contains("Claude completes"),
            "compact Select descriptions should render in the official two-column row; canvas=\n{text}"
        );
        assert!(
            first_row.contains("✔"),
            "selected option should show official checkmark; canvas=\n{text}"
        );
        assert_eq!(pointer.background_color, None, "canvas=\n{text}");
        assert_eq!(
            pointer.text_style().and_then(|style| style.color),
            Some(current_theme.suggestion),
            "canvas=\n{text}"
        );
        assert_eq!(
            desc_style.color,
            Some(current_theme.inactive),
            "ThemedText dimColor overrides selected success color; canvas=\n{text}"
        );
        assert_eq!(desc_style.weight, Weight::Normal, "canvas=\n{text}");
        assert!(!desc_style.dim, "canvas=\n{text}");
    }
}
