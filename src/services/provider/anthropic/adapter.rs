//! `ProviderAdapter` for the Anthropic Messages protocol.
//!
//! @nyan: `send` opens one streaming attempt against the profile's target and
//! returns normalized IR events; every SDK type stays on this side. One call is
//! one HTTP request: retry policy belongs to the caller (decision 23 keeps the
//! existing Anthropic retry defaults until PR 8).
//!
//! The wire *request* is still produced by the proven query-layer builder in
//! `services/api/claude.rs`, which owns the cache-prefix-sensitive bytes and is
//! pinned by `fixtures.rs`. This adapter therefore accepts a prepared
//! [`WireRequest`] alongside the IR request; migrating request construction
//! itself onto `ModelRequest` is a follow-up that those fixtures guard.

use std::collections::BTreeMap;

use anthropic_sdk::resources::beta::messages::BetaMessageCreateParams;
use futures::StreamExt;
use futures::future::BoxFuture;

use super::client::{GetAnthropicClientOptions, get_anthropic_client};
use super::target::AnthropicTarget;
use super::wire::{StreamNormalizer, provider_error};
use crate::services::provider::adapter::{
    Cancel, EventStream, OpenedStream, ProviderAdapter, ResolvedTarget,
};
use crate::services::provider::ir::{
    Capability, CapabilityError, ModelRequest, Protocol, ProviderError, ProviderErrorKind,
};

/// A fully built Anthropic request: SDK params plus the per-request options
/// (beta header, body patches, extra headers) the query layer computed.
pub struct WireRequest {
    pub params: BetaMessageCreateParams,
    pub options: anthropic_sdk::RequestOptions,
}

pub struct AnthropicAdapter {
    target: AnthropicTarget,
    max_retries: u32,
    source: Option<String>,
    wire: std::sync::Mutex<Option<WireRequest>>,
}

impl AnthropicAdapter {
    pub fn new(target: AnthropicTarget, source: Option<String>) -> Self {
        Self {
            target,
            max_retries: 0,
            source,
            wire: std::sync::Mutex::new(None),
        }
    }

    /// Hand the adapter the request it should send next. Consumed by `send`.
    pub fn prepare(&self, wire: WireRequest) {
        *self.wire.lock().unwrap_or_else(|e| e.into_inner()) = Some(wire);
    }
}

