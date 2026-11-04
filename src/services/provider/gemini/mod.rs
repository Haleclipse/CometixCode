//! @nyan: Google Gemini adapter (provider redesign PR 6).
//!
//! Handles `ProviderProtocol::Gemini`. Maps request IR to Gemini
//! `GenerateContentRequest`, handles API key authentication, parses
//! Server-Sent Events into IR stream events, and maps Google RPC error statuses.

pub mod adapter;
pub mod errors;
pub mod policy;
pub mod target;
pub mod wire;

#[cfg(test)]
pub mod fixtures;
#[cfg(test)]
pub mod mock;

use crate::services::provider::adapter::{Cancel, OpenedStream, ProviderAdapter, ResolvedTarget};
use crate::services::provider::ir::{ModelRequest, ProviderError, ProviderErrorKind};
use crate::utils::model::profile::ResolvedModelProfile;

/// Open one streaming attempt for a Gemini-protocol profile: resolve its
/// credential, check the request against what the profile declares, send.
pub async fn open_stream(
    profile: &ResolvedModelProfile,
    request: &ModelRequest,
    cancel: &Cancel,
) -> Result<OpenedStream, ProviderError> {
    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target::target_for_profile(profile)
        .await
        .map_err(|error| invalid(error.to_string()))?;
    let adapter = adapter::GeminiAdapter::new(target);
    let resolved = ResolvedTarget::from_profile(profile);
    adapter
        .validate(&resolved, request)
        .map_err(|error| invalid(error.to_string()))?;
    adapter.send(&resolved, request, cancel).await
}

/// Execute one non-streaming attempt for a Gemini-protocol profile.
pub async fn execute_non_streaming(
    profile: &ResolvedModelProfile,
    request: &ModelRequest,
    cancel: &Cancel,
) -> Result<crate::services::provider::non_streaming::NonStreamingResponse, ProviderError> {
    use crate::services::provider::ir::{StopReason, Usage};
    use crate::services::provider::non_streaming::{NonStreamingContent, NonStreamingResponse};
    use crate::services::provider::openai::transport::post_json_classified;

    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target::target_for_profile(profile)
        .await
        .map_err(|error| invalid(error.to_string()))?;
    let resolved = ResolvedTarget::from_profile(profile);
    let adapter = adapter::GeminiAdapter::new(target.clone());
    adapter
        .validate(&resolved, request)
        .map_err(|error| invalid(error.to_string()))?;

    let url = format!(
        "{}/models/{}:generateContent",
        target.base_url, resolved.model
    );
    let headers = adapter.headers();
    let body = serde_json::to_value(wire::build_request(request, &resolved))
        .map_err(|e| invalid(e.to_string()))?;

    let (_headers_map, request_id, json_response) = post_json_classified(
        &url,
        headers,
        &body,
        cancel,
        crate::services::provider::openai::transport::CONNECT_TIMEOUT,
        errors::from_http,
    )
    .await?;

    let parsed: wire::GenerateContentResponse = serde_json::from_value(json_response)
        .map_err(|e| invalid(format!("Gemini response parse error: {e}")))?;

    let candidate = parsed
        .candidates
        .first()
        .ok_or_else(|| invalid("Gemini response contained no candidates".to_string()))?;

    let mut content = Vec::new();
    let mut saw_call = false;

    if let Some(candidate_content) = &candidate.content {
        for part in &candidate_content.parts {
            if let Some(text) = &part.text {
                if part.thought == Some(true) {
                    content.push(NonStreamingContent::Thinking {
                        text: text.clone(),
                        signature: part.thought_signature.clone(),
                    });
                } else {
                    content.push(NonStreamingContent::Text(text.clone()));
                }
            } else if let Some(call) = &part.function_call {
                let id = call.id.clone().unwrap_or_else(|| call.name.clone());
                content.push(NonStreamingContent::ToolUse {
                    id,
                    name: call.name.clone(),
                    input: call.args.clone(),
                });
                saw_call = true;
            }
        }
    }

    let stop_reason = candidate
        .finish_reason
        .as_deref()
        .map(|reason| match reason {
            "STOP" => {
                if saw_call {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                }
            }
            "MAX_TOKENS" => StopReason::MaxTokens,
            "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPI_RESTRICTION" => {
                StopReason::Refusal
            }
            other => StopReason::Other(other.to_string()),
        });

    let usage = parsed
        .usage_metadata
        .as_ref()
        .map(|u| Usage {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            cache_read_tokens: u.cached_content_token_count,
            reasoning_tokens: u.thoughts_token_count,
            ..Default::default()
        })
        .unwrap_or_default();

    let model = parsed
        .model_version
        .unwrap_or_else(|| resolved.model.clone());
    let id = format!("gemini-{}", uuid::Uuid::new_v4());

    Ok(NonStreamingResponse {
        id,
        model,
        request_id,
        content,
        stop_reason,
        usage,
    })
}

