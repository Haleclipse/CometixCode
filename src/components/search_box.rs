//! Maps to: CC `components/SearchBox.tsx`.
//! Pure rendering component: input state/editing is owned by the caller
//! (`use_search_input`), and the visual cursor is an inverse text cell.

use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct SearchBoxProps {
    pub query: String,
    pub placeholder: Option<String>,
    pub is_focused: bool,
    pub is_terminal_focused: bool,
    pub prefix: Option<String>,
    /// CC `width` (SearchBox.tsx:21, :34): the box's own width, which a
    /// caller sets rather than relying on its parent to stretch it. CC also
    /// accepts a string; no caller passes one.
    pub width: Option<u32>,
    pub cursor_offset: Option<usize>,
    pub borderless: bool,
}

fn cursor_parts(text: &str, cursor_offset: usize) -> (String, String, String) {
    let len = text.chars().count();
    let offset = cursor_offset.min(len);
    let before: String = text.chars().take(offset).collect();
    match text.chars().nth(offset) {
        Some(ch) => {
            let after: String = text.chars().skip(offset + 1).collect();
            (before, ch.to_string(), after)
        }
        None => (before, " ".to_string(), String::new()),
    }
}

#[component]
pub fn SearchBox(props: &SearchBoxProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let placeholder = props
        .placeholder
        .clone()
        .unwrap_or_else(|| "Search…".to_string());
    let prefix = props.prefix.clone().unwrap_or_else(|| "⌕".to_string());
    let offset = props
        .cursor_offset
        .unwrap_or_else(|| props.query.chars().count());
    let query_is_empty = props.query.is_empty();
    let (before_cursor, cursor_cell, after_cursor) = cursor_parts(&props.query, offset);
    let placeholder_first = placeholder.chars().next().unwrap_or(' ').to_string();
    let placeholder_rest: String = placeholder.chars().skip(1).collect();
    let prefix_color = if props.is_focused {
        None
    } else {
        Some(theme.inactive)
    };

    element! {
        View(
            flex_shrink: 0.0f32,
            border_style: if props.borderless { BorderStyle::None } else { BorderStyle::Round },
            border_color: if props.is_focused { theme.suggestion } else { theme.subtle },
            padding_left: if props.borderless { 0u32 } else { 1u32 },
            padding_right: if props.borderless { 0u32 } else { 1u32 },
            width: props.width.map(Size::Length).unwrap_or(Size::Auto),
        ) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: format!("{prefix} "), color: prefix_color, wrap: TextWrap::NoWrap)
                View(flex_direction: FlexDirection::Row, flex_grow: 1.0f32, overflow: Overflow::Hidden, height: 1u32) {
                    #(if props.is_focused && query_is_empty && props.is_terminal_focused {
                        Some(element! { Text(content: placeholder_first.clone(), invert: true, wrap: TextWrap::NoWrap) })
                    } else { None })
                    #(if props.is_focused && query_is_empty && props.is_terminal_focused {
                        Some(element! { Text(content: placeholder_rest.clone(), color: theme.inactive, wrap: TextWrap::NoWrap) })
                    } else { None })
                    #(if props.is_focused && query_is_empty && !props.is_terminal_focused {
                        Some(element! { Text(content: placeholder.clone(), color: theme.inactive, wrap: TextWrap::NoWrap) })
                    } else { None })

                    #(if props.is_focused && !query_is_empty && props.is_terminal_focused {
                        Some(element! { Text(content: before_cursor.clone(), wrap: TextWrap::NoWrap) })
                    } else { None })
                    #(if props.is_focused && !query_is_empty && props.is_terminal_focused {
                        Some(element! { Text(content: cursor_cell.clone(), invert: true, wrap: TextWrap::NoWrap) })
                    } else { None })
                    #(if props.is_focused && !query_is_empty && props.is_terminal_focused {
                        Some(element! { Text(content: after_cursor.clone(), wrap: TextWrap::NoWrap) })
                    } else { None })
                    #(if props.is_focused && !query_is_empty && !props.is_terminal_focused {
                        Some(element! { Text(content: props.query.clone(), wrap: TextWrap::NoWrap) })
                    } else { None })

                    #(if !props.is_focused && query_is_empty {
                        Some(element! { Text(content: placeholder.clone(), color: theme.inactive, wrap: TextWrap::NoWrap) })
                    } else { None })
                    #(if !props.is_focused && !query_is_empty {
                        Some(element! { Text(content: props.query.clone(), color: theme.inactive, wrap: TextWrap::NoWrap) })
                    } else { None })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn find_text_cell(canvas: &Canvas, needle: &str) -> Option<(usize, usize)> {
        for y in 0..canvas.height() {
            for x in 0..canvas.width() {
                if canvas
                    .cell(x, y)
                    .and_then(|cell| cell.text())
                    .is_some_and(|text| text == needle)
                {
                    return Some((x, y));
                }
            }
        }
        None
    }

    #[test]
    fn unfocused_query_uses_inactive_foreground_like_official_dim_color() {
        let theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                SearchBox(
                    query: "needle".to_string(),
                    is_focused: false,
                    is_terminal_focused: true,
                )
            }
        }
        .render(Some(40));
        let text = canvas.to_string();
        let (x, y) = find_text_cell(&canvas, "n").expect("query cell should render");
        let style = canvas
            .resolved_text_style(x, y)
            .expect("query cell should have resolved style");

        assert_eq!(style.color, Some(theme.inactive), "canvas=\n{text}");
    }

    #[test]
    fn focused_prefix_uses_default_foreground_like_official_search_box() {
        let theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                SearchBox(
                    query: "needle".to_string(),
                    is_focused: true,
                    is_terminal_focused: true,
                )
            }
        }
        .render(Some(40));
        let text = canvas.to_string();
        let (x, y) = find_text_cell(&canvas, "⌕").expect("prefix cell should render");
        let style = canvas
            .resolved_text_style(x, y)
            .expect("prefix cell should have resolved style");

        assert_eq!(style.color, None, "canvas=\n{text}");
    }

    #[test]
    fn width_sizes_the_box_inside_a_row_parent() {
        // CC SearchBox.tsx:34 `width={width}`: /plugin's Discover and
        // Installed views pass `terminalWidth - 4` inside a plain (row)
        // `<Box marginBottom={1}>`, which would not stretch the box.
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                View(margin_bottom: 1u32) {
                    SearchBox(query: String::new(), is_focused: false, is_terminal_focused: true, width: Some(36))
                }
            }
        }
        .render(Some(60));
        let text = canvas.to_string();
        let top = text.lines().find(|line| line.contains('╭')).unwrap();
        assert_eq!(top.trim_end().chars().count(), 36, "canvas=\n{text}");
    }
}
