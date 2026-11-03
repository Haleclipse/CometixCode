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
