//! API service layer.
//! Maps to: CC services/api/ — decomposed into focused submodules.
//!
//! Contains the Claude API interaction layer: client construction, streaming
//! queries, retry logic, error classification, and message conversion.

/// Stage-based API request tracing (`NYAN_TRACE` → `~/.claude/trace/`).
pub mod api_trace;

/// Claude API interaction: message conversion, caching, usage tracking, query wrappers.
/// Maps to: CC services/api/claude.ts
pub mod claude;

/// Request/response dump for debugging (`NYAN_DUMP_PROMPTS` → `dump-prompts/`).
/// Maps to: CC services/api/dumpPrompts.ts
pub mod dump_prompts;

/// Error formatting, SSL classification, HTML sanitization for API errors.
/// Maps to: CC services/api/errorUtils.ts
pub mod error_utils;

/// Overage credit grant cached API helpers.
/// Maps to: CC services/api/overageCreditGrant.ts
pub mod overage_credit_grant;

/// Guest passes referral cached API helpers.
/// Maps to: CC services/api/referral.ts
pub mod referral;

/// API error constants, classification, and user-facing message generation.
/// Maps to: CC services/api/errors.ts
pub mod errors;

/// Retry logic with exponential backoff, jitter, 529 fallback, and keep-alive.
/// Maps to: CC services/api/withRetry.ts
pub mod with_retry;
