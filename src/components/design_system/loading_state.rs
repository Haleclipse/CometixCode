//! Maps to: CC `components/design-system/LoadingState.tsx`.
//! Main-screen loading row: spinner glyph, message, and optional subtitle.

use crate::components::spinner::Spinner;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct LoadingStateProps {
    pub message: String,
    pub bold: bool,
    pub dim_color: bool,
    pub subtitle: Option<String>,
}

#[component]
pub fn LoadingState(props: &LoadingStateProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let color = if props.dim_color {
        Some(theme.inactive)
    } else {
        None
    };

    element! {
        View(flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
            View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
                Spinner
                Text(
                    content: format!(" {}", props.message),
                    color: color,
                    weight: if props.bold { Weight::Bold } else { Weight::Normal },
                    wrap: TextWrap::NoWrap,
                )
            }
            #(props.subtitle.as_ref().map(|subtitle| element! {
                Text(content: subtitle.clone(), dim: true, wrap: TextWrap::NoWrap)
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn loading_state_renders_spinner_message_and_subtitle() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                LoadingState(
                    message: "Loading sessions".to_string(),
                    bold: true,
                    subtitle: Some("Fetching your sessions...".to_string()),
                )
            }
        }
        .render(Some(80));
        let text = canvas.to_string();

        assert!(text.contains("Loading sessions"), "canvas=\n{text}");
        assert!(
            text.contains("Fetching your sessions..."),
            "canvas=\n{text}"
        );
        let message_column = text
            .lines()
            .next()
            .unwrap_or_default()
            .find("Loading")
            .unwrap_or(0);
        assert_eq!(
            canvas
                .resolved_text_style(message_column, 0)
                .expect("message style")
                .weight,
            Weight::Bold
        );
    }
}
