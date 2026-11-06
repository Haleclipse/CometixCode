//! Adapter contract: the only thing the query layer knows about a provider.
//!
//! @nyan: docs/nyan/provider-redesign.md §3.5. An adapter owns its wire body,
//! headers, betas, credentials, stream parsing, usage/stop-reason mapping and
//! error mapping. Nothing outside an adapter may name a wire type.

use std::pin::Pin;

use futures::Stream;
use futures::future::BoxFuture;

use super::ir::{CapabilityError, ModelRequest, Protocol, ProviderError, StreamEvent};

/// What the request needs from the resolved model, already merged from the
/// metadata snapshot and profile overrides by the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub profile: String,
    pub provider: String,
    pub protocol: Protocol,
    /// Provider wire model id.
    pub model: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
    /// What the profile declares (snapshot under profile overrides). An
    /// undeclared capability is a "no".
    pub capabilities: crate::utils::model::profile::ResolvedCapabilities,
    pub cache_retention: crate::utils::settings::nyan::CacheRetention,
    /// Profile `extra`: merged over the finished request body.
    pub extra: Option<serde_json::Value>,
}

impl ResolvedTarget {
    pub fn from_profile(profile: &crate::utils::model::profile::ResolvedModelProfile) -> Self {
        Self {
            profile: profile.profile.clone(),
            provider: profile.provider.clone(),
            protocol: match profile.protocol {
                crate::utils::settings::nyan::ProviderProtocol::AnthropicMessages => {
                    Protocol::AnthropicMessages
                }
                crate::utils::settings::nyan::ProviderProtocol::OpenaiChat => Protocol::OpenaiChat,
                crate::utils::settings::nyan::ProviderProtocol::OpenaiResponses => {
                    Protocol::OpenaiResponses
                }
                crate::utils::settings::nyan::ProviderProtocol::Gemini => Protocol::Gemini,
            },
            model: profile.model.clone(),
            context_window: profile.context_window,
            max_output_tokens: profile.max_output_tokens,
            capabilities: profile.capabilities,
            cache_retention: profile.cache_retention,
            extra: profile.extra.clone(),
        }
    }
}

/// Cooperative cancellation shared with the query layer.
pub type Cancel = anthropic_sdk::AbortSignal;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

/// A successfully opened stream plus what the HTTP layer learned up front.
pub struct OpenedStream {
    pub events: EventStream,
    /// Response headers, lower-cased names. Used for quota extraction only.
    pub response_headers: std::collections::BTreeMap<String, String>,
}

pub trait ProviderAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;

    /// Reject a request the model cannot serve, before any I/O.
    fn validate(
        &self,
        target: &ResolvedTarget,
        request: &ModelRequest,
    ) -> Result<(), CapabilityError>;

    /// Open one streaming attempt. Retry policy is the caller's; one call is
    /// one HTTP request. Boxed rather than `async fn` so the trait stays
    /// object-safe without adding a macro dependency.
    fn send<'a>(
        &'a self,
        target: &'a ResolvedTarget,
        request: &'a ModelRequest,
        cancel: &'a Cancel,
    ) -> BoxFuture<'a, Result<OpenedStream, ProviderError>>;
}

/// Merges a profile's `extra` over a finished request body, last, so it wins over
/// whatever the adapter computed (provider-redesign §5.3). `extra` is applied
/// as-is: a non-object `extra` or body has nothing to merge into and is ignored.
pub fn merge_extra(body: &mut serde_json::Value, extra: Option<&serde_json::Value>) {
    if let (Some(body), Some(serde_json::Value::Object(extra))) = (body.as_object_mut(), extra) {
        for (key, value) in extra {
            body.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::merge_extra;
    use serde_json::json;

    #[test]
    fn extra_is_merged_last_and_wins_over_the_computed_body() {
        let mut body = json!({"model": "m", "temperature": 1, "metadata": {"a": 1}});
        merge_extra(
            &mut body,
            Some(&json!({"temperature": 0.2, "custom": {"on": true}, "metadata": {"b": 2}})),
        );
        assert_eq!(body["model"], "m");
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["custom"], json!({"on": true}));
        // A shared key is replaced whole, not deep-merged: `extra` is "as written".
        assert_eq!(body["metadata"], json!({"b": 2}));
    }

    #[test]
    fn a_missing_or_non_object_extra_leaves_the_body_alone() {
        let original = json!({"model": "m"});
        for extra in [
            None,
            Some(&json!(null)),
            Some(&json!([1, 2])),
            Some(&json!("x")),
        ] {
            let mut body = original.clone();
            merge_extra(&mut body, extra);
            assert_eq!(body, original);
        }
        let mut not_an_object = json!([1]);
        merge_extra(&mut not_an_object, Some(&json!({"k": 1})));
        assert_eq!(not_an_object, json!([1]));
    }
}
