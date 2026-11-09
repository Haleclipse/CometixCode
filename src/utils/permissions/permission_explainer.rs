//! Maps to: CC `utils/permissions/permissionExplainer.ts`.
//!
//! Builds a side-query with forced `explain_command` tool and parses the
//! structured tool-use response into a permission explanation.

use crate::types::message::{AssistantContent, Message};
use crate::utils::config::GlobalConfig;
use crate::utils::side_query::{
    SideQueryOptions, SideQuerySystem, custom_tool, side_query, tool_choice_tool, user_text_message,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::str::FromStr;

/// Maps to: CC `RiskLevel = 'LOW' | 'MEDIUM' | 'HIGH'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl FromStr for RiskLevel {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "LOW" => Ok(Self::Low),
            "MEDIUM" => Ok(Self::Medium),
            "HIGH" => Ok(Self::High),
            _ => Err(()),
        }
    }
}

impl RiskLevel {
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

/// Maps to: CC `RISK_LEVEL_NUMERIC`.
pub fn risk_level_numeric(risk_level: RiskLevel) -> u8 {
    match risk_level {
        RiskLevel::Low => 1,
        RiskLevel::Medium => 2,
        RiskLevel::High => 3,
    }
}

/// Maps to: CC `PermissionExplanation`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionExplanation {
    pub risk_level: RiskLevel,
    pub explanation: String,
    pub reasoning: String,
    pub risk: String,
}

/// Maps to: CC `GenerateExplanationParams`.
#[derive(Clone, Debug)]
pub struct GenerateExplanationParams<'a> {
    pub tool_name: &'a str,
    pub tool_input: &'a Value,
    pub tool_description: Option<&'a str>,
    pub messages: Option<&'a [Message]>,
    /// Maps to: CC `signal.aborted` pre-check (legacy bool; prefer `abort_signal`).
    pub aborted: bool,
    /// Maps to: CC `signal?: AbortSignal` passed into `sideQuery`.
    pub abort_signal: Option<anthropic_sdk::AbortSignal>,
}

pub const SYSTEM_PROMPT: &str = "Analyze shell commands and explain what they do, why you're running them, and potential risks.";

pub const QUERY_SOURCE: &str = "permission_explainer";

/// `selection.utility.permission_explainer`; defaults to `@light` through
/// [`crate::utils::model::agent::default_utility_spec`].
pub const PERMISSION_EXPLAINER_ROLE: &str = "permission_explainer";

/// Soft cap for the explainer side_query so a hung network does not freeze the
/// permission dialog. CC has no hard timeout; we treat timeout as null-on-error.
pub const EXPLAINER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Maps to: CC `EXPLAIN_COMMAND_TOOL`.
pub fn explain_command_tool_schema() -> Value {
    json!({
        "name": "explain_command",
        "description": "Provide an explanation of a shell command",
        "input_schema": {
            "type": "object",
            "properties": {
                "explanation": {
                    "type": "string",
                    "description": "What this command does (1-2 sentences)"
                },
                "reasoning": {
                    "type": "string",
                    "description": "Why YOU are running this command. Start with \"I\" - e.g. \"I need to check the file contents\""
                },
                "risk": {
                    "type": "string",
                    "description": "What could go wrong, under 15 words"
                },
                "riskLevel": {
                    "type": "string",
                    "enum": ["LOW", "MEDIUM", "HIGH"],
                    "description": "LOW (safe dev workflows), MEDIUM (recoverable changes), HIGH (dangerous/irreversible)"
                }
            },
            "required": ["explanation", "reasoning", "risk", "riskLevel"]
        }
    })
}

/// Maps to: CC `formatToolInput(input)`.
pub fn format_tool_input(input: &Value) -> String {
    if let Some(text) = input.as_str() {
        return text.to_string();
    }
    serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string())
}

