//! Maps to: CC `utils/sessionTitle.ts:26-51` (`extractConversationText`).
//!
//! Rust carries upstream `isMeta` text as `UserContent::MetaText`; this owner
//! excludes it from title generation exactly like CC. `origin` remains outside
//! typed model history. The remaining user/assistant text projection and
//! recent-tail preference match CC.

use crate::types::message::{AssistantContent, Message, UserContent};

const MAX_CONVERSATION_TEXT_UTF16: usize = 1000;

fn tail_utf16(text: &str, max_units: usize) -> String {
    if text.encode_utf16().count() <= max_units {
        return text.to_string();
    }

    let mut units = 0;
    let mut start = text.len();
    for (index, character) in text.char_indices().rev() {
        let next = units + character.len_utf16();
        if next > max_units {
            break;
        }
        units = next;
        start = index;
    }
    text[start..].to_string()
}

/// Maps to: CC `utils/sessionTitle.ts:33-51` `extractConversationText`.
pub fn extract_conversation_text(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                parts.extend(user.content.iter().filter_map(|content| match content {
                    UserContent::Text(text) => Some(text.as_str()),
                    UserContent::MetaText(_)
                    | UserContent::Image { .. }
                    | UserContent::MetaImage { .. }
                    | UserContent::RawImage { .. }
                    | UserContent::Document { .. }
                    | UserContent::MetaDocument { .. }
                    | UserContent::ToolResult(_) => None,
                }))
            }
            Message::Assistant(assistant) => {
                parts.extend(assistant.content.iter().filter_map(|content| {
                    if let AssistantContent::Text(text) = content {
                        Some(text.as_str())
                    } else {
                        None
                    }
                }));
            }
            Message::System(_)
            | Message::Attachment(_)
            | Message::Progress(_)
            | Message::HookResult(_) => {}
        }
    }
    tail_utf16(&parts.join("\n"), MAX_CONVERSATION_TEXT_UTF16)
}

/// Maps to: CC `utils/sessionTitle.ts:57-71` `SESSION_TITLE_PROMPT`.
const SESSION_TITLE_PROMPT: &str = r#"Generate a concise, sentence-case title (3-7 words) that captures the main topic or goal of this coding session. The title should be clear enough that the user recognizes the session in a list. Use sentence case: capitalize only the first word and proper nouns.

Return JSON with a single "title" field.

Good examples:
{"title": "Fix login button on mobile"}
{"title": "Add OAuth authentication"}
{"title": "Debug failing CI tests"}
{"title": "Refactor API client error handling"}

Bad (too vague): {"title": "Code changes"}
Bad (too long): {"title": "Investigate and fix the issue where the login button does not respond on mobile devices"}
Bad (wrong case): {"title": "Fix Login Button On Mobile"}"#;

/// Maps to: CC `utils/sessionTitle.ts:73` `titleSchema` + the
/// `safeParseJSON`/`safeParse` step of `generateSessionTitle`.
fn parse_generated_title(content: &str) -> Option<String> {
    let response: serde_json::Value = serde_json::from_str(content).ok()?;
    let title = response.get("title")?.as_str()?.trim().to_string();
    if title.is_empty() { None } else { Some(title) }
}

/// Maps to: CC `utils/sessionTitle.ts:79-130` `generateSessionTitle`.
///
/// Returns `None` on empty input, query failure, or an unparseable Haiku
/// response. The `tengu_session_title_generated` analytics event is omitted
/// per the external-build telemetry exemption.
pub async fn generate_session_title(
    description: &str,
    abort_controller: &crate::tool::AbortController,
) -> Option<String> {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return None;
    }

    let system_prompt: crate::services::api::claude::SystemPrompt =
        vec![SESSION_TITLE_PROMPT.to_string()];
    let output_format = serde_json::json!({
        "type": "json_schema",
        "schema": {
            "type": "object",
            "properties": {
                "title": { "type": "string" }
            },
            "required": ["title"],
            "additionalProperties": false
        }
    });
    let mut options = crate::services::api::claude::Options::for_utility(
        "title",
        "generate_session_title".to_string(),
    );
    options.agents = Vec::new();
    // CC: reflect the actual session mode — this module is called from both
    // the SDK print path (non-interactive) and interactive REPL titling.
    options.is_non_interactive_session = crate::bootstrap::state::get_is_non_interactive_session();
    options.has_append_system_prompt = false;
    options.mcp_tools = Vec::new();
    options.abort_signal = Some(abort_controller.signal());

    match crate::services::api::claude::query_haiku(
        &system_prompt,
        trimmed,
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
            parse_generated_title(&content)
        }
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "generateSessionTitle failed: {error}"
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantMessage, StopReason, UserMessage};
    use chrono::Utc;

    #[test]
    fn extract_conversation_text_matches_official_roles_and_recent_tail() {
        let messages = vec![
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![
                    UserContent::Text("u".repeat(1_100)),
                    UserContent::MetaText("hidden init expansion".to_string()),
                    UserContent::Image {
                        media_type: "image/png".to_string(),
                        data: "ignored".to_string(),
                    },
                ],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![
                    AssistantContent::Thinking {
                        text: "ignored".to_string(),
                        signature: String::new(),
                    },
                    AssistantContent::Text("recent".to_string()),
                ],
                model: None,
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            }),
        ];

        let text = extract_conversation_text(&messages);
        assert_eq!(text.encode_utf16().count(), 1000);
        assert!(text.ends_with("\nrecent"));
        assert!(!text.contains("ignored"));
        assert!(!text.contains("hidden init expansion"));
    }

    #[test]
    fn extract_conversation_text_counts_astral_characters_as_utf16_units() {
        let text = tail_utf16(&"😀".repeat(600), 1000);
        assert_eq!(text.chars().count(), 500);
        assert_eq!(text.encode_utf16().count(), 1000);
    }

    /// CC `titleSchema().safeParse(safeParseJSON(text))` then
    /// `title.trim() || null` (`utils/sessionTitle.ts:113-117`).
    #[test]
    fn generated_title_parser_matches_official_json_contract() {
        assert_eq!(
            parse_generated_title(r#"{"title":"Fix login button on mobile"}"#).as_deref(),
            Some("Fix login button on mobile")
        );
        assert_eq!(
            parse_generated_title(r#"{"title":"  padded  "}"#).as_deref(),
            Some("padded")
        );
        assert_eq!(parse_generated_title(r#"{"title":"   "}"#), None);
        assert_eq!(parse_generated_title(r#"{"name":"wrong field"}"#), None);
        assert_eq!(parse_generated_title("not json"), None);
    }

    #[tokio::test]
    async fn generated_session_title_skips_model_when_description_is_empty() {
        assert_eq!(
            generate_session_title("   ", &crate::tool::AbortController::default()).await,
            None
        );
    }
}
