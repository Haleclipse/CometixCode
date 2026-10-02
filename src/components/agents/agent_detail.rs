//! Maps to: CC `components/agents/AgentDetail.tsx:1-148`.

use super::agent_file_utils::get_actual_relative_agent_file_path;
use crate::components::markdown::Markdown;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::tools::agent_tool::agent_color_manager::parse_agent_color_name;
use crate::tools::agent_tool::agent_memory::get_memory_scope_display;
use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use crate::utils::model::agent::get_agent_model_display;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::HashSet;

#[derive(Default, Props)]
pub struct AgentDetailProps<'a> {
    pub agent: Option<AgentDefinition>,
    pub available_tool_names: Vec<String>,
    pub on_back: HandlerMut<'a, ()>,
}

fn permission_mode_name(mode: crate::types::permissions::PermissionMode) -> &'static str {
    use crate::types::permissions::PermissionMode;
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::BypassPermissions => "bypassPermissions",
        PermissionMode::Auto => "auto",
        PermissionMode::Bubble => "bubble",
    }
}

#[component]
pub fn AgentDetail<'a>(
    props: &mut AgentDetailProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(agent) = props.agent.clone() else {
        return element! { Fragment }.into_any();
    };
    let theme = hooks.use_context::<Theme>();
    let mut pending_back = hooks.use_state(|| false);
    if pending_back.get() {
        pending_back.set(false);
        (props.on_back)(());
    }
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    for (name, context) in [
        ("confirm:no", ContextName::Confirmation),
        ("select:accept", ContextName::Select),
    ] {
        let mut pending_back = pending_back;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            name,
            context,
            || true,
            move || {
                pending_back.set(true);
                true
            },
        );
    }

    let cwd = crate::bootstrap::state::get_original_cwd();
    let config_home = crate::utils::env_utils::get_claude_config_home_dir();
    let path = get_actual_relative_agent_file_path(&agent, &cwd, &config_home);
    let available = props
        .available_tool_names
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let has_wildcard = agent.tools.is_none()
        || agent
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|tool| tool == "*"));
    let (valid_tools, invalid_tools) = agent
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| tool.as_str() != "*" && available.contains(*tool))
                .cloned()
                .collect::<Vec<_>>()
        })
        .map(|valid| {
            let invalid = agent
                .tools
                .as_ref()
                .unwrap()
                .iter()
                .filter(|tool| tool.as_str() != "*" && !available.contains(*tool))
                .cloned()
                .collect::<Vec<_>>();
            (valid, invalid)
        })
        .unwrap_or_default();
    let tools_text = if has_wildcard {
        "All tools".to_string()
    } else if agent.tools.as_ref().is_none_or(Vec::is_empty) {
        "None".to_string()
    } else {
        valid_tools.join(", ")
    };
    let invalid_text = (!invalid_tools.is_empty()).then(|| {
        format!(
            "{} Unrecognized: {}",
            crate::constants::figures::get().warning,
            invalid_tools.join(", ")
        )
    });
    let memory = agent
        .memory
        .map(|memory| get_memory_scope_display(Some(memory), &cwd));
    let mut hook_names = agent
        .hooks
        .as_ref()
        .map(|hooks| hooks.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    hook_names.sort();
    let skills = agent
        .skills
        .as_ref()
        .filter(|skills| !skills.is_empty())
        .map(|skills| {
            if skills.len() > 10 {
                format!("{} skills", skills.len())
            } else {
                skills.join(", ")
            }
        });
    let color = agent
        .color
        .as_deref()
        .and_then(parse_agent_color_name)
        .map(|color| {
            crate::utils::iocraft_color::to_iocraft_color(Some(color.official_name()), *theme)
        });
    let is_builtin = agent.source == AgentDefinitionSource::BuiltIn;

    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: path, dim: true)
            View(flex_direction: FlexDirection::Column) {
                MixedText(contents: vec![
                    MixedTextContent::new("Description").weight(Weight::Bold),
                    MixedTextContent::new(" (tells Claude when to use this agent):"),
                ])
                View(margin_left: 2u32) { Text(content: agent.when_to_use.clone()) }
            }
            View(flex_direction: FlexDirection::Row) {
                Text(content: "Tools: ".to_string(), weight: Weight::Bold)
                Text(content: tools_text)
            }
            #(invalid_text.map(|text| element! { Text(content: text, color: theme.warning) }))
            MixedText(contents: vec![
                MixedTextContent::new("Model: ").weight(Weight::Bold),
                MixedTextContent::new(get_agent_model_display(agent.model.as_deref())),
            ])
            #(agent.permission_mode.map(|mode| element! { MixedText(contents: vec![
                MixedTextContent::new("Permission mode: ").weight(Weight::Bold),
                MixedTextContent::new(permission_mode_name(mode)),
            ]) }))
            #(memory.map(|memory| element! { MixedText(contents: vec![
                MixedTextContent::new("Memory: ").weight(Weight::Bold), MixedTextContent::new(memory),
            ]) }))
            #((!hook_names.is_empty()).then(|| element! { MixedText(contents: vec![
                MixedTextContent::new("Hooks: ").weight(Weight::Bold), MixedTextContent::new(hook_names.join(", ")),
            ]) }))
            #(skills.map(|skills| element! { MixedText(contents: vec![
                MixedTextContent::new("Skills: ").weight(Weight::Bold), MixedTextContent::new(skills),
            ]) }))
            #(color.map(|background| element! { View(flex_direction: FlexDirection::Row) {
                Text(content: "Color: ".to_string(), weight: Weight::Bold)
                Text(content: format!(" {} ", agent.agent_type), background_color: Some(background), color: theme.inverse_text)
            }}))
            #(if !is_builtin { agent.system_prompt.clone().map(|prompt| element! { View(flex_direction: FlexDirection::Column) {
                Text(content: "System prompt:".to_string(), weight: Weight::Bold)
                View(margin_left: 2u32, margin_right: 2u32) { Markdown(content: prompt) }
            }}) } else { None })
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_renders_paths_metadata_invalid_tools_and_custom_prompt() {
        let mut agent = AgentDefinition::new(
            "reviewer",
            "Use for code review",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.system_prompt = Some("Review the code carefully.".to_string());
        agent.tools = Some(vec!["Read".to_string(), "Missing".to_string()]);
        agent.model = Some("opus".to_string());
        agent.color = Some("blue".to_string());
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentDetail(agent: Some(agent), available_tool_names: vec!["Read".to_string()])
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains(".claude/agents/reviewer.md"));
        assert!(text.contains("Tools: Read"));
        assert!(text.contains("Unrecognized: Missing"));
        assert!(text.contains("Model: Opus"));
        assert!(text.contains("System prompt:"));
    }

    #[test]
    fn builtin_hides_system_prompt_and_reports_all_tools() {
        let mut agent = AgentDefinition::new("Explore", "Explore", AgentDefinitionSource::BuiltIn);
        agent.system_prompt = Some("hidden prompt".to_string());
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentDetail(agent: Some(agent))
            }
        }
        .render(Some(80))
        .to_string();
        assert!(text.contains("Built-in"));
        assert!(text.contains("Tools: All tools"));
        assert!(!text.contains("hidden prompt"));
    }
}
