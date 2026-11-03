//! Internal conversation -> provider-neutral [`ModelRequest`].
//!
//! @nyan: this is the producer half of the IR. It knows nyan's `Message` /
//! `Tool` types and nothing about any wire format. History hygiene that every
//! provider needs (normalization, tool-result pairing, media limits) happens
//! here; anything shaped by one protocol (cache markers, betas, tool-search
//! deferral, system-block splitting) is left to that protocol's adapter.

use crate::constants::api_limits::API_MAX_MEDIA_PER_REQUEST;
use crate::types::message::{
    AssistantContent, Message as NyanMessage, ToolResult, ToolResultContentBlock, UserContent,
};
use crate::types::tools::Tool;
use crate::utils::thinking::ThinkingConfig;

use super::ir::{
    AssistantPart, CachePolicy, ClientContext, Effort, JsonSchemaOutput, MediaSource, Message,
    ModelRequest, Protocol, ProviderState, ThinkingPreference, ToolChoice, ToolResultContent,
    ToolResultPart, ToolSpec, UserPart,
};

/// Everything the query layer decides about one call, before any wire format.
#[derive(Debug, Clone)]
pub struct RequestParams {
    pub model: String,
    pub query_source: String,
    pub agent_id: Option<String>,
    pub max_output_tokens: u32,
    pub temperature: Option<f64>,
    pub tool_choice: ToolChoice,
    pub disable_parallel_tools: bool,
    pub thinking: ThinkingConfig,
    pub effort: Option<Effort>,
    pub output_schema: Option<serde_json::Value>,
    pub enable_prompt_caching: bool,
    pub skip_cache_write: bool,
    pub client: ClientContext,
    pub tools_pending: bool,
}

/// A `RawImage` is an image block that did not fit the plain base64 shape (URL
/// source, extra metadata). The neutral `source` is extracted for adapters that
/// only understand that; the untouched block rides along as Anthropic-tagged
/// state so the Anthropic request bytes, and with them the cache prefix, do
/// not change.
fn raw_image(block: &serde_json::Value) -> Option<(MediaSource, Option<ProviderState>)> {
    let source = block.get("source")?;
    let media = match source.get("type")?.as_str()? {
        "base64" => MediaSource::Base64 {
            media_type: source.get("media_type")?.as_str()?.to_string(),
            data: source.get("data")?.as_str()?.to_string(),
        },
        "url" => MediaSource::Url {
            url: source.get("url")?.as_str()?.to_string(),
        },
        _ => return None,
    };
    Some((
        media,
        Some(ProviderState {
            protocol: Protocol::AnthropicMessages,
            data: block.clone(),
        }),
    ))
}

fn tool_result_parts(result: &ToolResult) -> ToolResultContent {
    if result.content_blocks.is_empty() {
        return ToolResultContent::Text(result.content.clone());
    }
    ToolResultContent::Parts(
        result
            .content_blocks
            .iter()
            .filter_map(|block| match block {
                ToolResultContentBlock::Text { text } => {
                    Some(ToolResultPart::Text { text: text.clone() })
                }
                ToolResultContentBlock::ToolReference { tool_name } => {
                    Some(ToolResultPart::ToolReference {
                        tool_name: tool_name.clone(),
                    })
                }
                ToolResultContentBlock::Image { source } => Some(ToolResultPart::Image {
                    source: MediaSource::Base64 {
                        media_type: source.media_type.clone(),
                        data: source.data.clone(),
                    },
                    provider_state: None,
                }),
                ToolResultContentBlock::Document { source } => Some(ToolResultPart::Document {
                    media_type: source.media_type.clone(),
                    data: source.data.clone(),
                }),
                ToolResultContentBlock::RawImage(raw) => {
                    let (source, provider_state) = raw_image(raw)?;
                    Some(ToolResultPart::Image {
                        source,
                        provider_state,
                    })
                }
            })
            .collect(),
    )
}