/// Maps to: CC `extractConversationContext(messages, maxChars)`.
pub fn extract_conversation_context(messages: &[Message], max_chars: usize) -> String {
    let assistant_messages = messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(message) => Some(message),
            _ => None,
        })
        .rev()
        .take(3)
        .collect::<Vec<_>>();

    let mut context_parts = Vec::<String>::new();
    let mut total_chars = 0usize;

    for message in assistant_messages {
        let text_blocks = message
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");

        if text_blocks.is_empty() || total_chars >= max_chars {
            continue;
        }

        let remaining = max_chars - total_chars;
        let mut truncated = text_blocks.chars().take(remaining).collect::<String>();
        if text_blocks.chars().count() > remaining {
            truncated.push_str("...");
        }
        total_chars += truncated.chars().count();
        context_parts.insert(0, truncated);
    }

    context_parts.join("\n\n")
}

/// Maps to: CC `userPrompt` construction in `generatePermissionExplanation`.
pub fn build_permission_explanation_user_prompt(params: &GenerateExplanationParams<'_>) -> String {
    let formatted_input = format_tool_input(params.tool_input);
    let conversation_context = params
        .messages
        .filter(|messages| !messages.is_empty())
        .map(|messages| extract_conversation_context(messages, 1000))
        .unwrap_or_default();

    let mut prompt = format!("Tool: {}\n", params.tool_name);
    if let Some(description) = params
        .tool_description
        .filter(|description| !description.trim().is_empty())
    {
        prompt.push_str(&format!("Description: {description}\n"));
    }
    prompt.push_str("Input:\n");
    prompt.push_str(&formatted_input);
    if !conversation_context.is_empty() {
        prompt.push_str("\nRecent conversation context:\n");
        prompt.push_str(&conversation_context);
    }
    prompt.push_str("\n\nExplain this command in context.");
    prompt
}

/// Maps to: CC `isPermissionExplainerEnabled()`.
pub fn is_permission_explainer_enabled(config: &GlobalConfig) -> bool {
    config.permission_explainer_enabled != Some(false)
}

/// Maps to: CC `RiskAssessmentSchema().safeParse(toolUseBlock.input)`.
pub fn parse_permission_explanation_tool_input(input: &Value) -> Option<PermissionExplanation> {
    let object = input.as_object()?;
    let risk_level = object
        .get("riskLevel")
        .and_then(Value::as_str)
        .and_then(|value| RiskLevel::from_str(value).ok())?;
    let explanation = object.get("explanation")?.as_str()?.to_string();
    let reasoning = object.get("reasoning")?.as_str()?.to_string();
    let risk = object.get("risk")?.as_str()?.to_string();
    Some(PermissionExplanation {
        risk_level,
        explanation,
        reasoning,
        risk,
    })
}

