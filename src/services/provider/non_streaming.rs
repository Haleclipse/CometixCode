//! Provider-neutral non-streaming execution for side_query and fallback requests.
//!
//! @nyan: docs/nyan/provider-redesign.md §3.5, §7.2. Dispatches non-streaming
//! requests through the active profile's provider adapter (Anthropic, OpenAI
//! Chat, OpenAI Responses, Gemini, local openaiChat), reusing the request IR,
//! the unified retry controller (decision 19, 23) and each provider's RatePolicy.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::services::provider::adapter::{Cancel, ResolvedTarget};
use crate::services::provider::ir::{
    ModelRequest, ProviderError, ProviderErrorKind, StopReason, Usage,
};
use crate::services::provider::retry::{RetryLimits, to_retryable};
use crate::utils::model::profile::ResolvedModelProfile;
use crate::utils::settings::nyan::ProviderProtocol;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NonStreamingContent {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NonStreamingResponse {
    pub id: String,
    pub model: String,
    pub request_id: Option<String>,
    pub content: Vec<NonStreamingContent>,
    pub stop_reason: Option<StopReason>,
    pub usage: Usage,
}

impl NonStreamingResponse {
    /// Convert to Anthropic SDK `Message` for `side_query` callers.
    pub fn to_api_message(&self) -> anthropic_sdk::resources::messages::Message {
        let content: Vec<Value> = self
            .content
            .iter()
            .map(|item| match item {
                NonStreamingContent::Text(text) => json!({
                    "type": "text",
                    "text": text,
                }),
                NonStreamingContent::ToolUse { id, name, input } => json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }),
                NonStreamingContent::Thinking { text, signature } => json!({
                    "type": "thinking",
                    "thinking": text,
                    "signature": signature.as_deref().unwrap_or_default(),
                }),
            })
            .collect();

        let stop_reason = match &self.stop_reason {
            Some(StopReason::EndTurn) => Some("end_turn"),
            Some(StopReason::ToolUse) => Some("tool_use"),
            Some(StopReason::MaxTokens) => Some("max_tokens"),
            Some(StopReason::StopSequence) => Some("stop_sequence"),
            Some(StopReason::PauseTurn) => Some("pause_turn"),
            Some(StopReason::Refusal) => Some("refusal"),
            _ => None,
        };

        let raw = json!({
            "id": self.id,
            "_request_id": self.request_id,
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": content,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": self.usage.input_tokens,
                "output_tokens": self.usage.output_tokens,
                "cache_read_input_tokens": self.usage.cache_read_tokens,
                "cache_creation_input_tokens": self.usage.cache_write_tokens,
            }
        });

        serde_json::from_value(raw).expect("valid Anthropic Message shape")
    }

    /// Convert to `AssistantMessage` for query and fallback callers.
    pub fn to_assistant_message(
        &self,
        tools: &[crate::types::tools::Tool],
        agent_id: Option<&str>,
    ) -> crate::types::message::AssistantMessage {
        let mut content = Vec::new();
        for item in &self.content {
            match item {
                NonStreamingContent::Text(text) => {
                    content.push(crate::types::message::AssistantContent::Text(text.clone()));
                }
                NonStreamingContent::ToolUse { id, name, input } => {
                    content.push(crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId(id.clone()),
                            name: name.clone(),
                            input: input.clone(),
                        },
                    ));
                }
                NonStreamingContent::Thinking { text, signature } => {
                    content.push(crate::types::message::AssistantContent::Thinking {
                        text: text.clone(),
                        signature: signature.clone().unwrap_or_default(),
                    });
                }
            }
        }
        content.push(crate::types::message::AssistantContent::MessageIdentity(
            crate::types::message::AssistantMessageIdentity::new(
                self.request_id.clone(),
                Some(self.id.clone()),
            ),
        ));
        let mut message = crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content,
            model: Some(self.model.clone()),
            stop_reason: match self.stop_reason {
                Some(StopReason::EndTurn) => Some(crate::types::message::StopReason::EndTurn),
                Some(StopReason::ToolUse) => Some(crate::types::message::StopReason::ToolUse),
                Some(StopReason::MaxTokens) => Some(crate::types::message::StopReason::MaxTokens),
                Some(StopReason::StopSequence) => {
                    Some(crate::types::message::StopReason::StopSequence)
                }
                _ => None,
            },
            usage: Some(crate::types::message::TokenUsage {
                input_tokens: self.usage.input_tokens,
                output_tokens: self.usage.output_tokens,
                cache_read_input_tokens: self.usage.cache_read_tokens,
                cache_creation_input_tokens: self.usage.cache_write_tokens,
                cache_deleted_input_tokens: self.usage.cache_deleted_tokens,
            }),
        };
        message
    }
}

