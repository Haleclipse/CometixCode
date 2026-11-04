//! Anthropic Messages rate-limit policy.
//!
//! @nyan: docs/nyan/provider-redesign.md §7.3. 429, 529 (overloaded), 408 and
//! 5xx are repeated against the same endpoint; `Retry-After` is the wait when
//! it is there. Anthropic's own `x-should-retry` header is honoured here and
//! nowhere else, because no other protocol defines it. 401, 403, 404, 400 and
//! context overflow are never repeated.

use crate::services::provider::ir::ProviderError;
use crate::services::provider::retry::{RatePolicy, Verdict, base_verdict};

pub struct AnthropicPolicy;

impl RatePolicy for AnthropicPolicy {
    fn classify(&self, error: &ProviderError) -> Verdict {
        // 409 is Anthropic's lock timeout, which the previous retry loop also
        // repeated.
        if error.status == Some(409) {
            return Verdict::Retry {
                wait: error.retry_after,
            };
        }
        // An unambiguous "do not" from the server stands for retryable kinds;
        // a 4xx that is not retryable stays not retryable either way.
        let verdict = base_verdict(error);
        if matches!(verdict, Verdict::Retry { .. })
            && error
                .header("x-should-retry")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("false"))
            && error.status.is_some_and(|status| status < 500)
        {
            return Verdict::Fatal;
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::services::provider::ir::ProviderErrorKind;

    fn error(kind: ProviderErrorKind, status: u16) -> ProviderError {
        let mut error = ProviderError::new(kind, "x");
        error.status = Some(status);
        error
    }

    #[test]
    fn the_transient_statuses_are_retried_with_the_servers_wait() {
        let mut limited = error(ProviderErrorKind::RateLimited, 429);
        limited.retry_after = Some(Duration::from_secs(7));
        assert_eq!(
            AnthropicPolicy.classify(&limited),
            Verdict::Retry {
                wait: Some(Duration::from_secs(7))
            }
        );
        for (kind, status) in [
            (ProviderErrorKind::Overloaded, 529),
            (ProviderErrorKind::Transient { retryable: true }, 503),
            (ProviderErrorKind::Transient { retryable: true }, 408),
            (ProviderErrorKind::InvalidRequest, 409),
        ] {
            assert_eq!(
                AnthropicPolicy.classify(&error(kind, status)),
                Verdict::Retry { wait: None },
                "status {status}"
            );
        }
    }

    #[test]
    fn auth_routing_and_request_errors_are_never_repeated() {
        for (kind, status) in [
            (ProviderErrorKind::Unauthorized, 401),
            (ProviderErrorKind::Forbidden, 403),
            (ProviderErrorKind::NotFound, 404),
            (ProviderErrorKind::InvalidRequest, 400),
            (ProviderErrorKind::ContextOverflow, 400),
        ] {
            assert_eq!(
                AnthropicPolicy.classify(&error(kind, status)),
                Verdict::Fatal,
                "status {status}"
            );
        }
    }

    #[test]
    fn should_retry_false_stops_a_client_error_but_not_a_server_error() {
        let mut limited = error(ProviderErrorKind::RateLimited, 429);
        limited
            .headers
            .insert("x-should-retry".into(), "false".into());
        assert_eq!(AnthropicPolicy.classify(&limited), Verdict::Fatal);
        let mut server = error(ProviderErrorKind::Transient { retryable: true }, 500);
        server
            .headers
            .insert("x-should-retry".into(), "false".into());
        assert_eq!(
            AnthropicPolicy.classify(&server),
            Verdict::Retry { wait: None }
        );
    }
}
