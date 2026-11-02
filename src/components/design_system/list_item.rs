//! Maps to: CC `components/design-system/ListItem.tsx`.
//! Shared selector row primitive: pointer, selected checkmark, scroll hints,
//! optional description, default styled label colors, and optional cursor parking.

use crate::utils::theme::Theme;
use iocraft::prelude::*;

const POINTER: &str = "❯";
const TICK: &str = "✔";
const ARROW_DOWN: &str = "↓";
const ARROW_UP: &str = "↑";

#[derive(Default, Props)]
pub struct ListItemProps {
    pub is_focused: bool,
    pub is_selected: bool,
    pub description: Option<String>,
    pub show_scroll_down: bool,
    pub show_scroll_up: bool,
    /// Maps to official `styled`; defaults to true when omitted.
    pub styled: Option<bool>,
    pub disabled: bool,
    /// Text label path used when callers want ListItem to apply official colors.
    pub label: Option<String>,
    /// Maps to official `declareCursor`. Nyan defaults this off so main-screen
    /// native scrollback renders the pointer without a terminal cursor background.
    pub declare_cursor: Option<bool>,
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn ListItem(props: &mut ListItemProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let styled = props.styled.unwrap_or(true);
    let declare_cursor = props.declare_cursor.unwrap_or(false);
    hooks.use_declared_cursor(0, 0, props.is_focused && !props.disabled && declare_cursor);
    let indicator = if props.disabled {
        " "
    } else if props.is_focused {
        POINTER
    } else if props.show_scroll_down {
        ARROW_DOWN
    } else if props.show_scroll_up {
        ARROW_UP
    } else {
        " "
    };
    let indicator_color = if props.is_focused {
        Some(theme.suggestion)
    } else if props.show_scroll_down || props.show_scroll_up {
        Some(theme.inactive)
    } else {
        None
    };
    let text_color = if props.disabled {
        Some(theme.inactive)
    } else if !styled {
        None
    } else if props.is_selected {
        Some(theme.success)
    } else if props.is_focused {
        Some(theme.suggestion)
    } else {
        None
    };
    let description = props.description.clone();
    let label = props.label.clone();

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
                Text(content: indicator, color: indicator_color, wrap: TextWrap::Wrap)
                #(if let Some(label) = label {
                    vec![element! {
                        Text(content: label, color: text_color, wrap: TextWrap::Wrap)
                    }.into_any()]
                } else {
                    std::mem::take(&mut props.children)
                })
                #(if props.is_selected && !props.disabled {
                    Some(element! { Text(content: TICK, color: theme.success, wrap: TextWrap::Wrap) })
                } else { None })
            }
            #(description.filter(|s| !s.is_empty()).map(|desc| element! {
                View(padding_left: 2u32) {
                    Text(content: desc, color: theme.inactive, wrap: TextWrap::Wrap)
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
            .find_map(|(row, line)| line.find(needle).map(|column| (column, row)))
    }

    #[test]
    fn list_item_default_focus_uses_suggestion_color_without_cursor_background() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ListItem(is_focused: true, label: Some("Run".to_string()))
            }
        }
        .render(Some(40));
        let text = canvas_lines(&canvas).join("\n");
        let (pointer_column, pointer_row) = find_text_cell(&canvas, "❯").expect("pointer cell");
        let pointer = canvas
            .cell(pointer_column, pointer_row)
            .expect("pointer cell");
        let pointer_style = pointer.text_style().expect("pointer style");
        let (label_column, label_row) = find_text_cell(&canvas, "Run").expect("label cell");
        let label_style = canvas
            .cell(label_column, label_row)
            .and_then(|cell| cell.text_style())
            .expect("label style");

        assert!(text.contains("❯ Run"), "canvas=\n{text}");
        assert_eq!(
            pointer_style.color,
            Some(current_theme.suggestion),
            "canvas=\n{text}"
        );
        assert_eq!(
            label_style.color,
            Some(current_theme.suggestion),
            "canvas=\n{text}"
        );
        assert_eq!(pointer.background_color, None, "canvas=\n{text}");
        assert_eq!(canvas.cursor_declaration(), None, "canvas=\n{text}");
    }

    #[test]
    fn list_item_can_opt_into_cursor_declaration() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ListItem(
                    is_focused: true,
                    declare_cursor: Some(true),
                    label: Some("Run".to_string()),
                )
            }
        }
        .render(Some(40));

        assert_eq!(
            canvas
                .cursor_declaration()
                .map(|cursor| (cursor.x, cursor.y)),
            Some((0, 0))
        );
    }

    #[test]
    fn list_item_styled_false_leaves_label_default_color() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ListItem(
                    is_focused: true,
                    styled: Some(false),
                    label: Some("Run".to_string()),
                )
            }
        }
        .render(Some(40));
        let text = canvas_lines(&canvas).join("\n");
        let (column, row) = find_text_cell(&canvas, "Run").expect("label cell");
        let style = canvas
            .cell(column, row)
            .and_then(|cell| cell.text_style())
            .expect("label style");

        assert_eq!(style.color, None, "canvas=\n{text}");
        assert_eq!(canvas.cursor_declaration(), None);
    }
}
