//! Maps to: CC `components/agents/new-agent-creation/wizard-steps/MethodStep.tsx:1-65`.

use super::choice::WizardChoice;
use crate::components::custom_select::SelectOptionData;
use crate::components::wizard::{WizardDialogLayout, use_wizard};
use iocraft::prelude::*;
use serde_json::{Map, json};

#[component]
pub fn MethodStep(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let wizard = use_wizard(&mut hooks);
    let select = wizard.clone();
    let back = wizard.clone();
    element! {
        WizardDialogLayout(
            subtitle: Some("Creation method".to_string()),
            footer_text: Some("↑↓ to navigate · Enter to select · Esc to go back".to_string()),
        ) {
            WizardChoice(
                options: vec![
                    SelectOptionData { label: "Generate with nyan (recommended)".to_string(), value: "generate".to_string(), ..Default::default() },
                    SelectOptionData { label: "Manual configuration".to_string(), value: "manual".to_string(), ..Default::default() },
                ],
                on_select: move |value| {
                    let mut data = Map::new();
                    data.insert("method".to_string(), json!(value));
                    data.insert("wasGenerated".to_string(), json!(value == "generate"));
                    select.update_wizard_data(data);
                    if value == "generate" { select.go_next(); } else { select.go_to_step(3); }
                },
                on_cancel: move |_| back.go_back(),
            )
        }
    }
}
