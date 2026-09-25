//! Maps to: CC `components/BaseTextInput.tsx`.
//!
//! Base text input render boundary. Event routing and readline editing remain in
//! `hooks/use_text_input.rs` (the Rust equivalent of CC `useTextInput`); this
//! component renders the hook output, declares the native terminal cursor, owns
//! placeholder rendering via `hooks/renderPlaceholder.ts`, and appends command
//! argument hints using the official gate.

use crate::components::prompt_input::shimmered_input::{HighlightedInput, TextHighlight};
use crate::hooks::render_placeholder::{PlaceholderRenderOptions, render_placeholder};
use crate::hooks::use_text_input::TextInputState;
use crate::utils::cursor::RenderedLine;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseInputState {
    pub rendered_lines: Vec<RenderedLine>,
    pub cursor_line: usize,
    pub cursor_column: usize,
    pub viewport_char_offset: usize,
    pub viewport_char_end: usize,
    /// Maps to CC `useTextInput`'s inline ghost render carrier. The ghost is
    /// shown only when its insertion offset equals the live cursor offset.
    pub inline_ghost_text: Option<crate::hooks::use_typeahead::InlineGhostText>,
}

impl From<&TextInputState> for BaseInputState {
    fn from(state: &TextInputState) -> Self {
        Self {
            rendered_lines: state.rendered_lines.clone(),
            cursor_line: state.cursor_line,
            cursor_column: state.cursor_column,
            viewport_char_offset: state.viewport_char_offset,
            viewport_char_end: state.viewport_char_end,
            inline_ghost_text: state.inline_ghost_text.clone(),
        }
    }
}

#[derive(Default, Props)]
pub struct BaseTextInputProps {
    pub input_state: BaseInputState,
    pub value: String,
    pub placeholder: Option<String>,
    pub focus: bool,
    pub show_cursor: bool,
    pub terminal_focus: bool,
    pub cursor_offset: usize,
    pub argument_hint: Option<String>,
    pub dim_color: bool,
    pub hide_placeholder_text: bool,
    pub highlights: Vec<TextHighlight>,
}

/// Maps to: CC `BaseTextInput.tsx` `commandWithoutArgs` +
/// `showArgumentHint` branch.
pub fn base_text_input_show_argument_hint(
    value: &str,
    argument_hint: Option<&str>,
) -> Option<String> {
    let hint = argument_hint?;
    if value.is_empty() || !value.starts_with('/') {
        return None;
    }
    let command_without_args = value.trim().find(' ').is_none() || value.ends_with(' ');
    command_without_args.then(|| format!("{}{}", if value.ends_with(' ') { "" } else { " " }, hint))
}

fn local_highlights(highlights: &[TextHighlight], start: usize, end: usize) -> Vec<TextHighlight> {
    highlights
        .iter()
        .filter_map(|highlight| {
            let overlap_start = highlight.start.max(start);
            let overlap_end = highlight.end.min(end);
            (overlap_start < overlap_end).then(|| TextHighlight {
                start: overlap_start - start,
                end: overlap_end - start,
                color: highlight.color,
                dim_color: highlight.dim_color,
                inverse: highlight.inverse,
                shimmer_color: highlight.shimmer_color,
                priority: highlight.priority,
            })
        })
        .collect()
}