fn user_parts(content: &[UserContent]) -> Vec<UserPart> {
    content
        .iter()
        .filter_map(|part| match part {
            UserContent::Text(text) | UserContent::MetaText(text) => {
                Some(UserPart::Text { text: text.clone() })
            }
            UserContent::Image { media_type, data }
            | UserContent::MetaImage { media_type, data } => Some(UserPart::Image {
                source: MediaSource::Base64 {
                    media_type: media_type.clone(),
                    data: data.clone(),
                },
                provider_state: None,
            }),
            UserContent::RawImage { block, .. } => {
                let (source, provider_state) = raw_image(block)?;
                Some(UserPart::Image {
                    source,
                    provider_state,
                })
            }
            UserContent::Document { media_type, data }
            | UserContent::MetaDocument { media_type, data } => Some(UserPart::Document {
                media_type: media_type.clone(),
                data: data.clone(),
            }),
            UserContent::ToolResult(result) => Some(UserPart::ToolResult {
                tool_call_id: result.tool_use_id.0.clone(),
                content: tool_result_parts(result),
                is_error: result.is_error,
            }),
        })
        .collect()
}

fn assistant_parts(content: &[AssistantContent]) -> Vec<AssistantPart> {
    content
        .iter()
        .filter_map(|part| match part {
            AssistantContent::Text(text) => Some(AssistantPart::Text { text: text.clone() }),
            AssistantContent::Thinking { text, signature } => Some(AssistantPart::Thinking {
                text: text.clone(),
                signature: signature.clone(),
                provider_state: None,
            }),
            AssistantContent::RedactedThinking { data } => Some(AssistantPart::RedactedThinking {
                data: data.clone(),
                provider_state: None,
            }),
            AssistantContent::ToolUse(block) => Some(AssistantPart::ToolCall {
                id: block.id.0.clone(),
                name: block.name.clone(),
                input: block.input.clone(),
                provider_state: None,
            }),
            AssistantContent::ServerToolUse(block) => Some(AssistantPart::ServerToolCall {
                id: block.id.0.clone(),
                name: block.name.clone(),
                input: block.input.clone(),
            }),
            AssistantContent::WebSearchToolResult {
                tool_use_id,
                content,
            } => Some(AssistantPart::ServerToolResult {
                tool_call_id: tool_use_id.0.clone(),
                name: "web_search".to_string(),
                content: content.clone(),
            }),
            AssistantContent::Advisor {
                tool_use_id,
                content,
            } => Some(AssistantPart::ServerToolResult {
                tool_call_id: tool_use_id.0.clone(),
                name: "advisor".to_string(),
                content: serde_json::to_value(content).unwrap_or(serde_json::Value::Null),
            }),
            // Envelope bookkeeping, never model content.
            AssistantContent::MessageIdentity(_) => None,
        })
        .collect()
}

/// Drop the oldest media until at most `limit` images/documents remain.
fn strip_excess_media(messages: &mut [Message], limit: usize) {
    fn count(messages: &[Message]) -> usize {
        messages
            .iter()
            .map(|message| match message {
                Message::User { parts } => parts
                    .iter()
                    .map(|part| match part {
                        UserPart::Image { .. } | UserPart::Document { .. } => 1,
                        UserPart::ToolResult {
                            content: ToolResultContent::Parts(parts),
                            ..
                        } => parts
                            .iter()
                            .filter(|p| {
                                matches!(
                                    p,
                                    ToolResultPart::Image { .. } | ToolResultPart::Document { .. }
                                )
                            })
                            .count(),
                        _ => 0,
                    })
                    .sum::<usize>(),
                Message::Assistant { .. } => 0,
            })
            .sum()
    }

    let total = count(messages);
    if total <= limit {
        return;
    }
    let mut remove = total - limit;
    for message in messages.iter_mut() {
        if remove == 0 {
            break;
        }
        let Message::User { parts } = message else {
            continue;
        };
        parts.retain_mut(|part| {
            if remove == 0 {
                return true;
            }
            match part {
                UserPart::Image { .. } | UserPart::Document { .. } => {
                    remove -= 1;
                    false
                }
                UserPart::ToolResult {
                    content: ToolResultContent::Parts(parts),
                    ..
                } => {
                    parts.retain(|p| {
                        if remove > 0
                            && matches!(
                                p,
                                ToolResultPart::Image { .. } | ToolResultPart::Document { .. }
                            )
                        {
                            remove -= 1;
                            false
                        } else {
                            true
                        }
                    });
                    true
                }
                _ => true,
            }
        });
    }
}

