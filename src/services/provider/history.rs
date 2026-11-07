//! Cross-model history: one normalization pass before every request.
//!
//! @nyan: follows Pi `transformMessages` (packages/ai/src/api/transform-messages.ts).
//! An assistant message produced by the request's own model (same provider,
//! protocol and model id) is sent unchanged, so its bytes and the prompt-cache
//! prefix stay exactly as they were. Anything another model produced is
//! reduced to what every provider accepts: readable thinking becomes plain
//! text, encrypted/redacted thinking and every replay signature (Anthropic
//! signatures, Gemini `thoughtSignature`, stored [`ProviderState`]) are
//! dropped, and tool-call ids are rewritten to one portable form, with the
//! matching tool results following.
//!
//! The rewrite is a pure function of the history and the target, so the same
//! history produces byte-identical requests turn after turn.
//!
//! Orphaned tool calls are already repaired by
//! `utils::messages::ensure_tool_result_pairing`, which runs before this pass.
//!
//! [`ProviderState`]: super::ir::ProviderState

use std::collections::HashMap;

use crate::types::ids::ToolUseId;
use crate::types::message::{
    AssistantContent, AssistantMessage, Message, ModelProducer, UserContent,
};

/// The strictest tool-call id limit among supported protocols (OpenAI Chat
/// accepts 40; Anthropic 64).
const MAX_TOOL_CALL_ID: usize = 40;

/// Whether `message` came from the model `target` names. A message without a
/// recorded producer (synthetic rows, rows from before producers were
/// recorded) counts as the same model only if its model id matches.
fn is_same_model(message: &AssistantMessage, target: &ModelProducer) -> bool {
    match message
        .identity()
        .and_then(|identity| identity.producer.as_ref())
    {
        Some(producer) => producer.is_same_model(target),
        None => message.model.as_deref() == Some(target.model.as_str()),
    }
}

/// A tool-call id every protocol accepts: `^[A-Za-z0-9_-]{1,40}$`. An id that
/// already fits is kept; a longer one is cut and given a hash of the original
/// so two long ids never collide. A Responses composite id loses its item-id
/// half, which only the producing model can use.
pub fn portable_tool_call_id(id: &str) -> String {
    let call = super::openai::call_part(id);
    let clean: String = call
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !clean.is_empty() && clean.len() <= MAX_TOOL_CALL_ID {
        return clean;
    }
    // FNV-1a over the original id.
    let hash = id.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    let keep = clean.len().min(MAX_TOOL_CALL_ID - 9);
    format!("{}_{:08x}", &clean[..keep], hash as u32)
}

fn transform_foreign_assistant(
    mut message: AssistantMessage,
    id_map: &mut HashMap<String, String>,
) -> AssistantMessage {
    message.content = std::mem::take(&mut message.content)
        .into_iter()
        .filter_map(|block| match block {
            // @nyan: Pi drops foreign thinking; Yuki's decision keeps it as text.
            AssistantContent::Thinking { text, .. } => {
                (!text.trim().is_empty()).then_some(AssistantContent::Text(text))
            }
            AssistantContent::RedactedThinking { .. } => None,
            AssistantContent::ToolUse(mut tool_use) => {
                let portable = portable_tool_call_id(&tool_use.id.0);
                if portable != tool_use.id.0 {
                    id_map.insert(tool_use.id.0.clone(), portable.clone());
                    tool_use.id = ToolUseId(portable);
                }
                Some(AssistantContent::ToolUse(tool_use))
            }
            other => Some(other),
        })
        .collect();
    message
}

