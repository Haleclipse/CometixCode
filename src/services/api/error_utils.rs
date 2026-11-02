//! Error formatting and sanitization utilities for API errors.
//!
//! Maps to: CC services/api/errorUtils.ts (full file)
//!
//! Provides connection error extraction from cause chains, SSL error
//! classification, HTML sanitization (for CloudFlare error pages), and
//! user-facing error message formatting.
//!
//! The original CC module operates on the Anthropic SDK's `APIError` type.
//! In nyan, the equivalent intermediate representation is
//! `super::errors::ApiErrorInfo`, which is constructed by callers before
//! passing into these functions.

use super::errors::{ApiErrorInfo, ConnectionErrorDetails};

// ---------------------------------------------------------------------------
// SSL error codes
// ---------------------------------------------------------------------------

/// SSL/TLS error codes from OpenSSL (used by both Node.js/Bun and native TLS).
///
/// Maps to: CC services/api/errorUtils.ts:5-29
///
/// See: <https://www.openssl.org/docs/man3.1/man3/X509_STORE_CTX_get_error.html>
const SSL_ERROR_CODES: &[&str] = &[
    // Certificate verification errors
    "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
    "UNABLE_TO_GET_ISSUER_CERT",
    "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
    "CERT_SIGNATURE_FAILURE",
    "CERT_NOT_YET_VALID",
    "CERT_HAS_EXPIRED",
    "CERT_REVOKED",
    "CERT_REJECTED",
    "CERT_UNTRUSTED",
    // Self-signed certificate errors
    "DEPTH_ZERO_SELF_SIGNED_CERT",
    "SELF_SIGNED_CERT_IN_CHAIN",
    // Chain errors
    "CERT_CHAIN_TOO_LONG",
    "PATH_LENGTH_EXCEEDED",
    // Hostname/altname errors
    "ERR_TLS_CERT_ALTNAME_INVALID",
    "HOSTNAME_MISMATCH",
    // TLS handshake errors
    "ERR_TLS_HANDSHAKE_TIMEOUT",
    "ERR_SSL_WRONG_VERSION_NUMBER",
    "ERR_SSL_DECRYPTION_FAILED_OR_BAD_RECORD_MAC",
];

/// Returns true if the given code string matches a known SSL/TLS error code.
///
/// Maps to: CC services/api/errorUtils.ts:5-29 (SSL_ERROR_CODES.has(code))
fn is_ssl_error_code(code: &str) -> bool {
    SSL_ERROR_CODES.contains(&code)
}

// ---------------------------------------------------------------------------
// extractConnectionErrorDetails
// ---------------------------------------------------------------------------

/// Extracts connection error details from an `ApiErrorInfo`.
///
/// Maps to: CC services/api/errorUtils.ts:42-83
///
/// The original JS function walks the `cause` chain on an Error object up to
/// `maxDepth=5` levels, looking for an error with a `.code` property that
/// matches a known SSL or connection code.
///
/// In nyan, `ApiErrorInfo::Connection` already carries an optional
/// `ConnectionErrorDetails` from the SDK translation layer. For connection
/// errors without pre-extracted details, this function probes the message
/// string for known error codes (equivalent to the JS `.code` check).
///
/// For `ConnectionTimeout` variants, this synthesizes an `ETIMEDOUT` code.
pub fn extract_connection_error_details(error: &ApiErrorInfo) -> Option<ConnectionErrorDetails> {
    match error {
        ApiErrorInfo::Connection {
            details: Some(d), ..
        } => {
            // Details already extracted by the SDK translation layer.
            Some(d.clone())
        }
        ApiErrorInfo::Connection { message, .. } => {
            // No pre-extracted details; probe the message for known codes.
            extract_code_from_message(message).map(|code| {
                let is_ssl = is_ssl_error_code(&code);
                ConnectionErrorDetails {
                    code,
                    message: message.clone(),
                    is_ssl_error: is_ssl,
                }
            })
        }
        ApiErrorInfo::ConnectionTimeout { message } => Some(ConnectionErrorDetails {
            code: "ETIMEDOUT".to_string(),
            message: message.clone(),
            is_ssl_error: false,
        }),
        _ => None,
    }
}

