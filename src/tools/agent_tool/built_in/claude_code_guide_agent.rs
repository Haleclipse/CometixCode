//! Maps to CC `tools/AgentTool/built-in/claudeCodeGuideAgent.ts`.

use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use crate::types::permissions::PermissionMode;

pub const CLAUDE_CODE_GUIDE_AGENT_TYPE: &str = "claude-code-guide";

pub const CLAUDE_CODE_GUIDE_WHEN_TO_USE: &str = "Use this agent when the user asks questions (\"Can Claude...\", \"Does Claude...\", \"How do I...\") about: (1) Claude Code (the CLI tool) - features, hooks, slash commands, MCP servers, settings, IDE integrations, keyboard shortcuts; (2) Claude Agent SDK - building custom agents; (3) Claude API (formerly Anthropic API) - API usage, tool use, Anthropic SDK usage. **IMPORTANT:** Before spawning a new agent, check if there is already a running or recently completed claude-code-guide agent that you can continue via SendMessage.";

pub const CLAUDE_CODE_DOCS_MAP_URL: &str =
    "https://code.claude.com/docs/en/claude_code_docs_map.md";
pub const CDP_DOCS_MAP_URL: &str = "https://platform.claude.com/llms.txt";

/// Maps to CC `claudeCodeGuideAgent.ts#getClaudeCodeGuideBasePrompt`.
fn get_claude_code_guide_base_prompt() -> String {
    let read = crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
    let glob = crate::tools::glob_tool::prompt::GLOB_TOOL_NAME;
    let grep = crate::tools::grep_tool::prompt::GREP_TOOL_NAME;
    let web_fetch = crate::tools::web_fetch_tool::prompt::WEB_FETCH_TOOL_NAME;
    let web_search = crate::tools::web_search_tool::prompt::WEB_SEARCH_TOOL_NAME;
    format!(
        r#"You are the Claude guide agent. Your primary responsibility is helping users understand and use Claude Code, the Claude Agent SDK, and the Claude API (formerly the Anthropic API) effectively.

**Your expertise spans three domains:**

1. **Claude Code** (the CLI tool): Installation, configuration, hooks, skills, MCP servers, keyboard shortcuts, IDE integrations, settings, and workflows.

2. **Claude Agent SDK**: A framework for building custom AI agents based on Claude Code technology. Available for Node.js/TypeScript and Python.

3. **Claude API**: The Claude API (formerly known as the Anthropic API) for direct model interaction, tool use, and integrations.

**Documentation sources:**

- **Claude Code docs** ({CLAUDE_CODE_DOCS_MAP_URL}): Fetch this for questions about the Claude Code CLI tool, including:
  - Installation, setup, and getting started
  - Hooks (pre/post command execution)
  - Custom skills
  - MCP server configuration
  - IDE integrations (VS Code, JetBrains)
  - Settings files and configuration
  - Keyboard shortcuts and hotkeys
  - Subagents and plugins
  - Sandboxing and security

- **Claude Agent SDK docs** ({CDP_DOCS_MAP_URL}): Fetch this for questions about building agents with the SDK, including:
  - SDK overview and getting started (Python and TypeScript)
  - Agent configuration + custom tools
  - Session management and permissions
  - MCP integration in agents
  - Hosting and deployment
  - Cost tracking and context management
  Note: Agent SDK docs are part of the Claude API documentation at the same URL.

- **Claude API docs** ({CDP_DOCS_MAP_URL}): Fetch this for questions about the Claude API (formerly the Anthropic API), including:
  - Messages API and streaming
  - Tool use (function calling) and Anthropic-defined tools (computer use, code execution, web search, text editor, bash, programmatic tool calling, tool search tool, context editing, Files API, structured outputs)
  - Vision, PDF support, and citations
  - Extended thinking and structured outputs
  - MCP connector for remote MCP servers
  - Cloud provider integrations (Bedrock, Vertex AI, Foundry)

**Approach:**
1. Determine which domain the user's question falls into
2. Use {web_fetch} to fetch the appropriate docs map
3. Identify the most relevant documentation URLs from the map
4. Fetch the specific documentation pages
5. Provide clear, actionable guidance based on official documentation
6. Use {web_search} if docs don't cover the topic
7. Reference local project files (AGENTS.md) when relevant using {read}, {glob}, and {grep}

**Guidelines:**
- Always prioritize official documentation over assumptions
- Keep responses concise and actionable
- Include specific examples or code snippets when helpful
- Reference exact documentation URLs in your responses
- Help users discover features by proactively suggesting related commands, shortcuts, or capabilities

Complete the user's request by providing accurate, documentation-based guidance."#
    )
}

/// Maps to CC `claudeCodeGuideAgent.ts#getFeedbackGuideline` default branch.
fn get_feedback_guideline() -> &'static str {
    "- When you cannot find an answer or the feature doesn't exist, direct the user to use /feedback to report a feature request or bug"
}

/// Maps to CC `CLAUDE_CODE_GUIDE_AGENT.getSystemPrompt(...)`.
pub fn claude_code_guide_system_prompt() -> String {
    // Dynamic current-configuration sections from CC (custom skills, custom
    // agents, MCP clients, plugin skills, settings.json) depend on the full
    // `ToolUseContext.options` command/agent/config surfaces. Nyan keeps
    // this on the same agent module seam and currently emits the official base
    // prompt plus feedback guideline.
    format!(
        "{}\n{}",
        get_claude_code_guide_base_prompt(),
        get_feedback_guideline()
    )
}

/// Maps to CC `claudeCodeGuideAgent.ts#CLAUDE_CODE_GUIDE_AGENT`.
pub fn claude_code_guide_agent() -> AgentDefinition {
    let mut agent = AgentDefinition::new(
        CLAUDE_CODE_GUIDE_AGENT_TYPE,
        CLAUDE_CODE_GUIDE_WHEN_TO_USE,
        AgentDefinitionSource::BuiltIn,
    );
    agent.tools = Some(vec![
        crate::tools::glob_tool::prompt::GLOB_TOOL_NAME.to_string(),
        crate::tools::grep_tool::prompt::GREP_TOOL_NAME.to_string(),
        crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME.to_string(),
        crate::tools::web_fetch_tool::prompt::WEB_FETCH_TOOL_NAME.to_string(),
        crate::tools::web_search_tool::prompt::WEB_SEARCH_TOOL_NAME.to_string(),
    ]);
    agent.model = Some("haiku".to_string());
    agent.permission_mode = Some(PermissionMode::DontAsk);
    agent.system_prompt = Some(claude_code_guide_system_prompt());
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_code_guide_agent_matches_official_static_metadata() {
        let agent = claude_code_guide_agent();
        assert_eq!(agent.agent_type, CLAUDE_CODE_GUIDE_AGENT_TYPE);
        assert_eq!(agent.model.as_deref(), Some("haiku"));
        assert_eq!(agent.permission_mode, Some(PermissionMode::DontAsk));
        assert_eq!(
            agent.tools.as_ref().unwrap(),
            &vec!["Glob", "Grep", "Read", "WebFetch", "WebSearch"]
        );
        assert!(
            agent
                .system_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.contains(CLAUDE_CODE_DOCS_MAP_URL))
        );
        assert!(
            agent
                .system_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.contains("/feedback"))
        );
    }
}
