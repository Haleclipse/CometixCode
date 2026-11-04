//! `ProviderAdapter` for OpenAI Chat Completions and Responses.
//!
//! @nyan: one adapter value per protocol. `validate` refuses a profile of the
//! other protocol, so the two never answer each other's requests. One `send` is
//! one HTTP request: retry is the caller's (decision 23: none until PR 8).

use futures::future::BoxFuture;
use serde_json::Value;

use super::chat::{self, ChatStream};
use super::responses::{self, Backend, ResponsesStream};
use super::target::{OpenAiAuth, OpenAiTarget};
use super::transport::{self, Frame, FrameStream};
use crate::services::provider::adapter::{
    Cancel, EventStream, OpenedStream, ProviderAdapter, ResolvedTarget,
};
use crate::services::provider::ir::{
    Capability, CapabilityError, Message, ModelRequest, Protocol, ProviderError, ProviderErrorKind,
    StreamEvent, StreamEventKind, ToolResultContent, ToolResultPart, UserPart,
};

pub struct OpenAiAdapter {
    protocol: Protocol,
    target: OpenAiTarget,
}

impl OpenAiAdapter {
    pub fn chat(target: OpenAiTarget) -> Self {
        Self {
            protocol: Protocol::OpenaiChat,
            target,
        }
    }

    pub fn responses(target: OpenAiTarget) -> Self {
        Self {
            protocol: Protocol::OpenaiResponses,
            target,
        }
    }

    fn backend(&self) -> Backend {
        match self.target.auth {
            OpenAiAuth::Codex { .. } => Backend::Codex,
            _ => Backend::Api,
        }
    }

    fn url(&self) -> String {
        let path = match self.protocol {
            Protocol::OpenaiResponses => "responses",
            _ => "chat/completions",
        };
        format!("{}/{path}", self.target.base_url)
    }

    /// Per-request headers. Settings `httpHeaders` still override these.
    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        match &self.target.auth {
            OpenAiAuth::None => {}
            OpenAiAuth::ApiKey(key) => {
                headers.push(("authorization".into(), format!("Bearer {}", key.expose())));
            }
            OpenAiAuth::Codex { access, account_id } => {
                headers.push((
                    "authorization".into(),
                    format!("Bearer {}", access.expose()),
                ));
                headers.push(("chatgpt-account-id".into(), account_id.clone()));
                headers.push(("openai-beta".into(), "responses=experimental".into()));
                // Required by the ChatGPT backend on every request.
                headers.push(("originator".into(), "nyan".into()));
                let session = crate::bootstrap::state::get_session_id();
                headers.push(("session-id".into(), session.clone()));
                headers.push(("x-client-request-id".into(), session));
            }
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

impl ProviderAdapter for OpenAiAdapter {
    fn protocol(&self) -> Protocol {
        self.protocol
    }

    fn validate(
        &self,
        target: &ResolvedTarget,
        request: &ModelRequest,
    ) -> Result<(), CapabilityError> {
        if target.protocol != self.protocol {
            return Err(unsupported(
                Capability::Streaming,
                &format!(
                    "profile {:?} uses protocol {:?}, not {:?}",
                    target.profile, target.protocol, self.protocol
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
        // Tools and thinking are not errors: a request that asks for them to a
        // model that cannot is answered by leaving them out, as for any
        // provider. An agent whose every tool is dropped still talks.
        Ok(())
    }

    fn send<'a>(
        &'a self,
        target: &'a ResolvedTarget,
        request: &'a ModelRequest,
        cancel: &'a Cancel,
    ) -> BoxFuture<'a, Result<OpenedStream, ProviderError>> {
        Box::pin(async move {
            let body: Value = match self.protocol {
                Protocol::OpenaiResponses => responses::build_body(target, request, self.backend()),
                _ => chat::build_body(target, request),
            };
            let opened = transport::post_stream(&self.url(), self.headers(), &body, cancel).await?;
            let response_headers = opened.headers.clone();
            let events = match self.protocol {
                Protocol::OpenaiResponses => {
                    drive(opened.frames, ResponsesStream::new(opened.request_id))
                }
                _ => drive(opened.frames, ChatStream::new(opened.request_id)),
            };
            Ok(OpenedStream {
                events,
                response_headers,
            })
        })
    }
}

/// A pure stream machine: frames in, IR events out.
trait Machine: Send + 'static {
    fn feed(&mut self, frame: &Frame) -> Result<Vec<StreamEventKind>, ProviderError>;
    fn end(&mut self) -> Result<Vec<StreamEventKind>, ProviderError>;
    /// Reclassify a failure as `PartialStream` once output has been produced,
    /// so a caller never replays a request whose tool calls may have run.
    fn mark_partial(&self, error: ProviderError) -> ProviderError;
}

impl Machine for ChatStream {
    fn feed(&mut self, frame: &Frame) -> Result<Vec<StreamEventKind>, ProviderError> {
        ChatStream::feed(self, frame)
    }
    fn end(&mut self) -> Result<Vec<StreamEventKind>, ProviderError> {
        ChatStream::end(self)
    }
    fn mark_partial(&self, error: ProviderError) -> ProviderError {
        self.partial(error)
    }
}

impl Machine for ResponsesStream {
    fn feed(&mut self, frame: &Frame) -> Result<Vec<StreamEventKind>, ProviderError> {
        ResponsesStream::feed(self, frame)
    }
    fn end(&mut self) -> Result<Vec<StreamEventKind>, ProviderError> {
        ResponsesStream::end(self)
    }
    fn mark_partial(&self, error: ProviderError) -> ProviderError {
        self.partial(error)
    }
}

/// Run frames through a stream machine, yielding IR events. The machine is
/// pure; this is the only place frames and machine meet.
fn drive<M: Machine>(frames: FrameStream, machine: M) -> EventStream {
    use futures::StreamExt;
    struct State<M> {
        frames: FrameStream,
        machine: M,
        queue: std::collections::VecDeque<StreamEventKind>,
        closed: bool,
    }
    let state = State {
        frames,
        machine,
        queue: Default::default(),
        closed: false,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(kind) = state.queue.pop_front() {
                return Some((Ok(StreamEvent::new(kind)), state));
            }
            if state.closed {
                return None;
            }
            match state.frames.next().await {
                Some(Ok(frame)) => match state.machine.feed(&frame) {
                    Ok(kinds) => state.queue.extend(kinds),
                    Err(error) => {
                        state.closed = true;
                        return Some((Err(error), state));
                    }
                },
                Some(Err(error)) => {
                    state.closed = true;
                    // An abort is the user's choice, not a failed stream.
                    let error = if error.kind == ProviderErrorKind::Aborted {
                        error
                    } else {
                        state.machine.mark_partial(error)
                    };
                    return Some((Err(error), state));
                }
                None => {
                    state.closed = true;
                    match state.machine.end() {
                        Ok(kinds) => state.queue.extend(kinds),
                        Err(error) => return Some((Err(error), state)),
                    }
                }
            }
        }
    }))
}