/// Try to extract an error code from a message string.
///
/// Looks for patterns like "ETIMEDOUT", "ECONNREFUSED", or any of the
/// SSL_ERROR_CODES embedded in the message text.
///
/// Maps to: CC services/api/errorUtils.ts:54-67 (the inner `current.code`
/// check, adapted for string scanning since Rust errors don't carry a
/// separate `.code` property).
fn extract_code_from_message(message: &str) -> Option<String> {
    // Common POSIX/libuv connection error codes
    const CONN_CODES: &[&str] = &[
        "ETIMEDOUT",
        "ECONNREFUSED",
        "ECONNRESET",
        "ENOTFOUND",
        "EPIPE",
        "EHOSTUNREACH",
        "ENETUNREACH",
        "ECONNABORTED",
    ];

    for &code in CONN_CODES {
        if message.contains(code) {
            return Some(code.to_string());
        }
    }

    for &code in SSL_ERROR_CODES {
        if message.contains(code) {
            return Some(code.to_string());
        }
    }

    None
}

// ---------------------------------------------------------------------------
// getSSLErrorHint
// ---------------------------------------------------------------------------

/// Returns an actionable hint for SSL/TLS errors.
///
/// Maps to: CC services/api/errorUtils.ts:94-100
///
/// Intended for contexts outside the main API client (OAuth token exchange,
/// preflight connectivity checks) where `format_api_error` does not apply.
///
/// Motivation: enterprise users behind TLS-intercepting proxies (Zscaler et al.)
/// see OAuth complete in-browser but the CLI's token exchange silently fails
/// with a raw SSL code. Surfacing the likely fix saves a support round-trip.
pub fn get_ssl_error_hint(error: &ApiErrorInfo) -> Option<String> {
    let details = extract_connection_error_details(error)?;
    if !details.is_ssl_error {
        return None;
    }
    Some(format!(
        "SSL certificate error ({}). If you are behind a corporate proxy or \
         TLS-intercepting firewall, set NODE_EXTRA_CA_CERTS to your CA bundle path, \
         or ask IT to allowlist *.anthropic.com. Run /doctor for details.",
        details.code
    ))
}

// ---------------------------------------------------------------------------
// sanitizeMessageHTML (private)
// ---------------------------------------------------------------------------

/// Strips HTML content (e.g., CloudFlare error pages) from a message string,
/// returning a user-friendly title or empty string if HTML is detected.
/// Returns the original message unchanged if no HTML is found.
///
/// Maps to: CC services/api/errorUtils.ts:107-116
fn sanitize_message_html(message: &str) -> String {
    if message.contains("<!DOCTYPE html") || message.contains("<html") {
        // Try to extract <title>...</title>
        if let Some(start) = message.find("<title>") {
            let after_tag = &message[start + 7..];
            if let Some(end) = after_tag.find("</title>") {
                let title = after_tag[..end].trim();
                if !title.is_empty() {
                    return title.to_string();
                }
            }
        }
        return String::new();
    }
    message.to_string()
}

// ---------------------------------------------------------------------------
// sanitizeAPIError
// ---------------------------------------------------------------------------

/// Detects if an error message contains HTML content (e.g., CloudFlare error
/// pages) and returns a user-friendly message instead.
///
/// Maps to: CC services/api/errorUtils.ts:122-130
pub fn sanitize_api_error(error: &ApiErrorInfo) -> String {
    let message = error.message();
    if message.is_empty() {
        // CC: "Sometimes message is undefined. TODO: figure out why"
        return String::new();
    }
    sanitize_message_html(message)
}

// ---------------------------------------------------------------------------
// Nested error extraction (JSONL deserialization support)
// ---------------------------------------------------------------------------