/// Maps to: CC `generatePermissionExplanation(...)`.
///
/// Invokes `side_query` with `SYSTEM_PROMPT`, `EXPLAIN_COMMAND_TOOL`, forced
/// `tool_choice`, and `querySource: 'permission_explainer'`. Returns `None` on
/// disabled/aborted/error (official null-on-error contract).
pub async fn generate_permission_explanation(
    params: GenerateExplanationParams<'_>,
    config: &GlobalConfig,
) -> Option<PermissionExplanation> {
    if params.aborted
        || params.abort_signal.as_ref().is_some_and(|s| s.is_aborted())
        || !is_permission_explainer_enabled(config)
    {
        return None;
    }

    let user_prompt = build_permission_explanation_user_prompt(&params);
    let tool_schema = explain_command_tool_schema();

    // @nyan: the explainer is an auxiliary text task, so it follows the
    // `permission_explainer` utility role (default `@light`) instead of the
    // session's main-loop model. `side_query` resolves the role's model and
    // profile; an empty `model` selects that utility selection.
    let side_opts = SideQueryOptions {
        model: String::new(),
        utility_role: Some(PERMISSION_EXPLAINER_ROLE.to_string()),
        system: Some(SideQuerySystem::Text(SYSTEM_PROMPT.to_string())),
        messages: vec![user_text_message(user_prompt)],
        tools: Some(vec![custom_tool(
            "explain_command",
            tool_schema
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("Provide an explanation of a shell command"),
            tool_schema
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"})),
        )]),
        tool_choice: Some(tool_choice_tool("explain_command")),
        query_source: QUERY_SOURCE.to_string(),
        signal: params.abort_signal.clone(),
        max_tokens: Some(1024),
        max_retries: Some(2),
        ..SideQueryOptions::default()
    };
    let response = match tokio::time::timeout(EXPLAINER_TIMEOUT, side_query(side_opts)).await {
        Ok(Ok(message)) => message,
        Ok(Err(error)) => {
            if params.abort_signal.as_ref().is_some_and(|s| s.is_aborted()) {
                crate::utils::debug::log_for_debugging(&format!(
                    "Permission explainer: request aborted for {}",
                    params.tool_name
                ));
                return None;
            }
            crate::utils::debug::log_for_debugging(&format!("Permission explainer error: {error}"));
            return None;
        }
        Err(_elapsed) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Permission explainer: timed out after {}s for {}",
                EXPLAINER_TIMEOUT.as_secs(),
                params.tool_name
            ));
            return None;
        }
    };

    // Extract structured data from tool use block (CC response.content.find tool_use).
    // side_query returns anthropic_sdk Message.
    use anthropic_sdk::resources::messages::ContentBlock;
    let tool_input = response.content.iter().find_map(|block| match block {
        ContentBlock::ToolUse { name, input, .. } if name == "explain_command" => {
            Some(input.clone())
        }
        _ => None,
    });

    let Some(input) = tool_input else {
        crate::utils::debug::log_for_debugging(
            "Permission explainer: no parsed output in response",
        );
        return None;
    };

    parse_permission_explanation_tool_input(&input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantMessage, StopReason};
    use chrono::Utc;

    fn assistant(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::Text(text.to_string())],
            model: None,
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    #[test]
    fn permission_explainer_config_gate_defaults_enabled_like_official() {
        assert!(is_permission_explainer_enabled(&GlobalConfig::default()));
        let disabled = GlobalConfig {
            permission_explainer_enabled: Some(false),
            ..GlobalConfig::default()
        };
        assert!(!is_permission_explainer_enabled(&disabled));
    }

    #[test]
    fn permission_explainer_formats_input_prompt_and_context_like_official() {
        let input = json!({ "command": "rm -rf target", "description": "clean" });
        let messages = vec![
            assistant("old context"),
            assistant("middle context"),
            assistant("new context"),
        ];
        let params = GenerateExplanationParams {
            tool_name: "Bash",
            tool_input: &input,
            tool_description: Some("Remove build output"),
            messages: Some(&messages),
            aborted: false,
            abort_signal: None,
        };

        let prompt = build_permission_explanation_user_prompt(&params);
        assert!(prompt.starts_with("Tool: Bash\nDescription: Remove build output\nInput:\n"));
        assert!(prompt.contains("\"command\": \"rm -rf target\""));
        assert!(prompt.contains(
            "Recent conversation context:\nold context\n\nmiddle context\n\nnew context"
        ));
        assert!(prompt.ends_with("\n\nExplain this command in context."));
    }

    #[test]
    fn permission_explainer_schema_and_parse_match_official_shape() {
        let schema = explain_command_tool_schema();
        assert_eq!(schema["name"], "explain_command");
        assert_eq!(schema["input_schema"]["required"][3], "riskLevel");
        assert_eq!(risk_level_numeric(RiskLevel::High), 3);

        let parsed = parse_permission_explanation_tool_input(&json!({
            "riskLevel": "MEDIUM",
            "explanation": "Removes build output.",
            "reasoning": "I need to clean stale artifacts.",
            "risk": "Could delete generated files"
        }))
        .expect("valid explanation");
        assert_eq!(parsed.risk_level, RiskLevel::Medium);
        assert_eq!(parsed.risk_level.as_official_str(), "MEDIUM");
        assert_eq!(parsed.reasoning, "I need to clean stale artifacts.");

        assert!(
            parse_permission_explanation_tool_input(&json!({
                "riskLevel": "UNKNOWN",
                "explanation": "x",
                "reasoning": "y",
                "risk": "z"
            }))
            .is_none()
        );
    }
}
