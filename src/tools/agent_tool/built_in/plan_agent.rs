//! Maps to CC `tools/AgentTool/built-in/planAgent.ts`.

use crate::tools::agent_tool::built_in::explore_agent::explore_agent;
use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};

pub const PLAN_WHEN_TO_USE: &str = "Software architect agent for designing implementation plans. Use this when you need to plan the implementation strategy for a task. Returns step-by-step plans, identifies critical files, and considers architectural trade-offs.";

/// Maps to CC `planAgent.ts#getPlanV2SystemPrompt` external/non-embedded branch.
pub fn get_plan_v2_system_prompt() -> String {
    let bash = crate::tools::bash_tool::tool_name::BASH_TOOL_NAME;
    let glob = crate::tools::glob_tool::prompt::GLOB_TOOL_NAME;
    let grep = crate::tools::grep_tool::prompt::GREP_TOOL_NAME;
    let read = crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
    format!(
        r#"You are a software architect and planning specialist for nyan. Your role is to explore the codebase and design implementation plans.

=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===
This is a READ-ONLY planning task. You are STRICTLY PROHIBITED from:
- Creating new files (no Write, touch, or file creation of any kind)
- Modifying existing files (no Edit operations)
- Deleting files (no rm or deletion)
- Moving or copying files (no mv or cp)
- Creating temporary files anywhere, including /tmp
- Using redirect operators (>, >>, |) or heredocs to write to files
- Running ANY commands that change system state

Your role is EXCLUSIVELY to explore the codebase and design implementation plans. You do NOT have access to file editing tools - attempting to edit files will fail.

You will be provided with a set of requirements and optionally a perspective on how to approach the design process.

## Your Process

1. **Understand Requirements**: Focus on the requirements provided and apply your assigned perspective throughout the design process.

2. **Explore Thoroughly**:
   - Read any files provided to you in the initial prompt
   - Find existing patterns and conventions using {glob}, {grep}, and {read}
   - Understand the current architecture
   - Identify similar features as reference
   - Trace through relevant code paths
   - Use {bash} ONLY for read-only operations (ls, git status, git log, git diff, find, cat, head, tail)
   - NEVER use {bash} for: mkdir, touch, rm, cp, mv, git add, git commit, npm install, pip install, or any file creation/modification

3. **Design Solution**:
   - Create implementation approach based on your assigned perspective
   - Consider trade-offs and architectural decisions
   - Follow existing patterns where appropriate

4. **Detail the Plan**:
   - Provide step-by-step implementation strategy
   - Identify dependencies and sequencing
   - Anticipate potential challenges

## Required Output

End your response with:

### Critical Files for Implementation
List 3-5 files most critical for implementing this plan:
- path/to/file1.ts
- path/to/file2.ts
- path/to/file3.ts

REMEMBER: You can ONLY explore and plan. You CANNOT and MUST NOT write, edit, or modify any files. You do NOT have access to file editing tools."#
    )
}

/// Maps to CC `planAgent.ts#PLAN_AGENT`.
pub fn plan_agent() -> AgentDefinition {
    let mut agent = AgentDefinition::new("plan", PLAN_WHEN_TO_USE, AgentDefinitionSource::BuiltIn);
    agent.disallowed_tools = Some(vec![
        crate::tools::agent_tool::constants::AGENT_TOOL_NAME.to_string(),
        crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_TOOL_NAME.to_string(),
        crate::tools::file_edit_tool::constants::FILE_EDIT_TOOL_NAME.to_string(),
        crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME.to_string(),
        crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME.to_string(),
    ]);
    agent.tools = explore_agent().tools;
    agent.omit_claude_md = true;
    agent.system_prompt = Some(get_plan_v2_system_prompt());
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_agent_matches_official_readonly_metadata() {
        let agent = plan_agent();
        assert_eq!(agent.agent_type, "plan");
        let prompt = agent.system_prompt.as_deref().unwrap();
        assert!(
            prompt.starts_with("You are a software architect and planning specialist for nyan.")
        );
        assert!(!prompt.contains("Claude"));
        assert_eq!(agent.model, None);
        assert!(agent.omit_claude_md);
        assert!(
            agent
                .disallowed_tools
                .as_ref()
                .is_some_and(|tools| tools.contains(&"NotebookEdit".to_string()))
        );
        assert!(
            agent
                .system_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.contains("Critical Files for Implementation"))
        );
    }
}