fn tool_spec(tool: &Tool) -> ToolSpec {
    ToolSpec {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
        strict: tool.strict == Some(true),
        deferrable: crate::tools::tool_search_tool::prompt::is_deferred_tool(tool),
        is_mcp: tool.is_mcp,
    }
}

/// Build the IR request from already-normalized API messages.
///
/// `messages` must already be the output of `normalize_messages_for_api` and
/// tool-result pairing repair; this function only converts and applies the
/// media limit.
pub fn build_request(
    messages: &[NyanMessage],
    system: &[String],
    tools: &[Tool],
    discovered_tools: Vec<String>,
    params: RequestParams,
) -> ModelRequest {
    let mut ir_messages: Vec<Message> = messages
        .iter()
        .filter_map(|message| match message {
            NyanMessage::User(user) => Some(Message::User {
                parts: user_parts(&user.content),
            }),
            NyanMessage::Assistant(assistant) => Some(Message::Assistant {
                parts: assistant_parts(&assistant.content),
            }),
            // Progress, system rows, attachments and hook results never reach
            // a provider.
            _ => None,
        })
        .collect();
    strip_excess_media(&mut ir_messages, API_MAX_MEDIA_PER_REQUEST);

    let thinking = match params.thinking {
        ThinkingConfig::Disabled => ThinkingPreference::Disabled,
        ThinkingConfig::Adaptive => ThinkingPreference::Adaptive,
        ThinkingConfig::Enabled { budget_tokens } => ThinkingPreference::Budget {
            tokens: budget_tokens.map(|tokens| tokens.clamp(0, u32::MAX as i64) as u32),
        },
    };

    ModelRequest {
        model: params.model,
        system: system.to_vec(),
        messages: ir_messages,
        tools: tools.iter().map(tool_spec).collect(),
        tool_choice: params.tool_choice,
        thinking,
        effort: params.effort,
        max_output_tokens: params.max_output_tokens,
        temperature: params.temperature,
        structured_output: params
            .output_schema
            .map(|schema| JsonSchemaOutput { schema }),
        cache: CachePolicy {
            enabled: params.enable_prompt_caching,
            skip_write: params.skip_cache_write,
        },
        query_source: params.query_source,
        agent_id: params.agent_id,
        disable_parallel_tools: params.disable_parallel_tools,
        discovered_tools,
        tools_pending: params.tools_pending,
        client: params.client,
        extensions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::ToolUseId;
    use crate::types::message::{AssistantMessage, ToolUseBlock};

    fn params() -> RequestParams {
        RequestParams {
            model: "m".into(),
            query_source: "repl_main_thread".into(),
            agent_id: None,
            max_output_tokens: 100,
            temperature: None,
            tool_choice: ToolChoice::Auto,
            disable_parallel_tools: false,
            thinking: ThinkingConfig::Disabled,
            effort: None,
            output_schema: None,
            enable_prompt_caching: true,
            skip_cache_write: false,
            client: ClientContext::default(),
            tools_pending: false,
        }
    }

    fn user(content: Vec<UserContent>) -> NyanMessage {
        let mut message = crate::utils::messages::create_user_message(String::new());
        message.content = content;
        NyanMessage::User(message)
    }

    fn assistant(content: Vec<AssistantContent>) -> NyanMessage {
        let mut message = crate::utils::messages::create_assistant_message(String::new());
        message.content = content;
        NyanMessage::Assistant(AssistantMessage { ..message })
    }

    #[test]
    fn conversation_maps_text_tool_call_and_result() {
        let messages = vec![
            user(vec![UserContent::Text("read it".into())]),
            assistant(vec![
                AssistantContent::Thinking {
                    text: "hm".into(),
                    signature: "sig".into(),
                },
                AssistantContent::ToolUse(ToolUseBlock {
                    id: ToolUseId("t1".into()),
                    name: "Read".into(),
                    input: serde_json::json!({"p": "a"}),
                }),
            ]),
            user(vec![UserContent::ToolResult(ToolResult {
                tool_use_id: ToolUseId("t1".into()),
                content: "contents".into(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: None,
            })]),
        ];
        let request = build_request(&messages, &[], &[], Vec::new(), params());
        assert_eq!(request.messages.len(), 3);
        assert!(matches!(
            &request.messages[1],
            Message::Assistant { parts }
                if matches!(&parts[0], AssistantPart::Thinking { signature, .. } if signature == "sig")
                && matches!(&parts[1], AssistantPart::ToolCall { id, name, .. } if id == "t1" && name == "Read")
        ));
        assert!(matches!(
            &request.messages[2],
            Message::User { parts }
                if matches!(&parts[0], UserPart::ToolResult { tool_call_id, content: ToolResultContent::Text(t), is_error: false }
                    if tool_call_id == "t1" && t == "contents")
        ));
    }

    #[test]
    fn identity_envelope_and_non_model_rows_never_reach_the_ir() {
        let mut assistant_message = crate::utils::messages::create_assistant_message("x".into());
        assistant_message
            .content
            .push(AssistantContent::MessageIdentity(
                crate::types::message::AssistantMessageIdentity::default(),
            ));
        let messages = vec![
            user(vec![UserContent::Text("hi".into())]),
            NyanMessage::Assistant(assistant_message),
        ];
        let request = build_request(&messages, &[], &[], Vec::new(), params());
        assert!(matches!(
            &request.messages[1],
            Message::Assistant { parts } if parts.len() == 1
        ));
    }

    #[test]
    fn thinking_config_maps_to_preference() {
        let mut p = params();
        p.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(2048),
        };
        assert_eq!(
            build_request(&[], &[], &[], Vec::new(), p).thinking,
            ThinkingPreference::Budget { tokens: Some(2048) }
        );
        let mut p = params();
        p.thinking = ThinkingConfig::Adaptive;
        assert_eq!(
            build_request(&[], &[], &[], Vec::new(), p).thinking,
            ThinkingPreference::Adaptive
        );
    }

    #[test]
    fn excess_media_is_dropped_oldest_first_including_inside_tool_results() {
        let image = |n: usize| UserContent::Image {
            media_type: "image/png".into(),
            data: format!("d{n}"),
        };
        let mut messages = vec![user(vec![image(0), image(1)])];
        messages.push(user(vec![UserContent::ToolResult(ToolResult {
            tool_use_id: ToolUseId("t".into()),
            content: String::new(),
            is_error: false,
            content_blocks: vec![ToolResultContentBlock::image_base64("image/png", "d2")],
            tool_use_result: None,
        })]));
        let mut ir: Vec<Message> = messages
            .iter()
            .map(|m| match m {
                NyanMessage::User(u) => Message::User {
                    parts: user_parts(&u.content),
                },
                _ => unreachable!(),
            })
            .collect();
        strip_excess_media(&mut ir, 2);
        let Message::User { parts } = &ir[0] else {
            panic!()
        };
        assert_eq!(parts.len(), 1, "oldest image dropped first");
        assert!(
            matches!(&parts[0], UserPart::Image { source: MediaSource::Base64 { data, .. }, .. } if data == "d1")
        );
        let Message::User { parts } = &ir[1] else {
            panic!()
        };
        assert!(matches!(
            &parts[0],
            UserPart::ToolResult { content: ToolResultContent::Parts(p), .. } if p.len() == 1
        ));
    }

    #[test]
    fn tool_flags_come_from_the_internal_tool() {
        let mcp = Tool {
            name: "mcp__s__t".into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
            is_mcp: true,
            strict: Some(true),
            ..Default::default()
        };
        let request = build_request(&[], &[], &[mcp], Vec::new(), params());
        let spec = &request.tools[0];
        assert!(spec.is_mcp && spec.deferrable && spec.strict);
    }
}
