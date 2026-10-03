//! API error constants, classification, and message generation.
//!
//! Maps to: CC `services/api/errors.ts` (full file).
//!
//! This module ports the error handling layer that converts raw API errors
//! (from the Anthropic SDK or third-party providers) into user-facing
//! `AssistantMessage` values. Analytics/telemetry calls are omitted per
//! CometixCode conventions; upstream GrowthBook gates are represented by the
//! hardcoded `utils::feature_flags` switch collection when needed.

use regex::Regex;

use crate::constants::api_limits::{API_PDF_MAX_PAGES, PDF_TARGET_RAW_SIZE};
use crate::utils::format::format_file_size;

// ---------------------------------------------------------------------------
// Stub types for modules not yet ported
// ---------------------------------------------------------------------------

/// Stub for CC `types/message.AssistantMessage`.
/// TODO(port): Replace with the canonical CometixCode AssistantMessage once
/// `crate::types::message` gains `is_api_error_message`, `error_details`, and
/// `error` fields matching CC `types/message.ts:38-44`.
///
/// Maps to: CC types/message.ts:37-46
#[derive(Debug, Clone)]
pub struct ApiAssistantMessage {
    /// Whether this message was generated from an API error rather than a
    /// successful model response.
    pub is_api_error_message: bool,
    /// The text content blocks of the message.
    pub content: Vec<ApiAssistantContentBlock>,
    /// Raw error details string (e.g., the original API error message for
    /// prompt-too-long). Used by reactive compact retry.
    pub error_details: Option<String>,
    /// Categorized error type for SDK consumers.
    pub error: Option<SdkAssistantMessageError>,
}

/// A simplified content block for error messages.
/// Maps to: CC types — BetaContentBlock text variant
#[derive(Debug, Clone)]
pub struct ApiAssistantContentBlock {
    pub block_type: String,
    pub text: String,
}

/// Maps to: CC `entrypoints/agentSdkTypes.SDKAssistantMessageError`
/// (coreSchemas.ts:1256-1266).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkAssistantMessageError {
    AuthenticationFailed,
    BillingError,
    RateLimit,
    InvalidRequest,
    ServerError,
    Unknown,
    MaxOutputTokens,
}

impl SdkAssistantMessageError {
    /// Returns the wire-format string matching CC's Zod enum values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "authentication_failed",
            Self::BillingError => "billing_error",
            Self::RateLimit => "rate_limit",
            Self::InvalidRequest => "invalid_request",
            Self::ServerError => "server_error",
            Self::Unknown => "unknown",
            Self::MaxOutputTokens => "max_output_tokens",
        }
    }
}

/// Stub for CC `StopReason` — the SDK type `anthropic_sdk::StopReason` should
/// be used once wired. We need the `Refusal` variant for
/// `get_error_message_if_refusal`.
///
/// Maps to: CC @anthropic-ai/sdk BetaStopReason
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    PauseTurn,
    Refusal,
}

/// Compatibility re-export; canonical ownership is `services/claude_ai_limits.rs`.
pub use crate::services::claude_ai_limits::ClaudeAiLimits;

/// Stub for CC `ConnectionErrorDetails` from `services/api/errorUtils.ts:31-35`.
/// TODO(port): Replace once services/api/errorUtils.rs is ported.
///
/// Maps to: CC services/api/errorUtils.ts:31-35
#[derive(Debug, Clone)]
pub struct ConnectionErrorDetails {
    pub code: String,
    pub message: String,
    pub is_ssl_error: bool,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/errors.ts:54
pub const API_ERROR_MESSAGE_PREFIX: &str = "API Error";

/// Maps to: CC services/api/errors.ts:62
pub const PROMPT_TOO_LONG_ERROR_MESSAGE: &str = "Prompt is too long";

/// Maps to: CC services/api/errors.ts:154
pub const CREDIT_BALANCE_TOO_LOW_ERROR_MESSAGE: &str = "Credit balance is too low";

/// Maps to: CC services/api/errors.ts:155
pub const INVALID_API_KEY_ERROR_MESSAGE: &str = "Not logged in \u{00b7} Please run /login";

/// Maps to: CC services/api/errors.ts:156-157
pub const INVALID_API_KEY_ERROR_MESSAGE_EXTERNAL: &str =
    "Invalid API key \u{00b7} Fix external API key";

/// Maps to: CC services/api/errors.ts:158-159
pub const ORG_DISABLED_ERROR_MESSAGE_ENV_KEY_WITH_OAUTH: &str = "Your ANTHROPIC_API_KEY belongs to a disabled organization \u{00b7} Unset the environment variable to use your subscription instead";

/// Maps to: CC services/api/errors.ts:160-161
pub const ORG_DISABLED_ERROR_MESSAGE_ENV_KEY: &str = "Your ANTHROPIC_API_KEY belongs to a disabled organization \u{00b7} Update or unset the environment variable";

/// Maps to: CC services/api/errors.ts:162-163
pub const TOKEN_REVOKED_ERROR_MESSAGE: &str = "OAuth token revoked \u{00b7} Please run /login";

/// Maps to: CC services/api/errors.ts:164-165
pub const CCR_AUTH_ERROR_MESSAGE: &str =
    "Authentication error \u{00b7} This may be a temporary network issue, please try again";

/// Maps to: CC services/api/errors.ts:166
pub const REPEATED_529_ERROR_MESSAGE: &str = "Repeated 529 Overloaded errors";

/// Maps to: CC services/api/errors.ts:167-168
pub const CUSTOM_OFF_SWITCH_MESSAGE: &str =
    "Opus is experiencing high load, please use /model to switch to Sonnet";

/// Maps to: CC services/api/errors.ts:169
pub const API_TIMEOUT_ERROR_MESSAGE: &str = "Request timed out";

/// Maps to: CC services/api/errors.ts:197-198
pub const OAUTH_ORG_NOT_ALLOWED_ERROR_MESSAGE: &str =
    "Your account does not have access to Claude Code. Please run /login.";

/// Maps to: CC bootstrap/state.ts:getIsNonInteractiveSession.
fn get_is_non_interactive_session() -> bool {
    crate::bootstrap::state::get_is_non_interactive_session()
}

// ---------------------------------------------------------------------------
// Helper: create error message
// ---------------------------------------------------------------------------

/// Constructs an `ApiAssistantMessage` representing an API error.
/// Maps to: CC utils/messages.ts:435-458 `createAssistantAPIErrorMessage`
pub fn create_assistant_api_error_message(
    content: &str,
    error: Option<SdkAssistantMessageError>,
    error_details: Option<String>,
) -> ApiAssistantMessage {
    let text = if content.is_empty() {
        "(no content)".to_string()
    } else {
        content.to_string()
    };
    ApiAssistantMessage {
        is_api_error_message: true,
        content: vec![ApiAssistantContentBlock {
            block_type: "text".to_string(),
            text,
        }],
        error_details,
        error,
    }
}

// ---------------------------------------------------------------------------
// Public functions — pure predicates and parsers
// ---------------------------------------------------------------------------

/// Checks whether `text` starts with the API error message prefix.
///
/// Maps to: CC services/api/errors.ts:56-61
pub fn starts_with_api_error_prefix(text: &str) -> bool {
    text.starts_with(API_ERROR_MESSAGE_PREFIX)
        || text.starts_with(&format!(
            "Please run /login \u{00b7} {}",
            API_ERROR_MESSAGE_PREFIX
        ))
}

/// Predicate: does this assistant message represent a prompt-too-long error?
///
/// Maps to: CC services/api/errors.ts:64-77
pub fn is_prompt_too_long_message(msg: &ApiAssistantMessage) -> bool {
    if !msg.is_api_error_message {
        return false;
    }
    msg.content.iter().any(|block| {
        block.block_type == "text" && block.text.starts_with(PROMPT_TOO_LONG_ERROR_MESSAGE)
    })
}

/// Parse actual/limit token counts from a raw prompt-too-long API error
/// message like "prompt is too long: 137500 tokens > 135000 maximum".
/// The raw string may be wrapped in SDK prefixes or JSON envelopes, or
/// have different casing (Vertex), so this is intentionally lenient.
///
/// Maps to: CC services/api/errors.ts:85-96
pub fn parse_prompt_too_long_token_counts(raw_message: &str) -> (Option<u64>, Option<u64>) {
    let re = Regex::new(r"(?i)prompt is too long[^0-9]*(\d+)\s*tokens?\s*>\s*(\d+)").unwrap();
    match re.captures(raw_message) {
        Some(caps) => {
            let actual = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok());
            let limit = caps.get(2).and_then(|m| m.as_str().parse::<u64>().ok());
            (actual, limit)
        }
        None => (None, None),
    }
}

