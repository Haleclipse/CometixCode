//! Shared utilities for displaying agent information.
//!
//! Maps to: CC `tools/AgentTool/agentDisplay.ts`.
//! Used by CLI `agents` handler and interactive `/agents` UI.

use super::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use crate::utils::model::agent::default_subagent_spec;

/// Maps to: CC `agentDisplay.ts` `AgentSource`.
pub type AgentSource = AgentDefinitionSource;

/// Maps to: CC `agentDisplay.ts#AgentSourceGroup`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentSourceGroup {
    pub label: &'static str,
    pub source: AgentDefinitionSource,
}

/// Maps to: CC `agentDisplay.ts#AGENT_SOURCE_GROUPS`.
///
/// Note: CC also lists `localSettings`; Nyan `AgentDefinitionSource` does
/// not yet carry a Local variant (local agent dir loading still deferred).
pub const AGENT_SOURCE_GROUPS: &[AgentSourceGroup] = &[
    AgentSourceGroup {
        label: "User agents",
        source: AgentDefinitionSource::UserSettings,
    },
    AgentSourceGroup {
        label: "Project agents",
        source: AgentDefinitionSource::ProjectSettings,
    },
    AgentSourceGroup {
        label: "Plugin agents",
        source: AgentDefinitionSource::Plugin,
    },
    AgentSourceGroup {
        label: "CLI arg agents",
        source: AgentDefinitionSource::FlagSettings,
    },
    AgentSourceGroup {
        label: "Built-in agents",
        source: AgentDefinitionSource::BuiltIn,
    },
];

/// Maps to: CC `agentDisplay.ts#ResolvedAgent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAgent {
    pub agent: AgentDefinition,
    pub overridden_by: Option<AgentDefinitionSource>,
}

/// Maps to: CC `agentDisplay.ts#resolveAgentOverrides`.
pub fn resolve_agent_overrides(
    all_agents: &[AgentDefinition],
    active_agents: &[AgentDefinition],
) -> Vec<ResolvedAgent> {
    let active_map: std::collections::HashMap<&str, &AgentDefinition> = active_agents
        .iter()
        .map(|agent| (agent.agent_type.as_str(), agent))
        .collect();

    let mut seen = std::collections::HashSet::<String>::new();
    let mut resolved = Vec::new();

    for agent in all_agents {
        let key = format!("{}:{}", agent.agent_type, agent.source.official_name());
        if !seen.insert(key) {
            continue;
        }
        let overridden_by = active_map
            .get(agent.agent_type.as_str())
            .and_then(|active| (active.source != agent.source).then_some(active.source));
        resolved.push(ResolvedAgent {
            agent: agent.clone(),
            overridden_by,
        });
    }

    resolved
}

/// Maps to: CC `agentDisplay.ts#resolveAgentModelDisplay`.
pub fn resolve_agent_model_display(agent: &AgentDefinition) -> Option<String> {
    let model = agent
        .model
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_subagent_spec(&agent.agent_type));
    if model.is_empty() {
        return None;
    }
    Some(if model == "inherit" {
        "inherit".to_string()
    } else {
        model.to_string()
    })
}

/// Maps to: CC `agentDisplay.ts#getOverrideSourceLabel`.
pub fn get_override_source_label(source: AgentDefinitionSource) -> String {
    source_display_name_lowercase(source)
}

fn source_display_name_lowercase(source: AgentDefinitionSource) -> String {
    // Maps to CC `getSourceDisplayName(source).toLowerCase()` for agent sources.
    match source {
        AgentDefinitionSource::UserSettings => "user".to_string(),
        AgentDefinitionSource::ProjectSettings => "project".to_string(),
        AgentDefinitionSource::FlagSettings => "flag".to_string(),
        AgentDefinitionSource::BuiltIn => "built-in".to_string(),
        AgentDefinitionSource::Plugin => "plugin".to_string(),
    }
}

/// Maps to: CC `agentDisplay.ts#compareAgentsByName`.
pub fn compare_agents_by_name(a: &AgentDefinition, b: &AgentDefinition) -> std::cmp::Ordering {
    a.agent_type
        .to_ascii_lowercase()
        .cmp(&b.agent_type.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_overrides_marks_losing_source() {
        let all = vec![
            AgentDefinition::new("reviewer", "user", AgentDefinitionSource::UserSettings),
            AgentDefinition::new(
                "reviewer",
                "project",
                AgentDefinitionSource::ProjectSettings,
            ),
        ];
        let active = vec![AgentDefinition::new(
            "reviewer",
            "project",
            AgentDefinitionSource::ProjectSettings,
        )];
        let resolved = resolve_agent_overrides(&all, &active);
        assert_eq!(resolved.len(), 2);
        let user = resolved
            .iter()
            .find(|r| r.agent.source == AgentDefinitionSource::UserSettings)
            .unwrap();
        assert_eq!(
            user.overridden_by,
            Some(AgentDefinitionSource::ProjectSettings)
        );
        assert_eq!(
            get_override_source_label(AgentDefinitionSource::ProjectSettings),
            "project"
        );
    }

    #[test]
    fn resolve_model_display_handles_inherit_and_default() {
        let mut agent = AgentDefinition::new("x", "use", AgentDefinitionSource::BuiltIn);
        agent.model = Some("inherit".into());
        assert_eq!(
            resolve_agent_model_display(&agent).as_deref(),
            Some("inherit")
        );
        // No model of its own: the type default is what the agent runs on.
        agent.model = None;
        assert_eq!(
            resolve_agent_model_display(&agent).as_deref(),
            Some("inherit")
        );
        for (agent_type, expected) in [("explore", "@light"), ("review", "@heavy")] {
            let agent = AgentDefinition::new(agent_type, "use", AgentDefinitionSource::BuiltIn);
            assert_eq!(
                resolve_agent_model_display(&agent).as_deref(),
                Some(expected),
                "{agent_type}"
            );
        }
    }
}
