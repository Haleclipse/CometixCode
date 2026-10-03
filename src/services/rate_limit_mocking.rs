//! Rate-limit mock facade.
//!
//! Maps to: CC `services/rateLimitMocking.ts:1-144`.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockRateLimitError {
    pub message: String,
    pub headers: HashMap<String, String>,
}

/// Applies active internal mock headers over a real API response.
pub fn process_rate_limit_headers(headers: &HashMap<String, String>) -> HashMap<String, String> {
    if crate::services::mock_rate_limits::should_process_mock_limits() {
        crate::services::mock_rate_limits::apply_mock_headers(headers)
    } else {
        headers.clone()
    }
}

/// Rate limits are relevant for a real subscriber or an active internal mock.
pub fn should_process_rate_limits(is_subscriber: bool) -> bool {
    is_subscriber || crate::services::mock_rate_limits::should_process_mock_limits()
}

/// Maps to CC `checkMockRateLimitError(...)`.
pub fn check_mock_rate_limit_error(
    current_model: &str,
    is_fast_mode_active: bool,
) -> Option<MockRateLimitError> {
    if !crate::services::mock_rate_limits::should_process_mock_limits() {
        return None;
    }
    if let Some(message) = crate::services::mock_rate_limits::get_mock_headerless_429_message() {
        return Some(MockRateLimitError {
            message,
            headers: HashMap::new(),
        });
    }
    let headers = crate::services::mock_rate_limits::get_mock_headers()?;
    let status = headers
        .get("anthropic-ratelimit-unified-status")
        .map(String::as_str);
    let overage_status = headers
        .get("anthropic-ratelimit-unified-overage-status")
        .map(String::as_str);
    let claim = headers
        .get("anthropic-ratelimit-unified-representative-claim")
        .map(String::as_str);

    if claim == Some("seven_day_opus") && !current_model.contains("opus") {
        return None;
    }
    if crate::services::mock_rate_limits::is_mock_fast_mode_rate_limit_scenario() {
        return crate::services::mock_rate_limits::check_mock_fast_mode_rate_limit(
            is_fast_mode_active,
        )
        .map(|headers| MockRateLimitError {
            message: "Rate limit exceeded".to_string(),
            headers,
        });
    }
    (status == Some("rejected") && overage_status.is_none_or(|overage| overage == "rejected")).then(
        || MockRateLimitError {
            message: "Rate limit exceeded".to_string(),
            headers,
        },
    )
}

/// Active mock 429 errors never enter normal retry/backoff behavior.
pub fn is_mock_rate_limit_error(status: Option<u16>) -> bool {
    crate::services::mock_rate_limits::should_process_mock_limits() && status == Some(429)
}

pub use crate::services::mock_rate_limits::should_process_mock_limits;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "anthropic_internal")]
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn opus_mock_only_rejects_opus_model_like_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::services::mock_rate_limits::reset_for_test();
        crate::services::mock_rate_limits::set_mock_rate_limit_scenario(
            crate::services::mock_rate_limits::MockScenario::OpusLimit,
        );
        assert!(check_mock_rate_limit_error("claude-opus-4-6", false).is_some());
        assert!(check_mock_rate_limit_error("claude-sonnet-4-6", false).is_none());
        crate::services::mock_rate_limits::reset_for_test();
    }
}
