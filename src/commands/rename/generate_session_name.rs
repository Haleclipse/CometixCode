//! Maps to: CC `commands/rename/generateSessionName.ts:10-67`.

use crate::services::api::claude::{self, Options, SystemPrompt};
use crate::tool::AbortController;
use crate::types::message::{AssistantContent, Message};

const SESSION_NAME_PROMPT: &str = "Generate a short kebab-case name (2-4 words) that captures the main topic of this conversation. Use lowercase words separated by hyphens. Examples: \"fix-login-bug\", \"add-auth-feature\", \"refactor-api-client\", \"debug-test-failures\". Return JSON with a \"name\" field.";

fn parse_generated_name(content: &str) -> Option<String> {
    let response: serde_json::Value = serde_json::from_str(content).ok()?;
    response.get("name")?.as_str().map(str::to_string)
}

/// Maps to: CC `commands/rename/generateSessionName.ts:10-67`
/// `generateSessionName`.
pub async fn generate_session_name(
    messages: &[Message],
    abort_controller: &AbortController,
) -> Option<String> {
    let conversation_text = crate::utils::session_title::extract_conversation_text(messages);
    if conversation_text.is_empty() {
        return None;
    }

    let system_prompt: SystemPrompt = vec![SESSION_NAME_PROMPT.to_string()];
    let output_format = serde_json::json!({
        "type": "json_schema",
        "schema": {
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        }
    });
    let mut options = Options::for_utility("title", "rename_generate_name".to_string());
    options.agents = Vec::new();
    options.is_non_interactive_session = false;
    options.has_append_system_prompt = false;
    options.mcp_tools = Vec::new();
    options.abort_signal = Some(abort_controller.signal());

    match claude::query_haiku(
        &system_prompt,
        &conversation_text,
        Some(&output_format),
        &options,
    )
    .await
    {
        Ok(response) => {
            let content = response
                .content
                .iter()
                .filter_map(|content| match content {
                    AssistantContent::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            parse_generated_name(&content)
        }
        Err(error) => {
            // CC uses logForDebugging here because timeout/rate-limit/network
            // failures are expected operational outcomes.
            crate::utils::debug::log_for_debugging(&format!("generateSessionName failed: {error}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generated_session_name_skips_model_when_conversation_is_empty() {
        assert_eq!(
            generate_session_name(&[], &AbortController::default()).await,
            None
        );
    }

    #[test]
    fn generated_session_name_parser_matches_official_json_contract() {
        assert_eq!(
            parse_generated_name(r#"{"name":"fix-login-bug"}"#),
            Some("fix-login-bug".to_string())
        );
        assert_eq!(parse_generated_name(r#"{"title":"wrong"}"#), None);
        assert_eq!(parse_generated_name(r#"{"name":3}"#), None);
        assert_eq!(parse_generated_name("not-json"), None);
    }
}
