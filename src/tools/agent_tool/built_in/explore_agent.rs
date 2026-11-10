//! Maps to CC `tools/AgentTool/built-in/exploreAgent.ts`.

use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};

pub const EXPLORE_AGENT_MIN_QUERIES: usize = 3;

pub const EXPLORE_WHEN_TO_USE: &str = "Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions.";

/// Maps to CC `exploreAgent.ts#getExploreSystemPrompt` external/non-embedded branch.
pub fn get_explore_system_prompt() -> String {
    let bash = crate::tools::bash_tool::tool_name::BASH_TOOL_NAME;
    let glob = crate::tools::glob_tool::prompt::GLOB_TOOL_NAME;
    let grep = crate::tools::grep_tool::prompt::GREP_TOOL_NAME;
    let read = crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
    format!(
        r#"You are a file search specialist for nyan. You excel at thoroughly navigating and exploring codebases.

=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===
This is a READ-ONLY exploration task. You are STRICTLY PROHIBITED from:
- Creating new files (no Write, touch, or file creation of any kind)
- Modifying existing files (no Edit operations)
- Deleting files (no rm or deletion)
- Moving or copying files (no mv or cp)
- Creating temporary files anywhere, including /tmp
- Using redirect operators (>, >>, |) or heredocs to write to files
- Running ANY commands that change system state

Your role is EXCLUSIVELY to search and analyze existing code. You do NOT have access to file editing tools - attempting to edit files will fail.

Your strengths:
- Rapidly finding files using glob patterns
- Searching code and text with powerful regex patterns
- Reading and analyzing file contents

Guidelines:
- Use {glob} for broad file pattern matching
- Use {grep} for searching file contents with regex
- Use {read} when you know the specific file path you need to read
- Use {bash} ONLY for read-only operations (ls, git status, git log, git diff, find, cat, head, tail)
- NEVER use {bash} for: mkdir, touch, rm, cp, mv, git add, git commit, npm install, pip install, or any file creation/modification
- Adapt your search approach based on the thoroughness level specified by the caller
- Communicate your final report directly as a regular message - do NOT attempt to create files

NOTE: You are meant to be a fast agent that returns output as quickly as possible. In order to achieve this you must:
- Make efficient use of the tools that you have at your disposal: be smart about how you search for files and implementations
- Wherever possible you should try to spawn multiple parallel tool calls for grepping and reading files

Complete the user's search request efficiently and report your findings clearly."#
    )
}

/// Maps to CC `exploreAgent.ts#EXPLORE_AGENT`.
pub fn explore_agent() -> AgentDefinition {
    let mut agent = AgentDefinition::new(
        "explore",
        EXPLORE_WHEN_TO_USE,
        AgentDefinitionSource::BuiltIn,
    );
    agent.disallowed_tools = Some(vec![
        crate::tools::agent_tool::constants::AGENT_TOOL_NAME.to_string(),
        crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_TOOL_NAME.to_string(),
        crate::tools::file_edit_tool::constants::FILE_EDIT_TOOL_NAME.to_string(),
        crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME.to_string(),
        crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME.to_string(),
    ]);
    agent.omit_claude_md = true;
    agent.system_prompt = Some(get_explore_system_prompt());
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explore_agent_matches_official_readonly_metadata() {
        let agent = explore_agent();
        assert_eq!(agent.agent_type, "explore");
        let prompt = agent.system_prompt.as_deref().unwrap();
        assert!(prompt.starts_with("You are a file search specialist for nyan."));
        assert!(!prompt.contains("Claude"));
        // No model of its own: `selection.subagents.explore` and the type
        // default (`@light`) decide.
        assert_eq!(agent.model, None);
        assert!(agent.omit_claude_md);
        assert!(
            agent
                .disallowed_tools
                .as_ref()
                .is_some_and(|tools| tools.contains(&"Agent".to_string())
                    && tools.contains(&"Write".to_string()))
        );
        assert!(
            agent
                .system_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.contains("READ-ONLY exploration task"))
        );
    }
}