/// Returns how many tokens over the limit a prompt-too-long error reports,
/// or `None` if the message isn't PTL or its `error_details` are unparseable.
/// Reactive compact uses this gap to jump past multiple groups in one retry
/// instead of peeling one-at-a-time.
///
/// Maps to: CC services/api/errors.ts:104-118
pub fn get_prompt_too_long_token_gap(msg: &ApiAssistantMessage) -> Option<u64> {
    if !is_prompt_too_long_message(msg) || msg.error_details.is_none() {
        return None;
    }
    let details = msg.error_details.as_deref()?;
    let (actual, limit) = parse_prompt_too_long_token_counts(details);
    let actual = actual?;
    let limit = limit?;
    let gap = actual.checked_sub(limit)?;
    if gap > 0 { Some(gap) } else { None }
}

/// Is this raw API error text a media-size rejection that
/// `strip_images_from_messages` can fix?
///
/// Patterns MUST stay in sync with the `get_assistant_message_from_error`
/// branches that populate `error_details` (~L523 PDF, ~L560 image, ~L573
/// many-image) and the `classify_api_error` branches (~L929-946).
///
/// Maps to: CC services/api/errors.ts:133-139
pub fn is_media_size_error(raw: &str) -> bool {
    (raw.contains("image exceeds") && raw.contains("maximum"))
        || (raw.contains("image dimensions exceed") && raw.contains("many-image"))
        || Regex::new(r"maximum of \d+ PDF pages")
            .unwrap()
            .is_match(raw)
}

/// Message-level predicate: is this assistant message a media-size rejection?
///
/// Maps to: CC services/api/errors.ts:147-153
pub fn is_media_size_error_message(msg: &ApiAssistantMessage) -> bool {
    msg.is_api_error_message
        && msg
            .error_details
            .as_deref()
            .map_or(false, is_media_size_error)
}

// ---------------------------------------------------------------------------
// Error message generators (session-context-aware)
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/errors.ts:170-175
pub fn get_pdf_too_large_error_message() -> String {
    let limits = format!(
        "max {} pages, {}",
        API_PDF_MAX_PAGES,
        format_file_size(PDF_TARGET_RAW_SIZE)
    );
    if get_is_non_interactive_session() {
        format!(
            "PDF too large ({limits}). Try reading the file a different way (e.g., extract text with pdftotext)."
        )
    } else {
        format!(
            "PDF too large ({limits}). Double press esc to go back and try again, or use pdftotext to convert to text first."
        )
    }
}

/// Maps to: CC services/api/errors.ts:176-180
pub fn get_pdf_password_protected_error_message() -> &'static str {
    if get_is_non_interactive_session() {
        "PDF is password protected. Try using a CLI tool to extract or convert the PDF."
    } else {
        "PDF is password protected. Please double press esc to edit your message and try again."
    }
}

/// Maps to: CC services/api/errors.ts:181-185
pub fn get_pdf_invalid_error_message() -> &'static str {
    if get_is_non_interactive_session() {
        "The PDF file was not valid. Try converting it to text first (e.g., pdftotext)."
    } else {
        "The PDF file was not valid. Double press esc to go back and try again with a different file."
    }
}

/// Maps to: CC services/api/errors.ts:186-190
pub fn get_image_too_large_error_message() -> &'static str {
    if get_is_non_interactive_session() {
        "Image was too large. Try resizing the image or using a different approach."
    } else {
        "Image was too large. Double press esc to go back and try again with a smaller image."
    }
}

/// Maps to: CC services/api/errors.ts:191-196
pub fn get_request_too_large_error_message() -> String {
    let limits = format!("max {}", format_file_size(PDF_TARGET_RAW_SIZE));
    if get_is_non_interactive_session() {
        format!("Request too large ({limits}). Try with a smaller file.")
    } else {
        format!(
            "Request too large ({limits}). Double press esc to go back and try with a smaller file."
        )
    }
}

/// Maps to: CC services/api/errors.ts:200-204
pub fn get_token_revoked_error_message() -> &'static str {
    if get_is_non_interactive_session() {
        "Your account does not have access to Claude. Please login again or contact your administrator."
    } else {
        TOKEN_REVOKED_ERROR_MESSAGE
    }
}

/// Maps to: CC services/api/errors.ts:206-210
pub fn get_oauth_org_not_allowed_error_message() -> &'static str {
    if get_is_non_interactive_session() {
        "Your organization does not have access to Claude. Please login again or contact your administrator."
    } else {
        OAUTH_ORG_NOT_ALLOWED_ERROR_MESSAGE
    }
}

/// Check if we're in CCR (Claude Code Remote) mode.
///
/// Maps to: CC services/api/errors.ts:217-219
fn is_ccr_mode() -> bool {
    crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref())
}

// ---------------------------------------------------------------------------
// Type guard / validation
// ---------------------------------------------------------------------------

