//! Maps to: CC `components/MessageResponse.tsx`.
//! In CC this component wraps arbitrary children and prefixes them with the
//! subdued `⎿` response marker. The current Rust UI uses the same chrome for
//! simple system/auxiliary text; richer child composition can be added here
//! without changing message-type components.

use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Maps to: CC's private `MessageResponseContext`
/// (`components/MessageResponse.tsx:35-47`) — "a context that is used to
/// determine if the message response is rendered as a descendant of another
/// MessageResponse. We use it to avoid rendering nested ⎿ characters."
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageResponseContext;

#[derive(Default, Props)]
pub struct MessageResponseProps {
    pub content: String,
    pub color: Option<Color>,
    pub use_default_color: bool,
    /// Maps to: CC `height?: number` (`MessageResponse.tsx:9`) — applied to the
    /// row Box together with `overflowY="hidden"` (`:19`), which clamps a
    /// multi-line child to its first line. `AgentTool/UI.tsx` uses it on every
    /// single-line response row (`:304`, `:534`, `:584`, `:647`).
    pub height: Option<u32>,
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn MessageResponse(
    props: &mut MessageResponseProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    // CC `:12-15`: a nested response renders its children bare — no second
    // gutter, and no height clamp either, since both live on the Box this
    // branch skips.
    let is_nested = hooks.try_use_context::<MessageResponseContext>().is_some();
    let color = props.color;
    let children = std::mem::take(&mut props.children);
    let has_children = !children.is_empty();
    let body = if has_children {
        element! { Fragment { #(children) } }.into_any()
    } else if props.use_default_color || color.is_none() {
        element! { Text(content: props.content.clone()) }.into_any()
    } else {
        element! { Text(content: props.content.clone(), color: color) }.into_any()
    };

    if is_nested {
        return element! { Fragment { #(Some(body)) } }.into_any();
    }

    element! {
        ContextProvider(value: Context::owned(MessageResponseContext)) {
            View(
                flex_direction: FlexDirection::Row,
                // CC `:19` `height={height} overflowY="hidden"`. iocraft clips
                // both axes where CC clips only Y; the overwide case wraps into
                // the clipped second row in both trees, so X is inert here.
                height: props.height.map_or(Size::Auto, Size::Length),
                overflow: if props.height.is_some() { Overflow::Hidden } else { Overflow::Visible },
            ) {
                Text(content: "  ⎿ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                #(Some(body))
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(element: AnyElement<'static>) -> String {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                #(Some(element))
            }
        }
        .render(Some(40))
        .to_string()
    }

    /// Maps to: CC `MessageResponse.tsx:12-15` + `:35-47` — the nesting guard.
    ///
    /// The port had no `MessageResponseContext`, so every nested response drew
    /// a second `⎿`. It first became reachable when the AgentTool progress view
    /// started mounting real rows (K4-G1): a nested Bash tool use renders its
    /// own `<MessageResponse height={1}>Running…` inside the agent view's
    /// outer `MessageResponse` (`AgentTool/UI.tsx:665`,
    /// `BashTool/UI.tsx:141-147`).
    #[test]
    fn a_nested_response_renders_no_second_gutter() {
        let nested = render(
            element! {
                MessageResponse {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "outer".to_string())
                        MessageResponse(content: "inner".to_string())
                    }
                }
            }
            .into_any(),
        );
        assert_eq!(
            nested.matches('⎿').count(),
            1,
            "only the outermost response draws the marker: {nested:?}"
        );
        assert!(
            nested.contains("outer") && nested.contains("inner"),
            "{nested:?}"
        );

        // A sibling pair is unaffected — the context is a DESCENDANT test.
        let siblings = render(
            element! {
                View(flex_direction: FlexDirection::Column) {
                    MessageResponse(content: "first".to_string())
                    MessageResponse(content: "second".to_string())
                }
            }
            .into_any(),
        );
        assert_eq!(siblings.matches('⎿').count(), 2, "{siblings:?}");
    }

    /// Maps to: CC `MessageResponse.tsx:9`, `:19` — `height` plus
    /// `overflowY="hidden"` clamps a multi-line child to its first line.
    #[test]
    fn the_height_prop_clamps_a_multi_line_child() {
        let clamped = render(
            element! {
                MessageResponse(height: Some(1u32)) {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "first".to_string())
                        Text(content: "second".to_string())
                    }
                }
            }
            .into_any(),
        );
        assert!(clamped.contains("first"), "{clamped:?}");
        assert!(!clamped.contains("second"), "{clamped:?}");

        let unclamped = render(
            element! {
                MessageResponse {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "first".to_string())
                        Text(content: "second".to_string())
                    }
                }
            }
            .into_any(),
        );
        assert!(unclamped.contains("second"), "{unclamped:?}");
    }
}