/// Execute a non-streaming request against the given profile, with unified retry logic.
pub async fn execute_non_streaming(
    profile: &ResolvedModelProfile,
    request: &ModelRequest,
    cancel: &Cancel,
    limits: &RetryLimits,
) -> Result<NonStreamingResponse, ProviderError> {
    let protocol = ResolvedTarget::from_profile(profile).protocol;
    let limits_copy = *limits;
    let profile_clone = profile.clone();
    let request_clone = request.clone();
    let cancel_clone = cancel.clone();

    let (heartbeat_tx, _heartbeat_rx) = tokio::sync::mpsc::channel(8);
    let retry_options = crate::services::api::with_retry::RetryOptions {
        max_retries: Some(limits.max_retries),
        model: request.model.clone(),
        fallback_model: None,
        thinking_config: crate::utils::thinking::ThinkingConfig::Disabled,
        fast_mode: None,
        abort_rx: Some(cancel.watch_receiver()),
        query_source: None,
        initial_consecutive_529_errors: None,
    };

    let result = crate::services::api::with_retry::with_retry(
        || async { Ok(()) },
        move |_attempt, _retry_context| {
            let profile = profile_clone.clone();
            let request = request_clone.clone();
            let cancel = cancel_clone.clone();
            async move {
                let call_result = match profile.protocol {
                    ProviderProtocol::AnthropicMessages => {
                        execute_anthropic_non_streaming(&profile, &request, &cancel).await
                    }
                    ProviderProtocol::Gemini => {
                        crate::services::provider::gemini::execute_non_streaming(
                            &profile, &request, &cancel,
                        )
                        .await
                    }
                    ProviderProtocol::OpenaiChat | ProviderProtocol::OpenaiResponses => {
                        crate::services::provider::openai::execute_non_streaming(
                            &profile, &request, &cancel,
                        )
                        .await
                    }
                };

                call_result.map_err(|error| to_retryable(error, protocol, &limits_copy))
            }
        },
        retry_options,
        heartbeat_tx,
    )
    .await;

    match result {
        Ok(response) => Ok(response),
        Err(error) => match error {
            crate::services::api::with_retry::WithRetryError::CannotRetry(cannot_retry) => {
                match cannot_retry.original_error {
                    crate::services::api::with_retry::RetryableError::Aborted => Err(
                        ProviderError::new(ProviderErrorKind::Aborted, "Request was aborted."),
                    ),
                    crate::services::api::with_retry::RetryableError::Connection(conn) => Err(
                        ProviderError::new(ProviderErrorKind::Transport, conn.message),
                    ),
                    crate::services::api::with_retry::RetryableError::Judged {
                        api,
                        kind,
                        request_id,
                        ..
                    } => {
                        let mut pe = ProviderError::new(kind, api.message);
                        pe.status = api.status;
                        pe.headers = api.headers.into_iter().collect();
                        pe.request_id = request_id;
                        Err(pe)
                    }
                    crate::services::api::with_retry::RetryableError::Api(api) => {
                        let mut pe = ProviderError::new(
                            if api.status == Some(404) {
                                ProviderErrorKind::NotFound
                            } else if api.status == Some(401) {
                                ProviderErrorKind::Unauthorized
                            } else {
                                ProviderErrorKind::InvalidRequest
                            },
                            api.message,
                        );
                        pe.status = api.status;
                        pe.headers = api.headers.into_iter().collect();
                        Err(pe)
                    }
                    crate::services::api::with_retry::RetryableError::Other(e) => Err(
                        ProviderError::new(ProviderErrorKind::InvalidRequest, e.to_string()),
                    ),
                }
            }
            crate::services::api::with_retry::WithRetryError::FallbackTriggered(_) => {
                Err(ProviderError::new(
                    ProviderErrorKind::Overloaded,
                    "Model fallback triggered in non-streaming retry",
                ))
            }
        },
    }
}

