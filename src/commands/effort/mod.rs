//! Command descriptor for `/effort`.
//!
//! Per design §2:
//! - `/effort` only changes the current session.
//! - Supported levels are the four unified levels: `off < medium < high < max`.

pub mod effort;

pub const NAME: &str = "effort";
pub const DESCRIPTION: &str = "Set effort level for model usage (this session only)";

pub fn effort_argument_hint(_model: &str) -> String {
    "[off|medium|high|max]".to_string()
}

pub fn current_effort_argument_hint() -> String {
    "[off|medium|high|max]".to_string()
}
