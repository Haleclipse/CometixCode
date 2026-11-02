//! Maps to: CC `components/design-system/ThemedBox.tsx`.
//! Theme-aware View wrapper for Rust callers that already resolve theme keys to
//! `iocraft::Color`. It keeps the common color-resolution seam without trying to
//! duplicate every low-level layout prop.

use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct ThemedBoxProps {
    pub border_color: Option<Color>,
    pub border_top_color: Option<Color>,
    pub border_bottom_color: Option<Color>,
    pub border_left_color: Option<Color>,
    pub border_right_color: Option<Color>,
    pub background_color: Option<Color>,
    pub border_style: Option<BorderStyle>,
    pub flex_direction: Option<FlexDirection>,
    pub padding_left: u32,
    pub padding_right: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn ThemedBox(props: &mut ThemedBoxProps) -> impl Into<AnyElement<'static>> {
    let children = std::mem::take(&mut props.children);

    element! {
        View(
            flex_direction: props.flex_direction.unwrap_or(FlexDirection::Row),
            border_style: props.border_style.unwrap_or(BorderStyle::None),
            border_color: props.border_color,
            border_top_color: props.border_top_color,
            border_bottom_color: props.border_bottom_color,
            border_left_color: props.border_left_color,
            border_right_color: props.border_right_color,
            background_color: props.background_color,
            padding_left: props.padding_left,
            padding_right: props.padding_right,
            padding_top: props.padding_top,
            padding_bottom: props.padding_bottom,
        ) {
            #(children)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themed_box_applies_resolved_border_and_background_colors() {
        let canvas = element! {
            ThemedBox(
                border_style: Some(BorderStyle::Round),
                border_color: Some(Color::Blue),
                background_color: Some(Color::DarkGrey),
                padding_left: 1u32,
                padding_right: 1u32,
            ) {
                Text(content: "x".to_string())
            }
        }
        .render(Some(10));

        let text = canvas.to_string();
        assert!(text.contains("x"), "canvas=\n{text}");
        assert_eq!(
            canvas
                .cell(0, 0)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            Some(Color::Blue)
        );
    }
}
