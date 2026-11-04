//! Google Gemini rate-limit and error classification policy.
//!
//! @nyan: docs/nyan/provider-redesign.md §7.3.
//! Maps Gemini / Google RPC errors to retry verdicts:
//! - RESOURCE_EXHAUSTED / 429 burst is repeated against the same endpoint,
//!   honouring `RetryInfo.retryDelay` or `Retry-After` as the wait hint;
//! - Quota exceeded or billing exhaustion is never repeated and marked as
//!   `Verdict::SwitchOnly` so only configured fallback profiles may take over;
//! - UNAVAILABLE / 503, INTERNAL, and DEADLINE_EXCEEDED are retried;
//! - 401, 403, 404, and INVALID_ARGUMENT are fatal client errors;
//! - Interrupted streams (`PartialStream`) are fatal and never replayed.

use crate::services::provider::ir::{ProviderError, ProviderErrorKind};
use crate::services::provider::retry::{RatePolicy, Verdict, base_verdict};

pub struct GeminiPolicy;

fn is_quota_or_billing(error: &ProviderError) -> bool {
    let check_text = |text: &str| {
        let lower = text.to_ascii_lowercase();
        lower.contains("quota exceeded")
            || lower.contains("quota_exceeded")
            || lower.contains("exceeded your current quota")
            || lower.contains("billing")
            || lower.contains("check billing")
    };

    if check_text(&error.message) {
        return true;
    }

    if let Some(body) = &error.body {
        let err_obj = body.get("error").filter(|v| v.is_object()).unwrap_or(body);
        if let Some(msg) = err_obj.get("message").and_then(|m| m.as_str()) {
            if check_text(msg) {
                return true;
            }
        }
        if let Some(details) = err_obj.get("details").and_then(|d| d.as_array()) {
            for detail in details {
                if detail
                    .get("@type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t.ends_with("google.rpc.QuotaFailure"))
                {
                    return true;
                }
            }
        }
    }

    false
}

impl RatePolicy for GeminiPolicy {
    fn classify(&self, error: &ProviderError) -> Verdict {
        if (error.kind == ProviderErrorKind::RateLimited || error.status == Some(429))
            && is_quota_or_billing(error)
        {
            return Verdict::SwitchOnly;
        }
        base_verdict(error)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    fn error(kind: ProviderErrorKind, status: u16, message: &str) -> ProviderError {
        let mut error = ProviderError::new(kind, message);
        error.status = Some(status);
        error
    }

    #[test]
    fn a_burst_429_waits_for_retry_info_delay_or_retry_after() {
        let mut err_with_delay = error(
            ProviderErrorKind::RateLimited,
            429,
            "Resource has been exhausted (e.g. check quota).",
        );
        err_with_delay.retry_after = Some(Duration::from_millis(4500));
        assert_eq!(
            GeminiPolicy.classify(&err_with_delay),
            Verdict::Retry {
                wait: Some(Duration::from_millis(4500))
            }
        );

        let mut err_with_header = error(
            ProviderErrorKind::RateLimited,
            429,
            "Resource has been exhausted",
        );
        err_with_header.retry_after = Some(Duration::from_secs(10));
        assert_eq!(
            GeminiPolicy.classify(&err_with_header),
            Verdict::Retry {
                wait: Some(Duration::from_secs(10))
            }
        );

        let err_no_wait = error(
            ProviderErrorKind::RateLimited,
            429,
            "Resource has been exhausted",
        );
        assert_eq!(
            GeminiPolicy.classify(&err_no_wait),
            Verdict::Retry { wait: None }
        );
    }

    #[test]
    fn a_quota_or_billing_429_is_not_repeated_and_yields_switch_only() {
        let quota_msg = error(
            ProviderErrorKind::RateLimited,
            429,
            "Quota exceeded for quota metric 'Queries' and limit 'Queries per minute'",
        );
        assert_eq!(GeminiPolicy.classify(&quota_msg), Verdict::SwitchOnly);

        let billing_msg = error(
            ProviderErrorKind::RateLimited,
            429,
            "Billing account has been disabled or check billing details",
        );
        assert_eq!(GeminiPolicy.classify(&billing_msg), Verdict::SwitchOnly);

        let mut body_quota = error(
            ProviderErrorKind::RateLimited,
            429,
            "Resource has been exhausted",
        );
        body_quota.body = Some(json!({
            "error": {
                "code": 429,
                "message": "Resource has been exhausted",
                "status": "RESOURCE_EXHAUSTED",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                        "violations": [{"subject": "project:123"}]
                    }
                ]
            }
        }));
        assert_eq!(GeminiPolicy.classify(&body_quota), Verdict::SwitchOnly);
    }

    #[test]
    fn transient_errors_are_retried() {
        let unavailable = error(ProviderErrorKind::Overloaded, 503, "Service unavailable");
        assert_eq!(
            GeminiPolicy.classify(&unavailable),
            Verdict::Retry { wait: None }
        );

        let internal = error(
            ProviderErrorKind::Transient { retryable: true },
            500,
            "Internal error",
        );
        assert_eq!(
            GeminiPolicy.classify(&internal),
            Verdict::Retry { wait: None }
        );

        let deadline = error(
            ProviderErrorKind::Transient { retryable: true },
            504,
            "Deadline exceeded",
        );
        assert_eq!(
            GeminiPolicy.classify(&deadline),
            Verdict::Retry { wait: None }
        );
    }

    #[test]
    fn client_errors_and_interrupted_streams_are_fatal() {
        for (kind, status) in [
            (ProviderErrorKind::Unauthorized, 401),
            (ProviderErrorKind::Forbidden, 403),
            (ProviderErrorKind::NotFound, 404),
            (ProviderErrorKind::InvalidRequest, 400),
            (ProviderErrorKind::ContextOverflow, 400),
            (ProviderErrorKind::PartialStream, 500),
            (ProviderErrorKind::Aborted, 499),
        ] {
            let err = error(kind.clone(), status, "error");
            assert_eq!(
                GeminiPolicy.classify(&err),
                Verdict::Fatal,
                "testing kind {:?}",
                kind
            );
        }
    }
}
