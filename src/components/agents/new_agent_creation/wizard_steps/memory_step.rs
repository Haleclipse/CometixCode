//! Maps to: CC `components/agents/new-agent-creation/wizard-steps/MemoryStep.tsx:1-102`.

use super::choice::WizardChoice;
use crate::components::agents::new_agent_creation::types::string;
use crate::components::custom_select::SelectOptionData;
use crate::components::wizard::{WizardDialogLayout, use_wizard};
use iocraft::prelude::*;
use serde_json::{Map, Value, json};

fn option(label: &str, value: &str) -> SelectOptionData {
    SelectOptionData {
        label: label.to_string(),
        value: value.to_string(),
        ..Default::default()
    }
}

#[component]
pub fn MemoryStep(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let wizard = use_wizard(&mut hooks);
    let is_user = string(&wizard.wizard_data, "location").as_deref() == Some("userSettings");
    let options = if is_user {
        vec![
            option("User scope (~/.nyan/agent-memory/) (Recommended)", "user"),
            option("None (no persistent memory)", "none"),
            option("Project scope (.nyan/agent-memory/)", "project"),
            option("Local scope (.nyan/agent-memory-local/)", "local"),
        ]
    } else {
        vec![
            option(
                "Project scope (.nyan/agent-memory/) (Recommended)",
                "project",
            ),
            option("None (no persistent memory)", "none"),
            option("User scope (~/.nyan/agent-memory/)", "user"),
            option("Local scope (.nyan/agent-memory-local/)", "local"),
        ]
    };
    let select = wizard.clone();
    let back = wizard.clone();
    element! {
        WizardDialogLayout(
            subtitle: Some("Configure agent memory".to_string()),
            footer_text: Some("↑↓ to navigate · Enter to select · Esc to go back".to_string()),
        ) {
            WizardChoice(
                options: options,
                on_select: move |value| {
                    let memory = (value != "none").then_some(value);
                    let mut final_agent = select.wizard_data.get("finalAgent").and_then(Value::as_object).cloned();
                    if let Some(agent) = final_agent.as_mut() {
                        match &memory { Some(memory) => { agent.insert("memory".to_string(), json!(memory)); }, None => { agent.remove("memory"); } }
                    }
                    let mut data = Map::new();
                    data.insert("selectedMemory".to_string(), json!(memory));
                    data.insert("finalAgent".to_string(), final_agent.map(Value::Object).unwrap_or(Value::Null));
                    select.update_wizard_data(data);
                    select.go_next();
                },
                on_cancel: move |_| back.go_back(),
            )
        }
    }
}
