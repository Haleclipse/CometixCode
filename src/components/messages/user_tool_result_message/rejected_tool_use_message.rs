//! Maps to: CC `components/messages/UserToolResultMessage/RejectedToolUseMessage.tsx`.

use crate::components::message_response::MessageResponse;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Maps to: CC
/// `components/messages/UserToolResultMessage/RejectedToolUseMessage.tsx:5-11`
/// `RejectedToolUseMessage`. `<Text dimColor>` is the theme's `inactive`
/// (`ThemedText.tsx:108-112`), not terminal dim.
#[component]
pub fn RejectedToolUseMessage(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    element! {
        MessageResponse(height: Some(1)) {
            Text(content: "Tool use rejected".to_string(), color: theme.inactive)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(width: usize) -> Canvas {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                RejectedToolUseMessage
            }
        }
        .render(Some(width))
    }

    #[test]
    fn rejected_tool_use_is_one_inactive_row() {
        let canvas = render(80);
        let text = canvas.to_string();
        let line = text.lines().next().unwrap_or_default();
        let x = line[..line.find("Tool use rejected").expect("copy")].chars().count();
        let style = canvas.resolved_text_style(x, 0).expect("style");
        assert_eq!(style.color, Some(crate::utils::theme::current().inactive));
        // CC `height={1}`: a narrow terminal clips the copy instead of
        // wrapping it onto a second row.
        let narrow = render(14).to_string();
        assert_eq!(narrow.trim_end().lines().count(), 1, "canvas=\n{narrow}");
    }
}
