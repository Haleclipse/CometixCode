//! Maps to: CC `components/messages/UserToolResultMessage/RejectedPlanMessage.tsx`.

use crate::components::markdown::Markdown;
use crate::components::message_response::MessageResponse;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct RejectedPlanMessageProps {
    pub plan: String,
}

#[component]
pub fn RejectedPlanMessage(
    props: &RejectedPlanMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    element! {
        MessageResponse {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "User rejected nyan's plan:".to_string(), color: theme.subtle)
                View(
                    border_style: BorderStyle::Round,
                    border_color: theme.plan_mode,
                    padding_left: 1u32,
                    padding_right: 1u32,
                    overflow: Overflow::Hidden,
                ) {
                    Markdown(content: props.plan.clone())
                }
            }
        }
    }
}
