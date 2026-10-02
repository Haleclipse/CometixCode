//! Prompt hook execution.
//! Maps to: CC `utils/hooks/execPromptHook.ts`.
//!
//! Prompt hooks ask a small model to evaluate a condition and return
//! `{ok:boolean, reason?:string}`. This module owns the prompt-hook execution
//! boundary; matching/orchestration remains in the shared hook runner.

use super::hook_helpers::{HookResponse, add_arguments_to_prompt};
use crate::services::api::claude::{Options, SystemPrompt, query_model_without_streaming};
use crate::services::hooks::{HookBlockingError, HookEvent, HookOutcome, HookResult};
use crate::tool::ToolUseContext;
use crate::types::message::{
    AssistantContent, AssistantMessage, Message, UserContent, UserMessage,
};
use crate::types::tools::Tool;
use crate::utils::thinking::ThinkingConfig;
use chrono::Utc;
use futures::future::BoxFuture;
use std::sync::Arc;

/// Maps to: CC `PromptHook` from `utils/settings/types.ts` as consumed by
/// `execPromptHook(...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptHook {
    pub prompt: String,
    /// Timeout in seconds, matching CC settings shape.
    pub timeout: Option<u64>,
    pub model: Option<String>,
    pub status_message: Option<String>,
}

/// Request assembled for the prompt-hook model call.
/// Maps to the `queryModelWithoutStreaming({...})` argument object in CC
/// `execPromptHook(...)`.
#[derive(Clone, Debug)]
pub struct PromptHookRequest {
    pub hook_name: String,
    pub hook_event: HookEvent,
    pub processed_prompt: String,
    pub messages: Vec<Message>,
    pub system_prompt: SystemPrompt,
    pub tools: Vec<Tool>,
    pub model: String,
    pub timeout_ms: u64,
    pub agent_id: Option<String>,
}

/// Maps to the assistant response consumed by CC `extractTextContent(...)`.
#[derive(Clone, Debug)]
pub struct PromptHookResponse {
    pub message: AssistantMessage,
}

pub type PromptHookExecutor = Arc<
    dyn Fn(PromptHookRequest) -> BoxFuture<'static, anyhow::Result<PromptHookResponse>>
        + Send
        + Sync,
>;

fn small_fast_model() -> String {
    crate::utils::process_env::var("COMETIX_SMALL_FAST_MODEL").unwrap_or_else(|| "claude-3-5-haiku-latest".into())
}

fn assistant_text_content(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.as_str()),
            AssistantContent::Advisor { content, .. } => content.text(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn hook_event_name(event: HookEvent) -> &'static str {
    event.as_str()
}

fn hook_success_message(hook_name: &str, tool_use_id: &str, hook_event: HookEvent) -> String {
    serde_json::json!({
        "type": "hook_success",
        "hookName": hook_name,
        "toolUseID": tool_use_id,
        "hookEvent": hook_event_name(hook_event),
        "content": "",
    })
    .to_string()
}

fn hook_error_message(
    hook_name: &str,
    tool_use_id: &str,
    hook_event: HookEvent,
    stderr: &str,
    stdout: &str,
) -> String {
    serde_json::json!({
        "type": "hook_non_blocking_error",
        "hookName": hook_name,
        "toolUseID": tool_use_id,
        "hookEvent": hook_event_name(hook_event),
        "stderr": stderr,
        "stdout": stdout,
        "exitCode": 1,
    })
    .to_string()
}