/// Type guard to check if a JSON value looks like a valid Message response
/// from the API (has `content` array, `model` string, `usage` object).
///
/// In Rust, deserialization via serde handles most validation, but this is
/// useful for pre-checking untyped `serde_json::Value`.
///
/// Maps to: CC services/api/errors.ts:387-398
pub fn is_valid_api_message(value: &serde_json::Value) -> bool {
    value.is_object()
        && value.get("content").map_or(false, |c| c.is_array())
        && value.get("model").map_or(false, |m| m.is_string())
        && value.get("usage").map_or(false, |u| u.is_object())
}

/// Given a response that doesn't look quite right, see if it contains any
/// known error types we can extract (e.g. Bedrock AmazonError shape).
///
/// Maps to: CC services/api/errors.ts:411-423
pub fn extract_unknown_error_format(value: &serde_json::Value) -> Option<String> {
    // Amazon Bedrock routing errors: { Output: { __type: "..." } }
    value
        .get("Output")
        .and_then(|o| o.get("__type"))
        .and_then(|t| t.as_str())
        .map(String::from)
}

// ---------------------------------------------------------------------------
// Unified error-kind enum for classify + format
// ---------------------------------------------------------------------------

/// Standardized error classification for analytics/datadog tagging.
/// Each variant maps to a string tag in CC's `classifyAPIError`.
///
/// Maps to: CC services/api/errors.ts:965-1161 return values
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorClass {
    Aborted,
    ApiTimeout,
    Repeated529,
    CapacityOffSwitch,
    RateLimit,
    ServerOverload,
    PromptTooLong,
    PdfTooLarge,
    PdfPasswordProtected,
    ImageTooLarge,
    ToolUseMismatch,
    UnexpectedToolResult,
    DuplicateToolUseId,
    InvalidModel,
    CreditBalanceLow,
    InvalidApiKey,
    TokenRevoked,
    OauthOrgNotAllowed,
    AuthError,
    BedrockModelAccess,
    ServerError,
    ClientError,
    SslCertError,
    ConnectionError,
    Unknown,
}

impl ApiErrorClass {
    /// Returns the analytics tag string matching CC's `classifyAPIError` return values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::ApiTimeout => "api_timeout",
            Self::Repeated529 => "repeated_529",
            Self::CapacityOffSwitch => "capacity_off_switch",
            Self::RateLimit => "rate_limit",
            Self::ServerOverload => "server_overload",
            Self::PromptTooLong => "prompt_too_long",
            Self::PdfTooLarge => "pdf_too_large",
            Self::PdfPasswordProtected => "pdf_password_protected",
            Self::ImageTooLarge => "image_too_large",
            Self::ToolUseMismatch => "tool_use_mismatch",
            Self::UnexpectedToolResult => "unexpected_tool_result",
            Self::DuplicateToolUseId => "duplicate_tool_use_id",
            Self::InvalidModel => "invalid_model",
            Self::CreditBalanceLow => "credit_balance_low",
            Self::InvalidApiKey => "invalid_api_key",
            Self::TokenRevoked => "token_revoked",
            Self::OauthOrgNotAllowed => "oauth_org_not_allowed",
            Self::AuthError => "auth_error",
            Self::BedrockModelAccess => "bedrock_model_access",
            Self::ServerError => "server_error",
            Self::ClientError => "client_error",
            Self::SslCertError => "ssl_cert_error",
            Self::ConnectionError => "connection_error",
            Self::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Intermediate error representation
// ---------------------------------------------------------------------------

/// A provider-agnostic view of an API error, used by both
/// `classify_api_error` and `get_assistant_message_from_error`.
///
/// This is the Rust equivalent of discriminating on `instanceof APIError` /
/// `instanceof APIConnectionError` / `instanceof Error` in the TS source.
/// Callers construct this from `anthropic_sdk::ApiError` or `reqwest::Error`
/// before passing into the classifier.
///
/// Maps to: CC services/api/errors.ts error discrimination pattern
#[derive(Debug, Clone)]
pub enum ApiErrorInfo {
    /// SDK HTTP error with status, message, and optional headers.
    Http {
        status: u16,
        message: String,
        headers: std::collections::HashMap<String, String>,
        body: Option<serde_json::Value>,
    },
    /// Connection error (non-timeout).
    Connection {
        message: String,
        details: Option<ConnectionErrorDetails>,
    },
    /// Connection timeout.
    ConnectionTimeout { message: String },
    /// User aborted the request.
    Aborted { message: String },
    /// Client-side SDK error (serialization, missing auth, etc.)
    Sdk(String),
    /// Generic / unknown error.
    Other(String),
}

impl ApiErrorInfo {
    /// The error message string, regardless of variant.
    pub fn message(&self) -> &str {
        match self {
            Self::Http { message, .. } => message,
            Self::Connection { message, .. } => message,
            Self::ConnectionTimeout { message } => message,
            Self::Aborted { message } => message,
            Self::Sdk(msg) => msg,
            Self::Other(msg) => msg,
        }
    }

    /// HTTP status code, if this is an HTTP error.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Response headers, if this is an HTTP error.
    pub fn headers(&self) -> Option<&std::collections::HashMap<String, String>> {
        match self {
            Self::Http { headers, .. } => Some(headers),
            _ => None,
        }
    }

    /// Get a header value (case-insensitive key lookup on the stored map).
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers().and_then(|h| {
            let key_lower = key.to_lowercase();
            h.iter()
                .find(|(k, _)| k.to_lowercase() == key_lower)
                .map(|(_, v)| v.as_str())
        })
    }
}

// ---------------------------------------------------------------------------
// classifyAPIError
// ---------------------------------------------------------------------------

