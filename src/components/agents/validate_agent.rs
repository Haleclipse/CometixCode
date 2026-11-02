//! Maps to: CC `components/agents/validateAgent.ts:1-109`.

use super::types::AgentValidationResult;
use super::utils::get_agent_source_display_name;
use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static AGENT_TYPE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9-]*[a-zA-Z0-9]$").expect("valid agent type regex")
});

pub fn validate_agent_type(agent_type: &str) -> Option<String> {
    if agent_type.is_empty() {
        return Some("Agent type is required".to_string());
    }
    if !AGENT_TYPE_PATTERN.is_match(agent_type) {
        return Some("Agent type must start and end with alphanumeric characters and contain only letters, numbers, and hyphens".to_string());
    }
    if agent_type.len() < 3 {
        return Some("Agent type must be at least 3 characters long".to_string());
    }
    if agent_type.len() > 50 {
        return Some("Agent type must be less than 50 characters".to_string());
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentValidationInput {
    pub agent_type: String,
    pub source: AgentDefinitionSource,
    pub when_to_use: String,
    pub tools: Option<Vec<String>>,
    pub system_prompt: String,
}

pub fn validate_agent(
    agent: &AgentValidationInput,
    available_tool_names: &HashSet<String>,
    existing_agents: &[AgentDefinition],
) -> AgentValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if let Some(error) = validate_agent_type(&agent.agent_type) {
        errors.push(error);
    }
    if !agent.agent_type.is_empty()
        && let Some(duplicate) = existing_agents.iter().find(|candidate| {
            candidate.agent_type == agent.agent_type && candidate.source != agent.source
        })
    {
        errors.push(format!(
            "Agent type \"{}\" already exists in {}",
            agent.agent_type,
            get_agent_source_display_name(Some(duplicate.source))
        ));
    }

    if agent.when_to_use.is_empty() {
        errors.push("Description (description) is required".to_string());
    } else if agent.when_to_use.len() < 10 {
        warnings
            .push("Description should be more descriptive (at least 10 characters)".to_string());
    } else if agent.when_to_use.len() > 5_000 {
        warnings.push("Description is very long (over 5000 characters)".to_string());
    }

    match &agent.tools {
        None => warnings.push("Agent has access to all tools".to_string()),
        Some(tools) if tools.is_empty() => warnings
            .push("No tools selected - agent will have very limited capabilities".to_string()),
        Some(tools) => {
            let invalid = tools
                .iter()
                .filter(|tool| tool.as_str() != "*" && !available_tool_names.contains(*tool))
                .cloned()
                .collect::<Vec<_>>();
            if !invalid.is_empty() {
                errors.push(format!("Invalid tools: {}", invalid.join(", ")));
            }
        }
    }

    if agent.system_prompt.is_empty() {
        errors.push("System prompt is required".to_string());
    } else if agent.system_prompt.len() < 20 {
        errors.push("System prompt is too short (minimum 20 characters)".to_string());
    } else if agent.system_prompt.len() > 10_000 {
        warnings.push("System prompt is very long (over 10,000 characters)".to_string());
    }

    AgentValidationResult {
        is_valid: errors.is_empty(),
        errors,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> AgentValidationInput {
        AgentValidationInput {
            agent_type: "reviewer".to_string(),
            source: AgentDefinitionSource::ProjectSettings,
            when_to_use: "Review changes carefully".to_string(),
            tools: Some(vec!["Read".to_string()]),
            system_prompt: "You review changes carefully and report problems.".to_string(),
        }
    }

    #[test]
    fn agent_type_rules_preserve_order_and_exact_copy() {
        assert_eq!(
            validate_agent_type(""),
            Some("Agent type is required".to_string())
        );
        assert!(
            validate_agent_type("-bad")
                .unwrap()
                .contains("start and end")
        );
        assert_eq!(
            validate_agent_type("ab"),
            Some("Agent type must be at least 3 characters long".to_string())
        );
        assert!(validate_agent_type("a-b").is_none());
    }

    #[test]
    fn full_validation_reports_duplicate_tools_prompt_and_warnings() {
        let mut candidate = input();
        candidate.when_to_use = "short".to_string();
        candidate.tools = Some(vec!["Missing".to_string()]);
        candidate.system_prompt = "tiny".to_string();
        let existing = vec![AgentDefinition::new(
            "reviewer",
            "other",
            AgentDefinitionSource::UserSettings,
        )];
        let result = validate_agent(&candidate, &HashSet::from(["Read".to_string()]), &existing);
        assert!(!result.is_valid);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("already exists in User"))
        );
        assert!(
            result
                .errors
                .iter()
                .any(|error| error == "Invalid tools: Missing")
        );
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("too short"))
        );
        assert_eq!(
            result.warnings,
            vec!["Description should be more descriptive (at least 10 characters)"]
        );
    }

    #[test]
    fn undefined_and_empty_tools_have_distinct_warnings() {
        let mut candidate = input();
        candidate.tools = None;
        assert_eq!(
            validate_agent(&candidate, &HashSet::new(), &[]).warnings,
            vec!["Agent has access to all tools"]
        );
        candidate.tools = Some(Vec::new());
        assert_eq!(
            validate_agent(&candidate, &HashSet::new(), &[]).warnings,
            vec!["No tools selected - agent will have very limited capabilities"]
        );
    }
}
