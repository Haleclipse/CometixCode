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
    ToolResultContent, ToolResultPart, ToolSpec,
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
            let body = chat::build_body_non_streaming(&resolved, request, &target.compat);
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
            let body = responses::build_body_non_streaming(
                &resolved,
                request,
                adapter.backend(),
                &target.compat,
            );
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

/// What an OpenAI reasoning model accepts below and above the portable
/// `medium` / `high`.
struct ReasoningTiers {
    /// Value for `off`: the lowest tier the family takes. `None` leaves the
    /// field out, for families not known to accept any off-like value.
    off: Option<&'static str>,
    /// The family takes `xhigh`; without it `max` clamps to `high`.
    xhigh: bool,
}

/// Tiers by model family, from OpenAI's reasoning guide and model pages.
/// Anything unrecognised (compatible gateways, local servers) is treated as
/// taking only `medium` / `high`, so no value is sent that it might reject.
fn reasoning_tiers(model: &str) -> ReasoningTiers {
    let lower = model.to_ascii_lowercase();
    // `openai/gpt-5.5` style ids from gateways name the same model.
    let id = lower.rsplit('/').next().unwrap_or(&lower);
    // GPT-6.1 Sol and GPT-6 Astra reject both `none` and `minimal`.
    if id.starts_with("gpt-6.1-sol") || id.starts_with("gpt-6-astra") {
        return ReasoningTiers {
            off: Some("low"),
            xhigh: true,
        };
    }
    if id.starts_with("gpt-6") {
        return ReasoningTiers {
            off: Some("none"),
            xhigh: true,
        };
    }
    if let Some(rest) = id.strip_prefix("gpt-5") {
        let minor = rest
            .strip_prefix('.')
            .map(|rest| {
                rest.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("")
            })
            .and_then(|digits| digits.parse::<u32>().ok());
        return match minor {
            // gpt-5 itself starts at `minimal`; its Codex variant at `low`.
            None if rest.contains("codex") => ReasoningTiers {
                off: Some("low"),
                xhigh: false,
            },
            None => ReasoningTiers {
                off: Some("minimal"),
                xhigh: false,
            },
            Some(1) => ReasoningTiers {
                off: Some("none"),
                xhigh: false,
            },
            Some(_) => ReasoningTiers {
                off: Some("none"),
                xhigh: true,
            },
        };
    }
    // o-series reasoning models cannot turn reasoning off; `low` is the floor.
    if ["o1", "o3", "o4"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
    {
        return ReasoningTiers {
            off: Some("low"),
            xhigh: false,
        };
    }
    ReasoningTiers {
        off: None,
        xhigh: false,
    }
}

/// OpenAI `reasoning_effort` / `reasoning.effort` for the request, or `None`
/// when nothing should be sent.
///
/// The four unified tiers map onto what the model accepts, clamped silently
/// (provider-redesign §5.2): `off` is the family's lowest tier, `max` is
/// `xhigh` where the family has it and `high` where not. A profile without the
/// thinking capability never sends the field.
pub(crate) fn reasoning_effort(
    request: &ModelRequest,
    target: &ResolvedTarget,
) -> Option<&'static str> {
    if !target.capabilities.thinking {
        return None;
    }
    let tiers = reasoning_tiers(&target.model);
    match request.effort {
        Effort::Off => tiers.off,
        Effort::Medium => Some("medium"),
        Effort::High => Some("high"),
        Effort::Max if tiers.xhigh => Some("xhigh"),
        Effort::Max => Some("high"),
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

/// Maximum length of OpenAI `prompt_cache_key` parameter (64 characters).
pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// Clamps a prompt cache key string to OpenAI's 64-character limit.
pub fn clamp_prompt_cache_key(key: &str) -> String {
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        key.to_string()
    } else {
        key.chars()
            .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
            .collect()
    }
}

pub(crate) use crate::services::provider::usage::u64_at;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::ir::{
        CachePolicy, ClientContext, Message, ToolChoice, UserPart,
    };

    fn request(effort: Effort) -> ModelRequest {
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

    fn test_target(model: &str, thinking: bool) -> ResolvedTarget {
        ResolvedTarget {
            profile: "p".into(),
            provider: "pr".into(),
            protocol: crate::services::provider::ir::Protocol::OpenaiChat,
            model: model.into(),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: crate::utils::model::profile::ResolvedCapabilities {
                thinking,
                ..Default::default()
            },
            cache_retention: crate::utils::settings::nyan::CacheRetention::None,
            extra: None,
        }
    }

    #[test]
    fn system_text_drops_the_boundary_marker_and_empty_blocks() {
        let r = request(Effort::Off);
        assert_eq!(system_text(&r).as_deref(), Some("a\n\nb"));
    }

    fn sent(model: &str, effort: Effort) -> Option<&'static str> {
        reasoning_effort(&request(effort), &test_target(model, true))
    }

    #[test]
    fn a_profile_without_the_thinking_capability_never_sends_effort() {
        for effort in Effort::ALL {
            assert_eq!(
                reasoning_effort(&request(effort), &test_target("gpt-5.5", false)),
                None,
                "{effort}"
            );
        }
    }

    #[test]
    fn medium_and_high_pass_through_on_every_model() {
        for model in ["gpt-6.1-sol", "gpt-5.5", "gpt-5", "o3-mini", "qwen3:32b"] {
            assert_eq!(sent(model, Effort::Medium), Some("medium"), "{model}");
            assert_eq!(sent(model, Effort::High), Some("high"), "{model}");
        }
    }

    #[test]
    fn off_is_the_lowest_tier_each_family_accepts() {
        // GPT-6.1 Sol and Astra reject `none` and `minimal` with a 400.
        assert_eq!(sent("gpt-6.1-sol", Effort::Off), Some("low"));
        assert_eq!(sent("gpt-6-astra", Effort::Off), Some("low"));
        assert_eq!(sent("gpt-6-sol", Effort::Off), Some("none"));
        assert_eq!(sent("gpt-6-luna", Effort::Off), Some("none"));
        assert_eq!(sent("gpt-5.5", Effort::Off), Some("none"));
        assert_eq!(sent("gpt-5.2", Effort::Off), Some("none"));
        assert_eq!(sent("gpt-5.1", Effort::Off), Some("none"));
        assert_eq!(sent("gpt-5", Effort::Off), Some("minimal"));
        assert_eq!(sent("gpt-5-mini", Effort::Off), Some("minimal"));
        assert_eq!(sent("gpt-5-codex", Effort::Off), Some("low"));
        assert_eq!(sent("o3", Effort::Off), Some("low"));
        assert_eq!(sent("o4-mini", Effort::Off), Some("low"));
    }

    #[test]
    fn off_sends_nothing_for_families_not_known_to_accept_a_value() {
        for model in ["deepseek-reasoner", "qwen3:32b", "kimi-k2", "mystery-model"] {
            assert_eq!(sent(model, Effort::Off), None, "{model}");
        }
    }

    #[test]
    fn max_is_xhigh_where_the_family_has_it_and_high_elsewhere() {
        for model in ["gpt-6.1-sol", "gpt-6-sol", "gpt-5.5", "gpt-5.2"] {
            assert_eq!(sent(model, Effort::Max), Some("xhigh"), "{model}");
        }
        for model in [
            "gpt-5.1",
            "gpt-5",
            "gpt-5-codex",
            "o3",
            "o3-mini",
            "qwen3:32b",
        ] {
            assert_eq!(sent(model, Effort::Max), Some("high"), "{model}");
        }
    }

    #[test]
    fn a_gateway_prefix_names_the_same_family() {
        assert_eq!(sent("openai/gpt-5.5", Effort::Max), Some("xhigh"));
        assert_eq!(sent("openai/gpt-6.1-sol", Effort::Off), Some("low"));
        assert_eq!(sent("OpenAI/GPT-5.5", Effort::Off), Some("none"));
    }

    #[test]
    fn minor_versions_are_read_as_numbers_not_prefixes() {
        // `gpt-5.10` is a later minor than `gpt-5.1`, not a variant of it.
        assert_eq!(sent("gpt-5.10", Effort::Max), Some("xhigh"));
        assert_eq!(sent("gpt-5.1-codex", Effort::Max), Some("high"));
    }
}
