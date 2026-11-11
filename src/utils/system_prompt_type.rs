//! Maps to: CC `utils/systemPromptType.ts`.
//!
//! Dependency-free branding helper for system prompt arrays. The concrete
//! `SystemPrompt` type lives in `services/api/request.rs` (Vec alias); this
//! module only provides the `asSystemPrompt` identity wrapper.

pub use crate::services::api::request::SystemPrompt;

/// Maps to: CC `utils/systemPromptType.ts#asSystemPrompt`.
#[inline]
pub fn as_system_prompt(value: SystemPrompt) -> SystemPrompt {
    value
}
