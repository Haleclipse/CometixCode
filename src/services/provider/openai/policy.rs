//! OpenAI Chat Completions / Responses rate-limit policy.
//!
//! @nyan: docs/nyan/provider-redesign.md §7.3. Both protocols share one policy
//! because both report through the same error envelope (`errors.rs` parses
//! it). 429 `rate_limit_exceeded`, 408 and 5xx are repeated against the same
//! endpoint, waiting `retry-after-ms` / `Retry-After` when the server sent one.
//! A 429 whose code is `insufficient_quota` is a billing failure: waiting does
//! not restore a quota, so it is never repeated and only a configured fallback
//! may take over. The `x-ratelimit-*` headers describe the window and are not
//! used to decide anything.

use crate::services::provider::ir::{ProviderError, ProviderErrorKind};
use crate::services::provider::retry::{RatePolicy, Verdict, base_verdict};

pub struct OpenAiPolicy;

/// The `error.code` / `error.type` the body carried, lower-cased, if any.
fn body_codes(error: &ProviderError) -> [Option<String>; 2] {
    let parsed = error
        .body
        .as_ref()
        .map(super::errors::parse_error_body)
        .unwrap_or_default();
    [parsed.code, parsed.kind].map(|value| value.map(|value| value.to_ascii_lowercase()))
}

fn is_billing(error: &ProviderError) -> bool {
    body_codes(error)
        .iter()
        .flatten()
        .any(|code| code == "insufficient_quota" || code == "billing_hard_limit_reached")
}

impl RatePolicy for OpenAiPolicy {
    fn classify(&self, error: &ProviderError) -> Verdict {
        if error.kind == ProviderErrorKind::RateLimited && is_billing(error) {
            return Verdict::SwitchOnly;
        }
        base_verdict(error)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::*;

    fn limited(body: Value) -> ProviderError {
        let mut error = ProviderError::new(ProviderErrorKind::RateLimited, "slow");
        error.status = Some(429);
        error.body = Some(body);
        error
    }

    #[test]
    fn a_burst_429_waits_for_the_servers_hint() {
        let mut error = limited(json!({"error": {"code": "rate_limit_exceeded"}}));
        error.retry_after = Some(Duration::from_millis(1500));
        assert_eq!(
            OpenAiPolicy.classify(&error),
            Verdict::Retry {
                wait: Some(Duration::from_millis(1500))
            }
        );
    }

    #[test]
    fn a_billing_429_is_not_repeated_but_may_fail_over() {
        for body in [
            json!({"error": {"code": "insufficient_quota", "type": "insufficient_quota"}}),
            json!({"error": {"type": "insufficient_quota"}}),
            json!({"error": {"code": "billing_hard_limit_reached"}}),
        ] {
            assert_eq!(OpenAiPolicy.classify(&limited(body)), Verdict::SwitchOnly);
        }
    }

    #[test]
    fn a_429_without_a_body_is_a_burst() {
        let mut error = ProviderError::new(ProviderErrorKind::RateLimited, "slow");
        error.status = Some(429);
        assert_eq!(OpenAiPolicy.classify(&error), Verdict::Retry { wait: None });
    }

    #[test]
    fn server_errors_are_repeated_and_client_errors_are_not() {
        let server = ProviderError::new(ProviderErrorKind::Transient { retryable: true }, "x");
        assert_eq!(
            OpenAiPolicy.classify(&server),
            Verdict::Retry { wait: None }
        );
        for kind in [
            ProviderErrorKind::Unauthorized,
            ProviderErrorKind::InvalidRequest,
            ProviderErrorKind::ContextOverflow,
            ProviderErrorKind::PartialStream,
        ] {
            assert_eq!(
                OpenAiPolicy.classify(&ProviderError::new(kind, "x")),
                Verdict::Fatal
            );
        }
    }
}
