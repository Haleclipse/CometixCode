//! @nyan: Anthropic Messages adapter (provider redesign PR 4).
//!
//! Everything Anthropic-specific lives here: the SDK client factory for
//! Direct/Bedrock/Vertex/Foundry, outbound headers, beta flags, and (in later
//! commits) request/stream translation. Callers outside this module see only
//! the adapter contract in `services::provider::adapter`.

pub mod adapter;
pub mod betas;
pub mod cache;
pub mod client;
pub mod credential;
#[cfg(test)]
mod fixtures;
pub mod headers;
#[cfg(test)]
pub(crate) mod mock;
pub mod policy;
pub mod target;
pub mod wire;
