//! [`ProviderAdapter`] for the Google Gemini API.
//!
//! @nyan: docs/nyan/provider-redesign.md §3.5, §7.4. Manages Gemini request
//! validation, wire serialisation, SSE transport and incremental stream parsing.

use futures::future::BoxFuture;
use serde_json::Value;

use super::errors;
use super::target::{GeminiAuth, GeminiTarget};
use super::wire::{self, GenerateContentResponse};
use crate::services::provider::adapter::{
    Cancel, OpenedStream, ProviderAdapter, ResolvedTarget, merge_extra,
};
use crate::services::provider::ir::{
    BlockStart, Capability, CapabilityError, Delta, Message, ModelRequest, Protocol, ProviderError,
    ProviderErrorKind, StopReason, StreamEventKind, ToolResultContent, ToolResultPart, Usage,
    UserPart,
};
use crate::services::provider::openai::adapter::{Machine, drive};
use crate::services::provider::openai::transport::{
    CONNECT_TIMEOUT, Frame, post_stream_classified,
};

pub struct GeminiAdapter {
    pub target: GeminiTarget,
}

impl GeminiAdapter {
    pub fn new(target: GeminiTarget) -> Self {
        Self { target }
    }

    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("content-type".into(), "application/json".into())];
        if let GeminiAuth::ApiKey(key) = &self.target.auth {
            headers.push(("x-goog-api-key".into(), key.expose().to_string()));
        }
        headers
    }
}

fn unsupported(capability: Capability, detail: &str) -> CapabilityError {
    CapabilityError {
        capability,
        detail: detail.to_string(),
    }
}

fn has_images(request: &ModelRequest) -> bool {
    request.messages.iter().any(|message| match message {
        Message::User { parts } => parts.iter().any(|part| match part {
            UserPart::Image { .. } => true,
            UserPart::ToolResult {
                content: ToolResultContent::Parts(parts),
                ..
            } => parts
                .iter()
                .any(|p| matches!(p, ToolResultPart::Image { .. })),
            _ => false,
        }),
        Message::Assistant { .. } => false,
    })
}