/// Count tokens for raw text on a Gemini model.
pub async fn count_tokens(
    profile: &ResolvedModelProfile,
    text: &str,
) -> Result<usize, ProviderError> {
    use crate::services::provider::openai::transport::post_json_classified;
    use serde_json::Value;

    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target::target_for_profile(profile)
        .await
        .map_err(|error| invalid(error.to_string()))?;
    let resolved = ResolvedTarget::from_profile(profile);
    let adapter = adapter::GeminiAdapter::new(target.clone());

    let url = format!("{}/models/{}:countTokens", target.base_url, resolved.model);
    let headers = adapter.headers();
    let body = serde_json::json!({
        "contents": [
            {
                "parts": [
                    { "text": text }
                ]
            }
        ]
    });

    let cancel = Cancel::new();
    let (_headers, _req_id, response) = post_json_classified(
        &url,
        headers,
        &body,
        &cancel,
        crate::services::provider::openai::transport::CONNECT_TIMEOUT,
        errors::from_http,
    )
    .await?;

    let total = response
        .get("totalTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("Gemini countTokens response missing 'totalTokens'".into()))?;
    Ok(total as usize)
}

/// Count tokens for messages on a Gemini model.
pub async fn count_messages_tokens(
    profile: &ResolvedModelProfile,
    messages: &[anthropic_sdk::resources::beta::messages::BetaMessageParam],
) -> Result<usize, ProviderError> {
    use crate::services::provider::openai::transport::post_json_classified;
    use serde_json::Value;

    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target::target_for_profile(profile)
        .await
        .map_err(|error| invalid(error.to_string()))?;
    let resolved = ResolvedTarget::from_profile(profile);
    let adapter = adapter::GeminiAdapter::new(target.clone());

    let url = format!("{}/models/{}:countTokens", target.base_url, resolved.model);
    let headers = adapter.headers();

    let mut contents = Vec::new();
    for msg in messages {
        let role = if msg.role == "assistant" {
            "model"
        } else {
            "user"
        };
        let text = match &msg.content {
            anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(t) => t.clone(),
            anthropic_sdk::resources::beta::messages::BetaMessageContent::Blocks(blocks) => {
                let mut acc = String::new();
                for block in blocks {
                    if let Ok(val) = serde_json::to_value(block)
                        && let Some(t) = val.get("text").and_then(Value::as_str)
                    {
                        acc.push_str(t);
                    }
                }
                acc
            }
        };
        contents.push(serde_json::json!({
            "role": role,
            "parts": [{ "text": text }]
        }));
    }

    let body = serde_json::json!({ "contents": contents });
    let cancel = Cancel::new();
    let (_headers, _req_id, response) = post_json_classified(
        &url,
        headers,
        &body,
        &cancel,
        crate::services::provider::openai::transport::CONNECT_TIMEOUT,
        errors::from_http,
    )
    .await?;

    let total = response
        .get("totalTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("Gemini countTokens response missing 'totalTokens'".into()))?;
    Ok(total as usize)
}

/// Test support: install a Gemini main profile in the session settings cache,
/// pointing to `base_url` with key held in `env_name`.
#[cfg(test)]
pub(crate) fn install_test_main_profile(
    base_url: &str,
    env_name: &str,
    capabilities: crate::utils::settings::nyan::ModelCapabilities,
) {
    use crate::utils::settings::SettingsJson;
    use crate::utils::settings::nyan::{
        CredentialReference, ModelProfileSettings, NyanSettings, ProviderKind, ProviderProtocol,
        ProviderSettings, SelectionSettings,
    };
    use crate::utils::settings::settings_cache::set_session_settings_cache;
    use crate::utils::settings::validation::SettingsWithErrors;
    use std::collections::BTreeMap;

    let nyan = NyanSettings {
        providers: Some(BTreeMap::from([(
            "google".to_string(),
            ProviderSettings {
                kind: Some(ProviderKind::Gemini),
                protocols: Some(vec![ProviderProtocol::Gemini]),
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
                provider: Some("google".to_string()),
                model: Some("gemini-2.5-flash".to_string()),
                protocol: Some(ProviderProtocol::Gemini),
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
