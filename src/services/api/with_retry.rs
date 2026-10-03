//! Retry logic for Anthropic API calls with exponential backoff, jitter,
//! fast-mode cooldown, 529 fallback, and persistent-retry keep-alive.
//!
//! Maps to: CC `services/api/withRetry.ts:1-823`
//!
//! This module is a 1:1 Rust port of the CC TypeScript retry infrastructure.

use crate::constants::query_source::RetryQuerySource;
use crate::types::message::SystemMessage;
use crate::utils::fast_mode::{is_fast_mode_cooldown, is_fast_mode_enabled};
use crate::utils::messages::create_system_api_error_message;
use crate::utils::model::model::is_non_custom_opus_model;
use crate::utils::process_env::JsTruthy;
use crate::utils::thinking::ThinkingConfig;
use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;

/// Minimal API error representation used by retry logic.
///
/// Maps to: CC `@anthropic-ai/sdk` APIError.
/// TODO: Replace with `anthropic_sdk::ApiError` once the SDK crate is wired
/// into Cargo.toml. For now we define enough surface to express the retry
/// predicates faithfully.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: Option<u16>,
    pub message: String,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Option<serde_json::Value>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(status) = self.status {
            write!(f, "[{}] {}", status, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    /// Check a named header value (case-insensitive key lookup).
    pub fn header(&self, name: &str) -> Option<&str> {
        let key = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Connection-level error (no HTTP status).
///
/// Maps to: CC `@anthropic-ai/sdk` APIConnectionError.
/// TODO: Replace with `anthropic_sdk::ApiError::Connection` variant.
#[derive(Debug, Clone)]
pub struct ConnectionError {
    pub message: String,
    pub code: Option<String>,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "connection error: {}", self.message)
    }
}

impl std::error::Error for ConnectionError {}

/// Unified retry-relevant error type.
///
/// Wraps the subset of error shapes the retry loop needs to inspect.
/// Maps to: the union of `APIError | APIConnectionError | Error` that CC
/// inspects in `withRetry`.
#[derive(Debug)]
pub enum RetryableError {
    Api(ApiError),
    Connection(ConnectionError),
    Aborted,
    Other(anyhow::Error),
}

impl fmt::Display for RetryableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(e) => write!(f, "{}", e),
            Self::Connection(e) => write!(f, "{}", e),
            Self::Aborted => write!(f, "request aborted"),
            Self::Other(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for RetryableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(e) => Some(e),
            Self::Connection(e) => Some(e),
            Self::Other(e) => e.source(),
            Self::Aborted => None,
        }
    }
}

impl RetryableError {
    fn status(&self) -> Option<u16> {
        match self {
            Self::Api(e) => e.status,
            _ => None,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Api(e) => e.message.clone(),
            Self::Connection(e) => e.message.clone(),
            Self::Aborted => "request aborted".to_string(),
            Self::Other(e) => format!("{}", e),
        }
    }

    fn as_api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(e) => Some(e),
            _ => None,
        }
    }

    fn is_connection(&self) -> bool {
        matches!(self, Self::Connection(_))
    }
}

// ---------------------------------------------------------------------------
// Constants
// Maps to: CC services/api/withRetry.ts:51-55
// ---------------------------------------------------------------------------

/// Base delay for exponential backoff in milliseconds.
///
/// Maps to: CC `services/api/withRetry.ts:54` `BASE_DELAY_MS`.
pub const BASE_DELAY_MS: u64 = 500;

/// Maps to: CC `services/api/withRetry.ts:53`
const DEFAULT_MAX_RETRIES: u32 = 10;

/// Maps to: CC `services/api/withRetry.ts:54`
const FLOOR_OUTPUT_TOKENS: u64 = 3000;

/// Maps to: CC `services/api/withRetry.ts:53` `MAX_529_RETRIES`.
const MAX_529_RETRIES: u32 = 3;

/// Maps to: CC `services/api/withRetry.ts:96`
const PERSISTENT_MAX_BACKOFF_MS: u64 = 5 * 60 * 1000;

/// Maps to: CC `services/api/withRetry.ts:97`
const PERSISTENT_RESET_CAP_MS: u64 = 6 * 60 * 60 * 1000;

/// Maps to: CC `services/api/withRetry.ts:98`
const HEARTBEAT_INTERVAL_MS: u64 = 30_000;

/// Maps to: CC `services/api/withRetry.ts:799`
const DEFAULT_FAST_MODE_FALLBACK_HOLD_MS: u64 = 30 * 60 * 1000;

/// Maps to: CC `services/api/withRetry.ts:800`
const SHORT_RETRY_THRESHOLD_MS: u64 = 20 * 1000;

/// Maps to: CC `services/api/withRetry.ts:801`
const MIN_COOLDOWN_MS: u64 = 10 * 60 * 1000;

/// Maps to: CC `services/api/withRetry.ts:47`
const REPEATED_529_ERROR_MESSAGE: &str = "Repeated 529 Overloaded errors";

/// Default maximum delay for regular (non-persistent) backoff.
const DEFAULT_MAX_DELAY_MS: u64 = 32_000;

// ---------------------------------------------------------------------------
// Foreground 529 retry sources
// Maps to: CC services/api/withRetry.ts:62-82
// ---------------------------------------------------------------------------

/// Set of query sources where the user IS blocking on the result.
/// These retry on 529. Background sources bail immediately.
///
/// Maps to: CC `services/api/withRetry.ts:62-82`
static FOREGROUND_529_RETRY_SOURCES: LazyLock<HashSet<RetryQuerySource>> = LazyLock::new(|| {
    let mut set = HashSet::new();
    set.insert(RetryQuerySource::ReplMainThread);
    set.insert(RetryQuerySource::Sdk);
    set.insert(RetryQuerySource::AgentCustom);
    set.insert(RetryQuerySource::AgentDefault);
    set.insert(RetryQuerySource::AgentBuiltin);
    set.insert(RetryQuerySource::Compact);
    set.insert(RetryQuerySource::HookAgent);
    set.insert(RetryQuerySource::HookPrompt);
    set.insert(RetryQuerySource::VerificationAgent);
    set.insert(RetryQuerySource::SideQuestion);
    set.insert(RetryQuerySource::AutoMode);
    // bash_classifier is ant-only; included unconditionally for now.
    set.insert(RetryQuerySource::BashClassifier);
    set
});

// ---------------------------------------------------------------------------
// RetryContext
// Maps to: CC services/api/withRetry.ts:120-125
// ---------------------------------------------------------------------------

/// Mutable context threaded through retry attempts so the operation can adapt
/// (e.g. reduced max_tokens after overflow, model swap after fast-mode cooldown).
///
/// Maps to: CC `services/api/withRetry.ts:120-125`
#[derive(Debug, Clone)]
pub struct RetryContext {
    /// Override max_tokens after a context-overflow 400 error.
    pub max_tokens_override: Option<u64>,
    /// Model name for this attempt (may change on fast-mode fallback).
    pub model: String,
    /// Thinking configuration.
    pub thinking_config: ThinkingConfig,
    /// Whether fast mode is requested for this attempt.
    pub fast_mode: Option<bool>,
}

