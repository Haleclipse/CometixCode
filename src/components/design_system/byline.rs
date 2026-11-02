//! Maps to: CC `components/design-system/Byline.tsx`.
//! Joins inline metadata children with a dim middot separator.

use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct BylineProps {
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn Byline(props: &mut BylineProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let inherited = hooks
        .try_use_context::<super::keyboard_shortcut_hint::KeyboardShortcutHintStyleContext>()
        .map(|context| *context)
        .unwrap_or_default();
    let children = std::mem::take(&mut props.children);

    element! {
        View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
            #(children.into_iter().enumerate().flat_map(|(index, child)| {
                let mut items = Vec::new();
                if index > 0 {
                    items.push(element! {
                        Text(content: " · ".to_string(), dim: true, italic: inherited.italic, wrap: TextWrap::NoWrap)
                    }.into_any());
                }
                items.push(child);
                items
            }).collect::<Vec<_>>())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;

    #[test]
    fn byline_joins_children_with_dim_middot() {
        let canvas = element! {
            Byline {
                KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                KeyboardShortcutHint(shortcut: "Esc".to_string(), action: "cancel".to_string())
            }
        }
        .render(Some(80));

        assert_eq!(
            canvas.to_string().trim_end(),
            "Enter to confirm · Esc to cancel"
        );
        let middot_column = "Enter to confirm ".chars().count();
        assert_eq!(
            canvas
                .resolved_text_style(middot_column, 0)
                .expect("separator style")
                .weight,
            Weight::Light
        );
    }
}