/// Normalize already-paired API history for a request to `target`.
pub fn transform_for_target(messages: Vec<Message>, target: &ModelProducer) -> Vec<Message> {
    let mut id_map = HashMap::new();
    messages
        .into_iter()
        .map(|message| match message {
            Message::Assistant(assistant) if !is_same_model(&assistant, target) => {
                Message::Assistant(transform_foreign_assistant(assistant, &mut id_map))
            }
            Message::User(mut user) if !id_map.is_empty() => {
                for content in &mut user.content {
                    if let UserContent::ToolResult(result) = content
                        && let Some(portable) = id_map.get(&result.tool_use_id.0)
                    {
                        result.tool_use_id = ToolUseId(portable.clone());
                    }
                }
                Message::User(user)
            }
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::ir::{Protocol, ProviderState};
    use crate::types::message::{AssistantMessageIdentity, ToolResult, ToolUseBlock};

    fn producer(provider: &str, protocol: Protocol, model: &str) -> ModelProducer {
        ModelProducer {
            profile: format!("{provider}-profile"),
            provider: provider.into(),
            protocol,
            model: model.into(),
        }
    }

    fn claude() -> ModelProducer {
        producer("anthropic", Protocol::AnthropicMessages, "claude-opus-5-5")
    }

    fn deepseek() -> ModelProducer {
        producer("deepseek", Protocol::OpenaiChat, "deepseek-v4")
    }

    fn assistant(producer: Option<ModelProducer>, content: Vec<AssistantContent>) -> Message {
        let mut message = crate::utils::messages::create_assistant_message(String::new());
        message.model = producer.as_ref().map(|p| p.model.clone());
        message.content = content;
        message.content.push(AssistantContent::MessageIdentity(
            AssistantMessageIdentity {
                producer,
                ..AssistantMessageIdentity::default()
            },
        ));
        Message::Assistant(message)
    }

    fn tool_use(id: &str) -> AssistantContent {
        AssistantContent::ToolUse(ToolUseBlock {
            id: ToolUseId(id.into()),
            name: "Read".into(),
            input: serde_json::json!({"p": "a"}),
        })
    }

    fn tool_result(id: &str) -> Message {
        let mut message = crate::utils::messages::create_user_message(String::new());
        message.content = vec![UserContent::ToolResult(ToolResult {
            tool_use_id: ToolUseId(id.into()),
            content: "ok".into(),
            is_error: false,
            content_blocks: Vec::new(),
            tool_use_result: None,
        })];
        Message::User(message)
    }

    fn history(from: ModelProducer, call_id: &str) -> Vec<Message> {
        vec![
            assistant(
                Some(from),
                vec![
                    AssistantContent::Thinking {
                        text: "let me think".into(),
                        signature: "EqQBsig==".into(),
                    },
                    AssistantContent::RedactedThinking {
                        data: "opaque".into(),
                    },
                    AssistantContent::Text("reading".into()),
                    tool_use(call_id),
                ],
            ),
            tool_result(call_id),
        ]
    }

    fn content(message: &Message) -> Vec<AssistantContent> {
        let Message::Assistant(message) = message else {
            panic!("not an assistant")
        };
        message
            .content
            .iter()
            .filter(|c| !matches!(c, AssistantContent::MessageIdentity(_)))
            .cloned()
            .collect()
    }

    fn result_id(message: &Message) -> String {
        let Message::User(user) = message else {
            panic!("not a user")
        };
        let UserContent::ToolResult(result) = &user.content[0] else {
            panic!("not a tool result")
        };
        result.tool_use_id.0.clone()
    }

    #[test]
    fn same_model_history_is_untouched_byte_for_byte() {
        let long_id = format!("call_{}|x", "y".repeat(80));
        let messages = history(claude(), &long_id);
        assert_eq!(transform_for_target(messages.clone(), &claude()), messages);
    }

    #[test]
    fn a_different_provider_protocol_or_model_each_count_as_foreign() {
        let base = claude();
        let mut other_provider = base.clone();
        other_provider.provider = "cpa".into();
        let mut other_protocol = base.clone();
        other_protocol.protocol = Protocol::OpenaiChat;
        let mut other_model = base.clone();
        other_model.model = "claude-sonnet-5".into();
        let mut other_profile = base.clone();
        other_profile.profile = "renamed".into();
        for (target, foreign) in [
            (other_provider, true),
            (other_protocol, true),
            (other_model, true),
            (other_profile, false),
        ] {
            let out = transform_for_target(history(base.clone(), "toolu_1"), &target);
            let has_thinking = content(&out[0])
                .iter()
                .any(|c| matches!(c, AssistantContent::Thinking { .. }));
            assert_eq!(has_thinking, !foreign, "{target:?}");
        }
    }

    #[test]
    fn foreign_thinking_becomes_text_and_redacted_thinking_and_signatures_are_dropped() {
        let out = transform_for_target(history(deepseek(), "call_1"), &claude());
        assert_eq!(
            content(&out[0]),
            vec![
                AssistantContent::Text("let me think".into()),
                AssistantContent::Text("reading".into()),
                tool_use("call_1"),
            ]
        );
        let dump = format!("{out:?}");
        assert!(
            !dump.contains("EqQBsig==") && !dump.contains("opaque"),
            "{dump}"
        );
    }

    #[test]
    fn stored_provider_state_and_empty_thinking_from_another_model_are_dropped() {
        let state = ProviderState {
            protocol: Protocol::OpenaiResponses,
            data: serde_json::json!({"type": "reasoning", "encrypted_content": "enc"}),
        };
        let gpt = producer("openai", Protocol::OpenaiResponses, "gpt-5.6");
        let messages = vec![assistant(
            Some(gpt),
            vec![
                AssistantContent::Thinking {
                    text: String::new(),
                    signature: state.to_signature(),
                },
                AssistantContent::RedactedThinking {
                    data: state.to_signature(),
                },
                AssistantContent::Text("done".into()),
            ],
        )];
        let out = transform_for_target(messages, &claude());
        assert_eq!(
            content(&out[0]),
            vec![AssistantContent::Text("done".into())]
        );
    }

    #[test]
    fn foreign_tool_ids_are_portable_stable_and_follow_into_tool_results() {
        let gpt = producer("openai", Protocol::OpenaiResponses, "gpt-5.6");
        let long_id = format!(
            "{}-nyanfc-fc_{}",
            format!("call_{}|ab", "z".repeat(440)),
            "q".repeat(30)
        );
        let messages = history(gpt, &long_id);
        let first = transform_for_target(messages.clone(), &claude());
        let second = transform_for_target(messages, &claude());
        assert_eq!(first, second);
        let AssistantContent::ToolUse(call) = &content(&first[0])[2] else {
            panic!()
        };
        let id = &call.id.0;
        assert!(id.len() <= MAX_TOOL_CALL_ID, "{id}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
        assert!(!id.contains("nyanfc"), "{id}");
        assert_eq!(result_id(&first[1]), *id);
    }

    #[test]
    fn portable_ids_keep_fitting_ids_and_keep_long_ones_distinct() {
        assert_eq!(portable_tool_call_id("toolu_01ABC"), "toolu_01ABC");
        assert_eq!(portable_tool_call_id("call_1-nyanfc-fc_9"), "call_1");
        assert_eq!(portable_tool_call_id("toolu_01.A:b"), "toolu_01_A_b");
        let a = portable_tool_call_id(&format!("{}A", "x".repeat(60)));
        let b = portable_tool_call_id(&format!("{}B", "x".repeat(60)));
        assert!(a.len() <= MAX_TOOL_CALL_ID && b.len() <= MAX_TOOL_CALL_ID);
        assert_ne!(a, b);
        assert!(!portable_tool_call_id("").is_empty());
    }

    #[test]
    fn a_message_without_a_recorded_producer_is_the_same_model_only_by_model_id() {
        let mut message = crate::utils::messages::create_assistant_message(String::new());
        message.model = Some("claude-opus-5-5".into());
        message.content = vec![AssistantContent::Thinking {
            text: "t".into(),
            signature: "sig".into(),
        }];
        let old = vec![Message::Assistant(message)];
        let kept = transform_for_target(old.clone(), &claude());
        assert_eq!(kept, old);
        let moved = transform_for_target(old, &deepseek());
        assert_eq!(content(&moved[0]), vec![AssistantContent::Text("t".into())]);
    }
}
