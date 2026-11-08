//! Maps to: CC `components/agents/utils.ts:1-18`.

use crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource;

pub fn get_agent_source_display_name(source: Option<AgentDefinitionSource>) -> &'static str {
    match source {
        None => "Agents",
        Some(AgentDefinitionSource::BuiltIn) => "Built-in agents",
        Some(AgentDefinitionSource::Plugin) => "Plugin agents",
        Some(AgentDefinitionSource::UserSettings) => "User",
        Some(AgentDefinitionSource::ProjectSettings) => "Project",
        Some(AgentDefinitionSource::FlagSettings) => "Cli flag",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names_match_settings_capitalization() {
        assert_eq!(get_agent_source_display_name(None), "Agents");
        assert_eq!(
            get_agent_source_display_name(Some(AgentDefinitionSource::BuiltIn)),
            "Built-in agents"
        );
        assert_eq!(
            get_agent_source_display_name(Some(AgentDefinitionSource::Plugin)),
            "Plugin agents"
        );
        assert_eq!(
            get_agent_source_display_name(Some(AgentDefinitionSource::ProjectSettings)),
            "Project"
        );
    }
}