fn rendered_value(lines: &[RenderedLine]) -> String {
    lines
        .iter()
        .map(|line| format!("{}{}{}", line.before, line.cursor, line.after))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Maps to: CC `components/BaseTextInput.tsx#BaseTextInput`.
#[component]
pub fn BaseTextInput(
    props: &BaseTextInputProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Maps to CC `useDeclaredCursor({ line, column, active })`: park the
    // physical terminal cursor for IME/accessibility coordinates, but keep it
    // hidden in the normal synthetic-cursor path. CC hides the native cursor at
    // the App boundary; the visible caret is the inverted cell rendered below,
    // whose background is the terminal foreground color.
    hooks.use_declared_cursor_with_visibility(
        props.input_state.cursor_line as isize,
        props.input_state.cursor_column as isize,
        props.focus && props.show_cursor && props.terminal_focus,
        false,
    );

    let placeholder = render_placeholder(PlaceholderRenderOptions {
        placeholder: props.placeholder.clone(),
        value: props.value.clone(),
        show_cursor: props.show_cursor,
        focus: props.focus,
        terminal_focus: props.terminal_focus,
        hide_placeholder_text: props.hide_placeholder_text,
    });
    let argument_hint =
        base_text_input_show_argument_hint(&props.value, props.argument_hint.as_deref());
    let ghost_text = props
        .input_state
        .inline_ghost_text
        .as_ref()
        .filter(|ghost| ghost.insert_position == props.cursor_offset)
        .map(|ghost| ghost.text.clone());

    if placeholder.show_placeholder {
        let rendered_placeholder = placeholder.rendered_placeholder.unwrap_or_default();
        return element! {
            View(flex_direction: FlexDirection::Row, overflow: Overflow::Hidden) {
                Ansi(content: rendered_placeholder, wrap: TextWrap::Truncate)
            }
        }
        .into_any();
    }

    let lines = if props.input_state.rendered_lines.is_empty() {
        vec![RenderedLine {
            before: String::new(),
            cursor: String::new(),
            after: String::new(),
        }]
    } else {
        props.input_state.rendered_lines.clone()
    };
    let last_idx = lines.len().saturating_sub(1);
    let argument_hint_for_render = argument_hint.clone();
    let mut visible_offset = props.input_state.viewport_char_offset;
    let rendered_lines = lines
        .into_iter()
        .map(|line| {
            let before_len = line.before.encode_utf16().count();
            let cursor_len = line.cursor.encode_utf16().count();
            let after_len = line.after.encode_utf16().count();
            let before_highlights = local_highlights(
                &props.highlights,
                visible_offset,
                visible_offset + before_len,
            );
            let after_start = visible_offset + before_len + cursor_len;
            let after_highlights =
                local_highlights(&props.highlights, after_start, after_start + after_len);
            visible_offset += before_len + cursor_len + after_len;
            (line, before_highlights, after_highlights)
        })
        .collect::<Vec<_>>();

    element! {
        View(flex_direction: FlexDirection::Column, overflow: Overflow::Hidden) {
            #(rendered_lines.into_iter().enumerate().map(|(idx, (line, before_highlights, after_highlights))| {
                let show_hint_here = idx == last_idx;
                let hint = argument_hint_for_render.clone();
                element! {
                    View(flex_direction: FlexDirection::Row, height: 1u32, overflow: Overflow::Hidden) {
                        #(if before_highlights.is_empty() {
                            element! { Text(content: line.before, dim: props.dim_color, wrap: TextWrap::NoWrap) }.into_any()
                        } else {
                            element! { HighlightedInput(text: line.before, highlights: before_highlights) }.into_any()
                        })
                        // CC TextInput hands `useTextInput` an identity
                        // `invert` when the terminal is blurred (or
                        // accessibility is on), so the cursor cell keeps its
                        // width but loses its inversion; `terminal_focus` is
                        // that gate here.
                        Text(content: line.cursor.clone(), invert: props.show_cursor && props.terminal_focus && !line.cursor.is_empty(), wrap: TextWrap::NoWrap)
                        #(if after_highlights.is_empty() {
                            element! { Text(content: line.after, dim: props.dim_color, wrap: TextWrap::NoWrap) }.into_any()
                        } else {
                            element! { HighlightedInput(text: line.after, highlights: after_highlights) }.into_any()
                        })
                        #(if show_hint_here {
                            hint.map(|hint| element! {
                                Text(content: hint, dim: true, wrap: TextWrap::NoWrap)
                            })
                        } else {
                            None
                        })
                        #(if show_hint_here {
                            ghost_text.clone().map(|ghost| element! {
                                Text(content: ghost, dim: true, wrap: TextWrap::NoWrap)
                            })
                        } else {
                            None
                        })
                    }
                }
            }))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn state(lines: Vec<RenderedLine>) -> BaseInputState {
        BaseInputState {
            rendered_lines: lines,
            cursor_line: 0,
            cursor_column: 0,
            viewport_char_offset: 0,
            viewport_char_end: 0,
            inline_ghost_text: None,
        }
    }

    #[test]
    fn base_text_input_argument_hint_matches_official_gate() {
        assert_eq!(
            base_text_input_show_argument_hint("/commit", Some("[message]")),
            Some(" [message]".to_string())
        );
        assert_eq!(
            base_text_input_show_argument_hint("/commit ", Some("[message]")),
            Some("[message]".to_string())
        );
        assert!(base_text_input_show_argument_hint("/commit now", Some("[message]")).is_none());
        assert!(base_text_input_show_argument_hint("hello", Some("[message]")).is_none());
        assert!(base_text_input_show_argument_hint("/commit", None).is_none());
    }

    #[test]
    fn base_text_input_renders_placeholder_via_official_helper() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                BaseTextInput(
                    input_state: state(Vec::new()),
                    value: String::new(),
                    placeholder: Some("Ask Claude".to_string()),
                    focus: true,
                    show_cursor: true,
                    terminal_focus: true,
                )
            }
        }
        .render(Some(80));
        let text = canvas.to_string();

        assert_eq!(text, "Ask Claude\n");
        assert!(
            canvas
                .resolved_text_style(0, 0)
                .is_some_and(|style| style.invert)
        );
        assert!(
            canvas
                .resolved_text_style(1, 0)
                .is_some_and(|style| !style.invert)
        );
    }

    #[test]
    fn base_text_input_renders_lines_cursor_and_argument_hint() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                BaseTextInput(
                    input_state: state(vec![RenderedLine {
                        before: "/commit".to_string(),
                        cursor: " ".to_string(),
                        after: String::new(),
                    }]),
                    value: "/commit".to_string(),
                    focus: true,
                    show_cursor: true,
                    terminal_focus: true,
                    argument_hint: Some("[message]".to_string()),
                )
            }
        }
        .render(Some(80))
        .to_string();

        assert!(text.contains("/commit"), "canvas=\n{text}");
        assert!(text.contains("[message]"), "canvas=\n{text}");
    }

    #[test]
    fn base_text_input_cursor_uses_hidden_declared_cursor_and_terminal_fg_cell() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                BaseTextInput(
                    input_state: state(vec![RenderedLine {
                        before: "abc".to_string(),
                        cursor: "d".to_string(),
                        after: "ef".to_string(),
                    }]),
                    value: "abcdef".to_string(),
                    focus: true,
                    show_cursor: true,
                    terminal_focus: true,
                    dim_color: true,
                )
            }
        }
        .render(Some(80));

        let declaration = canvas.cursor_declaration().expect("cursor declaration");
        assert!(
            !declaration.visible,
            "normal text input should park but hide the physical cursor so the visible caret is the synthetic terminal-fg inverted cell"
        );
        let cursor_style = canvas.resolved_text_style(3, 0).expect("cursor style");
        assert!(cursor_style.invert, "cursor cell should be inverted");
        assert_eq!(
            cursor_style.color, None,
            "cursor cell must not force a theme foreground; inversion should use the terminal foreground color"
        );
        assert_eq!(
            cursor_style.weight,
            Weight::Normal,
            "cursor cell should not inherit dim input text styling"
        );
        assert_eq!(
            canvas
                .resolved_text_style(0, 0)
                .expect("before style")
                .weight,
            Weight::Light,
            "non-cursor input text still honors dim_color"
        );
    }

    // CC TextInput.tsx:82-84: a blurred terminal (or accessibility mode)
    // makes `invert` the identity, so the cursor cell keeps its width and
    // text but is not inverted; the declared cursor is inactive too.
    #[test]
    fn base_text_input_blurred_terminal_keeps_cursor_cell_without_inversion() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                BaseTextInput(
                    input_state: state(vec![RenderedLine {
                        before: "abc".to_string(),
                        cursor: "d".to_string(),
                        after: "ef".to_string(),
                    }]),
                    value: "abcdef".to_string(),
                    focus: true,
                    show_cursor: true,
                    terminal_focus: false,
                )
            }
        }
        .render(Some(80));

        assert!(
            canvas.to_string().starts_with("abcdef"),
            "blurred input keeps its text: {:?}",
            canvas.to_string()
        );
        let cursor_style = canvas.resolved_text_style(3, 0).expect("cursor style");
        assert!(
            !cursor_style.invert,
            "cursor cell must not be inverted while the terminal is blurred"
        );
        assert!(
            canvas.cursor_declaration().is_none_or(|declaration| !declaration.visible),
            "no visible physical cursor while blurred"
        );
    }

    #[test]
    fn rendered_value_joins_rendered_lines_like_base_input_state() {
        assert_eq!(
            rendered_value(&[
                RenderedLine {
                    before: "a".to_string(),
                    cursor: "b".to_string(),
                    after: "c".to_string(),
                },
                RenderedLine {
                    before: "d".to_string(),
                    cursor: String::new(),
                    after: "e".to_string(),
                },
            ]),
            "abc\nde"
        );
    }
}
