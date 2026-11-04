//! @nyan: OpenAI adapters (provider redesign PR 5): Chat Completions and
//! Responses, two protocols of one provider kind.
//!
//! A profile speaks exactly one of them (`ResolvedModelProfile::protocol`); the
//! adapter for the other refuses it in `validate`. Replay state is tagged with
//! the protocol that produced it, so Responses reasoning items never reach a
//! Chat request and neither reaches Anthropic. Retries are not done here: one
//! `send` is one HTTP request (decision 23 keeps the count at 0 until PR 8).

pub mod adapter;
pub mod chat;
pub mod errors;
#[cfg(test)]
mod fixtures;
#[cfg(test)]
pub(crate) mod mock;
pub mod policy;
#[cfg(test)]
mod query_path;
pub mod responses;
pub mod target;
pub mod transport;

use serde_json::Value;

use super::adapter::{Cancel, OpenedStream, ProviderAdapter, ResolvedTarget};
use super::ir::{
    Effort, MediaSource, ModelRequest, ProviderError, ProviderErrorKind, SYSTEM_BOUNDARY,
    ThinkingPreference, ToolResultContent, ToolResultPart, ToolSpec,
};
use crate::utils::model::profile::ResolvedModelProfile;
#[cfg(test)]
use crate::utils::settings::SettingsJson;
use crate::utils::settings::nyan::ProviderProtocol;

/// Open one streaming attempt for an OpenAI-protocol profile: resolve its
/// credential, check the request against what the profile declares, send.
///
/// Every failure that happens before the HTTP request (missing credential,
/// unsupported capability, wrong protocol) is an `InvalidRequest` with a
/// message that names the setting to fix; none of them is retryable.
pub async fn open_stream(
    profile: &ResolvedModelProfile,
    request: &ModelRequest,
    cancel: &Cancel,
) -> Result<OpenedStream, ProviderError> {
    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target::target_for_profile(profile)
        .await
        .map_err(|error| invalid(error.to_string()))?;
    let adapter = match profile.protocol {
        ProviderProtocol::OpenaiChat => adapter::OpenAiAdapter::chat(target),
        ProviderProtocol::OpenaiResponses => adapter::OpenAiAdapter::responses(target),
        other => return Err(invalid(format!("{other:?} is not an OpenAI protocol"))),
    };
    // The profile names the wire model. The caller's string never overrides it,
    // so the model sent is the one the settings declare.
    let resolved = ResolvedTarget::from_profile(profile);
    adapter
        .validate(&resolved, request)
        .map_err(|error| invalid(error.to_string()))?;
    adapter.send(&resolved, request, cancel).await
}

/// Execute one non-streaming attempt for an OpenAI-protocol profile.
pub async fn execute_non_streaming(
    profile: &ResolvedModelProfile,
    request: &ModelRequest,
    cancel: &Cancel,
) -> Result<crate::services::provider::non_streaming::NonStreamingResponse, ProviderError> {
    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target::target_for_profile(profile)
        .await
        .map_err(|error| invalid(error.to_string()))?;
    let resolved = ResolvedTarget::from_profile(profile);

    match profile.protocol {
        ProviderProtocol::OpenaiChat => {
            let adapter = adapter::OpenAiAdapter::chat(target.clone());
            adapter
                .validate(&resolved, request)
                .map_err(|error| invalid(error.to_string()))?;
            let url = format!("{}/chat/completions", target.base_url);
            let headers = adapter.headers();
            let body = chat::build_body_non_streaming(&resolved, request, target.compat);
            let connect_timeout = adapter.connect_timeout();

            let (_headers, request_id, json_response) =
                transport::post_json(&url, headers, &body, cancel, connect_timeout)
                    .await
                    .map_err(|e| adapter.explain_unauthorized(e))?;

            chat::parse_non_streaming_response(&json_response, request_id)
        }
        ProviderProtocol::OpenaiResponses => {
            let adapter = adapter::OpenAiAdapter::responses(target.clone());
            adapter
                .validate(&resolved, request)
                .map_err(|error| invalid(error.to_string()))?;
            let url = format!("{}/responses", target.base_url);
            let headers = adapter.headers();
            let body = responses::build_body_non_streaming(&resolved, request, adapter.backend());
            let connect_timeout = adapter.connect_timeout();

            let (_headers, request_id, json_response) =
                transport::post_json(&url, headers, &body, cancel, connect_timeout)
                    .await
                    .map_err(|e| adapter.explain_unauthorized(e))?;

            responses::parse_non_streaming_response(&json_response, request_id)
        }
        other => Err(invalid(format!("{other:?} is not an OpenAI protocol"))),
    }
}