/// Classifies an API error into a specific error type for analytics tracking.
/// Returns a standardized `ApiErrorClass` suitable for Datadog tagging.
///
/// Maps to: CC services/api/errors.ts:965-1161
pub fn classify_api_error(error: &ApiErrorInfo) -> ApiErrorClass {
    let msg = error.message();
    let msg_lower = msg.to_lowercase();

    // Aborted requests
    if msg == "Request was aborted." {
        return ApiErrorClass::Aborted;
    }

    // Timeout errors
    match error {
        ApiErrorInfo::ConnectionTimeout { .. } => return ApiErrorClass::ApiTimeout,
        ApiErrorInfo::Connection { message, .. } if message.to_lowercase().contains("timeout") => {
            return ApiErrorClass::ApiTimeout;
        }
        _ => {}
    }

    // Repeated 529
    if msg.contains(REPEATED_529_ERROR_MESSAGE) {
        return ApiErrorClass::Repeated529;
    }

    // Emergency capacity off switch
    if msg.contains(CUSTOM_OFF_SWITCH_MESSAGE) {
        return ApiErrorClass::CapacityOffSwitch;
    }

    // HTTP-status-specific checks
    if let Some(status) = error.status() {
        // Rate limiting (429)
        if status == 429 {
            return ApiErrorClass::RateLimit;
        }

        // Server overload (529 or overloaded_error in body)
        if status == 529 || msg.contains("\"type\":\"overloaded_error\"") {
            return ApiErrorClass::ServerOverload;
        }

        // Image size (400 + "image exceeds" + "maximum")
        if status == 400 && msg.contains("image exceeds") && msg.contains("maximum") {
            return ApiErrorClass::ImageTooLarge;
        }

        // Many-image dimension (400 + "image dimensions exceed" + "many-image")
        if status == 400 && msg.contains("image dimensions exceed") && msg.contains("many-image") {
            return ApiErrorClass::ImageTooLarge;
        }

        // Tool use mismatch (400)
        if status == 400
            && msg.contains(
                "`tool_use` ids were found without `tool_result` blocks immediately after",
            )
        {
            return ApiErrorClass::ToolUseMismatch;
        }

        // Unexpected tool_result (400)
        if status == 400 && msg.contains("unexpected `tool_use_id` found in `tool_result`") {
            return ApiErrorClass::UnexpectedToolResult;
        }

        // Duplicate tool_use ID (400)
        if status == 400 && msg.contains("`tool_use` ids must be unique") {
            return ApiErrorClass::DuplicateToolUseId;
        }

        // Invalid model (400)
        if status == 400 && msg_lower.contains("invalid model name") {
            return ApiErrorClass::InvalidModel;
        }

        // OAuth token revoked (403)
        if status == 403 && msg.contains("OAuth token has been revoked") {
            return ApiErrorClass::TokenRevoked;
        }

        // OAuth org not allowed (401/403)
        if (status == 401 || status == 403)
            && msg.contains("OAuth authentication is currently not allowed for this organization")
        {
            return ApiErrorClass::OauthOrgNotAllowed;
        }

        // Generic auth (401/403)
        if status == 401 || status == 403 {
            return ApiErrorClass::AuthError;
        }

        // Status-code fallbacks
        if status >= 500 {
            return ApiErrorClass::ServerError;
        }
        if status >= 400 {
            return ApiErrorClass::ClientError;
        }
    }

    // Prompt too long (may come from Error, not APIError)
    if msg_lower.contains(&PROMPT_TOO_LONG_ERROR_MESSAGE.to_lowercase()) {
        return ApiErrorClass::PromptTooLong;
    }

    // PDF page limit
    if Regex::new(r"maximum of \d+ PDF pages")
        .unwrap()
        .is_match(msg)
    {
        return ApiErrorClass::PdfTooLarge;
    }

    // PDF password protected
    if msg.contains("The PDF specified is password protected") {
        return ApiErrorClass::PdfPasswordProtected;
    }

    // Credit/billing
    if msg_lower.contains(&CREDIT_BALANCE_TOO_LOW_ERROR_MESSAGE.to_lowercase()) {
        return ApiErrorClass::CreditBalanceLow;
    }

    // Authentication (x-api-key mention)
    if msg_lower.contains("x-api-key") {
        return ApiErrorClass::InvalidApiKey;
    }

    // Bedrock model access
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_BEDROCK").as_deref(),
    ) && msg_lower.contains("model id")
    {
        return ApiErrorClass::BedrockModelAccess;
    }

    // Connection errors
    if let ApiErrorInfo::Connection { details, .. } = error {
        if details.as_ref().map_or(false, |d| d.is_ssl_error) {
            return ApiErrorClass::SslCertError;
        }
        return ApiErrorClass::ConnectionError;
    }

    ApiErrorClass::Unknown
}

// ---------------------------------------------------------------------------
// categorizeRetryableAPIError
// ---------------------------------------------------------------------------

/// Maps HTTP status to retry category for SDK consumers.
///
/// Maps to: CC services/api/errors.ts:1163-1182
pub fn categorize_retryable_api_error(
    status: Option<u16>,
    message: &str,
) -> SdkAssistantMessageError {
    match status {
        Some(529) => SdkAssistantMessageError::RateLimit,
        Some(s) if message.contains("\"type\":\"overloaded_error\"") && s == 529 => {
            SdkAssistantMessageError::RateLimit
        }
        Some(429) => SdkAssistantMessageError::RateLimit,
        Some(401) | Some(403) => SdkAssistantMessageError::AuthenticationFailed,
        Some(s) if s >= 408 => SdkAssistantMessageError::ServerError,
        _ => SdkAssistantMessageError::Unknown,
    }
}

// ---------------------------------------------------------------------------
// getAssistantMessageFromError
// ---------------------------------------------------------------------------

