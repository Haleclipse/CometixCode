//! Side Question ("/btw") feature — ask quick questions without interrupting
//! the main agent context.
//!
//! Maps to: CC `utils/sideQuestion.ts`.
//!
//! Uses [`crate::utils::forked_agent::run_forked_agent`] to leverage prompt
//! caching from the parent context while keeping the side-question response
//! separate from the main conversation.

use crate::services::api::request::NonNullableUsage;
use crate::types::message::{AssistantContent, Message, SystemMessage, UserContent, UserMessage};
use crate::types::permissions::{PermissionDecision, PermissionDecisionReason};
use crate::utils::forked_agent::{CacheSafeParams, run_forked_agent};
use regex::Regex;
use std::sync::OnceLock;

/// Pattern to detect "/btw" at start of input (case-insensitive, word boundary).
/// Maps to: CC `BTW_PATTERN`.
fn btw_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^/btw\b").expect("valid /btw pattern"))
}

/// Find positions of "/btw" keyword at the start of text for highlighting.
/// Maps to: CC `findBtwTriggerPositions`.
pub fn find_btw_trigger_positions(text: &str) -> Vec<BtwTriggerPosition> {
    let mut positions = Vec::new();
    for m in btw_pattern().find_iter(text) {
        positions.push(BtwTriggerPosition {
            word: m.as_str().to_string(),
            start: m.start(),
            end: m.end(),
        });
    }
    positions
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtwTriggerPosition {
    pub word: String,
    /// Byte offset (Nyan editing unit). Official JS `start` is UTF-16.
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideQuestionResult {
    pub response: Option<String>,
    pub usage: NonNullableUsage,
}

/// Run a side question using a forked agent.
/// Maps to: CC `runSideQuestion`.
pub async fn run_side_question(
    question: &str,
    cache_safe_params: CacheSafeParams,
) -> anyhow::Result<SideQuestionResult> {
    let wrapped_question = format!(
        "<system-reminder>This is a side question from the user. You must answer this question directly in a single response.

IMPORTANT CONTEXT:
- You are a separate, lightweight agent spawned to answer this one question
- The main agent is NOT interrupted - it continues working independently in the background
- You share the conversation context but are a completely separate instance
- Do NOT reference being interrupted or what you were \"previously doing\" - that framing is incorrect

CRITICAL CONSTRAINTS:
- You have NO tools available - you cannot read files, run commands, search, or take any actions
- This is a one-off response - there will be no follow-up turns
- You can ONLY provide information based on what you already know from the conversation context
- NEVER say things like \"Let me try...\", \"I'll now...\", \"Let me check...\", or promise to take any action
- If you don't know the answer, say so - do not offer to look it up or investigate

Simply answer the question with the information you have.</system-reminder>

{question}"
    );

    let prompt_messages = vec![Message::User(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::Text(wrapped_question)],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    })];

    let agent_result = run_forked_agent(crate::utils::forked_agent::ForkedAgentParams {
        prompt_messages,
        cache_safe_params,
        // Maps to: CC `utils/sideQuestion.ts:86-90` — `{ behavior: 'deny',
        // message: 'Side questions cannot use tools', decisionReason: { type:
        // 'other', reason: 'side_question' } }`.
        can_use_tool: crate::tool::CanUseToolCallback::new(
            |_tool, _input, _context, _assistant_message, _tool_use_id, _force_decision| {
                PermissionDecision::Deny {
                    message: "Side questions cannot use tools".to_string(),
                    decision_reason: PermissionDecisionReason::Other {
                        reason: "side_question".to_string(),
                    },
                    tool_use_id: None,
                }
            },
        ),
        query_source: crate::constants::query_source::QuerySource::SideQuestion,
        fork_label: "side_question".to_string(),
        overrides: None,
        max_output_tokens: None,
        max_turns: Some(1),
        on_message: None,
        on_progress: None,
        skip_cache_write: true,
        // Official `/btw` does not set `skipTranscript`; preserve its sidechain.
        skip_transcript: false,
    })
    .await?;

    Ok(SideQuestionResult {
        response: extract_side_question_response(&agent_result.messages),
        usage: agent_result.total_usage,
    })
}

/// Extract a display string from forked agent messages.
/// Maps to: CC `extractSideQuestionResponse`.
pub fn extract_side_question_response(messages: &[Message]) -> Option<String> {
    let assistant_blocks: Vec<&AssistantContent> = messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant.content.iter()),
            _ => None,
        })
        .flatten()
        .collect();

    if !assistant_blocks.is_empty() {
        let text = assistant_blocks
            .iter()
            .filter_map(|block| match block {
                AssistantContent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
            .trim()
            .to_string();
        if !text.is_empty() {
            return Some(text);
        }

        if let Some(tool_name) = assistant_blocks.iter().find_map(|block| match block {
            AssistantContent::ToolUse(tool_use) => Some(tool_use.name.as_str()),
            _ => None,
        }) {
            return Some(format!(
                "(The model tried to call {tool_name} instead of answering directly. Try rephrasing or ask in the main conversation.)"
            ));
        }
    }

    // CC `sideQuestion.ts:145-151`: find the first system api_error message
    // and format its `error` payload.
    if let Some(api_err) = messages.iter().find_map(|message| match message {
        Message::System(SystemMessage::ApiError { error, .. }) => Some(error.clone()),
        _ => None,
    }) {
        let formatted = crate::services::api::error_utils::format_api_error(
            &crate::services::api::errors::ApiErrorInfo::Other(api_err),
        );
        return Some(format!("(API error: {formatted})"));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::ToolUseId;
    use crate::types::message::{AssistantMessage, ToolUseBlock};

    #[test]
    fn find_btw_trigger_positions_matches_leading_slash_command() {
        let positions = find_btw_trigger_positions("/btw what is this?");
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].word.to_lowercase(), "/btw");
        assert_eq!(positions[0].start, 0);
        assert_eq!(positions[0].end, 4);
        assert!(find_btw_trigger_positions("please /btw later").is_empty());
    }

    #[test]
    fn extract_side_question_response_flattens_text_across_assistant_blocks() {
        let messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Thinking {
                    text: "plan".into(),
                    signature: String::new(),
                }],
                model: None,
                stop_reason: None,
                usage: None,
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("Answer body".into())],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ];
        assert_eq!(
            extract_side_question_response(&messages).as_deref(),
            Some("Answer body")
        );
    }

    #[test]
    fn extract_side_question_response_reports_tool_use_attempt() {
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: ToolUseId("t1".into()),
                name: "Bash".into(),
                input: serde_json::json!({}),
            })],
            model: None,
            stop_reason: None,
            usage: None,
        })];
        let text = extract_side_question_response(&messages).expect("tool message");
        assert!(text.contains("Bash"));
        assert!(text.contains("main conversation"));
    }

    #[test]
    fn extract_side_question_response_formats_api_error_like_official() {
        let messages = vec![Message::System(SystemMessage::ApiError {
            base: crate::types::message::SystemBase::new(),
            error: "Service Unavailable".to_string(),
            retry_in_ms: 0,
            retry_attempt: 0,
            max_retries: 0,
        })];
        assert_eq!(
            extract_side_question_response(&messages).as_deref(),
            Some("(API error: Service Unavailable)")
        );
    }
}
