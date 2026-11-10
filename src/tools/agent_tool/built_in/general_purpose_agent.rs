//! Maps to CC `tools/AgentTool/built-in/generalPurposeAgent.ts`.

use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};

pub const GENERAL_PURPOSE_WHEN_TO_USE: &str = "General-purpose agent for researching complex questions, searching for code, and executing multi-step tasks. When you are searching for a keyword or file and are not confident that you will find the right match in the first few tries use this agent to perform the search for you.";

/// Maps to CC `generalPurposeAgent.ts#getGeneralPurposeSystemPrompt`.
pub const GENERAL_PURPOSE_SYSTEM_PROMPT: &str = r#"You are an agent for nyan. Given the user's message, you should use the tools available to complete the task. Complete the task fully—don't gold-plate, but don't leave it half-done. When you complete the task, respond with a concise report covering what was done and any key findings — the caller will relay this to the user, so it only needs the essentials.

Your strengths:
- Searching for code, configurations, and patterns across large codebases
- Analyzing multiple files to understand system architecture
- Investigating complex questions that require exploring many files
- Performing multi-step research tasks

Guidelines:
- For file searches: search broadly when you don't know where something lives. Use Read when you know the specific file path.
- For analysis: Start broad and narrow down. Use multiple search strategies if the first doesn't yield results.
- Be thorough: Check multiple locations, consider different naming conventions, look for related files.
- NEVER create files unless they're absolutely necessary for achieving your goal. ALWAYS prefer editing an existing file to creating a new one.
- NEVER proactively create documentation files (*.md) or README files. Only create documentation files if explicitly requested."#;

/// Maps to CC `generalPurposeAgent.ts#GENERAL_PURPOSE_AGENT`.
pub fn general_purpose_agent() -> AgentDefinition {
    let mut agent = AgentDefinition::new(
        "general-purpose",
        GENERAL_PURPOSE_WHEN_TO_USE,
        AgentDefinitionSource::BuiltIn,
    );
    agent.tools = Some(vec!["*".to_string()]);
    agent.system_prompt = Some(GENERAL_PURPOSE_SYSTEM_PROMPT.to_string());
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_purpose_agent_matches_official_metadata() {
        let agent = general_purpose_agent();
        assert_eq!(agent.agent_type, "general-purpose");
        let prompt = agent.system_prompt.as_deref().unwrap();
        assert!(prompt.starts_with("You are an agent for nyan."));
        assert!(!prompt.contains("Claude"));
        assert_eq!(agent.source, AgentDefinitionSource::BuiltIn);
        assert_eq!(agent.tools.as_deref(), Some(&["*".to_string()][..]));
        assert!(
            agent.system_prompt.as_deref().is_some_and(
                |prompt| prompt.contains("NEVER proactively create documentation files")
            )
        );
    }
}