impl ProviderAdapter for GeminiAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Gemini
    }

    fn validate(
        &self,
        target: &ResolvedTarget,
        request: &ModelRequest,
    ) -> Result<(), CapabilityError> {
        if target.protocol != Protocol::Gemini {
            return Err(unsupported(
                Capability::Streaming,
                &format!(
                    "profile {:?} uses protocol {:?}, not Gemini",
                    target.profile, target.protocol
                ),
            ));
        }
        if request.messages.is_empty() {
            return Err(unsupported(
                Capability::Streaming,
                "cannot query a model without at least one message",
            ));
        }
        if request.max_output_tokens == 0 {
            return Err(unsupported(
                Capability::Streaming,
                "max_output_tokens must be positive",
            ));
        }
        let caps = target.capabilities;
        if has_images(request) && !caps.images {
            return Err(unsupported(
                Capability::Images,
                &format!("profile {:?} does not declare image input", target.profile),
            ));
        }
        if request.structured_output.is_some() && !caps.structured_output {
            return Err(unsupported(
                Capability::StructuredOutput,
                &format!(
                    "profile {:?} does not declare structured output",
                    target.profile
                ),
            ));
        }
        Ok(())
    }

    fn send<'a>(
        &'a self,
        target: &'a ResolvedTarget,
        request: &'a ModelRequest,
        cancel: &'a Cancel,
    ) -> BoxFuture<'a, Result<OpenedStream, ProviderError>> {
        Box::pin(async move {
            let url = format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                self.target.base_url, target.model
            );
            let wire_body = wire::build_request(request, target);
            let mut body = serde_json::to_value(&wire_body).map_err(|e| {
                ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    format!("failed to serialize request: {e}"),
                )
            })?;
            merge_extra(&mut body, target.extra.as_ref());
            let headers = self.headers();
            let opened = post_stream_classified(
                &url,
                headers,
                &body,
                cancel,
                CONNECT_TIMEOUT,
                errors::from_http,
            )
            .await?;

            let machine = GeminiStream::new(target.clone(), opened.request_id);
            let events = drive(opened.frames, machine);
            Ok(OpenedStream {
                events,
                response_headers: opened.headers,
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Pure stream machine for Gemini SSE
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveBlock {
    None,
    Text,
    Thinking,
}

pub struct GeminiStream {
    target: ResolvedTarget,
    active_block: ActiveBlock,
    block_index: usize,
    saw_output: bool,
    saw_tool_call: bool,
    accumulated_usage: Usage,
    request_id: Option<String>,
    message_started: bool,
    stop_reason: Option<StopReason>,
}

impl GeminiStream {
    pub fn new(target: ResolvedTarget, request_id: Option<String>) -> Self {
        Self {
            target,
            active_block: ActiveBlock::None,
            block_index: 0,
            saw_output: false,
            saw_tool_call: false,
            accumulated_usage: Usage::default(),
            request_id,
            message_started: false,
            stop_reason: None,
        }
    }
}

impl Machine for GeminiStream {
    fn feed(&mut self, frame: &Frame) -> Result<Vec<StreamEventKind>, ProviderError> {
        let trimmed = frame.data.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Ok(Vec::new());
        }

        let json_val: Value = serde_json::from_str(trimmed).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::Transport,
                format!("failed to parse Gemini stream chunk: {e}"),
            )
        })?;

        // In-stream error check
        if json_val.get("error").is_some() {
            let err = errors::from_stream_error(&json_val);
            return Err(self.mark_partial(err));
        }

        let resp: GenerateContentResponse = serde_json::from_value(json_val).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::Transport,
                format!("unexpected Gemini response schema: {e}"),
            )
        })?;

        let mut events = Vec::new();

        // 1. Emit MessageStart on the first response
        if !self.message_started {
            self.message_started = true;
            events.push(StreamEventKind::MessageStart {
                request_id: self.request_id.clone(),
                message_id: None,
                model: Some(self.target.model.clone()),
                usage: self.accumulated_usage,
            });
        }

        // 2. Usage update
        if let Some(usage) = resp.usage_metadata {
            self.accumulated_usage = Usage::from_gemini_metadata(
                Some(usage.prompt_token_count),
                Some(usage.candidates_token_count),
                Some(usage.cached_content_token_count),
                Some(usage.thoughts_token_count),
            );
        }

        // 3. Process candidate content parts
        for candidate in resp.candidates {
            if let Some(content) = candidate.content {
                for part in content.parts {
                    if part.thought == Some(true) {
                        self.saw_output = true;
                        if self.active_block != ActiveBlock::Thinking {
                            if self.active_block == ActiveBlock::Text {
                                events.push(StreamEventKind::BlockStop {
                                    index: self.block_index,
                                });
                                self.block_index += 1;
                            }
                            events.push(StreamEventKind::BlockStart {
                                index: self.block_index,
                                block: BlockStart::Thinking,
                            });
                            self.active_block = ActiveBlock::Thinking;
                        }
                        if let Some(text) = part.text
                            && !text.is_empty()
                        {
                            events.push(StreamEventKind::BlockDelta {
                                index: self.block_index,
                                delta: Delta::Thinking { text },
                            });
                        }
                        if let Some(sig) = part.thought_signature
                            && !sig.is_empty()
                        {
                            events.push(StreamEventKind::BlockDelta {
                                index: self.block_index,
                                delta: Delta::Signature { signature: sig },
                            });
                        }
                    } else if let Some(fc) = part.function_call {
                        self.saw_output = true;
                        self.saw_tool_call = true;
                        if self.active_block != ActiveBlock::None {
                            events.push(StreamEventKind::BlockStop {
                                index: self.block_index,
                            });
                            self.block_index += 1;
                            self.active_block = ActiveBlock::None;
                        }
                        let call_id = fc
                            .id
                            .unwrap_or_else(|| format!("gemini_call_{}", self.block_index));
                        events.push(StreamEventKind::BlockStart {
                            index: self.block_index,
                            block: BlockStart::ToolCall {
                                id: call_id,
                                name: fc.name,
                                server: false,
                                input: Value::Object(Default::default()),
                            },
                        });
                        let json_args = serde_json::to_string(&fc.args).unwrap_or_default();
                        events.push(StreamEventKind::BlockDelta {
                            index: self.block_index,
                            delta: Delta::ToolInput { json: json_args },
                        });
                        events.push(StreamEventKind::BlockStop {
                            index: self.block_index,
                        });
                        self.block_index += 1;
                    } else if let Some(text) = part.text {
                        if text.is_empty() {
                            continue;
                        }
                        self.saw_output = true;
                        if self.active_block != ActiveBlock::Text {
                            if self.active_block == ActiveBlock::Thinking {
                                events.push(StreamEventKind::BlockStop {
                                    index: self.block_index,
                                });
                                self.block_index += 1;
                            }
                            events.push(StreamEventKind::BlockStart {
                                index: self.block_index,
                                block: BlockStart::Text,
                            });
                            self.active_block = ActiveBlock::Text;
                        }
                        events.push(StreamEventKind::BlockDelta {
                            index: self.block_index,
                            delta: Delta::Text { text },
                        });
                    }
                }
            }

            if let Some(reason) = candidate.finish_reason {
                let mapped = match reason.as_str() {
                    "STOP" => {
                        if self.saw_tool_call {
                            StopReason::ToolUse
                        } else {
                            StopReason::EndTurn
                        }
                    }
                    "MAX_TOKENS" => StopReason::MaxTokens,
                    "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT"
                    | "SPI_RESTRICTION" => StopReason::Refusal,
                    _ => StopReason::Other(reason),
                };
                self.stop_reason = Some(mapped);
            }
        }

        Ok(events)
    }

    fn end(&mut self) -> Result<Vec<StreamEventKind>, ProviderError> {
        if !self.message_started {
            return Err(ProviderError::new(
                ProviderErrorKind::Transport,
                "Gemini stream ended without producing any events",
            ));
        }

        let mut events = Vec::new();
        if self.active_block != ActiveBlock::None {
            events.push(StreamEventKind::BlockStop {
                index: self.block_index,
            });
            self.active_block = ActiveBlock::None;
        }

        let stop_reason = self.stop_reason.take().unwrap_or(if self.saw_tool_call {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        });

        events.push(StreamEventKind::MessageDelta {
            stop_reason: Some(stop_reason),
            usage: self.accumulated_usage,
        });
        events.push(StreamEventKind::MessageStop);

        Ok(events)
    }

    fn mark_partial(&self, error: ProviderError) -> ProviderError {
        if self.saw_output {
            ProviderError {
                kind: ProviderErrorKind::PartialStream,
                message: format!("Gemini stream interrupted: {}", error.message),
                status: error.status,
                retry_after: error.retry_after,
                request_id: error.request_id,
                headers: error.headers,
                body: error.body,
            }
        } else {
            error
        }
    }
}