fn message_from_prompt(prompt: String) -> Message {
    Message::User(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        content: vec![UserContent::Text(prompt)],
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

/// Maps to: CC `execPromptHook(...)`, with the model executor injected for
/// deterministic tests and safe orchestration.
pub async fn exec_prompt_hook_with_executor(
    hook: &PromptHook,
    hook_name: &str,
    hook_event: HookEvent,
    json_input: &str,
    tool_use_context: &ToolUseContext,
    messages: Option<&[Message]>,
    tool_use_id: Option<&str>,
    executor: PromptHookExecutor,
) -> HookResult {
    let effective_tool_use_id = tool_use_id
        .map(str::to_string)
        .unwrap_or_else(|| format!("hook-{}", uuid::Uuid::new_v4()));

    let processed_prompt = add_arguments_to_prompt(&hook.prompt, json_input);
    let user_message = message_from_prompt(processed_prompt.clone());
    let mut messages_to_query = messages.map_or_else(Vec::new, ToOwned::to_owned);
    messages_to_query.push(user_message);

    let timeout_ms = hook.timeout.unwrap_or(30) * 1000;
    let model = hook.model.clone().unwrap_or_else(small_fast_model);
    let request = PromptHookRequest {
        hook_name: hook_name.to_string(),
        hook_event,
        processed_prompt,
        messages: messages_to_query,
        system_prompt: vec!["You are evaluating a hook in Claude Code.\n\nYour response must be a JSON object matching one of the following schemas:\n1. If the condition is met, return: {\"ok\": true}\n2. If the condition is not met, return: {\"ok\": false, \"reason\": \"Reason for why it is not met\"}".to_string()],
        tools: tool_use_context.tools.clone(),
        model: model.clone(),
        timeout_ms,
        agent_id: tool_use_context.agent_id.clone(),
    };

    let response = match executor(request).await {
        Ok(response) => response,
        Err(error) => {
            return HookResult {
                outcome: HookOutcome::NonBlockingError,
                system_message: Some(hook_error_message(
                    hook_name,
                    &effective_tool_use_id,
                    hook_event,
                    &format!("Error executing prompt hook: {error}"),
                    "",
                )),
                ..Default::default()
            };
        }
    };

    let full_response = assistant_text_content(&response.message).trim().to_string();
    let json = match serde_json::from_str::<serde_json::Value>(&full_response) {
        Ok(json) => json,
        Err(_) => {
            return HookResult {
                outcome: HookOutcome::NonBlockingError,
                system_message: Some(hook_error_message(
                    hook_name,
                    &effective_tool_use_id,
                    hook_event,
                    "JSON validation failed",
                    &full_response,
                )),
                ..Default::default()
            };
        }
    };

    let parsed = match serde_json::from_value::<HookResponse>(json) {
        Ok(parsed) => parsed,
        Err(error) => {
            return HookResult {
                outcome: HookOutcome::NonBlockingError,
                system_message: Some(hook_error_message(
                    hook_name,
                    &effective_tool_use_id,
                    hook_event,
                    &format!("Schema validation failed: {error}"),
                    &full_response,
                )),
                ..Default::default()
            };
        }
    };

    if !parsed.ok {
        let reason = parsed.reason.unwrap_or_default();
        return HookResult {
            outcome: HookOutcome::Blocking,
            blocking_error: Some(HookBlockingError {
                blocking_error: format!("Prompt hook condition was not met: {reason}"),
                command: hook.prompt.clone(),
            }),
            prevent_continuation: true,
            stop_reason: Some(reason),
            ..Default::default()
        };
    }

    HookResult {
        outcome: HookOutcome::Success,
        system_message: Some(hook_success_message(
            hook_name,
            &effective_tool_use_id,
            hook_event,
        )),
        ..Default::default()
    }
}

/// Maps to: CC `execPromptHook(...)` using the production Claude API executor.
pub async fn exec_prompt_hook(
    hook: &PromptHook,
    hook_name: &str,
    hook_event: HookEvent,
    json_input: &str,
    tool_use_context: &ToolUseContext,
    messages: Option<&[Message]>,
    tool_use_id: Option<&str>,
) -> HookResult {
    exec_prompt_hook_with_executor(
        hook,
        hook_name,
        hook_event,
        json_input,
        tool_use_context,
        messages,
        tool_use_id,
        Arc::new(default_prompt_hook_executor),
    )
    .await
}

fn default_prompt_hook_executor(
    request: PromptHookRequest,
) -> BoxFuture<'static, anyhow::Result<PromptHookResponse>> {
    Box::pin(async move {
        let mut options = Options::new(request.model, "hook_prompt".to_string());
        options.is_non_interactive_session = true;
        options.has_append_system_prompt = false;
        options.tool_choice = None;
        options.output_format = Some(serde_json::json!({
            "type": "json_schema",
            "schema": {
                "type": "object",
                "properties": {
                    "ok": {"type": "boolean"},
                    "reason": {"type": "string"}
                },
                "required": ["ok"],
                "additionalProperties": false
            }
        }));
        options.agent_id = request.agent_id.clone().map(crate::types::ids::AgentId);
        let message = query_model_without_streaming(
            &request.messages,
            &request.system_prompt,
            &ThinkingConfig::Disabled,
            &request.tools,
            &options,
        )
        .await?;
        Ok(PromptHookResponse { message })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::AssistantMessage;
    use std::sync::Mutex;

    fn assistant(text: &str) -> PromptHookResponse {
        PromptHookResponse {
            message: AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![AssistantContent::Text(text.to_string())],
                model: Some("haiku".to_string()),
                stop_reason: None,
                usage: None,
            },
        }
    }

    fn context() -> ToolUseContext {
        ToolUseContext::default()
    }

    fn prompt_hook() -> PromptHook {
        PromptHook {
            prompt: "Check $ARGUMENTS".to_string(),
            timeout: Some(12),
            model: Some("test-model".to_string()),
            status_message: None,
        }
    }

    #[tokio::test]
    async fn success_builds_official_request_and_success_attachment_shape() {
        let seen: Arc<Mutex<Vec<PromptHookRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_for_exec = Arc::clone(&seen);
        let result = exec_prompt_hook_with_executor(
            &prompt_hook(),
            "my-hook",
            HookEvent::Stop,
            r#"{"x":1}"#,
            &context(),
            None,
            Some("toolu_1"),
            Arc::new(move |request| {
                seen_for_exec.lock().unwrap().push(request);
                Box::pin(async { Ok(assistant(r#"{"ok":true}"#)) })
            }),
        )
        .await;

        assert_eq!(result.outcome, HookOutcome::Success);
        assert!(result.system_message.unwrap().contains("hook_success"));
        let request = seen.lock().unwrap().pop().unwrap();
        assert_eq!(request.hook_name, "my-hook");
        assert_eq!(request.hook_event, HookEvent::Stop);
        assert_eq!(request.processed_prompt, r#"Check {"x":1}"#);
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.model, "test-model");
        assert_eq!(request.timeout_ms, 12_000);
    }

    #[tokio::test]
    async fn false_response_blocks_with_reason_like_official() {
        let result = exec_prompt_hook_with_executor(
            &prompt_hook(),
            "guard",
            HookEvent::UserPromptSubmit,
            "{}",
            &context(),
            None,
            None,
            Arc::new(|_| Box::pin(async { Ok(assistant(r#"{"ok":false,"reason":"bad"}"#)) })),
        )
        .await;

        assert_eq!(result.outcome, HookOutcome::Blocking);
        assert!(result.prevent_continuation);
        assert_eq!(result.stop_reason.as_deref(), Some("bad"));
        assert_eq!(
            result.blocking_error.unwrap().blocking_error,
            "Prompt hook condition was not met: bad"
        );
    }

    #[tokio::test]
    async fn parse_and_schema_errors_are_non_blocking_attachments() {
        let invalid_json = exec_prompt_hook_with_executor(
            &prompt_hook(),
            "guard",
            HookEvent::Stop,
            "{}",
            &context(),
            None,
            Some("toolu_2"),
            Arc::new(|_| Box::pin(async { Ok(assistant("not json")) })),
        )
        .await;
        assert_eq!(invalid_json.outcome, HookOutcome::NonBlockingError);
        assert!(
            invalid_json
                .system_message
                .unwrap()
                .contains("JSON validation failed")
        );

        let schema_error = exec_prompt_hook_with_executor(
            &prompt_hook(),
            "guard",
            HookEvent::Stop,
            "{}",
            &context(),
            None,
            Some("toolu_3"),
            Arc::new(|_| Box::pin(async { Ok(assistant(r#"{"reason":"missing ok"}"#)) })),
        )
        .await;
        assert_eq!(schema_error.outcome, HookOutcome::NonBlockingError);
        assert!(
            schema_error
                .system_message
                .unwrap()
                .contains("Schema validation failed")
        );
    }

    #[tokio::test]
    async fn query_errors_are_non_blocking_error_attachments() {
        let result = exec_prompt_hook_with_executor(
            &prompt_hook(),
            "guard",
            HookEvent::Stop,
            "{}",
            &context(),
            None,
            Some("toolu_4"),
            Arc::new(|_| Box::pin(async { Err(anyhow::anyhow!("boom")) })),
        )
        .await;
        assert_eq!(result.outcome, HookOutcome::NonBlockingError);
        assert!(
            result
                .system_message
                .unwrap()
                .contains("Error executing prompt hook: boom")
        );
    }
}
