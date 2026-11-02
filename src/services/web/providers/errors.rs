//! Per-provider HTTP error classification (external-search 2.3 / 3.6).
//!
//! @nyan: a quota error is not a rate limit. Each provider signals "out of
//! credits" differently, so the status alone is not enough. Raw response
//! bodies never leave this module: they are redacted and logged for debugging
//! only, and the caller gets a named [`ProviderError`].

use reqwest::header::HeaderMap;
use serde_json::Value;

use super::{ProviderError, ProviderKind};
use crate::services::web::redact::redact_error;

const EXA_QUOTA_CODES: [&str; 3] = [
    "NO_MORE_CREDITS",
    "API_KEY_BUDGET_EXCEEDED",
    "TEAM_BUDGET_EXCEEDED",
];

pub(super) fn classify(
    kind: ProviderKind,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Auth,
        // Exa also uses 402 for non-credit problems; only the documented
        // credit codes are a quota failure.
        402 if kind == ProviderKind::Exa && !exa_quota_code(body) => ProviderError::Http(402),
        402 => ProviderError::QuotaExhausted,
        // Tavily's "plan usage limit exceeded".
        432 if kind == ProviderKind::Tavily => ProviderError::QuotaExhausted,
        429 if kind == ProviderKind::Brave && brave_monthly_exhausted(headers) => {
            ProviderError::QuotaExhausted
        }
        429 => ProviderError::RateLimited,
        status => ProviderError::Http(status),
    }
}

/// Brave sends `x-ratelimit-remaining: <per-second>, <monthly>`; the last
/// window is the monthly one. A lone value is treated as the monthly window.
pub(super) fn brave_monthly_exhausted(headers: &HeaderMap) -> bool {
    headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .is_some_and(|window| window.trim() == "0")
}

fn exa_quota_code(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let mut codes = Vec::new();
    for key in ["code", "tag", "errorCode"] {
        codes.extend(value.get(key).and_then(Value::as_str));
        codes.extend(
            value
                .pointer(&format!("/error/{key}"))
                .and_then(Value::as_str),
        );
    }
    codes.iter().any(|code| EXA_QUOTA_CODES.contains(code))
}

/// Debug-log a failed response without ever surfacing it.
///
/// AnySearch's anonymous 402 body can carry a freshly generated account and
/// key, which no loaded-secret replacement can catch, so only the status is
/// logged for it.
pub(super) fn log_failure(kind: ProviderKind, status: u16, body: &[u8], api_key: Option<&str>) {
    if kind == ProviderKind::AnySearch && status == 402 {
        crate::utils::debug::log_for_debugging("web provider anysearch: HTTP 402 (body withheld)");
        return;
    }
    let secrets = api_key.map(str::to_string).into_iter().collect::<Vec<_>>();
    crate::utils::debug::log_for_debugging(&format!(
        "web provider {}: HTTP {status}: {}",
        kind.name(),
        redact_error(&String::from_utf8_lossy(body), &secrets)
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(remaining: Option<&str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(value) = remaining {
            map.insert("x-ratelimit-remaining", value.parse().unwrap());
        }
        map
    }

    #[test]
    fn auth_statuses_are_auth_for_every_provider() {
        for status in [401, 403] {
            assert_eq!(
                classify(ProviderKind::Exa, status, &headers(None), b""),
                ProviderError::Auth
            );
        }
    }

    #[test]
    fn plain_402_is_quota_except_for_exa_without_a_credit_code() {
        for kind in [
            ProviderKind::AnySearch,
            ProviderKind::Brave,
            ProviderKind::DeepSeek,
            ProviderKind::Firecrawl,
        ] {
            assert_eq!(
                classify(kind, 402, &headers(None), b"{}"),
                ProviderError::QuotaExhausted,
                "{kind:?}"
            );
        }
        assert_eq!(
            classify(
                ProviderKind::Exa,
                402,
                &headers(None),
                b"{\"error\":\"payment\"}"
            ),
            ProviderError::Http(402)
        );
        for body in [
            &br#"{"tag":"NO_MORE_CREDITS"}"#[..],
            br#"{"code":"API_KEY_BUDGET_EXCEEDED"}"#,
            br#"{"error":{"code":"TEAM_BUDGET_EXCEEDED"}}"#,
        ] {
            assert_eq!(
                classify(ProviderKind::Exa, 402, &headers(None), body),
                ProviderError::QuotaExhausted
            );
        }
    }

    #[test]
    fn tavily_432_is_quota_and_other_providers_keep_it_as_http() {
        assert_eq!(
            classify(
                ProviderKind::Tavily,
                432,
                &headers(None),
                br#"{"detail":{"error":"This request exceeds your plan's set usage limit."}}"#
            ),
            ProviderError::QuotaExhausted
        );
        assert_eq!(
            classify(ProviderKind::Exa, 432, &headers(None), b""),
            ProviderError::Http(432)
        );
    }

    #[test]
    fn rate_limit_is_not_quota_unless_brave_month_is_spent() {
        assert_eq!(
            classify(ProviderKind::DeepSeek, 429, &headers(None), b""),
            ProviderError::RateLimited
        );
        assert_eq!(
            classify(ProviderKind::Brave, 429, &headers(Some("0, 1500")), b""),
            ProviderError::RateLimited
        );
        assert_eq!(
            classify(ProviderKind::Brave, 429, &headers(Some("1, 0")), b""),
            ProviderError::QuotaExhausted
        );
    }

    #[test]
    fn brave_monthly_window_is_the_last_value() {
        assert!(brave_monthly_exhausted(&headers(Some("1, 0"))));
        assert!(brave_monthly_exhausted(&headers(Some("0"))));
        assert!(!brave_monthly_exhausted(&headers(Some("0, 12"))));
        assert!(!brave_monthly_exhausted(&headers(None)));
    }

    #[test]
    fn other_statuses_keep_their_code() {
        assert_eq!(
            classify(ProviderKind::Firecrawl, 503, &headers(None), b""),
            ProviderError::Http(503)
        );
    }
}
