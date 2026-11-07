//! @nyan: provider-neutral request IR and adapter contract
//! (docs/nyan/provider-redesign.md §3.5, PR 4).

pub mod adapter;
pub mod anthropic;
pub mod gemini;
pub mod history;
pub mod ir;
pub mod non_streaming;
pub mod openai;
pub mod request;
pub mod retry;
pub mod usage;

pub use non_streaming::{NonStreamingContent, NonStreamingResponse, execute_non_streaming};
pub use usage::{CACHE_MISS_NOISE_FLOOR_TOKENS, CacheMiss};