async fn execute_anthropic_non_streaming(
    profile: &ResolvedModelProfile,
    request: &ModelRequest,
    cancel: &Cancel,
) -> Result<NonStreamingResponse, ProviderError> {
    use crate::services::provider::anthropic::client::{
        GetAnthropicClientOptions, get_anthropic_client,
    };
    use crate::services::provider::anthropic::target::target_for_profile;
    use anthropic_sdk::resources::beta::messages::{
        BetaContentBlock, BetaMessageContent, BetaMessageCreateParams, BetaMessageParam,
        BetaStopReason, BetaToolUnion,
    };
    use anthropic_sdk::resources::messages::{SystemPrompt, TextBlockParam};

    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target_for_profile(profile).map_err(|e| invalid(e.to_string()))?;
    let handle = get_anthropic_client(GetAnthropicClientOptions {
        target,
        max_retries: 0,
        model: Some(request.model.clone()),
        source: Some(request.query_source.clone()),
    })
    .await
    .map_err(|e| invalid(e.to_string()))?;

    let client = handle.build().map_err(|e| invalid(e.to_string()))?;

    let mut messages = Vec::new();
    for msg in &request.messages {
        let role = if msg.is_user() {
            "user".to_string()
        } else {
            "assistant".to_string()
        };
        match msg {
            crate::services::provider::ir::Message::User { parts } => {
                let mut text = String::new();
                for part in parts {
                    if let crate::services::provider::ir::UserPart::Text { text: t } = part {
                        text.push_str(t);
                    }
                }
                messages.push(BetaMessageParam {
                    role,
                    content: BetaMessageContent::Text(text),
                });
            }
            crate::services::provider::ir::Message::Assistant { parts } => {
                let mut text = String::new();
                for part in parts {
                    if let crate::services::provider::ir::AssistantPart::Text { text: t } = part {
                        text.push_str(t);
                    }
                }
                messages.push(BetaMessageParam {
                    role,
                    content: BetaMessageContent::Text(text),
                });
            }
        }
    }

    let system_blocks: Vec<TextBlockParam> = request
        .system
        .iter()
        .filter(|s| !s.is_empty() && s.as_str() != crate::services::provider::ir::SYSTEM_BOUNDARY)
        .map(|s| TextBlockParam {
            text: s.clone(),
            cache_control: None,
            citations: None,
            type_name: Some("text".to_string()),
        })
        .collect();

    let tools: Option<Vec<BetaToolUnion>> = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|t| {
                    BetaToolUnion::Base(anthropic_sdk::resources::messages::ToolUnion::Custom(
                        anthropic_sdk::resources::messages::Tool {
                            name: t.name.clone(),
                            description: Some(t.description.clone()),
                            input_schema: t.input_schema.clone(),
                            cache_control: None,
                            eager_input_streaming: None,
                            strict: Some(t.strict),
                            type_name: Some("custom".to_string()),
                        },
                    ))
                })
                .collect(),
        )
    };

    let tool_choice = match &request.tool_choice {
        crate::services::provider::ir::ToolChoice::Tool { name } => {
            Some(anthropic_sdk::resources::messages::ToolChoice::Tool {
                name: name.clone(),
                disable_parallel_tool_use: None,
            })
        }
        crate::services::provider::ir::ToolChoice::Auto => {
            Some(anthropic_sdk::resources::messages::ToolChoice::Auto {
                disable_parallel_tool_use: None,
            })
        }
        crate::services::provider::ir::ToolChoice::Any => {
            Some(anthropic_sdk::resources::messages::ToolChoice::Any {
                disable_parallel_tool_use: None,
            })
        }
        crate::services::provider::ir::ToolChoice::None => None,
    };

    let params = BetaMessageCreateParams {
        model: request.model.clone(),
        max_tokens: request.max_output_tokens as i64,
        messages,
        system: if system_blocks.is_empty() {
            None
        } else {
            Some(SystemPrompt::Blocks(system_blocks))
        },
        tools,
        tool_choice,
        temperature: request.temperature,
        stream: Some(false),
        ..Default::default()
    };

    let response = client
        .beta()
        .messages()
        .create_with_options(
            &params,
            Some(&anthropic_sdk::RequestOptions {
                max_retries: Some(0),
                signal: Some(cancel.clone()),
                ..Default::default()
            }),
        )
        .await
        .map_err(|error| crate::services::provider::anthropic::wire::provider_error(&error))?;

    let mut content = Vec::new();
    for block in response.content {
        match block {
            BetaContentBlock::Text { text, .. } => {
                content.push(NonStreamingContent::Text(text));
            }
            BetaContentBlock::ToolUse {
                id, name, input, ..
            } => {
                content.push(NonStreamingContent::ToolUse { id, name, input });
            }
            BetaContentBlock::Thinking {
                thinking,
                signature,
                ..
            } => {
                content.push(NonStreamingContent::Thinking {
                    text: thinking,
                    signature: Some(signature),
                });
            }
            _ => {}
        }
    }

    let stop_reason = match response.stop_reason {
        Some(BetaStopReason::EndTurn) => Some(StopReason::EndTurn),
        Some(BetaStopReason::ToolUse) => Some(StopReason::ToolUse),
        Some(BetaStopReason::MaxTokens) => Some(StopReason::MaxTokens),
        Some(BetaStopReason::StopSequence) => Some(StopReason::StopSequence),
        Some(BetaStopReason::PauseTurn) => Some(StopReason::PauseTurn),
        Some(BetaStopReason::Refusal) => Some(StopReason::Refusal),
        _ => None,
    };

    let usage = Usage {
        input_tokens: response.usage.input_tokens.max(0) as u64,
        output_tokens: response.usage.output_tokens.max(0) as u64,
        cache_read_tokens: response.usage.cache_read_input_tokens.unwrap_or(0).max(0) as u64,
        cache_write_tokens: response
            .usage
            .cache_creation_input_tokens
            .unwrap_or(0)
            .max(0) as u64,
        ..Default::default()
    };

    Ok(NonStreamingResponse {
        id: response.id,
        model: response.model,
        request_id: response.request_id,
        content,
        stop_reason,
        usage,
    })
}