// ---------------------------------------------------------------------------
// RetryOptions
// Maps to: CC services/api/withRetry.ts:127-142
// ---------------------------------------------------------------------------

/// Options controlling retry behavior.
///
/// Maps to: CC `services/api/withRetry.ts:127-142`
pub struct RetryOptions {
    pub max_retries: Option<u32>,
    pub model: String,
    pub fallback_model: Option<String>,
    pub thinking_config: ThinkingConfig,
    pub fast_mode: Option<bool>,
    /// Cancellation token. When the receiver is dropped or a message is sent,
    /// the retry loop will abort.
    pub abort_rx: Option<tokio::sync::watch::Receiver<bool>>,
    pub query_source: Option<RetryQuerySource>,
    /// Pre-seed the consecutive 529 counter. Used when this retry loop is a
    /// non-streaming fallback after a streaming 529.
    pub initial_consecutive_529_errors: Option<u32>,
}

// ---------------------------------------------------------------------------
// Error types
// Maps to: CC services/api/withRetry.ts:144-167
// ---------------------------------------------------------------------------

/// The operation exhausted all retries or hit a non-retryable error.
///
/// Maps to: CC `services/api/withRetry.ts:144-158` CannotRetryError
#[derive(Debug)]
pub struct CannotRetryError {
    /// The underlying error that caused the retry to fail.
    pub original_error: RetryableError,
    /// Snapshot of the retry context at the time of failure.
    pub retry_context: RetryContext,
}

impl fmt::Display for CannotRetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot retry: {}", self.original_error)
    }
}

impl std::error::Error for CannotRetryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.original_error)
    }
}

/// Model fallback was triggered after repeated 529s on the primary model.
///
/// Maps to: CC `services/api/withRetry.ts:160-168` FallbackTriggeredError
#[derive(Debug, Clone)]
pub struct FallbackTriggeredError {
    pub original_model: String,
    pub fallback_model: String,
}

impl fmt::Display for FallbackTriggeredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Model fallback triggered: {} -> {}",
            self.original_model, self.fallback_model
        )
    }
}

impl std::error::Error for FallbackTriggeredError {}

/// Error from the retry loop.
#[derive(Debug)]
pub enum WithRetryError {
    CannotRetry(CannotRetryError),
    FallbackTriggered(FallbackTriggeredError),
}

impl fmt::Display for WithRetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotRetry(e) => write!(f, "{}", e),
            Self::FallbackTriggered(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for WithRetryError {}

impl From<CannotRetryError> for WithRetryError {
    fn from(e: CannotRetryError) -> Self {
        Self::CannotRetry(e)
    }
}

impl From<FallbackTriggeredError> for WithRetryError {
    fn from(e: FallbackTriggeredError) -> Self {
        Self::FallbackTriggered(e)
    }
}

// ---------------------------------------------------------------------------
// Predicate helpers
// Maps to: CC services/api/withRetry.ts:84-118, 600-695, 696-787
// ---------------------------------------------------------------------------

/// Whether this query source should retry on 529 errors.
/// `None` means retry (conservative for untagged call paths).
///
/// Maps to: CC `services/api/withRetry.ts:84-89`
fn should_retry_529(query_source: Option<&RetryQuerySource>) -> bool {
    match query_source {
        None => true,
        Some(qs) => FOREGROUND_529_RETRY_SOURCES.contains(qs),
    }
}

/// Whether persistent (unattended) retry mode is enabled via env var.
///
/// Maps to: CC `services/api/withRetry.ts:100-104`
fn is_persistent_retry_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_UNATTENDED_RETRY")
            .as_deref(),
    )
}

/// Check if an error is a transient capacity error (429 or 529).
///
/// Maps to: CC `services/api/withRetry.ts:106-110`
fn is_transient_capacity_error(error: &RetryableError) -> bool {
    is_529_error_retryable(error) || error.status() == Some(429)
}

/// Check if the error is a stale keep-alive connection (ECONNRESET / EPIPE).
///
/// Maps to: CC `services/api/withRetry.ts:112-118`
fn is_stale_connection_error(error: &RetryableError) -> bool {
    if let RetryableError::Connection(conn) = error {
        match &conn.code {
            Some(code) => code == "ECONNRESET" || code == "EPIPE",
            None => false,
        }
    } else {
        false
    }
}

/// Returns `true` if the error represents a 529 (overloaded) status or
/// contains an `overloaded_error` JSON type in the message body.
///
/// Maps to: CC `services/api/withRetry.ts:610-621`
pub fn is_529_error(error: &ApiError) -> bool {
    error.status == Some(529) || error.message.contains("\"type\":\"overloaded_error\"")
}

/// Same as `is_529_error` but accepts `RetryableError`.
fn is_529_error_retryable(error: &RetryableError) -> bool {
    match error.as_api() {
        Some(api) => is_529_error(api),
        None => false,
    }
}

/// Whether the error is a 403 "OAuth token has been revoked".
///
/// Maps to: CC `services/api/withRetry.ts:623-629`
fn is_oauth_token_revoked_error(error: &RetryableError) -> bool {
    match error.as_api() {
        Some(api) => {
            api.status == Some(403) && api.message.contains("OAuth token has been revoked")
        }
        None => false,
    }
}

/// Whether the error is a Bedrock auth error.
///
/// Maps to: CC `services/api/withRetry.ts:631-644`
fn is_bedrock_auth_error(error: &RetryableError) -> bool {
    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_BEDROCK").as_deref(),
    ) {
        return false;
    }
    // AWS libs reject with CredentialsProviderError or API returns 403
    // TODO: Check for AWS CredentialsProviderError once aws utils are ported.
    // See CC `utils/aws.ts` isAwsCredentialsProviderError.
    error.status() == Some(403)
}

/// Whether the error is a Vertex (GCP) auth error.
///
/// Maps to: CC `services/api/withRetry.ts:670-682`
fn is_vertex_auth_error(error: &RetryableError) -> bool {
    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_VERTEX").as_deref(),
    ) {
        return false;
    }
    // google-auth-library credential failure messages
    let msg = error.message();
    if msg.contains("Could not load the default credentials")
        || msg.contains("Could not refresh access token")
        || msg.contains("invalid_grant")
    {
        return true;
    }
    error.status() == Some(401)
}

/// Handle AWS credential errors by clearing caches.
///
/// Maps to: CC `services/api/withRetry.ts:650-656`
fn handle_aws_credential_error(error: &RetryableError) -> bool {
    if is_bedrock_auth_error(error) {
        // TODO: Call clearAwsCredentialsCache() once auth module is ported.
        // See CC `utils/auth.ts` clearAwsCredentialsCache.
        true
    } else {
        false
    }
}

