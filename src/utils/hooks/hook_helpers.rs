//! Shared prompt/agent hook helpers.
//! Maps to: CC `utils/hooks/hookHelpers.ts`.

use crate::types::message::{AssistantContent, Message, UserContent};
use crate::types::tools::Tool;
use serde::{Deserialize, Serialize};

/// Maps to: CC `hookResponseSchema`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Maps to: CC `addArgumentsToPrompt(prompt, jsonInput)`.
pub fn add_arguments_to_prompt(prompt: &str, json_input: &str) -> String {
    crate::utils::argument_substitution::substitute_arguments(prompt, Some(json_input), true, &[])
        .expect("hook substitution has no dynamic argument-name regex")
}

/// Maps to: CC `createStructuredOutputTool()`.
pub fn create_structured_output_tool() -> Tool {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "ok": {
                "type": "boolean",
                "description": "Whether the condition was met"
            },
            "reason": {
                "type": "string",
                "description": "Reason, if the condition was not met"
            }
        },
        "required": ["ok"],
        "additionalProperties": false
    });

    Tool {
        name: crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME.into(),
        description: "Use this tool to return your verification result. You MUST call this tool exactly once at the end of your response.".into(),
        input_schema: schema,
        input_zod_schema: Some(crate::types::tools::InputSchema(hook_response_schema())),
        ..Default::default()
    }
}

/// Maps to: CC `utils/messages.ts#hasSuccessfulToolCall` as used by
/// `registerStructuredOutputEnforcement(...)`.
pub fn has_successful_tool_call(messages: &[Message], tool_name: &str) -> bool {
    let mut most_recent_tool_use_id: Option<String> = None;

    for message in messages.iter().rev() {
        if let Message::Assistant(assistant) = message
            && let Some(tool_use) = assistant.content.iter().find_map(|content| match content {
                AssistantContent::ToolUse(tool_use) if tool_use.name == tool_name => Some(tool_use),
                _ => None,
            })
        {
            most_recent_tool_use_id = Some(tool_use.id.0.clone());
            break;
        }
    }

    let Some(most_recent_tool_use_id) = most_recent_tool_use_id else {
        return false;
    };

    for message in messages.iter().rev() {
        if let Message::User(user) = message
            && let Some(result) = user.content.iter().find_map(|content| match content {
                UserContent::ToolResult(result)
                    if result.tool_use_id.0 == most_recent_tool_use_id =>
                {
                    Some(result)
                }
                _ => None,
            })
        {
            return !result.is_error;
        }
    }

    false
}

/// Maps to: CC `hookHelpers.ts:18-27#hookResponseSchema`.
pub fn hook_response_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        crate::utils::zod::object(vec![
            (
                "ok",
                crate::utils::zod::boolean().describe("Whether the condition was met"),
            ),
            (
                "reason",
                crate::utils::zod::string()
                    .describe("Reason, if the condition was not met")
                    .optional(),
            ),
        ])
    })
}

/// Boolean projection of CC `hookResponseSchema().safeParse(...)`.
pub fn has_valid_structured_output_response(value: &serde_json::Value) -> bool {
    crate::utils::zod::safe_parse(hook_response_schema(), value).is_ok()
}

/// Maps to: CC `registerStructuredOutputEnforcement(setAppState, sessionId)`.
pub fn register_structured_output_enforcement(session_id: &str) {
    let tool_name = crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME.to_string();
    let error_message = format!(
        "You MUST call the {} tool to complete this request. Call this tool now.",
        tool_name
    );
    let _ = super::session_hooks::add_function_hook(
        session_id,
        crate::services::hooks::HookEvent::Stop,
        "",
        std::sync::Arc::new(move |messages| {
            let tool_name = tool_name.clone();
            Box::pin(async move { has_successful_tool_call(&messages, &tool_name) })
        }),
        error_message,
        Some(super::session_hooks::FunctionHookOptions {
            timeout: Some(5_000),
            ..Default::default()
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::ToolUseId;
    use crate::types::message::{AssistantMessage, ToolResult, ToolUseBlock, UserMessage};
    use chrono::Utc;

    fn assistant_tool_use(id: &str, name: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: ToolUseId(id.to_string()),
                name: name.to_string(),
                input: serde_json::json!({}),
            })],
            model: None,
            stop_reason: None,
            usage: None,
        })
    }

    fn user_tool_result(id: &str, is_error: bool) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![UserContent::ToolResult(ToolResult {
                tool_use_id: ToolUseId(id.to_string()),
                content: "ok".to_string(),
                is_error,
                content_blocks: Vec::new(),
                tool_use_result: None,
            })],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })
    }

    #[test]
    fn add_arguments_to_prompt_matches_official_placeholder_forms() {
        assert_eq!(
            add_arguments_to_prompt("check $ARGUMENTS $ARGUMENTS[0] $0", r#"foo bar"#),
            "check foo bar foo foo"
        );
        assert_eq!(
            add_arguments_to_prompt("No placeholder", r#"foo"#),
            "No placeholder\n\nARGUMENTS: foo"
        );
    }

    #[test]
    fn structured_output_tool_matches_official_schema() {
        let tool = create_structured_output_tool();
        assert_eq!(
            tool.name,
            crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME
        );
        assert_eq!(tool.input_schema["required"], serde_json::json!(["ok"]));
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert!(has_valid_structured_output_response(
            &serde_json::json!({"ok": true})
        ));
        assert!(!has_valid_structured_output_response(
            &serde_json::json!({"reason": "missing ok"})
        ));
    }

    #[test]
    fn has_successful_tool_call_matches_most_recent_tool_use_result() {
        let tool_name = crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME;
        let messages = vec![
            assistant_tool_use("old", tool_name),
            user_tool_result("old", true),
        ];
        assert!(!has_successful_tool_call(&messages, tool_name));

        let messages = vec![
            assistant_tool_use("old", tool_name),
            user_tool_result("old", false),
            assistant_tool_use("new", tool_name),
            user_tool_result("new", true),
        ];
        assert!(!has_successful_tool_call(&messages, tool_name));

        let messages = vec![
            assistant_tool_use("new", tool_name),
            user_tool_result("new", false),
        ];
        assert!(has_successful_tool_call(&messages, tool_name));
        assert!(!has_successful_tool_call(&messages, "OtherTool"));
    }

    #[tokio::test]
    async fn register_structured_output_enforcement_adds_stop_function_hook() {
        super::super::session_hooks::clear_all_session_hooks();
        register_structured_output_enforcement("agent-structured");
        let hooks = super::super::session_hooks::get_session_function_hooks(
            "agent-structured",
            Some(crate::services::hooks::HookEvent::Stop),
        );
        let stop = hooks
            .get(&crate::services::hooks::HookEvent::Stop)
            .expect("stop function hooks");
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0].matcher, "");
        assert_eq!(stop[0].hooks[0].timeout, Some(5_000));
        assert!(
            stop[0].hooks[0]
                .error_message
                .contains("You MUST call the StructuredOutput tool")
        );
        let messages = vec![
            assistant_tool_use(
                "toolu_structured",
                crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME,
            ),
            user_tool_result("toolu_structured", false),
        ];
        assert!((stop[0].hooks[0].callback)(messages).await);
        super::super::session_hooks::clear_all_session_hooks();
    }
    #[test]
    fn arguments_prompt_matches_official_unicode_and_string_replacement() {
        assert_eq!(
            add_arguments_to_prompt("中文 $ARGUMENTS", r#"{"x":"$&"}"#),
            "中文 {\"x\":\"$ARGUMENTS\"}"
        );
    }
}
