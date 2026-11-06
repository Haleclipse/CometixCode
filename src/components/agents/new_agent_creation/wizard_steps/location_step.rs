//! Maps to: CC `components/agents/new-agent-creation/wizard-steps/LocationStep.tsx:1-55`.

use super::choice::WizardChoice;
use crate::components::agents::new_agent_creation::types::update;
use crate::components::custom_select::SelectOptionData;
use crate::components::wizard::{WizardDialogLayout, use_wizard};
use iocraft::prelude::*;
use serde_json::json;

#[component]
pub fn LocationStep(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let wizard = use_wizard(&mut hooks);
    let next = wizard.clone();
    let cancel = wizard.clone();
    element! {
        WizardDialogLayout(
            subtitle: Some("Choose location".to_string()),
            footer_text: Some("↑↓ to navigate · Enter to select · Esc to cancel".to_string()),
        ) {
            WizardChoice(
                options: vec![
                    SelectOptionData { label: "Project (.nyan/agents/)".to_string(), value: "projectSettings".to_string(), ..Default::default() },
                    SelectOptionData { label: "Personal (~/.nyan/agents/)".to_string(), value: "userSettings".to_string(), ..Default::default() },
                ],
                on_select: move |value| { next.update_wizard_data(update("location", json!(value))); next.go_next(); },
                on_cancel: move |_| cancel.cancel(),
            )
        }
    }
}