/// Handle GCP credential errors by clearing caches.
///
/// Maps to: CC `services/api/withRetry.ts:688-694`
fn handle_gcp_credential_error(error: &RetryableError) -> bool {
    if is_vertex_auth_error(error) {
        // TODO: Call clearGcpCredentialsCache() once auth module is ported.
        // See CC `utils/auth.ts` clearGcpCredentialsCache.
        true
    } else {
        false
    }
}

/// Whether the API error indicates fast mode is not enabled for the org.
///
/// Maps to: CC `services/api/withRetry.ts:600-608`
fn is_fast_mode_not_enabled_error(error: &RetryableError) -> bool {
    match error.as_api() {
        Some(api) => api.status == Some(400) && api.message.contains("Fast mode is not enabled"),
        None => false,
    }
}

/// Whether the error is retryable (standard retry predicate).
///
/// Maps to: CC `services/api/withRetry.ts:696-787`
fn should_retry(error: &ApiError) -> bool {
    // Maps to CC `isMockRateLimitError`: internal mock 429s are terminal and
    // must never enter ordinary retry/persistent backoff.
    if crate::services::rate_limit_mocking::is_mock_rate_limit_error(error.status) {
        return false;
    }

    // Persistent mode: 429/529 always retryable
    if is_persistent_retry_enabled()
        && (error.status == Some(429) || error.status == Some(529) || is_529_error(error))
    {
        return true;
    }

    // CCR mode: auth errors are transient blips
    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref())
        && (error.status == Some(401) || error.status == Some(403))
    {
        return true;
    }

    // Overloaded error in message body
    if error.message.contains("\"type\":\"overloaded_error\"") {
        return true;
    }

    // Context overflow that we can handle
    if parse_max_tokens_context_overflow(error).is_some() {
        return true;
    }

    // x-should-retry header
    let should_retry_header = error.header("x-should-retry");
    if should_retry_header == Some("true") {
        // For Max/Pro users should-retry is true but retry-after is hours.
        // Enterprise users can retry.
        // TODO: Check isClaudeAISubscriber/isEnterpriseSubscriber once auth is ported.
        // See CC `utils/auth.ts:isClaudeAISubscriber`, `isEnterpriseSubscriber`.
        // For now: allow retry (safe default for API-key users).
        return true;
    }
    if should_retry_header == Some("false") {
        let is_5xx = error.status.map_or(false, |s| s >= 500);
        // Internal builds can ignore x-should-retry:false for 5xx only.
        if !crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Api,
        ) || !is_5xx
        {
            return false;
        }
    }

    // Connection errors are retryable (handled separately in the error enum)
    // Status-based retries
    match error.status {
        None => false,
        Some(408) => true, // Request timeout
        Some(409) => true, // Lock timeout
        Some(429) => {
            // TODO: Subscriber gate — see CC `utils/auth.ts`.
            // Default: allow retry for API-key users.
            true
        }
        Some(401) => {
            // TODO: clearApiKeyHelperCache() once auth is ported.
            // See CC `utils/auth.ts:clearApiKeyHelperCache`.
            true
        }
        Some(status) if status >= 500 => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Retry delay calculation
// Maps to: CC services/api/withRetry.ts:530-548
// ---------------------------------------------------------------------------

/// Calculate the delay before the next retry attempt using exponential backoff
/// with jitter.
///
/// If a valid `retry_after_header` is provided (seconds as a decimal string),
/// that value is used directly (converted to milliseconds) — it bypasses the
/// `max_delay_ms` cap because honouring the server directive is correct.
///
/// Otherwise: `min(BASE_DELAY_MS * 2^(attempt-1), max_delay_ms) + jitter`.
///
/// Maps to: CC `services/api/withRetry.ts:530-548`
pub fn get_retry_delay(attempt: u32, retry_after_header: Option<&str>, max_delay_ms: u64) -> u64 {
    if let Some(header) = retry_after_header {
        if let Ok(seconds) = header.parse::<u64>() {
            return seconds * 1000;
        }
    }

    let exp: u64 = 1u64
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u64::MAX);
    let base_delay = BASE_DELAY_MS.saturating_mul(exp).min(max_delay_ms);

    let jitter = (random_unit_interval() * 0.25 * base_delay as f64) as u64;
    base_delay + jitter
}

/// Uniform sample in `[0.0, 1.0)`, mirroring `Math.random()`.
///
/// Maps to: CC `services/api/withRetry.ts:546` `Math.random()`
fn random_unit_interval() -> f64 {
    let mut bytes = [0_u8; 8];
    // A failing OS RNG degrades to zero jitter rather than aborting a retry.
    getrandom::fill(&mut bytes).ok();
    // 53 bits is the widest integer range an f64 represents exactly.
    (u64::from_le_bytes(bytes) >> 11) as f64 / (1_u64 << 53) as f64
}

// ---------------------------------------------------------------------------
// Context overflow parsing
// Maps to: CC services/api/withRetry.ts:550-595
// ---------------------------------------------------------------------------

/// Parsed fields from a "max_tokens exceed context limit" 400 error.
///
/// Maps to: CC `services/api/withRetry.ts:550-556` return type
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextOverflow {
    pub input_tokens: u64,
    pub max_tokens: u64,
    pub context_limit: u64,
}

/// Parse a 400 error for "input length and `max_tokens` exceed context limit"
/// and extract the numeric components.
///
/// Returns `None` if the error is not this specific 400 or the message cannot
/// be parsed.
///
/// Maps to: CC `services/api/withRetry.ts:550-595`
pub fn parse_max_tokens_context_overflow(error: &ApiError) -> Option<ContextOverflow> {
    if error.status != Some(400) {
        return None;
    }

    if !error
        .message
        .contains("input length and `max_tokens` exceed context limit")
    {
        return None;
    }

    // Example format:
    //   "input length and `max_tokens` exceed context limit: 188059 + 20000 > 200000"
    let re = regex::Regex::new(
        r"input length and `max_tokens` exceed context limit: (\d+) \+ (\d+) > (\d+)",
    )
    .ok()?;
    let caps = re.captures(&error.message)?;

    let input_tokens: u64 = caps.get(1)?.as_str().parse().ok()?;
    let max_tokens: u64 = caps.get(2)?.as_str().parse().ok()?;
    let context_limit: u64 = caps.get(3)?.as_str().parse().ok()?;

    Some(ContextOverflow {
        input_tokens,
        max_tokens,
        context_limit,
    })
}

// ---------------------------------------------------------------------------
// Retry-after helpers
// Maps to: CC services/api/withRetry.ts:519-528, 803-822
// ---------------------------------------------------------------------------

/// Extract the `retry-after` header value (seconds string) from an error.
///
/// Maps to: CC `services/api/withRetry.ts:519-528`
fn get_retry_after(error: &RetryableError) -> Option<String> {
    error.as_api()?.header("retry-after").map(|s| s.to_string())
}