/// Test support: install an OpenAI main profile (`protocol`) in the session
/// settings cache, with the key held in `env_name`.
#[cfg(test)]
pub(crate) fn install_test_main_profile(
    base_url: &str,
    env_name: &str,
    protocol: ProviderProtocol,
    capabilities: crate::utils::settings::nyan::ModelCapabilities,
) {
    use crate::utils::settings::nyan::{
        CredentialReference, ModelProfileSettings, NyanSettings, ProviderKind, ProviderSettings,
        SelectionSettings,
    };
    use crate::utils::settings::settings_cache::set_session_settings_cache;
    use crate::utils::settings::validation::SettingsWithErrors;
    use std::collections::BTreeMap;

    let nyan = NyanSettings {
        providers: Some(BTreeMap::from([(
            "openai".to_string(),
            ProviderSettings {
                kind: Some(ProviderKind::Openai),
                protocols: Some(vec![
                    ProviderProtocol::OpenaiChat,
                    ProviderProtocol::OpenaiResponses,
                ]),
                base_url: Some(base_url.to_string()),
                credential: Some(CredentialReference {
                    env: Some(env_name.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )])),
        models: Some(BTreeMap::from([(
            "main".to_string(),
            ModelProfileSettings {
                provider: Some("openai".to_string()),
                model: Some("gpt-5".to_string()),
                protocol: Some(protocol),
                capabilities: Some(capabilities),
                ..Default::default()
            },
        )])),
        selection: Some(SelectionSettings {
            main: Some("main".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    set_session_settings_cache(SettingsWithErrors {
        settings: SettingsJson {
            nyan: Some(Box::new(nyan)),
            ..Default::default()
        },
        ..Default::default()
    });
}

/// The tools an OpenAI request carries. `ToolSearch` is Anthropic's deferred
/// loading mechanism: it answers with `tool_reference` blocks only Anthropic
/// resolves. Here every tool is sent in full instead, so the one that would
/// defer the rest is left out.
pub(crate) fn wire_tools(request: &ModelRequest) -> impl Iterator<Item = &ToolSpec> {
    request
        .tools
        .iter()
        .filter(|tool| tool.name != crate::tools::tool_search_tool::prompt::TOOL_SEARCH_TOOL_NAME)
}

/// The system prompt as one string. The cache boundary marker is an
/// instruction to cache-aware adapters, never wire text.
pub(crate) fn system_text(request: &ModelRequest) -> Option<String> {
    let text = request
        .system
        .iter()
        .filter(|block| block.as_str() != SYSTEM_BOUNDARY && !block.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

/// OpenAI `reasoning_effort` / `reasoning.effort` for the request, or `None`
/// when nothing should be sent.
///
/// Only a request that asks for thinking sends it. A named effort passes
/// through when OpenAI defines it; `max`/`xhigh` are Anthropic-side names with
/// no portable OpenAI equivalent and are sent as `high`. A token budget has no
/// OpenAI counterpart, so it is bucketed. Adaptive without an effort leaves the
/// model's own default.
pub(crate) fn reasoning_effort(request: &ModelRequest) -> Option<&'static str> {
    if request.thinking == ThinkingPreference::Disabled {
        return None;
    }
    match &request.effort {
        Some(Effort::Named(name)) => match name.to_ascii_lowercase().as_str() {
            "minimal" => Some("minimal"),
            "low" => Some("low"),
            "medium" => Some("medium"),
            "high" | "xhigh" | "max" => Some("high"),
            _ => None,
        },
        Some(Effort::Numeric(_)) => None,
        None => match request.thinking {
            ThinkingPreference::Budget {
                tokens: Some(tokens),
            } => Some(match tokens {
                0..=2_048 => "low",
                2_049..=16_384 => "medium",
                _ => "high",
            }),
            _ => None,
        },
    }
}

pub(crate) fn data_url(media_type: &str, data: &str) -> String {
    format!("data:{media_type};base64,{data}")
}

pub(crate) fn media_url(source: &MediaSource) -> String {
    match source {
        MediaSource::Base64 { media_type, data } => data_url(media_type, data),
        MediaSource::Url { url } => url.clone(),
    }
}

/// What a tool result says in words. Images and documents inside it are not
/// text; each protocol places them its own way.
pub(crate) fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(text) => text.clone(),
        ToolResultContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ToolResultPart::Text { text } => Some(text.clone()),
                ToolResultPart::ToolReference { tool_name } => {
                    Some(format!("[tool reference: {tool_name}]"))
                }
                ToolResultPart::Image { .. } | ToolResultPart::Document { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Placeholder for a tool result with no words, because both OpenAI protocols
/// reject an absent output.
pub(crate) const EMPTY_TOOL_OUTPUT: &str = "(no tool output)";

/// A Responses function call has two ids: the `call_id` its output must echo and
/// the `fc_...` item id the server issued. The transcript has one slot for a
/// tool call's id, so it holds both, joined by this separator. The separator
/// uses only characters every provider accepts in a tool id.
pub(crate) const ID_SEPARATOR: &str = "-nyanfc-";

pub(crate) fn join_call_id(call_id: &str, item_id: &str) -> String {
    format!("{call_id}{ID_SEPARATOR}{item_id}")
}

/// The `call_id` half of a (possibly composite) tool-call id.
pub(crate) fn call_part(id: &str) -> &str {
    id.split_once(ID_SEPARATOR).map_or(id, |(call, _)| call)
}

/// The server item id half, if the id is composite.
pub(crate) fn item_part(id: &str) -> Option<&str> {
    id.split_once(ID_SEPARATOR).map(|(_, item)| item)
}

pub(crate) fn u64_at(value: &Value, pointer: &str) -> u64 {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::ir::{
        CachePolicy, ClientContext, Message, ToolChoice, UserPart,
    };

    fn request(thinking: ThinkingPreference, effort: Option<Effort>) -> ModelRequest {
        ModelRequest {
            model: "m".into(),
            system: vec![
                "a".into(),
                SYSTEM_BOUNDARY.into(),
                String::new(),
                "b".into(),
            ],
            messages: vec![Message::User {
                parts: vec![UserPart::Text { text: "hi".into() }],
            }],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            thinking,
            effort,
            max_output_tokens: 100,
            temperature: None,
            structured_output: None,
            cache: CachePolicy::default(),
            query_source: "t".into(),
            agent_id: None,
            disable_parallel_tools: false,
            discovered_tools: vec![],
            tools_pending: false,
            client: ClientContext::default(),
            extensions: vec![],
        }
    }

    #[test]
    fn system_text_drops_the_boundary_marker_and_empty_blocks() {
        let r = request(ThinkingPreference::Disabled, None);
        assert_eq!(system_text(&r).as_deref(), Some("a\n\nb"));
    }

    #[test]
    fn effort_is_sent_only_when_thinking_is_requested_and_openai_defines_it() {
        let named = |n: &str| Some(Effort::Named(n.into()));
        assert_eq!(
            reasoning_effort(&request(ThinkingPreference::Disabled, named("high"))),
            None
        );
        for (given, sent) in [("low", "low"), ("medium", "medium"), ("max", "high")] {
            assert_eq!(
                reasoning_effort(&request(ThinkingPreference::Adaptive, named(given))),
                Some(sent)
            );
        }
        assert_eq!(
            reasoning_effort(&request(ThinkingPreference::Adaptive, named("weird"))),
            None
        );
        assert_eq!(
            reasoning_effort(&request(ThinkingPreference::Adaptive, None)),
            None
        );
        assert_eq!(
            reasoning_effort(&request(
                ThinkingPreference::Budget {
                    tokens: Some(1_000)
                },
                None
            )),
            Some("low")
        );
        assert_eq!(
            reasoning_effort(&request(
                ThinkingPreference::Budget {
                    tokens: Some(30_000)
                },
                None
            )),
            Some("high")
        );
    }
}
