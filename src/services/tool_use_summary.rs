//! Tool-use summary generator.
//! Maps to official `services/toolUseSummary/toolUseSummaryGenerator.ts`.
//!
//! The summary is an SDK/mobile progress message generated after a tool batch
//! completes. It is deliberately non-critical: failures are swallowed and the
//! main Query Agent Turn continues unchanged, matching Claude Code.

use crate::services::api::claude::{self, Options, SystemPrompt};
use serde_json::Value;

const TOOL_USE_SUMMARY_SYSTEM_PROMPT: &str = "Write a short summary label describing what these tool calls accomplished. It appears as a single-line row in a mobile app and truncates around 30 characters, so think git-commit-subject, not sentence.\n\nKeep the verb in past tense and the most distinctive noun. Drop articles, connectors, and long location context first.\n\nExamples:\n- Searched in auth/\n- Fixed NPE in UserService\n- Created signup endpoint\n- Read config.json\n- Ran failing tests";

#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub input: Value,
    pub output: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerateToolUseSummaryParams {
    pub tools: Vec<ToolInfo>,
    pub is_non_interactive_session: bool,
    pub last_assistant_text: Option<String>,
}

/// Maps to: CC `services/toolUseSummary/toolUseSummaryGenerator.ts`
/// `generateToolUseSummary(...)`.
///
/// Returns `None` on any failure. Tool-use summaries must never fail the main
/// query turn; official Claude Code catches, logs, and returns `null`.
pub async fn generate_tool_use_summary(params: GenerateToolUseSummaryParams) -> Option<String> {
    if params.tools.is_empty() {
        return None;
    }

    let user_prompt =
        build_tool_use_summary_user_prompt(&params.tools, params.last_assistant_text.as_deref());
    let system_prompt: SystemPrompt = vec![TOOL_USE_SUMMARY_SYSTEM_PROMPT.to_string()];
    let mut options = Options::for_utility("summary", "tool_use_summary_generation".to_string());
    options.query_source = "tool_use_summary_generation".to_string();
    options.enable_prompt_caching = Some(true);
    options.agents = Vec::new();
    options.is_non_interactive_session = params.is_non_interactive_session;
    options.has_append_system_prompt = false;
    options.mcp_tools = Vec::new();

    let response = claude::query_haiku(&system_prompt, &user_prompt, None, &options)
        .await
        .ok()?;
    let summary = response
        .content
        .into_iter()
        .filter_map(|content| match content {
            crate::types::message::AssistantContent::Text(text) => Some(text),
            _ => None,
        })
        .collect::<String>()
        .trim()
        .to_string();

    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

fn build_tool_use_summary_user_prompt(
    tools: &[ToolInfo],
    last_assistant_text: Option<&str>,
) -> String {
    // Maps to CC `generateToolUseSummary(...)` prompt assembly: compact JSON
    // input/output snippets, optional last assistant text as user intent, then
    // a trailing `Label:` completion cue.
    let tool_summaries = tools
        .iter()
        .map(|tool| {
            let input = truncate_json(&tool.input, 300);
            let output = tool
                .output
                .as_ref()
                .map(|value| truncate_json(value, 300))
                .unwrap_or_else(|| "null".to_string());
            format!("Tool: {}\nInput: {}\nOutput: {}", tool.name, input, output)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let context_prefix = last_assistant_text
        .filter(|text| !text.is_empty())
        .map(|text| {
            let truncated = text.chars().take(200).collect::<String>();
            format!("User's intent (from assistant's last message): {truncated}\n\n")
        })
        .unwrap_or_default();

    format!("{context_prefix}Tools completed:\n\n{tool_summaries}\n\nLabel:")
}

fn truncate_json(value: &Value, max_length: usize) -> String {
    // Maps to CC local `truncateJson(...)`.
    let Ok(serialized) = serde_json::to_string(value) else {
        return "[unable to serialize]".to_string();
    };
    if serialized.chars().count() <= max_length {
        return serialized;
    }
    let take = max_length.saturating_sub(3);
    format!("{}...", serialized.chars().take(take).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_tool_use_summary_prompt_matches_official_shape() {
        let prompt = build_tool_use_summary_user_prompt(
            &[ToolInfo {
                name: "Read".to_string(),
                input: serde_json::json!({ "file_path": "Cargo.toml" }),
                output: Some(serde_json::json!("contents")),
            }],
            Some("I will inspect the manifest before answering."),
        );

        assert!(prompt.starts_with(
            "User's intent (from assistant's last message): I will inspect the manifest"
        ));
        assert!(prompt.contains("Tools completed:\n\nTool: Read"));
        assert!(prompt.contains("Input: {\"file_path\":\"Cargo.toml\"}"));
        assert!(prompt.contains("Output: \"contents\""));
        assert!(prompt.ends_with("\n\nLabel:"));
    }

    #[test]
    fn truncate_json_keeps_official_ellipsis_budget() {
        let value = serde_json::json!("abcdefghijklmnopqrstuvwxyz");
        assert_eq!(truncate_json(&value, 10), "\"abcdef...");
    }
}