impl ProviderAdapter for AnthropicAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::AnthropicMessages
    }

    /// Reject what this adapter can never serve, before any I/O. Profile
    /// capability checks (tools/thinking/images per model) arrive with the
    /// metadata snapshot in PR 2; until then only structural limits apply.
    fn validate(
        &self,
        target: &ResolvedTarget,
        request: &ModelRequest,
    ) -> Result<(), CapabilityError> {
        if target.protocol != Protocol::AnthropicMessages {
            return Err(CapabilityError {
                capability: Capability::Streaming,
                detail: format!(
                    "profile {:?} uses protocol {:?}, not anthropicMessages",
                    target.profile, target.protocol
                ),
            });
        }
        if request.messages.is_empty() {
            return Err(CapabilityError {
                capability: Capability::Streaming,
                detail: "cannot query a model without at least one message".to_string(),
            });
        }
        if request.max_output_tokens == 0 {
            return Err(CapabilityError {
                capability: Capability::Streaming,
                detail: "max_output_tokens must be positive".to_string(),
            });
        }
        Ok(())
    }

    fn send<'a>(
        &'a self,
        target: &'a ResolvedTarget,
        _request: &'a ModelRequest,
        cancel: &'a Cancel,
    ) -> BoxFuture<'a, Result<OpenedStream, ProviderError>> {
        Box::pin(async move {
            let wire = self
                .wire
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidRequest,
                        "no wire request was prepared for this attempt",
                    )
                })?;
            let handle = get_anthropic_client(GetAnthropicClientOptions {
                target: self.target.clone(),
                max_retries: self.max_retries,
                model: Some(target.model.clone()),
                source: self.source.clone(),
            })
            .await
            .map_err(|error| {
                ProviderError::new(ProviderErrorKind::InvalidRequest, error.to_string())
            })?;
            let client = handle.build().map_err(|error| {
                ProviderError::new(ProviderErrorKind::InvalidRequest, error.to_string())
            })?;

            let mut options = wire.options;
            if options.signal.is_none() {
                options.signal = Some(cancel.clone());
            }
            let response = client
                .beta()
                .messages()
                .create_stream_with_response_and_options(&wire.params, Some(&options))
                .await
                .map_err(|error| provider_error(&error))?;

            let response_headers: BTreeMap<String, String> = response
                .response
                .headers
                .iter()
                .map(|(key, value)| (key.to_ascii_lowercase(), value.clone()))
                .collect();
            let request_id = response.request_id.clone();
            let mut normalizer = StreamNormalizer::default();
            let events: EventStream = Box::pin(response.data.filter_map(move |item| {
                let event = match item {
                    Err(error) => return futures::future::ready(Some(Err(provider_error(&error)))),
                    Ok(event) => event,
                };
                // Re-serialize once: the normalizer reads wire-only usage
                // extensions from the raw JSON, and callers keep it as `wire`.
                let wire = serde_json::to_value(&event).ok();
                let normalized =
                    serde_json::from_value(wire.clone().unwrap_or(serde_json::Value::Null))
                        .ok()
                        .and_then(|event| normalizer.event(event, wire));
                futures::future::ready(normalized.map(|mut event| {
                    if let crate::services::provider::ir::StreamEventKind::MessageStart {
                        request_id: ref mut id,
                        ..
                    } = event.kind
                        && id.is_none()
                    {
                        *id = request_id.clone();
                    }
                    Ok(event)
                }))
            }));
            Ok(OpenedStream {
                events,
                response_headers,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::anthropic::mock::{
        MockAnthropic, Reply, StreamSpec, message_stream,
    };
    use crate::services::provider::ir::{BlockStart, Delta, StopReason, StreamEventKind};
    use serde_json::json;

    fn target(model: &str) -> ResolvedTarget {
        ResolvedTarget {
            profile: "main".into(),
            provider: "anthropic".into(),
            protocol: Protocol::AnthropicMessages,
            model: model.into(),
            context_window: 200_000,
            max_output_tokens: 64_000,
        }
    }

    fn ir_request() -> ModelRequest {
        use crate::services::provider::ir::{
            CachePolicy, ClientContext, Message, ThinkingPreference, ToolChoice, UserPart,
        };
        ModelRequest {
            model: "claude-sonnet-4-6".into(),
            system: vec![],
            messages: vec![Message::User {
                parts: vec![UserPart::Text { text: "hi".into() }],
            }],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            thinking: ThinkingPreference::Disabled,
            effort: None,
            max_output_tokens: 1024,
            temperature: None,
            structured_output: None,
            cache: CachePolicy::default(),
            query_source: "test".into(),
            agent_id: None,
            disable_parallel_tools: false,
            discovered_tools: vec![],
            tools_pending: false,
            client: ClientContext::default(),
            extensions: vec![],
        }
    }

    fn wire(model: &str) -> WireRequest {
        WireRequest {
            params: BetaMessageCreateParams {
                model: model.to_string(),
                max_tokens: 1024,
                messages: vec![anthropic_sdk::resources::beta::messages::BetaMessageParam {
                    role: "user".to_string(),
                    content: anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(
                        "hi".to_string(),
                    ),
                }],
                stream: Some(true),
                ..Default::default()
            },
            options: anthropic_sdk::RequestOptions::default(),
        }
    }

    fn adapter_for(base_url: &str) -> AnthropicAdapter {
        AnthropicAdapter::new(
            AnthropicTarget {
                profile: "main".into(),
                provider: "anthropic".into(),
                base_url: Some(base_url.to_string()),
                api_key: Some(
                    crate::services::provider::anthropic::credential::ApiKey::for_test(
                        "sk-adapter",
                    ),
                ),
            },
            Some("test".into()),
        )
    }

    fn env() -> (
        crate::utils::test_env::TestEnvGuard<'static>,
        Vec<crate::utils::test_env::EnvVarGuard>,
    ) {
        use crate::utils::test_env::EnvVarGuard;
        let lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let guards = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ]
        .into_iter()
        .map(EnvVarGuard::unset)
        .chain([
            EnvVarGuard::set("NO_PROXY", "*"),
            EnvVarGuard::set("no_proxy", "*"),
            EnvVarGuard::set("API_TIMEOUT_MS", "5000"),
        ])
        .collect();
        (lock, guards)
    }

    #[test]
    fn validate_rejects_a_profile_for_another_protocol_and_empty_requests() {
        let adapter = adapter_for("http://127.0.0.1:1");
        let mut other = target("m");
        other.protocol = Protocol::OpenaiChat;
        assert!(adapter.validate(&other, &ir_request()).is_err());

        let mut empty = ir_request();
        empty.messages.clear();
        assert!(adapter.validate(&target("m"), &empty).is_err());

        let mut zero = ir_request();
        zero.max_output_tokens = 0;
        assert!(adapter.validate(&target("m"), &zero).is_err());

        assert!(adapter.validate(&target("m"), &ir_request()).is_ok());
    }

    #[tokio::test]
    async fn send_returns_normalized_ir_events_with_the_header_request_id() {
        let (_lock, _env) = env();
        let mock = MockAnthropic::start(vec![Reply::Sse(message_stream(StreamSpec {
            thinking: Some("hm"),
            text: Some("done"),
            tool_call: Some(("Probe", json!({"a": 1}))),
            stop_reason: "tool_use",
            cache_read_tokens: 40,
            ..StreamSpec::default()
        }))])
        .await;
        let adapter = adapter_for(&mock.base_url);
        adapter.prepare(wire("claude-sonnet-4-6"));
        let cancel = anthropic_sdk::AbortSignal::new();
        let opened = adapter
            .send(&target("claude-sonnet-4-6"), &ir_request(), &cancel)
            .await
            .expect("stream opens");
        let kinds: Vec<StreamEventKind> = opened
            .events
            .map(|event| event.expect("no stream error").kind)
            .collect()
            .await;

        // The adapter, unlike the legacy stream loop, carries the header id.
        match &kinds[0] {
            StreamEventKind::MessageStart {
                request_id,
                message_id,
                usage,
                ..
            } => {
                assert_eq!(request_id.as_deref(), Some("req_mock"));
                assert_eq!(message_id.as_deref(), Some("msg_mock"));
                assert_eq!(usage.cache_read_tokens, 40);
            }
            other => panic!("{other:?}"),
        }
        assert!(kinds.iter().any(|k| matches!(
            k,
            StreamEventKind::BlockStart {
                block: BlockStart::Thinking,
                ..
            }
        )));
        assert!(kinds.iter().any(|k| matches!(
            k,
            StreamEventKind::BlockDelta { delta: Delta::Signature { signature }, .. }
                if signature == "sig-mock"
        )));
        let tool_json: String = kinds
            .iter()
            .filter_map(|k| match k {
                StreamEventKind::BlockDelta {
                    delta: Delta::ToolInput { json },
                    ..
                } => Some(json.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tool_json).unwrap(),
            json!({"a": 1})
        );
        match kinds
            .iter()
            .rev()
            .find(|k| matches!(k, StreamEventKind::MessageDelta { .. }))
        {
            Some(StreamEventKind::MessageDelta { stop_reason, usage }) => {
                assert_eq!(*stop_reason, Some(StopReason::ToolUse));
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.cache_read_tokens, 40);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(kinds.last(), Some(&StreamEventKind::MessageStop));

        let recorded = mock.requests();
        assert_eq!(recorded[0].header("x-api-key"), Some("sk-adapter"));
        assert_eq!(recorded[0].header("authorization"), None);
    }

    #[tokio::test]
    async fn http_errors_surface_as_classified_provider_errors() {
        let (_lock, _env) = env();
        for (status, kind) in [
            (429, ProviderErrorKind::RateLimited),
            (529, ProviderErrorKind::Overloaded),
            (401, ProviderErrorKind::Unauthorized),
        ] {
            let mock = MockAnthropic::start(vec![Reply::Error {
                status,
                body: json!({"type":"error","error":{"type":"x","message":"boom"}}),
            }])
            .await;
            let adapter = adapter_for(&mock.base_url);
            adapter.prepare(wire("claude-sonnet-4-6"));
            let cancel = anthropic_sdk::AbortSignal::new();
            let error = adapter
                .send(&target("claude-sonnet-4-6"), &ir_request(), &cancel)
                .await
                .err()
                .expect("error status must not open a stream");
            assert_eq!(error.kind, kind, "status {status}");
            assert_eq!(error.status, Some(status));
            assert_eq!(error.request_id.as_deref(), Some("req_mock"));
        }
    }

    #[tokio::test]
    async fn send_without_a_prepared_request_is_an_invalid_request() {
        let (_lock, _env) = env();
        let adapter = adapter_for("http://127.0.0.1:1");
        let cancel = anthropic_sdk::AbortSignal::new();
        let error = adapter
            .send(&target("m"), &ir_request(), &cancel)
            .await
            .err()
            .expect("must fail");
        assert_eq!(error.kind, ProviderErrorKind::InvalidRequest);
    }
}