/// Parse retry-after header into milliseconds.
///
/// Maps to: CC `services/api/withRetry.ts:803-812`
fn get_retry_after_ms(error: &RetryableError) -> Option<u64> {
    let header = get_retry_after(error)?;
    let seconds: u64 = header.parse().ok()?;
    Some(seconds * 1000)
}

/// Parse the `anthropic-ratelimit-unified-reset` header to compute how long
/// until the rate limit window resets.
///
/// Maps to: CC `services/api/withRetry.ts:814-822`
fn get_rate_limit_reset_delay_ms(error: &ApiError) -> Option<u64> {
    let reset_header = error.header("anthropic-ratelimit-unified-reset")?;
    let reset_unix_sec: f64 = reset_header.parse().ok()?;
    if !reset_unix_sec.is_finite() {
        return None;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let reset_ms = (reset_unix_sec * 1000.0) as u64;
    if reset_ms <= now_ms {
        return None;
    }
    Some((reset_ms - now_ms).min(PERSISTENT_RESET_CAP_MS))
}

// ---------------------------------------------------------------------------
// Default max retries
// Maps to: CC services/api/withRetry.ts:789-797
// ---------------------------------------------------------------------------

/// Read the default maximum number of retries from the environment.
///
/// Falls back to `DEFAULT_MAX_RETRIES` (10) when `CLAUDE_CODE_MAX_RETRIES`
/// is unset or unparseable.
///
/// Maps to: CC `services/api/withRetry.ts:789-794`
pub fn get_default_max_retries() -> u32 {
    crate::utils::process_env::var("CLAUDE_CODE_MAX_RETRIES")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_RETRIES)
}

/// Resolve the effective max retries from options or environment default.
///
/// Maps to: CC `services/api/withRetry.ts:795-797`
fn get_max_retries(options: &RetryOptions) -> u32 {
    options.max_retries.unwrap_or_else(get_default_max_retries)
}

