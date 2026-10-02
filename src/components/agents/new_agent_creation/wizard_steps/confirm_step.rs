//! Maps to: CC `components/agents/new-agent-creation/wizard-steps/ConfirmStep.tsx:1-168`.

use crate::components::agents::new_agent_creation::types::{AgentWizardFinal, final_agent};
use crate::components::agents::tool_selector::AgentToolOption;
use crate::components::agents::validate_agent::{AgentValidationInput, validate_agent};
use crate::components::wizard::{WizardDialogLayout, use_wizard};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::tools::agent_tool::agent_memory::{AgentMemoryScope, get_memory_scope_display};
use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use crate::utils::model::agent::get_agent_model_display;
use crate::utils::theme::Theme;
use crate::utils::truncate::truncate_to_width;
use iocraft::prelude::*;
use std::collections::HashSet;

pub fn tools_display(tool_names: Option<&[String]>) -> String {
    let Some(names) = tool_names else {
        return "All tools".to_string();
    };
    match names {
        [] => "None".to_string(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => format!(
            "{}, and {}",
            names[..names.len() - 1].join(", "),
            names.last().unwrap()
        ),
    }
}

fn source(value: &str) -> AgentDefinitionSource {
    match value {
        "userSettings" => AgentDefinitionSource::UserSettings,
        "policySettings" => AgentDefinitionSource::PolicySettings,
        "flagSettings" => AgentDefinitionSource::FlagSettings,
        _ => AgentDefinitionSource::ProjectSettings,
    }
}

fn location(agent: &AgentWizardFinal) -> String {
    if agent.source == "projectSettings" {
        format!(".claude/agents/{}.md", agent.agent_type)
    } else {
        crate::utils::env_utils::get_claude_config_home_dir()
            .join("agents")
            .join(format!("{}.md", agent.agent_type))
            .display()
            .to_string()
    }
}

fn memory_scope(value: Option<&str>) -> Option<AgentMemoryScope> {
    match value {
        Some("user") => Some(AgentMemoryScope::User),
        Some("project") => Some(AgentMemoryScope::Project),
        Some("local") => Some(AgentMemoryScope::Local),
        _ => None,
    }
}

#[derive(Default, Props)]
pub struct ConfirmStepProps<'a> {
    pub tools: Vec<AgentToolOption>,
    pub existing_agents: Vec<AgentDefinition>,
    pub on_save: HandlerMut<'a, ()>,
    pub on_save_and_edit: HandlerMut<'a, ()>,
    pub error: Option<String>,
}

#[component]
pub fn ConfirmStep<'a>(
    props: &mut ConfirmStepProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let wizard = use_wizard(&mut hooks);
    let Some(agent) = final_agent(&wizard.wizard_data) else {
        return element! { Fragment }.into_any();
    };
    let available = props
        .tools
        .iter()
        .filter(|tool| tool.available_to_custom_agent)
        .map(|tool| tool.name.clone())
        .collect::<HashSet<_>>();
    let validation = validate_agent(
        &AgentValidationInput {
            agent_type: agent.agent_type.clone(),
            source: source(&agent.source),
            when_to_use: agent.when_to_use.clone(),
            tools: agent.tools.clone(),
            system_prompt: agent.system_prompt.clone(),
        },
        &available,
        &props.existing_agents,
    );
    let mut pending_save = hooks.use_state(|| None::<bool>);
    let action = { *pending_save.read() };
    if let Some(open_editor) = action {
        pending_save.set(None);
        if open_editor {
            (props.on_save_and_edit)(());
        } else {
            (props.on_save)(());
        }
    }
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|value| value.clone());
    let back = wizard.clone();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        || true,
        move || {
            back.go_back();
            true
        },
    );
    for (name, open_editor) in [
        ("confirm:yes", false),
        ("agent:save", false),
        ("agent:saveAndEdit", true),
    ] {
        let mut pending_save = pending_save;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            name,
            ContextName::Confirmation,
            || true,
            move || {
                pending_save.set(Some(open_editor));
                true
            },
        );
    }
    let theme = hooks.use_context::<Theme>();
    let memory = memory_scope(agent.memory.as_deref()).map(|scope| {
        get_memory_scope_display(Some(scope), &crate::bootstrap::state::get_original_cwd())
    });
    let warnings = validation
        .warnings
        .into_iter()
        .map(|warning| element! { Text(content: format!(" • {warning}"), dim: true) })
        .collect::<Vec<_>>();
    let errors = validation
        .errors
        .into_iter()
        .map(|error| element! { Text(content: format!(" • {error}"), color: theme.error) })
        .collect::<Vec<_>>();
    element! {
        WizardDialogLayout(
            subtitle: Some("Confirm and save".to_string()),
            footer_text: Some("s/Enter to save · e to edit in your editor · Esc to cancel".to_string()),
        ) {
            View(flex_direction: FlexDirection::Column) {
                MixedText(contents: vec![MixedTextContent::new("Name").weight(Weight::Bold), MixedTextContent::new(format!(": {}", agent.agent_type))])
                MixedText(contents: vec![MixedTextContent::new("Location").weight(Weight::Bold), MixedTextContent::new(format!(": {}", location(&agent)))])
                MixedText(contents: vec![MixedTextContent::new("Tools").weight(Weight::Bold), MixedTextContent::new(format!(": {}", tools_display(agent.tools.as_deref())))])
                MixedText(contents: vec![MixedTextContent::new("Model").weight(Weight::Bold), MixedTextContent::new(format!(": {}", get_agent_model_display(agent.model.as_deref())))])
                #(memory.map(|memory| element! { MixedText(contents: vec![MixedTextContent::new("Memory").weight(Weight::Bold), MixedTextContent::new(format!(": {memory}"))]) }))
                View(margin_top: 1u32) { MixedText(contents: vec![MixedTextContent::new("Description").weight(Weight::Bold), MixedTextContent::new(" (tells Claude when to use this agent):")]) }
                View(margin_left: 2u32, margin_top: 1u32) { Text(content: truncate_to_width(&agent.when_to_use, 240)) }
                View(margin_top: 1u32) { MixedText(contents: vec![MixedTextContent::new("System prompt").weight(Weight::Bold), MixedTextContent::new(":")]) }
                View(margin_left: 2u32, margin_top: 1u32) { Text(content: truncate_to_width(&agent.system_prompt, 240)) }
                #((!warnings.is_empty()).then(|| element! { View(margin_top: 1u32, flex_direction: FlexDirection::Column) { Text(content: "Warnings:".to_string(), color: theme.warning) #(warnings) } }))
                #((!errors.is_empty()).then(|| element! { View(margin_top: 1u32, flex_direction: FlexDirection::Column) { Text(content: "Errors:".to_string(), color: theme.error) #(errors) } }))
                #(props.error.clone().map(|error| element! { View(margin_top: 1u32) { Text(content: error, color: theme.error) } }))
                View(margin_top: 2u32) { Text(content: "Press s or Enter to save, e to save and edit".to_string(), color: theme.success) }
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_display_preserves_all_none_and_english_joining() {
        assert_eq!(tools_display(None), "All tools");
        assert_eq!(tools_display(Some(&[])), "None");
        assert_eq!(
            tools_display(Some(&["Read".to_string(), "Edit".to_string()])),
            "Read and Edit"
        );
        assert_eq!(
            tools_display(Some(&[
                "Read".to_string(),
                "Edit".to_string(),
                "Bash".to_string()
            ])),
            "Read, Edit, and Bash"
        );
    }
}