/// Extract a human-readable message from a deserialized API error that lacks
/// a top-level message.
///
/// Maps to: CC services/api/errorUtils.ts:169-198
///
/// After JSON round-tripping (e.g. session JSONL), the SDK's APIError loses
/// its `.message` property. The actual message lives at different nesting
/// levels depending on the provider:
///
/// - Bedrock/proxy: `{ error: { message: "..." } }`
/// - Standard Anthropic API: `{ error: { error: { message: "..." } } }`
///   (the outer `.error` is the response body, the inner `.error` is the
///   API error)
///
/// This function checks two nesting levels (deeper first for specificity):
/// 1. `body.error.error.message` -- standard Anthropic API shape
/// 2. `body.error.message` -- Bedrock shape
fn extract_nested_error_message(error: &ApiErrorInfo) -> Option<String> {
    // Only HTTP errors carry a body
    let body = match error {
        ApiErrorInfo::Http { body: Some(b), .. } => b,
        _ => return None,
    };

    // Try standard Anthropic API shape: { error: { error: { message } } }
    if let Some(outer_error) = body.get("error") {
        // Deep path: error.error.message
        if let Some(inner_error) = outer_error.get("error")
            && let Some(deep_msg) = inner_error.get("message").and_then(|v| v.as_str())
            && !deep_msg.is_empty()
        {
            let sanitized = sanitize_message_html(deep_msg);
            if !sanitized.is_empty() {
                return Some(sanitized);
            }
        }
        // Bedrock shape: error.message
        if let Some(msg) = outer_error.get("message").and_then(|v| v.as_str())
            && !msg.is_empty()
        {
            let sanitized = sanitize_message_html(msg);
            if !sanitized.is_empty() {
                return Some(sanitized);
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// formatAPIError
// ---------------------------------------------------------------------------

/// Main entry point for formatting an `ApiErrorInfo` into a user-facing string.
///
/// Maps to: CC services/api/errorUtils.ts:200-260
///
/// Combines connection error extraction, SSL-specific messages, HTML
/// sanitization, and nested JSONL-deserialized error extraction.
pub fn format_api_error(error: &ApiErrorInfo) -> String {
    // Extract connection error details from the cause chain
    let connection_details = extract_connection_error_details(error);

    if let Some(ref details) = connection_details {
        // Handle timeout errors
        if details.code == "ETIMEDOUT" {
            return "Request timed out. Check your internet connection and proxy settings"
                .to_string();
        }

        // Handle SSL/TLS errors with specific messages
        if details.is_ssl_error {
            return match details.code.as_str() {
                "UNABLE_TO_VERIFY_LEAF_SIGNATURE"
                | "UNABLE_TO_GET_ISSUER_CERT"
                | "UNABLE_TO_GET_ISSUER_CERT_LOCALLY" => {
                    "Unable to connect to API: SSL certificate verification failed. \
                     Check your proxy or corporate SSL certificates"
                        .to_string()
                }
                "CERT_HAS_EXPIRED" => {
                    "Unable to connect to API: SSL certificate has expired".to_string()
                }
                "CERT_REVOKED" => {
                    "Unable to connect to API: SSL certificate has been revoked".to_string()
                }
                "DEPTH_ZERO_SELF_SIGNED_CERT" | "SELF_SIGNED_CERT_IN_CHAIN" => {
                    "Unable to connect to API: Self-signed certificate detected. \
                     Check your proxy or corporate SSL certificates"
                        .to_string()
                }
                "ERR_TLS_CERT_ALTNAME_INVALID" | "HOSTNAME_MISMATCH" => {
                    "Unable to connect to API: SSL certificate hostname mismatch".to_string()
                }
                "CERT_NOT_YET_VALID" => {
                    "Unable to connect to API: SSL certificate is not yet valid".to_string()
                }
                code => format!("Unable to connect to API: SSL error ({code})"),
            };
        }
    }

    // Check for generic "Connection error." message
    let msg = error.message();
    if msg == "Connection error." {
        if let Some(ref details) = connection_details {
            return format!("Unable to connect to API ({})", details.code);
        }
        return "Unable to connect to API. Check your internet connection".to_string();
    }

    // Guard: when deserialized from JSONL (e.g. --resume), the error object may
    // lack a `.message`. Return a safe fallback instead of panicking.
    if msg.is_empty() {
        return extract_nested_error_message(error).unwrap_or_else(|| {
            let status_str = error
                .status()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            format!("API error (status {status_str})")
        });
    }

    let sanitized = sanitize_api_error(error);
    // Use sanitized message if it differs from the original (i.e. HTML was stripped)
    if sanitized != msg && !sanitized.is_empty() {
        sanitized
    } else {
        msg.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // -- SSL code identification --

    #[test]
    fn is_ssl_error_code_recognizes_known_codes() {
        assert!(is_ssl_error_code("CERT_HAS_EXPIRED"));
        assert!(is_ssl_error_code("DEPTH_ZERO_SELF_SIGNED_CERT"));
        assert!(is_ssl_error_code("ERR_TLS_CERT_ALTNAME_INVALID"));
    }

    #[test]
    fn is_ssl_error_code_rejects_unknown_codes() {
        assert!(!is_ssl_error_code("ETIMEDOUT"));
        assert!(!is_ssl_error_code("ECONNREFUSED"));
        assert!(!is_ssl_error_code("random_string"));
    }

    // -- sanitize_message_html --

    #[test]
    fn sanitize_html_passes_plain_text_through() {
        let msg = "Something went wrong";
        assert_eq!(sanitize_message_html(msg), "Something went wrong");
    }

    #[test]
    fn sanitize_html_extracts_title_from_html() {
        let html = "<!DOCTYPE html><html><head><title>Access Denied</title></head></html>";
        assert_eq!(sanitize_message_html(html), "Access Denied");
    }

    #[test]
    fn sanitize_html_returns_empty_for_html_without_title() {
        let html = "<!DOCTYPE html><html><body>error</body></html>";
        assert_eq!(sanitize_message_html(html), "");
    }

    #[test]
    fn sanitize_html_detects_html_tag_without_doctype() {
        let html = "<html><head><title>Forbidden</title></head></html>";
        assert_eq!(sanitize_message_html(html), "Forbidden");
    }

    // -- extract_code_from_message --

    #[test]
    fn extract_code_finds_etimedout() {
        assert_eq!(
            extract_code_from_message("connect ETIMEDOUT 1.2.3.4:443"),
            Some("ETIMEDOUT".to_string())
        );
    }

    #[test]
    fn extract_code_finds_ssl_code() {
        assert_eq!(
            extract_code_from_message("TLS error: CERT_HAS_EXPIRED for api.anthropic.com"),
            Some("CERT_HAS_EXPIRED".to_string())
        );
    }

    #[test]
    fn extract_code_returns_none_for_no_match() {
        assert_eq!(extract_code_from_message("some generic error"), None);
    }

    // -- extract_connection_error_details --

    #[test]
    fn extract_connection_details_from_timeout_variant() {
        let err = ApiErrorInfo::ConnectionTimeout {
            message: "timed out after 600s".to_string(),
        };
        let details = extract_connection_error_details(&err).unwrap();
        assert_eq!(details.code, "ETIMEDOUT");
        assert!(!details.is_ssl_error);
    }

    #[test]
    fn extract_connection_details_from_connection_with_pre_extracted() {
        let err = ApiErrorInfo::Connection {
            message: "ssl handshake failed".to_string(),
            details: Some(ConnectionErrorDetails {
                code: "DEPTH_ZERO_SELF_SIGNED_CERT".to_string(),
                message: "self-signed cert".to_string(),
                is_ssl_error: true,
            }),
        };
        let details = extract_connection_error_details(&err).unwrap();
        assert_eq!(details.code, "DEPTH_ZERO_SELF_SIGNED_CERT");
        assert!(details.is_ssl_error);
    }

    #[test]
    fn extract_connection_details_from_connection_with_code_in_message() {
        let err = ApiErrorInfo::Connection {
            message: "CERT_HAS_EXPIRED for api.anthropic.com".to_string(),
            details: None,
        };
        let details = extract_connection_error_details(&err).unwrap();
        assert_eq!(details.code, "CERT_HAS_EXPIRED");
        assert!(details.is_ssl_error);
    }

    #[test]
    fn extract_connection_details_returns_none_for_http_errors() {
        let err = ApiErrorInfo::Http {
            status: 400,
            message: "bad request".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert!(extract_connection_error_details(&err).is_none());
    }

    #[test]
    fn extract_connection_details_returns_none_for_generic_connection() {
        let err = ApiErrorInfo::Connection {
            message: "some unknown error".to_string(),
            details: None,
        };
        assert!(extract_connection_error_details(&err).is_none());
    }

    // -- get_ssl_error_hint --

    #[test]
    fn ssl_hint_returns_message_for_ssl_connection() {
        let err = ApiErrorInfo::Connection {
            message: "CERT_HAS_EXPIRED for api.anthropic.com".to_string(),
            details: None,
        };
        let hint = get_ssl_error_hint(&err).unwrap();
        assert!(hint.contains("SSL certificate error"));
        assert!(hint.contains("CERT_HAS_EXPIRED"));
        assert!(hint.contains("NODE_EXTRA_CA_CERTS"));
    }

    #[test]
    fn ssl_hint_returns_none_for_non_ssl_connection() {
        let err = ApiErrorInfo::Connection {
            message: "ECONNREFUSED 127.0.0.1:443".to_string(),
            details: None,
        };
        assert!(get_ssl_error_hint(&err).is_none());
    }

    #[test]
    fn ssl_hint_returns_none_for_http_error() {
        let err = ApiErrorInfo::Http {
            status: 500,
            message: "server error".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert!(get_ssl_error_hint(&err).is_none());
    }

    // -- sanitize_api_error --

    #[test]
    fn sanitize_api_error_strips_html() {
        let err = ApiErrorInfo::Http {
            status: 400,
            message: "<html><head><title>Bad Request</title></head></html>".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(sanitize_api_error(&err), "Bad Request");
    }

    #[test]
    fn sanitize_api_error_passes_plain_through() {
        let err = ApiErrorInfo::Http {
            status: 400,
            message: "Invalid parameters".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(sanitize_api_error(&err), "Invalid parameters");
    }

    #[test]
    fn sanitize_api_error_returns_empty_for_empty_message() {
        let err = ApiErrorInfo::Other(String::new());
        assert_eq!(sanitize_api_error(&err), "");
    }

    // -- extract_nested_error_message --

    #[test]
    fn extract_nested_standard_api_shape() {
        let body = serde_json::json!({
            "error": {
                "error": {
                    "message": "The model is overloaded"
                }
            }
        });
        let err = ApiErrorInfo::Http {
            status: 500,
            message: String::new(),
            headers: HashMap::new(),
            body: Some(body),
        };
        assert_eq!(
            extract_nested_error_message(&err),
            Some("The model is overloaded".to_string())
        );
    }

    #[test]
    fn extract_nested_bedrock_shape() {
        let body = serde_json::json!({
            "error": {
                "message": "Access denied to model"
            }
        });
        let err = ApiErrorInfo::Http {
            status: 403,
            message: String::new(),
            headers: HashMap::new(),
            body: Some(body),
        };
        assert_eq!(
            extract_nested_error_message(&err),
            Some("Access denied to model".to_string())
        );
    }

    #[test]
    fn extract_nested_prefers_deep_over_shallow() {
        let body = serde_json::json!({
            "error": {
                "message": "shallow message",
                "error": {
                    "message": "deep message"
                }
            }
        });
        let err = ApiErrorInfo::Http {
            status: 500,
            message: String::new(),
            headers: HashMap::new(),
            body: Some(body),
        };
        // Should return deep message (standard API shape) over shallow (Bedrock)
        assert_eq!(
            extract_nested_error_message(&err),
            Some("deep message".to_string())
        );
    }

    #[test]
    fn extract_nested_sanitizes_html_in_deep_message() {
        let body = serde_json::json!({
            "error": {
                "error": {
                    "message": "<!DOCTYPE html><html><head><title>Gateway Timeout</title></head></html>"
                }
            }
        });
        let err = ApiErrorInfo::Http {
            status: 504,
            message: String::new(),
            headers: HashMap::new(),
            body: Some(body),
        };
        assert_eq!(
            extract_nested_error_message(&err),
            Some("Gateway Timeout".to_string())
        );
    }

    #[test]
    fn extract_nested_returns_none_for_non_http() {
        let err = ApiErrorInfo::Connection {
            message: "connection failed".to_string(),
            details: None,
        };
        assert!(extract_nested_error_message(&err).is_none());
    }

    #[test]
    fn extract_nested_returns_none_for_no_body() {
        let err = ApiErrorInfo::Http {
            status: 500,
            message: String::new(),
            headers: HashMap::new(),
            body: None,
        };
        assert!(extract_nested_error_message(&err).is_none());
    }

    // -- format_api_error --

    #[test]
    fn format_timeout_error() {
        let err = ApiErrorInfo::ConnectionTimeout {
            message: "timed out".to_string(),
        };
        assert_eq!(
            format_api_error(&err),
            "Request timed out. Check your internet connection and proxy settings"
        );
    }

    #[test]
    fn format_ssl_verification_error() {
        let err = ApiErrorInfo::Connection {
            message: "UNABLE_TO_VERIFY_LEAF_SIGNATURE".to_string(),
            details: None,
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: SSL certificate verification failed. \
             Check your proxy or corporate SSL certificates"
        );
    }

    #[test]
    fn format_ssl_expired_error() {
        let err = ApiErrorInfo::Connection {
            message: "CERT_HAS_EXPIRED".to_string(),
            details: None,
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: SSL certificate has expired"
        );
    }

    #[test]
    fn format_ssl_self_signed_error() {
        let err = ApiErrorInfo::Connection {
            message: "SELF_SIGNED_CERT_IN_CHAIN".to_string(),
            details: None,
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: Self-signed certificate detected. \
             Check your proxy or corporate SSL certificates"
        );
    }

    #[test]
    fn format_ssl_hostname_mismatch() {
        let err = ApiErrorInfo::Connection {
            message: "HOSTNAME_MISMATCH".to_string(),
            details: None,
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: SSL certificate hostname mismatch"
        );
    }

    #[test]
    fn format_ssl_not_yet_valid() {
        let err = ApiErrorInfo::Connection {
            message: "CERT_NOT_YET_VALID".to_string(),
            details: None,
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: SSL certificate is not yet valid"
        );
    }

    #[test]
    fn format_ssl_revoked() {
        let err = ApiErrorInfo::Connection {
            message: "CERT_REVOKED".to_string(),
            details: None,
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: SSL certificate has been revoked"
        );
    }

    #[test]
    fn format_ssl_generic_fallback() {
        let err = ApiErrorInfo::Connection {
            message: "ERR_SSL_WRONG_VERSION_NUMBER".to_string(),
            details: None,
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: SSL error (ERR_SSL_WRONG_VERSION_NUMBER)"
        );
    }

    #[test]
    fn format_generic_connection_error_message() {
        let err = ApiErrorInfo::Connection {
            message: "Connection error.".to_string(),
            details: None,
        };
        // No extractable code, so falls through to the generic message
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API. Check your internet connection"
        );
    }

    #[test]
    fn format_connection_error_dot_with_code_in_details() {
        let err = ApiErrorInfo::Connection {
            message: "Connection error.".to_string(),
            details: Some(ConnectionErrorDetails {
                code: "ECONNREFUSED".to_string(),
                message: "connection refused".to_string(),
                is_ssl_error: false,
            }),
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API (ECONNREFUSED)"
        );
    }

    #[test]
    fn format_plain_http_error_passes_through() {
        let err = ApiErrorInfo::Http {
            status: 400,
            message: "Invalid model specified".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(format_api_error(&err), "Invalid model specified");
    }

    #[test]
    fn format_html_error_sanitizes() {
        let err = ApiErrorInfo::Http {
            status: 503,
            message: "<!DOCTYPE html><html><head><title>Service Unavailable</title></head></html>"
                .to_string(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(format_api_error(&err), "Service Unavailable");
    }

    #[test]
    fn format_error_without_message_falls_back_to_status() {
        let err = ApiErrorInfo::Http {
            status: 418,
            message: String::new(),
            headers: HashMap::new(),
            body: None,
        };
        assert_eq!(format_api_error(&err), "API error (status 418)");
    }

    #[test]
    fn format_error_without_message_extracts_nested() {
        let body = serde_json::json!({
            "error": {
                "message": "The model is temporarily unavailable"
            }
        });
        let err = ApiErrorInfo::Http {
            status: 503,
            message: String::new(),
            headers: HashMap::new(),
            body: Some(body),
        };
        assert_eq!(
            format_api_error(&err),
            "The model is temporarily unavailable"
        );
    }

    #[test]
    fn format_sdk_error() {
        let err = ApiErrorInfo::Sdk("missing API key".to_string());
        assert_eq!(format_api_error(&err), "missing API key");
    }

    #[test]
    fn format_other_error() {
        let err = ApiErrorInfo::Other("something unexpected".to_string());
        assert_eq!(format_api_error(&err), "something unexpected");
    }

    // -- integration: pre-extracted SSL details --

    #[test]
    fn format_pre_extracted_ssl_details() {
        let err = ApiErrorInfo::Connection {
            message: "ssl handshake failed".to_string(),
            details: Some(ConnectionErrorDetails {
                code: "CERT_HAS_EXPIRED".to_string(),
                message: "certificate expired".to_string(),
                is_ssl_error: true,
            }),
        };
        assert_eq!(
            format_api_error(&err),
            "Unable to connect to API: SSL certificate has expired"
        );
    }

    #[test]
    fn ssl_hint_with_pre_extracted_details() {
        let err = ApiErrorInfo::Connection {
            message: "ssl error".to_string(),
            details: Some(ConnectionErrorDetails {
                code: "SELF_SIGNED_CERT_IN_CHAIN".to_string(),
                message: "self-signed cert in chain".to_string(),
                is_ssl_error: true,
            }),
        };
        let hint = get_ssl_error_hint(&err).unwrap();
        assert!(hint.contains("SELF_SIGNED_CERT_IN_CHAIN"));
        assert!(hint.contains("/doctor"));
    }
}
