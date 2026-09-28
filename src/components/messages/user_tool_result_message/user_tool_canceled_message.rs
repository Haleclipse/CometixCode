//! Maps to: CC `components/messages/UserToolResultMessage/UserToolCanceledMessage.tsx`.

use crate::components::interrupted_by_user::InterruptedByUser;
use crate::components::message_response::MessageResponse;
use iocraft::prelude::*;

/// Maps to: CC
/// `components/messages/UserToolResultMessage/UserToolCanceledMessage.tsx:5-11`
/// `UserToolCanceledMessage`.
#[component]
pub fn UserToolCanceledMessage() -> impl Into<AnyElement<'static>> {
    element! {
        MessageResponse(height: Some(1)) {
            InterruptedByUser
        }
    }
}