/// Core error-to-message converter. Takes a provider-agnostic `ApiErrorInfo`
/// and produces a user-facing `ApiAssistantMessage` with appropriate content
/// and error classification.
///
/// The original CC function is a single 500+ line if/else chain. This Rust
/// port preserves the same match order and semantics, decomposed into
/// sequential checks with early returns.
///
/// Analytics calls (`logEvent`, `logToolUseToolResultMismatch`) are omitted.
/// Auth helper calls (`getAnthropicApiKeyWithSource`, `isClaudeAISubscriber`,
/// etc.) are stubbed with TODO markers.
///
/// Maps to: CC services/api/errors.ts:425-934
pub fn get_assistant_message_from_error(error: &ApiErrorInfo, model: &str) -> ApiAssistantMessage {
    let msg = error.message();
    let msg_lower = msg.to_lowercase();

    // --- Timeout errors (SDK timeout or connection error containing "timeout") ---
    match error {
        ApiErrorInfo::ConnectionTimeout { .. } => {
            return create_assistant_api_error_message(
                API_TIMEOUT_ERROR_MESSAGE,
                Some(SdkAssistantMessageError::Unknown),
                None,
            );
        }
        ApiErrorInfo::Connection { message, .. } if message.to_lowercase().contains("timeout") => {
            return create_assistant_api_error_message(
                API_TIMEOUT_ERROR_MESSAGE,
                Some(SdkAssistantMessageError::Unknown),
                None,
            );
        }
        _ => {}
    }

    // --- Image size/resize errors (thrown before API call during validation) ---
    // In CC these are `instanceof ImageSizeError || instanceof ImageResizeError`.
    // Since we don't have those error types yet, callers should convert them to
    // ApiErrorInfo::Other with an identifying message before calling this fn.
    // TODO(port): Add ImageSizeError / ImageResizeError variants to ApiErrorInfo
    // once utils/imageResizer.rs and utils/imageValidation.rs are ported.

    // --- Emergency capacity off switch for Opus PAYG ---
    if msg.contains(CUSTOM_OFF_SWITCH_MESSAGE) {
        return create_assistant_api_error_message(
            CUSTOM_OFF_SWITCH_MESSAGE,
            Some(SdkAssistantMessageError::RateLimit),
            None,
        );
    }

    // --- 429 Rate limit ---
    if let Some(status) = error.status() {
        if status == 429 {
            // Maps to CC `errors.ts:463-537`: unified quota headers use the
            // centralized limits message formatter (including active internal
            // mocks) before the generic 429/entitlement path.
            let config = crate::utils::config::load_global_config();
            let is_subscriber = crate::utils::auth::is_claude_ai_subscriber();
            if crate::services::rate_limit_mocking::should_process_rate_limits(is_subscriber) {
                let has_unified_headers = error
                    .header("anthropic-ratelimit-unified-representative-claim")
                    .is_some()
                    || error
                        .header("anthropic-ratelimit-unified-overage-status")
                        .is_some();
                if has_unified_headers {
                    let limits = crate::services::claude_ai_limits::limits_from_error_headers(
                        error.headers().expect("HTTP 429 carries a header map"),
                    );
                    let context =
                        crate::services::rate_limit_messages::RateLimitUiContext::from_global_config(
                            &config,
                        );
                    let content =
                        crate::services::rate_limit_messages::get_rate_limit_error_message(
                            &limits, &context,
                        )
                        .unwrap_or_else(|| {
                            crate::utils::messages::NO_RESPONSE_REQUESTED.to_string()
                        });
                    return create_assistant_api_error_message(
                        &content,
                        Some(SdkAssistantMessageError::RateLimit),
                        None,
                    );
                }
            }

            // Check for "Extra usage is required for long context"
            if msg.contains("Extra usage is required for long context") {
                let hint = if get_is_non_interactive_session() {
                    "enable extra usage at claude.ai/settings/usage, or use --model to switch to standard context"
                } else {
                    "run /extra-usage to enable, or /model to switch to standard context"
                };
                return create_assistant_api_error_message(
                    &format!(
                        "{API_ERROR_MESSAGE_PREFIX}: Extra usage is required for 1M context \u{00b7} {hint}"
                    ),
                    Some(SdkAssistantMessageError::RateLimit),
                    None,
                );
            }

            // Strip SDK prefix "429 " and try to extract inner message
            let stripped = msg.strip_prefix("429 ").unwrap_or(msg);
            let inner_re = Regex::new(r#""message"\s*:\s*"([^"]*)""#).unwrap();
            let inner_message = inner_re
                .captures(stripped)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
            let detail = inner_message.as_deref().unwrap_or(stripped);
            let detail = if detail.is_empty() {
                "this may be a temporary capacity issue \u{2014} check status.anthropic.com"
            } else {
                detail
            };
            return create_assistant_api_error_message(
                &format!("{API_ERROR_MESSAGE_PREFIX}: Request rejected (429) \u{00b7} {detail}"),
                Some(SdkAssistantMessageError::RateLimit),
                None,
            );
        }
    }

    // --- Prompt too long (Vertex 413, direct API 400) ---
    if msg_lower.contains("prompt is too long") {
        return create_assistant_api_error_message(
            PROMPT_TOO_LONG_ERROR_MESSAGE,
            Some(SdkAssistantMessageError::InvalidRequest),
            Some(msg.to_string()),
        );
    }

    // --- PDF page limit ---
    if Regex::new(r"maximum of \d+ PDF pages")
        .unwrap()
        .is_match(msg)
    {
        return create_assistant_api_error_message(
            &get_pdf_too_large_error_message(),
            Some(SdkAssistantMessageError::InvalidRequest),
            Some(msg.to_string()),
        );
    }

    // --- Password-protected PDF ---
    if msg.contains("The PDF specified is password protected") {
        return create_assistant_api_error_message(
            get_pdf_password_protected_error_message(),
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- Invalid PDF ---
    if msg.contains("The PDF specified was not valid") {
        return create_assistant_api_error_message(
            get_pdf_invalid_error_message(),
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- Image size errors (400 + "image exceeds" + "maximum") ---
    if error.status() == Some(400) && msg.contains("image exceeds") && msg.contains("maximum") {
        return create_assistant_api_error_message(
            get_image_too_large_error_message(),
            None,
            Some(msg.to_string()),
        );
    }

    // --- Many-image dimension errors ---
    if error.status() == Some(400)
        && msg.contains("image dimensions exceed")
        && msg.contains("many-image")
    {
        let content = if get_is_non_interactive_session() {
            "An image in the conversation exceeds the dimension limit for many-image requests (2000px). Start a new session with fewer images."
        } else {
            "An image in the conversation exceeds the dimension limit for many-image requests (2000px). Run /compact to remove old images from context, or start a new session."
        };
        return create_assistant_api_error_message(
            content,
            Some(SdkAssistantMessageError::InvalidRequest),
            Some(msg.to_string()),
        );
    }

    // --- AFK mode beta header rejection ---
    // TODO(port): Wire AFK_MODE_BETA_HEADER from constants/betas.rs once ported.
    // CC services/api/errors.ts:644-655 — skipped for now (AFK_MODE_BETA_HEADER
    // is '' in non-TRANSCRIPT_CLASSIFIER builds).

    // --- Request too large (413) ---
    if error.status() == Some(413) {
        return create_assistant_api_error_message(
            &get_request_too_large_error_message(),
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- tool_use/tool_result mismatch (400) ---
    if error.status() == Some(400)
        && msg.contains("`tool_use` ids were found without `tool_result` blocks immediately after")
    {
        // Analytics logging omitted (logToolUseToolResultMismatch)
        let base_message = "API Error: 400 due to tool use concurrency issues.";
        let rewind_instruction = if get_is_non_interactive_session() {
            ""
        } else {
            " Run /rewind to recover the conversation."
        };
        return create_assistant_api_error_message(
            &format!("{base_message}{rewind_instruction}"),
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- Unexpected tool_result (400) ---
    // Analytics event omitted (tengu_unexpected_tool_result)

    // --- Duplicate tool_use IDs (400, CC-1212) ---
    if error.status() == Some(400) && msg.contains("`tool_use` ids must be unique") {
        // Analytics event omitted (tengu_duplicate_tool_use_id)
        let rewind_instruction = if get_is_non_interactive_session() {
            ""
        } else {
            " Run /rewind to recover the conversation."
        };
        return create_assistant_api_error_message(
            &format!(
                "API Error: 400 duplicate tool_use ID in conversation history.{rewind_instruction}"
            ),
            Some(SdkAssistantMessageError::InvalidRequest),
            Some(msg.to_string()),
        );
    }

    // --- Invalid model name for subscription users trying Opus ---
    // TODO(port): Wire isClaudeAISubscriber() and isNonCustomOpusModel()
    // from utils/auth.rs and utils/model/model.rs once ported.
    // CC services/api/errors.ts:736-748 — stubbed.
    if error.status() == Some(400) && msg_lower.contains("invalid model name") {
        // Generic invalid model message (CC has subscriber-specific and ant-specific
        // branches that require auth state we don't have yet)
        return create_assistant_api_error_message(
            &format!(
                "{API_ERROR_MESSAGE_PREFIX}: Invalid model name ({model}). Run /model to pick a different model."
            ),
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- Credit balance too low ---
    if msg.contains("Your credit balance is too low") {
        return create_assistant_api_error_message(
            CREDIT_BALANCE_TOO_LOW_ERROR_MESSAGE,
            Some(SdkAssistantMessageError::BillingError),
            None,
        );
    }

    // --- Organization disabled (400) ---
    if error.status() == Some(400) && msg_lower.contains("organization has been disabled") {
        // TODO(port): Wire getAnthropicApiKeyWithSource(), isClaudeAISubscriber(),
        // getClaudeAIOAuthTokens() from utils/auth.rs.
        // For now, emit the generic env-key message.
        return create_assistant_api_error_message(
            ORG_DISABLED_ERROR_MESSAGE_ENV_KEY,
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- x-api-key authentication errors ---
    if msg_lower.contains("x-api-key") {
        if is_ccr_mode() {
            return create_assistant_api_error_message(
                CCR_AUTH_ERROR_MESSAGE,
                Some(SdkAssistantMessageError::AuthenticationFailed),
                None,
            );
        }
        // TODO(port): Wire getAnthropicApiKeyWithSource() to distinguish
        // external vs internal key sources.
        return create_assistant_api_error_message(
            INVALID_API_KEY_ERROR_MESSAGE,
            Some(SdkAssistantMessageError::AuthenticationFailed),
            None,
        );
    }

    // --- OAuth token revoked (403) ---
    if error.status() == Some(403) && msg.contains("OAuth token has been revoked") {
        return create_assistant_api_error_message(
            get_token_revoked_error_message(),
            Some(SdkAssistantMessageError::AuthenticationFailed),
            None,
        );
    }

    // --- OAuth org not allowed (401/403) ---
    if matches!(error.status(), Some(401) | Some(403))
        && msg.contains("OAuth authentication is currently not allowed for this organization")
    {
        return create_assistant_api_error_message(
            get_oauth_org_not_allowed_error_message(),
            Some(SdkAssistantMessageError::AuthenticationFailed),
            None,
        );
    }

    // --- Generic 401/403 ---
    if matches!(error.status(), Some(401) | Some(403)) {
        if is_ccr_mode() {
            return create_assistant_api_error_message(
                CCR_AUTH_ERROR_MESSAGE,
                Some(SdkAssistantMessageError::AuthenticationFailed),
                None,
            );
        }
        let content = if get_is_non_interactive_session() {
            format!("Failed to authenticate. {API_ERROR_MESSAGE_PREFIX}: {msg}")
        } else {
            format!("Please run /login \u{00b7} {API_ERROR_MESSAGE_PREFIX}: {msg}")
        };
        return create_assistant_api_error_message(
            &content,
            Some(SdkAssistantMessageError::AuthenticationFailed),
            None,
        );
    }

    // --- Bedrock model ID errors ---
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_BEDROCK").as_deref(),
    ) && msg_lower.contains("model id")
    {
        let switch_cmd = if get_is_non_interactive_session() {
            "--model"
        } else {
            "/model"
        };
        // TODO(port): Wire get3PModelFallbackSuggestion from CC errors.ts:940-959
        return create_assistant_api_error_message(
            &format!(
                "{API_ERROR_MESSAGE_PREFIX} ({model}): {msg}. Run {switch_cmd} to pick a different model."
            ),
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- 404 Not Found (model doesn't exist or isn't available) ---
    if error.status() == Some(404) {
        let switch_cmd = if get_is_non_interactive_session() {
            "--model"
        } else {
            "/model"
        };
        // TODO(port): Wire get3PModelFallbackSuggestion + getAPIProvider
        return create_assistant_api_error_message(
            &format!(
                "There's an issue with the selected model ({model}). It may not exist or you may not have access to it. Run {switch_cmd} to pick a different model."
            ),
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
    }

    // --- Connection errors (non-timeout) ---
    if let ApiErrorInfo::Connection { message, .. } = error {
        // TODO(port): Wire formatAPIError from services/api/errorUtils.rs
        return create_assistant_api_error_message(
            &format!("{API_ERROR_MESSAGE_PREFIX}: {message}"),
            Some(SdkAssistantMessageError::Unknown),
            None,
        );
    }

    // --- Generic error fallback ---
    let content = if msg.is_empty() {
        API_ERROR_MESSAGE_PREFIX.to_string()
    } else {
        format!("{API_ERROR_MESSAGE_PREFIX}: {msg}")
    };
    create_assistant_api_error_message(&content, Some(SdkAssistantMessageError::Unknown), None)
}

// ---------------------------------------------------------------------------
// getErrorMessageIfRefusal
// ---------------------------------------------------------------------------

/// Checks if the stop reason is `refusal` and returns an appropriate error
/// message. Analytics event (`tengu_refusal_api_response`) is omitted.
///
/// Maps to: CC services/api/errors.ts:1184-1207
pub fn get_error_message_if_refusal(
    stop_reason: Option<ApiStopReason>,
    model: &str,
) -> Option<ApiAssistantMessage> {
    if stop_reason != Some(ApiStopReason::Refusal) {
        return None;
    }

    let base_message = if get_is_non_interactive_session() {
        format!(
            "{API_ERROR_MESSAGE_PREFIX}: Claude Code is unable to respond to this request, which appears to violate our Usage Policy (https://www.anthropic.com/legal/aup). Try rephrasing the request or attempting a different approach."
        )
    } else {
        format!(
            "{API_ERROR_MESSAGE_PREFIX}: Claude Code is unable to respond to this request, which appears to violate our Usage Policy (https://www.anthropic.com/legal/aup). Please double press esc to edit your last message or start a new session for Claude Code to assist with a different task."
        )
    };

    let model_suggestion = if model != "claude-sonnet-4-20250514" {
        " If you are seeing this refusal repeatedly, try running /model claude-sonnet-4-20250514 to switch models."
    } else {
        ""
    };

    Some(create_assistant_api_error_message(
        &format!("{base_message}{model_suggestion}"),
        Some(SdkAssistantMessageError::InvalidRequest),
        None,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "anthropic_internal")]
    use crate::utils::test_env::TEST_ENV_LOCK;
    use std::collections::HashMap;

    #[test]
    fn starts_with_api_error_prefix_matches_both_forms() {
        assert!(starts_with_api_error_prefix("API Error: something"));
        assert!(starts_with_api_error_prefix(
            "Please run /login \u{00b7} API Error: auth failed"
        ));
        assert!(!starts_with_api_error_prefix("Some other error"));
    }

    #[test]
    fn parse_prompt_too_long_token_counts_extracts_numbers() {
        let (actual, limit) = parse_prompt_too_long_token_counts(
            "prompt is too long: 137500 tokens > 135000 maximum",
        );
        assert_eq!(actual, Some(137500));
        assert_eq!(limit, Some(135000));
    }

    #[test]
    fn parse_prompt_too_long_token_counts_case_insensitive() {
        let (actual, limit) =
            parse_prompt_too_long_token_counts("Prompt Is Too Long: 200000 token > 180000");
        assert_eq!(actual, Some(200000));
        assert_eq!(limit, Some(180000));
    }

    #[test]
    fn parse_prompt_too_long_token_counts_returns_none_on_mismatch() {
        let (actual, limit) = parse_prompt_too_long_token_counts("some unrelated error message");
        assert_eq!(actual, None);
        assert_eq!(limit, None);
    }

    #[test]
    fn is_prompt_too_long_message_checks_content() {
        let msg = create_assistant_api_error_message(
            PROMPT_TOO_LONG_ERROR_MESSAGE,
            Some(SdkAssistantMessageError::InvalidRequest),
            None,
        );
        assert!(is_prompt_too_long_message(&msg));
    }

    #[test]
    fn is_prompt_too_long_message_false_for_non_error() {
        let msg = ApiAssistantMessage {
            is_api_error_message: false,
            content: vec![ApiAssistantContentBlock {
                block_type: "text".to_string(),
                text: PROMPT_TOO_LONG_ERROR_MESSAGE.to_string(),
            }],
            error_details: None,
            error: None,
        };
        assert!(!is_prompt_too_long_message(&msg));
    }

    #[test]
    fn get_prompt_too_long_token_gap_computes_difference() {
        let msg = ApiAssistantMessage {
            is_api_error_message: true,
            content: vec![ApiAssistantContentBlock {
                block_type: "text".to_string(),
                text: PROMPT_TOO_LONG_ERROR_MESSAGE.to_string(),
            }],
            error_details: Some("prompt is too long: 137500 tokens > 135000 maximum".to_string()),
            error: Some(SdkAssistantMessageError::InvalidRequest),
        };
        assert_eq!(get_prompt_too_long_token_gap(&msg), Some(2500));
    }

    #[test]
    fn get_prompt_too_long_token_gap_none_when_under_limit() {
        let msg = ApiAssistantMessage {
            is_api_error_message: true,
            content: vec![ApiAssistantContentBlock {
                block_type: "text".to_string(),
                text: PROMPT_TOO_LONG_ERROR_MESSAGE.to_string(),
            }],
            error_details: Some("prompt is too long: 100000 tokens > 135000 maximum".to_string()),
            error: Some(SdkAssistantMessageError::InvalidRequest),
        };
        assert_eq!(get_prompt_too_long_token_gap(&msg), None);
    }

    #[test]
    fn is_media_size_error_detects_image_exceeds() {
        assert!(is_media_size_error(
            "image exceeds 5 MB maximum: 5316852 bytes"
        ));
    }

    #[test]
    fn is_media_size_error_detects_many_image() {
        assert!(is_media_size_error(
            "image dimensions exceed the limit for many-image requests"
        ));
    }

    #[test]
    fn is_media_size_error_detects_pdf_pages() {
        assert!(is_media_size_error("maximum of 100 PDF pages allowed"));
    }

    #[test]
    fn is_media_size_error_false_for_unrelated() {
        assert!(!is_media_size_error("some random error"));
    }

    #[test]
    fn is_valid_api_message_checks_shape() {
        let valid = serde_json::json!({
            "content": [{"type": "text", "text": "hello"}],
            "model": "claude-sonnet-4-20250514",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        assert!(is_valid_api_message(&valid));

        let missing_content = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "usage": {}
        });
        assert!(!is_valid_api_message(&missing_content));
    }

    #[test]
    fn extract_unknown_error_format_finds_bedrock_type() {
        let val = serde_json::json!({
            "Output": {"__type": "UnauthorizedException"},
            "Version": "1.0"
        });
        assert_eq!(
            extract_unknown_error_format(&val),
            Some("UnauthorizedException".to_string())
        );
    }

    #[test]
    fn extract_unknown_error_format_returns_none_for_normal() {
        let val = serde_json::json!({"content": []});
        assert_eq!(extract_unknown_error_format(&val), None);
    }

    #[test]
    fn classify_api_error_timeout() {
        let err = ApiErrorInfo::ConnectionTimeout {
            message: "timed out".to_string(),
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::ApiTimeout);
    }

    #[test]
    fn classify_api_error_connection_timeout_in_message() {
        let err = ApiErrorInfo::Connection {
            message: "Connection timeout after 30s".to_string(),
            details: None,
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::ApiTimeout);
    }

    #[test]
    fn classify_api_error_rate_limit() {
        let err = ApiErrorInfo::Http {
            status: 429,
            message: "rate limited".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::RateLimit);
    }

    #[test]
    fn classify_api_error_server_overload_529() {
        let err = ApiErrorInfo::Http {
            status: 529,
            message: "overloaded".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::ServerOverload);
    }

    #[test]
    fn classify_api_error_prompt_too_long() {
        let err = ApiErrorInfo::Other("prompt is too long: 100k tokens".to_string());
        assert_eq!(classify_api_error(&err), ApiErrorClass::PromptTooLong);
    }

    #[test]
    fn classify_api_error_invalid_model() {
        let err = ApiErrorInfo::Http {
            status: 400,
            message: "Invalid model name: claude-opus-99".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::InvalidModel);
    }

    #[test]
    fn classify_api_error_auth_401() {
        let err = ApiErrorInfo::Http {
            status: 401,
            message: "unauthorized".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::AuthError);
    }

    #[test]
    fn classify_api_error_server_error_500() {
        let err = ApiErrorInfo::Http {
            status: 500,
            message: "internal server error".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::ServerError);
    }

    #[test]
    fn classify_api_error_connection() {
        let err = ApiErrorInfo::Connection {
            message: "connection refused".to_string(),
            details: None,
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::ConnectionError);
    }

    #[test]
    fn classify_api_error_ssl() {
        let err = ApiErrorInfo::Connection {
            message: "ssl error".to_string(),
            details: Some(ConnectionErrorDetails {
                code: "ERR_SSL_WRONG_VERSION_NUMBER".to_string(),
                message: "ssl handshake failed".to_string(),
                is_ssl_error: true,
            }),
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::SslCertError);
    }

    #[test]
    fn classify_api_error_aborted() {
        let err = ApiErrorInfo::Aborted {
            message: "Request was aborted.".to_string(),
        };
        assert_eq!(classify_api_error(&err), ApiErrorClass::Aborted);
    }

    #[test]
    fn categorize_retryable_rate_limit() {
        assert_eq!(
            categorize_retryable_api_error(Some(429), "rate limited"),
            SdkAssistantMessageError::RateLimit
        );
        assert_eq!(
            categorize_retryable_api_error(Some(529), "overloaded"),
            SdkAssistantMessageError::RateLimit
        );
    }

    #[test]
    fn categorize_retryable_auth() {
        assert_eq!(
            categorize_retryable_api_error(Some(401), "unauthorized"),
            SdkAssistantMessageError::AuthenticationFailed
        );
        assert_eq!(
            categorize_retryable_api_error(Some(403), "forbidden"),
            SdkAssistantMessageError::AuthenticationFailed
        );
    }

    #[test]
    fn categorize_retryable_server_error() {
        assert_eq!(
            categorize_retryable_api_error(Some(500), "internal"),
            SdkAssistantMessageError::ServerError
        );
    }

    #[test]
    fn categorize_retryable_unknown() {
        assert_eq!(
            categorize_retryable_api_error(Some(400), "bad request"),
            SdkAssistantMessageError::Unknown
        );
    }

    #[test]
    fn get_assistant_message_from_error_timeout() {
        let err = ApiErrorInfo::ConnectionTimeout {
            message: "timed out".to_string(),
        };
        let msg = get_assistant_message_from_error(&err, "claude-sonnet-4-20250514");
        assert!(msg.is_api_error_message);
        assert_eq!(msg.content[0].text, API_TIMEOUT_ERROR_MESSAGE);
    }

    #[test]
    fn get_assistant_message_from_error_prompt_too_long() {
        let err =
            ApiErrorInfo::Other("prompt is too long: 137500 tokens > 135000 maximum".to_string());
        let msg = get_assistant_message_from_error(&err, "claude-sonnet-4-20250514");
        assert!(msg.is_api_error_message);
        assert_eq!(msg.content[0].text, PROMPT_TOO_LONG_ERROR_MESSAGE);
        assert!(msg.error_details.is_some());
    }

    #[test]
    fn get_assistant_message_from_error_429() {
        let err = ApiErrorInfo::Http {
            status: 429,
            message: "rate limited".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        let msg = get_assistant_message_from_error(&err, "claude-sonnet-4-20250514");
        assert!(msg.is_api_error_message);
        assert!(msg.content[0].text.contains("429"));
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn unified_mock_429_uses_centralized_rate_limit_message() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::services::mock_rate_limits::reset_for_test();
        crate::services::mock_rate_limits::set_mock_rate_limit_scenario(
            crate::services::mock_rate_limits::MockScenario::WeeklyLimitReached,
        );
        let headers = crate::services::mock_rate_limits::get_mock_headers().unwrap();
        let err = ApiErrorInfo::Http {
            status: 429,
            message: "Rate limit exceeded".to_string(),
            headers,
            body: None,
        };
        let msg = get_assistant_message_from_error(&err, "claude-sonnet-4-6");
        assert!(
            msg.content[0]
                .text
                .starts_with("You've hit your weekly limit · resets ")
        );
        assert_eq!(msg.error, Some(SdkAssistantMessageError::RateLimit));
        crate::services::mock_rate_limits::reset_for_test();
    }

    #[test]
    fn get_assistant_message_from_error_auth_401() {
        let err = ApiErrorInfo::Http {
            status: 401,
            message: "unauthorized".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        let msg = get_assistant_message_from_error(&err, "claude-sonnet-4-20250514");
        assert!(msg.is_api_error_message);
        assert_eq!(
            msg.error,
            Some(SdkAssistantMessageError::AuthenticationFailed)
        );
    }

    #[test]
    fn get_assistant_message_from_error_credit_balance() {
        let err = ApiErrorInfo::Other("Your credit balance is too low".to_string());
        let msg = get_assistant_message_from_error(&err, "claude-sonnet-4-20250514");
        assert_eq!(msg.content[0].text, CREDIT_BALANCE_TOO_LOW_ERROR_MESSAGE);
        assert_eq!(msg.error, Some(SdkAssistantMessageError::BillingError));
    }

    #[test]
    fn get_assistant_message_from_error_generic_fallback() {
        let err = ApiErrorInfo::Other("something weird happened".to_string());
        let msg = get_assistant_message_from_error(&err, "claude-sonnet-4-20250514");
        assert!(msg.content[0].text.starts_with(API_ERROR_MESSAGE_PREFIX));
        assert_eq!(msg.error, Some(SdkAssistantMessageError::Unknown));
    }

    #[test]
    fn get_error_message_if_refusal_returns_none_for_non_refusal() {
        assert!(get_error_message_if_refusal(Some(ApiStopReason::EndTurn), "m").is_none());
        assert!(get_error_message_if_refusal(None, "m").is_none());
    }

    #[test]
    fn get_error_message_if_refusal_returns_message_for_refusal() {
        let msg =
            get_error_message_if_refusal(Some(ApiStopReason::Refusal), "claude-opus-4-20250514");
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert!(msg.content[0].text.contains("Usage Policy"));
        // Non-default model should get model suggestion
        assert!(
            msg.content[0]
                .text
                .contains("try running /model claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn get_error_message_if_refusal_no_model_suggestion_for_sonnet() {
        let msg =
            get_error_message_if_refusal(Some(ApiStopReason::Refusal), "claude-sonnet-4-20250514");
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert!(!msg.content[0].text.contains("try running /model"));
    }

    #[test]
    fn format_file_size_formats_correctly() {
        assert_eq!(format_file_size(512), "512 bytes");
        assert_eq!(format_file_size(1536), "1.5KB");
        assert_eq!(format_file_size(1024), "1KB");
        assert_eq!(format_file_size(20 * 1024 * 1024), "20MB");
    }

    #[test]
    fn sdk_assistant_message_error_round_trips() {
        let variants = [
            (
                SdkAssistantMessageError::AuthenticationFailed,
                "authentication_failed",
            ),
            (SdkAssistantMessageError::BillingError, "billing_error"),
            (SdkAssistantMessageError::RateLimit, "rate_limit"),
            (SdkAssistantMessageError::InvalidRequest, "invalid_request"),
            (SdkAssistantMessageError::ServerError, "server_error"),
            (SdkAssistantMessageError::Unknown, "unknown"),
            (
                SdkAssistantMessageError::MaxOutputTokens,
                "max_output_tokens",
            ),
        ];
        for (variant, expected) in &variants {
            assert_eq!(variant.as_str(), *expected);
        }
    }

    #[test]
    fn api_error_class_as_str_matches_cc() {
        assert_eq!(ApiErrorClass::Aborted.as_str(), "aborted");
        assert_eq!(ApiErrorClass::ApiTimeout.as_str(), "api_timeout");
        assert_eq!(ApiErrorClass::RateLimit.as_str(), "rate_limit");
        assert_eq!(ApiErrorClass::Unknown.as_str(), "unknown");
    }

    #[test]
    fn is_media_size_error_message_checks_error_details() {
        let msg = ApiAssistantMessage {
            is_api_error_message: true,
            content: vec![],
            error_details: Some("image exceeds 5 MB maximum: 5316852 bytes".to_string()),
            error: None,
        };
        assert!(is_media_size_error_message(&msg));

        let msg_no_details = ApiAssistantMessage {
            is_api_error_message: true,
            content: vec![],
            error_details: None,
            error: None,
        };
        assert!(!is_media_size_error_message(&msg_no_details));
    }
}
