//! Maps to: CC `components/FallbackToolUseRejectedMessage.tsx`.

use crate::components::interrupted_by_user::InterruptedByUser;
use crate::components::message_response::MessageResponse;
use iocraft::prelude::*;

/// Maps to: CC `components/FallbackToolUseRejectedMessage.tsx#FallbackToolUseRejectedMessage`.
#[component]
pub fn FallbackToolUseRejectedMessage() -> impl Into<AnyElement<'static>> {
    element! {
        MessageResponse(height: Some(1)) {
            InterruptedByUser
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::interrupted_by_user::INTERRUPTED_BY_USER_TEXT;
    use crate::utils::theme;

    #[test]
    fn fallback_tool_rejected_message_wraps_interrupted_by_user_copy() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                FallbackToolUseRejectedMessage
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("⎿"), "canvas=\n{text}");
        assert!(text.contains(INTERRUPTED_BY_USER_TEXT), "canvas=\n{text}");
    }
}