/// Cancellation-aware async sleep.
///
/// Returns `Ok(())` on normal completion, `Err(())` if cancelled.
async fn cancellable_sleep(
    duration: Duration,
    abort_rx: &mut Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<(), ()> {
    match abort_rx {
        Some(rx) => {
            tokio::select! {
                _ = tokio::time::sleep(duration) => Ok(()),
                _ = rx.changed() => Err(()),
            }
        }
        None => {
            tokio::time::sleep(duration).await;
            Ok(())
        }
    }
}

/// Check if the abort signal has fired.
fn is_aborted(abort_rx: &Option<tokio::sync::watch::Receiver<bool>>) -> bool {
    match abort_rx {
        Some(rx) => *rx.borrow(),
        None => false,
    }
}

/// Pre-format the API error text stored on `SystemMessage::ApiError.error`.
///
/// CC stores the whole `APIError` object and calls `formatAPIError(error)` at
/// render time (`SystemAPIErrorMessage.tsx:41`); the D1 decision on the Rust
/// enum is to format at yield time instead. This mirrors the branches of
/// `services/api/errorUtils.ts:200-260` that apply to an `APIError` yielded
/// here (heartbeats are only emitted for `RetryableError::Api`, so the
/// connection/SSL cause-chain branches never apply): the message itself
/// (`:259`), with the missing-message guard `API error (status …)`
/// (`:248-252`).
fn format_api_error(error: &ApiError) -> String {
    if error.message.is_empty() {
        return format!(
            "API error (status {})",
            error
                .status
                .map_or_else(|| "unknown".to_string(), |status| status.to_string())
        );
    }
    error.message.clone()
}

// ---------------------------------------------------------------------------
// withRetry — the core retry loop
// Maps to: CC services/api/withRetry.ts:170-517
// ---------------------------------------------------------------------------

/// Core retry loop that wraps an async API operation with exponential backoff,
/// jitter, 529/429 handling, fast-mode cooldown, persistent retry, and
/// heartbeat keep-alive.
///
/// CC uses `async function* : AsyncGenerator<SystemAPIErrorMessage, T>`
/// (`withRetry.ts:170-178`): every yield IS the system message itself, built
/// by `createSystemAPIErrorMessage(...)` (`:493-498` persistent chunked
/// keep-alive, `:509` regular). Rust equivalent: the sender carries
/// `SystemMessage` values (always the `ApiError` member — the union has no
/// per-variant type to name), and the function returns the generator's final
/// `Result<T, WithRetryError>`.
///
/// Callers must enforce the shared model-I/O safety gate before invoking live
/// API operations through this retry helper.
///
/// Maps to: CC `services/api/withRetry.ts:170-517`
pub async fn with_retry<T, Fut, GetClient, GetClientFut, Op>(
    get_client: GetClient,
    operation: Op,
    mut options: RetryOptions,
    heartbeat_tx: tokio::sync::mpsc::Sender<SystemMessage>,
) -> Result<T, WithRetryError>
where
    T: Send + 'static,
    Fut: std::future::Future<Output = Result<T, RetryableError>> + Send,
    GetClientFut: std::future::Future<Output = Result<(), RetryableError>> + Send,
    GetClient: Fn() -> GetClientFut + Send + Sync,
    Op: Fn(u32, RetryContext) -> Fut + Send + Sync,
{
    let max_retries = get_max_retries(&options);
    let mut retry_context = RetryContext {
        model: options.model.clone(),
        thinking_config: options.thinking_config.clone(),
        fast_mode: if is_fast_mode_enabled() {
            options.fast_mode
        } else {
            None
        },
        max_tokens_override: None,
    };

    let mut client_initialized = false;
    let mut consecutive_529_errors = options.initial_consecutive_529_errors.unwrap_or(0);
    let mut last_error: Option<RetryableError> = None;
    let mut persistent_attempt: u32 = 0;

    let mut attempt: u32 = 1;
    let end = max_retries + 1;

    while attempt <= end {
        // Check abort
        if is_aborted(&options.abort_rx) {
            return Err(CannotRetryError {
                original_error: RetryableError::Aborted,
                retry_context,
            }
            .into());
        }

        // Capture fast-mode state before this attempt
        let was_fast_mode_active = if is_fast_mode_enabled() {
            retry_context.fast_mode.unwrap_or(false) && !is_fast_mode_cooldown()
        } else {
            false
        };
        // Maps to CC `withRetry.ts:201-211`: mock rejection occurs before
        // client construction, so mock scenarios never make a network call.
        let mock_error = crate::services::rate_limit_mocking::check_mock_rate_limit_error(
            &retry_context.model,
            was_fast_mode_active,
        );

        // Maps to CC `withRetry.ts:217-230`: after a stale keep-alive socket,
        // behind its gate, stop pooling so the retry opens a fresh connection.
        // CC throws a mock error (`:207-209`) before this runs.
        if mock_error.is_none()
            && last_error.as_ref().is_some_and(is_stale_connection_error)
            && crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::DisableKeepaliveOnEconnreset,
            )
        {
            crate::utils::debug::log_for_debugging(
                "Stale connection (ECONNRESET/EPIPE) — disabling keep-alive for retry",
            );
            crate::utils::proxy::disable_keep_alive();
        }

        // Refresh client when needed (first attempt, after auth errors, stale connections)
        let needs_client_refresh = !client_initialized
            || last_error
                .as_ref()
                .map(|e| {
                    e.status() == Some(401)
                        || is_oauth_token_revoked_error(e)
                        || is_bedrock_auth_error(e)
                        || is_vertex_auth_error(e)
                        || is_stale_connection_error(e)
                })
                .unwrap_or(false);

        if mock_error.is_none() && needs_client_refresh {
            // TODO: On 401/403 OAuth, call handleOAuth401Error once auth module is ported.
            // See CC `utils/auth.ts:handleOAuth401Error`.
            match (get_client)().await {
                Ok(()) => {
                    client_initialized = true;
                }
                Err(e) => {
                    return Err(CannotRetryError {
                        original_error: e,
                        retry_context,
                    }
                    .into());
                }
            }
        }

        // Bind / refresh request trace for this attempt when the caller has
        // not already set one (streaming open sets a parent trace_id).
        let attempt_trace = crate::services::api::api_trace::current_trace()
            .map(|trace| trace.with_attempt(attempt))
            .unwrap_or_else(|| crate::services::api::api_trace::ApiTrace::new(attempt));
        crate::services::api::api_trace::set_current_trace(Some(attempt_trace.clone()));
        crate::services::api::api_trace::emit(
            &attempt_trace,
            "retry_attempt",
            serde_json::json!({
                "attempt": attempt,
                "max_retries": max_retries,
                "model": retry_context.model,
            }),
        );

        // Execute the operation, or inject the internal mock 429 before I/O.
        let operation_result = if let Some(mock) = mock_error {
            crate::services::claude_ai_limits::extract_quota_status_from_error(
                Some(429),
                Some(&mock.headers),
            );
            Err(RetryableError::Api(ApiError {
                status: Some(429),
                message: mock.message.clone(),
                headers: mock.headers,
                body: Some(serde_json::json!({
                    "error": {"type": "rate_limit_error", "message": mock.message}
                })),
            }))
        } else {
            (operation)(attempt, retry_context.clone()).await
        };
        match operation_result {
            Ok(result) => return Ok(result),
            Err(error) => {
                // Log the error (debug level)
                tracing::debug!(
                    "API error (attempt {}/{}): {}",
                    attempt,
                    max_retries + 1,
                    error
                );
                crate::services::api::api_trace::emit(
                    &attempt_trace,
                    "request_failed",
                    serde_json::json!({
                        "attempt": attempt,
                        "error": error.to_string(),
                    }),
                );

                // --- Fast mode fallback on 429/529 ---
                // Maps to: CC services/api/withRetry.ts:267-305
                if was_fast_mode_active && !is_persistent_retry_enabled() {
                    if let Some(api_err) = error.as_api() {
                        if api_err.status == Some(429) || is_529_error(api_err) {
                            // Check overage-disabled header
                            if let Some(overage_reason) = api_err
                                .header("anthropic-ratelimit-unified-overage-disabled-reason")
                            {
                                if !overage_reason.is_empty() {
                                    // TODO: handleFastModeOverageRejection once fast_mode is ported.
                                    // See CC `utils/fastMode.ts:handleFastModeOverageRejection`.
                                    retry_context.fast_mode = Some(false);
                                    last_error = Some(error);
                                    attempt += 1;
                                    continue;
                                }
                            }

                            let retry_after_ms = get_retry_after_ms(&error);
                            if let Some(ms) = retry_after_ms {
                                if ms < SHORT_RETRY_THRESHOLD_MS {
                                    // Short retry-after: wait with fast mode still active
                                    if cancellable_sleep(
                                        Duration::from_millis(ms),
                                        &mut options.abort_rx,
                                    )
                                    .await
                                    .is_err()
                                    {
                                        return Err(CannotRetryError {
                                            original_error: RetryableError::Aborted,
                                            retry_context,
                                        }
                                        .into());
                                    }
                                    last_error = Some(error);
                                    attempt += 1;
                                    continue;
                                }
                            }

                            // Long or unknown retry-after: enter cooldown
                            let _cooldown_ms = retry_after_ms
                                .unwrap_or(DEFAULT_FAST_MODE_FALLBACK_HOLD_MS)
                                .max(MIN_COOLDOWN_MS);
                            // TODO: triggerFastModeCooldown once fast_mode module is ported.
                            // See CC `utils/fastMode.ts:triggerFastModeCooldown`.
                            if is_fast_mode_enabled() {
                                retry_context.fast_mode = Some(false);
                            }
                            last_error = Some(error);
                            attempt += 1;
                            continue;
                        }
                    }
                }

                // --- Fast mode not enabled error ---
                // Maps to: CC services/api/withRetry.ts:310-314
                if was_fast_mode_active && is_fast_mode_not_enabled_error(&error) {
                    // TODO: handleFastModeRejectedByAPI once fast_mode is ported.
                    // See CC `utils/fastMode.ts:handleFastModeRejectedByAPI`.
                    retry_context.fast_mode = Some(false);
                    last_error = Some(error);
                    attempt += 1;
                    continue;
                }

                // --- Non-foreground 529 bail ---
                // Maps to: CC services/api/withRetry.ts:318-324
                if is_529_error_retryable(&error)
                    && !should_retry_529(options.query_source.as_ref())
                {
                    return Err(CannotRetryError {
                        original_error: error,
                        retry_context,
                    }
                    .into());
                }

                // --- Track consecutive 529 errors ---
                // Maps to: CC services/api/withRetry.ts:327-365
                if is_529_error_retryable(&error) {
                    // TODO: Check FALLBACK_FOR_ALL_PRIMARY_MODELS and isNonCustomOpusModel
                    // once model utils are ported.
                    // See CC `utils/model/model.ts:isNonCustomOpusModel`.
                    let should_track = crate::utils::process_env::var("FALLBACK_FOR_ALL_PRIMARY_MODELS").truthy().is_some()
                        || is_non_custom_opus_model(&options.model);
                    if should_track {
                        consecutive_529_errors += 1;
                        if consecutive_529_errors >= MAX_529_RETRIES {
                            if let Some(ref fallback_model) = options.fallback_model {
                                return Err(FallbackTriggeredError {
                                    original_model: options.model.clone(),
                                    fallback_model: fallback_model.clone(),
                                }
                                .into());
                            }

                            // External users (non-sandbox, non-persistent) get a terminal error
                            if !crate::utils::build_profile::has_internal_capability(
                                crate::utils::build_profile::InternalCapability::Api,
                            ) && crate::utils::process_env::var("IS_SANDBOX").truthy().is_none()
                                && !is_persistent_retry_enabled()
                            {
                                return Err(CannotRetryError {
                                    original_error: RetryableError::Other(anyhow::anyhow!(
                                        "{}",
                                        REPEATED_529_ERROR_MESSAGE
                                    )),
                                    retry_context,
                                }
                                .into());
                            }
                        }
                    }
                }

                // --- Check retry budget ---
                // Maps to: CC services/api/withRetry.ts:368-372
                let persistent =
                    is_persistent_retry_enabled() && is_transient_capacity_error(&error);
                if attempt > max_retries && !persistent {
                    return Err(CannotRetryError {
                        original_error: error,
                        retry_context,
                    }
                    .into());
                }

                // --- Cloud auth error handling ---
                // Maps to: CC services/api/withRetry.ts:375-382
                let handled_cloud_auth =
                    handle_aws_credential_error(&error) || handle_gcp_credential_error(&error);
                if !handled_cloud_auth {
                    match error.as_api() {
                        Some(api) => {
                            if !should_retry(api) {
                                return Err(CannotRetryError {
                                    original_error: error,
                                    retry_context,
                                }
                                .into());
                            }
                        }
                        None if !error.is_connection() => {
                            // Non-API, non-connection errors are not retryable
                            return Err(CannotRetryError {
                                original_error: error,
                                retry_context,
                            }
                            .into());
                        }
                        _ => {
                            // Connection errors are retryable
                        }
                    }
                }

                // --- Context overflow adjustment ---
                // Maps to: CC services/api/withRetry.ts:385-427
                if let Some(api_err) = error.as_api() {
                    if let Some(overflow) = parse_max_tokens_context_overflow(api_err) {
                        let safety_buffer: u64 = 1000;
                        let available_context = overflow
                            .context_limit
                            .saturating_sub(overflow.input_tokens)
                            .saturating_sub(safety_buffer);

                        if available_context < FLOOR_OUTPUT_TOKENS {
                            tracing::error!(
                                "availableContext {} is less than FLOOR_OUTPUT_TOKENS {}",
                                available_context,
                                FLOOR_OUTPUT_TOKENS
                            );
                            return Err(CannotRetryError {
                                original_error: error,
                                retry_context,
                            }
                            .into());
                        }

                        let min_required = match &retry_context.thinking_config {
                            ThinkingConfig::Enabled { budget_tokens } => {
                                budget_tokens.unwrap_or(0).max(0) as u64 + 1
                            }
                            _ => 1,
                        };

                        let adjusted = FLOOR_OUTPUT_TOKENS.max(available_context).max(min_required);
                        retry_context.max_tokens_override = Some(adjusted);

                        last_error = Some(error);
                        attempt += 1;
                        continue;
                    }
                }

                // --- Compute retry delay ---
                // Maps to: CC services/api/withRetry.ts:430-463
                let retry_after = get_retry_after(&error);
                let delay_ms: u64;

                if persistent {
                    persistent_attempt += 1;
                    if let Some(api_err) = error.as_api() {
                        if api_err.status == Some(429) {
                            // Window-based rate limits: wait until reset
                            let reset_delay = get_rate_limit_reset_delay_ms(api_err);
                            delay_ms = reset_delay.unwrap_or_else(|| {
                                get_retry_delay(
                                    persistent_attempt,
                                    retry_after.as_deref(),
                                    PERSISTENT_MAX_BACKOFF_MS,
                                )
                                .min(PERSISTENT_RESET_CAP_MS)
                            });
                        } else {
                            delay_ms = get_retry_delay(
                                persistent_attempt,
                                retry_after.as_deref(),
                                PERSISTENT_MAX_BACKOFF_MS,
                            )
                            .min(PERSISTENT_RESET_CAP_MS);
                        }
                    } else {
                        delay_ms = get_retry_delay(
                            persistent_attempt,
                            retry_after.as_deref(),
                            PERSISTENT_MAX_BACKOFF_MS,
                        )
                        .min(PERSISTENT_RESET_CAP_MS);
                    }
                } else {
                    delay_ms =
                        get_retry_delay(attempt, retry_after.as_deref(), DEFAULT_MAX_DELAY_MS);
                }

                let reported_attempt = if persistent {
                    persistent_attempt
                } else {
                    attempt
                };

                // --- Emit heartbeat(s) and sleep ---
                // Maps to: CC services/api/withRetry.ts:477-512
                if persistent {
                    // Chunk long sleeps for keep-alive heartbeats
                    let mut remaining = delay_ms;
                    while remaining > 0 {
                        if is_aborted(&options.abort_rx) {
                            return Err(CannotRetryError {
                                original_error: RetryableError::Aborted,
                                retry_context,
                            }
                            .into());
                        }

                        // CC `withRetry.ts:492-499`: `yield
                        // createSystemAPIErrorMessage(error, remaining,
                        // reportedAttempt, maxRetries)` — the system message
                        // itself, no intermediate carrier.
                        if let Some(api_err) = error.as_api() {
                            let msg = create_system_api_error_message(
                                format_api_error(api_err),
                                remaining,
                                reported_attempt,
                                max_retries,
                            );
                            let _ = heartbeat_tx.send(msg).await;
                        }

                        let chunk = remaining.min(HEARTBEAT_INTERVAL_MS);
                        if cancellable_sleep(Duration::from_millis(chunk), &mut options.abort_rx)
                            .await
                            .is_err()
                        {
                            return Err(CannotRetryError {
                                original_error: RetryableError::Aborted,
                                retry_context,
                            }
                            .into());
                        }
                        remaining = remaining.saturating_sub(chunk);
                    }

                    // Clamp attempt so the for-loop never terminates in persistent mode.
                    if attempt >= max_retries {
                        attempt = max_retries;
                    }
                } else {
                    // CC `withRetry.ts:508-510`: `yield
                    // createSystemAPIErrorMessage(error, delayMs, attempt,
                    // maxRetries)`.
                    if let Some(api_err) = error.as_api() {
                        let msg = create_system_api_error_message(
                            format_api_error(api_err),
                            delay_ms,
                            attempt,
                            max_retries,
                        );
                        let _ = heartbeat_tx.send(msg).await;
                    }

                    if cancellable_sleep(Duration::from_millis(delay_ms), &mut options.abort_rx)
                        .await
                        .is_err()
                    {
                        return Err(CannotRetryError {
                            original_error: RetryableError::Aborted,
                            retry_context,
                        }
                        .into());
                    }
                }

                last_error = Some(error);
            }
        }

        attempt += 1;
    }

    Err(CannotRetryError {
        original_error: last_error.unwrap_or(RetryableError::Other(anyhow::anyhow!(
            "retry loop exhausted with no error captured"
        ))),
        retry_context,
    }
    .into())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    // -- BASE_DELAY_MS --

    #[test]
    fn base_delay_ms_value() {
        assert_eq!(BASE_DELAY_MS, 500);
    }

    // -- get_retry_delay --

    #[test]
    fn get_retry_delay_exponential_backoff() {
        // Attempt 1: base = 500ms, with some jitter
        let d1 = get_retry_delay(1, None, DEFAULT_MAX_DELAY_MS);
        assert!(d1 >= 500 && d1 < 700, "attempt 1 delay was {}", d1);

        // Attempt 2: base = 1000ms
        let d2 = get_retry_delay(2, None, DEFAULT_MAX_DELAY_MS);
        assert!(d2 >= 1000 && d2 < 1300, "attempt 2 delay was {}", d2);

        // Attempt 3: base = 2000ms
        let d3 = get_retry_delay(3, None, DEFAULT_MAX_DELAY_MS);
        assert!(d3 >= 2000 && d3 < 2600, "attempt 3 delay was {}", d3);
    }

    #[test]
    fn get_retry_delay_respects_max_delay() {
        // Very high attempt should cap at max_delay_ms + jitter
        let d = get_retry_delay(20, None, 5000);
        assert!(d >= 5000 && d < 6300, "high attempt delay was {}", d);
    }

    #[test]
    fn get_retry_delay_retry_after_header() {
        // retry-after header value (seconds) overrides backoff
        let d = get_retry_delay(1, Some("10"), DEFAULT_MAX_DELAY_MS);
        assert_eq!(d, 10_000);
    }

    #[test]
    fn get_retry_delay_invalid_retry_after_falls_back() {
        let d = get_retry_delay(1, Some("not-a-number"), DEFAULT_MAX_DELAY_MS);
        // Should fall back to normal backoff
        assert!(d >= 500 && d < 700, "fallback delay was {}", d);
    }

    // -- parse_max_tokens_context_overflow --

    #[test]
    fn parse_overflow_valid() {
        let err = ApiError {
            status: Some(400),
            message: "input length and `max_tokens` exceed context limit: 188059 + 20000 > 200000"
                .to_string(),
            headers: Default::default(),
            body: None,
        };
        let result = parse_max_tokens_context_overflow(&err).unwrap();
        assert_eq!(result.input_tokens, 188059);
        assert_eq!(result.max_tokens, 20000);
        assert_eq!(result.context_limit, 200000);
    }

    #[test]
    fn parse_overflow_wrong_status() {
        let err = ApiError {
            status: Some(500),
            message: "input length and `max_tokens` exceed context limit: 100 + 100 > 200"
                .to_string(),
            headers: Default::default(),
            body: None,
        };
        assert!(parse_max_tokens_context_overflow(&err).is_none());
    }

    #[test]
    fn parse_overflow_different_message() {
        let err = ApiError {
            status: Some(400),
            message: "some other error".to_string(),
            headers: Default::default(),
            body: None,
        };
        assert!(parse_max_tokens_context_overflow(&err).is_none());
    }

    // -- is_529_error --

    #[test]
    fn is_529_by_status() {
        let err = ApiError {
            status: Some(529),
            message: "".to_string(),
            headers: Default::default(),
            body: None,
        };
        assert!(is_529_error(&err));
    }

    #[test]
    fn is_529_by_message() {
        let err = ApiError {
            status: Some(500),
            message: r#"{"type":"overloaded_error","message":"overloaded"}"#.to_string(),
            headers: Default::default(),
            body: None,
        };
        assert!(is_529_error(&err));
    }

    #[test]
    fn not_529_error() {
        let err = ApiError {
            status: Some(429),
            message: "rate limited".to_string(),
            headers: Default::default(),
            body: None,
        };
        assert!(!is_529_error(&err));
    }

    // -- get_default_max_retries --

    #[test]
    fn default_max_retries_fallback() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        // When env var is not set (which it shouldn't be in tests), default is 10
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_RETRIES");
        assert_eq!(get_default_max_retries(), 10);
    }

    // -- should_retry_529 --

    #[test]
    fn should_retry_529_none_returns_true() {
        assert!(should_retry_529(None));
    }

    #[test]
    fn should_retry_529_foreground_returns_true() {
        assert!(should_retry_529(Some(&RetryQuerySource::ReplMainThread)));
        assert!(should_retry_529(Some(&RetryQuerySource::Sdk)));
        assert!(should_retry_529(Some(&RetryQuerySource::AutoMode)));
    }

    #[test]
    fn should_retry_529_background_returns_false() {
        assert!(!should_retry_529(Some(&RetryQuerySource::Other(
            "summary".to_string()
        ))));
    }

    // -- CannotRetryError / FallbackTriggeredError display --

    #[test]
    fn cannot_retry_error_display() {
        let err = CannotRetryError {
            original_error: RetryableError::Api(ApiError {
                status: Some(500),
                message: "internal server error".to_string(),
                headers: Default::default(),
                body: None,
            }),
            retry_context: RetryContext {
                max_tokens_override: None,
                model: "claude-sonnet-4-20250514".to_string(),
                thinking_config: ThinkingConfig::Disabled,
                fast_mode: None,
            },
        };
        let display = format!("{}", err);
        assert!(display.contains("500"));
        assert!(display.contains("internal server error"));
    }

    #[test]
    fn fallback_triggered_error_display() {
        let err = FallbackTriggeredError {
            original_model: "claude-opus-4-20250514".to_string(),
            fallback_model: "claude-sonnet-4-20250514".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("claude-opus-4-20250514"));
        assert!(display.contains("claude-sonnet-4-20250514"));
    }

    #[tokio::test]
    async fn with_retry_yields_the_system_api_error_message_itself() {
        // CC `withRetry.ts:508-511`: on a retryable failure the generator
        // yields `createSystemAPIErrorMessage(error, delayMs, attempt,
        // maxRetries)` — the system message itself — then sleeps and retries.
        let (heartbeat_tx, mut heartbeat_rx) = tokio::sync::mpsc::channel(4);
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_for_closure = attempts.clone();
        let result: Result<(), WithRetryError> = with_retry(
            || async { Ok(()) },
            move |_attempt, _context| {
                let attempts = attempts_for_closure.clone();
                async move {
                    if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        Err(RetryableError::Api(ApiError {
                            status: Some(500),
                            message: "internal server error".to_string(),
                            headers: Default::default(),
                            body: None,
                        }))
                    } else {
                        Ok(())
                    }
                }
            },
            RetryOptions {
                max_retries: Some(2),
                model: "claude-sonnet-4-20250514".to_string(),
                fallback_model: None,
                thinking_config: ThinkingConfig::Disabled,
                fast_mode: None,
                abort_rx: None,
                query_source: Some(RetryQuerySource::ReplMainThread),
                initial_consecutive_529_errors: None,
            },
            heartbeat_tx,
        )
        .await;

        assert!(result.is_ok());
        match heartbeat_rx.try_recv() {
            Ok(SystemMessage::ApiError {
                error,
                retry_attempt,
                max_retries,
                retry_in_ms,
                ..
            }) => {
                // `error` is the formatAPIError text (errorUtils.ts:259 —
                // the message itself), formatted at yield time per D1.
                assert_eq!(error, "internal server error");
                assert_eq!(retry_attempt, 1);
                assert_eq!(max_retries, 2);
                assert!(retry_in_ms > 0);
            }
            other => panic!("expected one ApiError heartbeat, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn with_retry_triggers_model_fallback_for_non_custom_opus_after_repeated_529() {
        let (heartbeat_tx, _heartbeat_rx) = tokio::sync::mpsc::channel(1);
        let result: Result<(), WithRetryError> = with_retry(
            || async { Ok(()) },
            |_attempt, _context| async {
                Err(RetryableError::Api(ApiError {
                    status: Some(529),
                    message: "overloaded".to_string(),
                    headers: Default::default(),
                    body: None,
                }))
            },
            RetryOptions {
                max_retries: Some(3),
                model: "claude-opus-4-20250514".to_string(),
                fallback_model: Some("claude-sonnet-4-20250514".to_string()),
                thinking_config: ThinkingConfig::Disabled,
                fast_mode: None,
                abort_rx: None,
                query_source: Some(RetryQuerySource::ReplMainThread),
                initial_consecutive_529_errors: Some(MAX_529_RETRIES - 1),
            },
            heartbeat_tx,
        )
        .await;

        match result {
            Err(WithRetryError::FallbackTriggered(fallback)) => {
                assert_eq!(fallback.original_model, "claude-opus-4-20250514");
                assert_eq!(fallback.fallback_model, "claude-sonnet-4-20250514");
            }
            other => panic!("expected fallback trigger, got {other:?}"),
        }
    }

    #[cfg(feature = "anthropic_internal")]
    #[tokio::test]
    async fn internal_mock_429_short_circuits_network_and_normal_retry() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::services::mock_rate_limits::reset_for_test();
        crate::services::claude_ai_limits::reset_for_test();
        crate::services::mock_rate_limits::set_mock_rate_limit_scenario(
            crate::services::mock_rate_limits::MockScenario::SessionLimitReached,
        );
        let client_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let operation_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client_calls_for_closure = client_calls.clone();
        let operation_calls_for_closure = operation_calls.clone();
        let (heartbeat_tx, _heartbeat_rx) = tokio::sync::mpsc::channel(2);
        let result: Result<(), WithRetryError> = with_retry(
            move || {
                let calls = client_calls_for_closure.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            },
            move |_attempt, _context| {
                let calls = operation_calls_for_closure.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            },
            RetryOptions {
                max_retries: Some(3),
                model: "claude-sonnet-4-6".to_string(),
                fallback_model: None,
                thinking_config: ThinkingConfig::Disabled,
                fast_mode: None,
                abort_rx: None,
                query_source: Some(RetryQuerySource::ReplMainThread),
                initial_consecutive_529_errors: None,
            },
            heartbeat_tx,
        )
        .await;

        assert!(matches!(result, Err(WithRetryError::CannotRetry(_))));
        assert_eq!(client_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(operation_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            crate::services::claude_ai_limits::current_limits().status,
            crate::services::claude_ai_limits::QuotaStatus::Rejected
        );
        crate::services::mock_rate_limits::reset_for_test();
        crate::services::claude_ai_limits::reset_for_test();
    }

    // -- random_unit_interval --

    #[test]
    fn jitter_is_bounded() {
        for _ in 0..100 {
            let j = random_unit_interval();
            assert!((0.0..1.0).contains(&j), "jitter {} out of bounds", j);
        }
    }

    #[test]
    fn jitter_is_not_deterministic() {
        let first = random_unit_interval();
        assert!(
            (0..64).any(|_| random_unit_interval() != first),
            "jitter repeated {} across 64 draws",
            first
        );
    }

    #[test]
    fn retry_delay_jitter_varies_across_calls_for_one_attempt() {
        let first = get_retry_delay(6, None, DEFAULT_MAX_DELAY_MS);
        assert!(
            (0..64).any(|_| get_retry_delay(6, None, DEFAULT_MAX_DELAY_MS) != first),
            "attempt 6 delay repeated {} across 64 draws",
            first
        );
    }

    /// CC `withRetry.ts:217-230`: a stale connection turns keep-alive off
    /// only behind `tengu_disable_keepalive_on_econnreset`, whose frozen
    /// default is CC's fallback, off.
    #[tokio::test]
    async fn stale_connection_keeps_keep_alive_while_its_gate_is_off() {
        assert!(!crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::DisableKeepaliveOnEconnreset
        ));
        async fn reset_then_succeed() {
            let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
            let (heartbeat_tx, _heartbeat_rx) = tokio::sync::mpsc::channel(4);
            let result: Result<(), WithRetryError> = with_retry(
                || async { Ok(()) },
                move |_attempt, _context| {
                    let attempts = attempts.clone();
                    async move {
                        if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                            Err(RetryableError::Connection(ConnectionError {
                                message: "connection reset".to_string(),
                                code: Some("ECONNRESET".to_string()),
                            }))
                        } else {
                            Ok(())
                        }
                    }
                },
                RetryOptions {
                    max_retries: Some(2),
                    model: "claude-sonnet-4-20250514".to_string(),
                    fallback_model: None,
                    thinking_config: ThinkingConfig::Disabled,
                    fast_mode: None,
                    abort_rx: None,
                    query_source: Some(RetryQuerySource::ReplMainThread),
                    initial_consecutive_529_errors: None,
                },
                heartbeat_tx,
            )
            .await;
            assert!(result.is_ok());
        }

        crate::utils::proxy::reset_keep_alive_for_testing();
        reset_then_succeed().await;
        assert!(!crate::utils::proxy::is_keep_alive_disabled_for_testing());
    }

    // -- is_stale_connection_error --

    #[test]
    fn stale_connection_econnreset() {
        let err = RetryableError::Connection(ConnectionError {
            message: "connection reset".to_string(),
            code: Some("ECONNRESET".to_string()),
        });
        assert!(is_stale_connection_error(&err));
    }

    #[test]
    fn stale_connection_epipe() {
        let err = RetryableError::Connection(ConnectionError {
            message: "broken pipe".to_string(),
            code: Some("EPIPE".to_string()),
        });
        assert!(is_stale_connection_error(&err));
    }

    #[test]
    fn non_stale_connection() {
        let err = RetryableError::Connection(ConnectionError {
            message: "timeout".to_string(),
            code: Some("ETIMEDOUT".to_string()),
        });
        assert!(!is_stale_connection_error(&err));
    }

    // -- ContextOverflow struct --

    #[test]
    fn context_overflow_equality() {
        let a = ContextOverflow {
            input_tokens: 100,
            max_tokens: 50,
            context_limit: 200,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    // -- ApiError header lookup --

    #[test]
    fn api_error_header_case_insensitive() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("Retry-After".to_string(), "30".to_string());
        let err = ApiError {
            status: Some(429),
            message: "rate limited".to_string(),
            headers,
            body: None,
        };
        assert_eq!(err.header("retry-after"), Some("30"));
        assert_eq!(err.header("RETRY-AFTER"), Some("30"));
    }
}
