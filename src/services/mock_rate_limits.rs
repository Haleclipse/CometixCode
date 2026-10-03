//! Internal rate-limit scenarios used by `/mock-limits`.
//!
//! Maps to: CC `services/mockRateLimits.ts:1-882`.
//!
//! The command module itself is a generated stub in the 2.1.88 rebuild, so
//! this file owns only the source-backed service contract. All mutators are
//! compile-time gated by `InternalCapability::Api`; external binaries cannot
//! activate mock state through environment or runtime identity overrides.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use chrono::{Datelike, Local, TimeZone, Utc};

use crate::utils::process_env::JsTruthy;

pub type MockHeaders = HashMap<String, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MockScenario {
    Normal,
    SessionLimitReached,
    ApproachingWeeklyLimit,
    WeeklyLimitReached,
    OverageActive,
    OverageWarning,
    OverageExhausted,
    OutOfCredits,
    OrgZeroCreditLimit,
    OrgSpendCapHit,
    MemberZeroCreditLimit,
    SeatTierZeroCreditLimit,
    OpusLimit,
    OpusWarning,
    SonnetLimit,
    SonnetWarning,
    FastModeLimit,
    FastModeShortLimit,
    ExtraUsageRequired,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExceededLimitType {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
}

impl ExceededLimitType {
    fn as_header(self) -> &'static str {
        match self {
            Self::FiveHour => "five_hour",
            Self::SevenDay => "seven_day",
            Self::SevenDayOpus => "seven_day_opus",
            Self::SevenDaySonnet => "seven_day_sonnet",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExceededLimit {
    kind: ExceededLimitType,
    resets_at: i64,
}

#[derive(Clone, Debug, Default)]
struct MockState {
    headers: MockHeaders,
    enabled: bool,
    headerless_429_message: Option<String>,
    subscription_type: Option<String>,
    billing_access_override: Option<bool>,
    fast_mode_duration_ms: Option<i64>,
    fast_mode_expires_at_ms: Option<i64>,
    exceeded_limits: Vec<ExceededLimit>,
}

static MOCK_STATE: LazyLock<Mutex<MockState>> = LazyLock::new(|| Mutex::new(MockState::default()));

fn internal_mock_capability() -> bool {
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    )
}

fn now_seconds() -> i64 {
    Utc::now().timestamp()
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

fn end_of_current_month_seconds() -> i64 {
    let now = Local::now();
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    Local
        .with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .map(|value| value.timestamp())
        .unwrap_or_else(|| now_seconds() + 30 * 24 * 60 * 60)
}

fn update_retry_after(state: &mut MockState) {
    let rejected = state
        .headers
        .get("anthropic-ratelimit-unified-status")
        .is_some_and(|status| status == "rejected");
    let overage_rejected = state
        .headers
        .get("anthropic-ratelimit-unified-overage-status")
        .is_none_or(|status| status == "rejected");
    let reset = state
        .headers
        .get("anthropic-ratelimit-unified-reset")
        .and_then(|value| value.parse::<i64>().ok());
    if rejected && overage_rejected {
        if let Some(reset) = reset {
            state.headers.insert(
                "retry-after".to_string(),
                reset.saturating_sub(now_seconds()).max(0).to_string(),
            );
            return;
        }
    }
    state.headers.remove("retry-after");
}

fn update_representative_claim(state: &mut MockState) {
    let Some(furthest) = state
        .exceeded_limits
        .iter()
        .max_by_key(|limit| limit.resets_at)
        .cloned()
    else {
        state
            .headers
            .remove("anthropic-ratelimit-unified-representative-claim");
        state.headers.remove("anthropic-ratelimit-unified-reset");
        state.headers.remove("retry-after");
        return;
    };
    state.headers.insert(
        "anthropic-ratelimit-unified-representative-claim".to_string(),
        furthest.kind.as_header().to_string(),
    );
    state.headers.insert(
        "anthropic-ratelimit-unified-reset".to_string(),
        furthest.resets_at.to_string(),
    );
    update_retry_after(state);
}

/// Maps to CC `setMockHeader(...)`.
pub fn set_mock_header(key: &str, mut value: Option<String>) {
    if !internal_mock_capability() {
        return;
    }
    let mut state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state.enabled = true;
    let full_key = if key == "retry-after" {
        "retry-after".to_string()
    } else {
        format!("anthropic-ratelimit-unified-{key}")
    };
    if value.as_deref().is_none_or(|value| value == "clear") {
        state.headers.remove(&full_key);
        if key == "claim" {
            state.exceeded_limits.clear();
        }
        if key == "status" || key == "overage-status" {
            update_retry_after(&mut state);
        }
        // CC returns from the clear branch before its trailing empty-header
        // disable check, so the service stays enabled until `clearMockHeaders`.
        return;
    }
    if key == "reset" || key == "overage-reset" {
        if let Some(hours) = value.as_deref().and_then(|value| value.parse::<f64>().ok()) {
            value = Some((now_seconds() as f64 + hours * 3600.0).floor().to_string());
        }
    }
    if key == "claim" {
        let kind = match value.as_deref() {
            Some("five_hour") => Some(ExceededLimitType::FiveHour),
            Some("seven_day") => Some(ExceededLimitType::SevenDay),
            Some("seven_day_opus") => Some(ExceededLimitType::SevenDayOpus),
            Some("seven_day_sonnet") => Some(ExceededLimitType::SevenDaySonnet),
            _ => None,
        };
        if let Some(kind) = kind {
            let hours = if kind == ExceededLimitType::FiveHour {
                5
            } else {
                7 * 24
            };
            state.exceeded_limits.retain(|limit| limit.kind != kind);
            state.exceeded_limits.push(ExceededLimit {
                kind,
                resets_at: now_seconds() + hours * 3600,
            });
            update_representative_claim(&mut state);
            return;
        }
    }
    if let Some(value) = value {
        state.headers.insert(full_key, value);
    }
    if key == "status" || key == "overage-status" {
        update_retry_after(&mut state);
    }
}

/// Maps to CC `addExceededLimit(...)`.
pub fn add_exceeded_limit(kind: ExceededLimitType, hours_from_now: f64) {
    if !internal_mock_capability() {
        return;
    }
    let mut state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state.enabled = true;
    state.exceeded_limits.retain(|limit| limit.kind != kind);
    state.exceeded_limits.push(ExceededLimit {
        kind,
        resets_at: (now_seconds() as f64 + hours_from_now * 3600.0).floor() as i64,
    });
    state.headers.insert(
        "anthropic-ratelimit-unified-status".to_string(),
        "rejected".to_string(),
    );
    update_representative_claim(&mut state);
}

/// Maps to CC `setMockEarlyWarning(...)`.
pub fn set_mock_early_warning(claim: &str, utilization: f64, hours_from_now: Option<f64>) {
    if !internal_mock_capability() || !matches!(claim, "5h" | "7d" | "overage") {
        return;
    }
    let mut state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state.enabled = true;
    clear_early_warning_locked(&mut state);
    let default_hours = if claim == "5h" { 4.0 } else { 5.0 * 24.0 };
    let reset =
        (now_seconds() as f64 + hours_from_now.unwrap_or(default_hours) * 3600.0).floor() as i64;
    state.headers.insert(
        format!("anthropic-ratelimit-unified-{claim}-utilization"),
        utilization.to_string(),
    );
    state.headers.insert(
        format!("anthropic-ratelimit-unified-{claim}-reset"),
        reset.to_string(),
    );
    state.headers.insert(
        format!("anthropic-ratelimit-unified-{claim}-surpassed-threshold"),
        utilization.to_string(),
    );
    state
        .headers
        .entry("anthropic-ratelimit-unified-status".to_string())
        .or_insert_with(|| "allowed".to_string());
}

fn clear_early_warning_locked(state: &mut MockState) {
    for claim in ["5h", "7d"] {
        for suffix in ["utilization", "reset", "surpassed-threshold"] {
            state
                .headers
                .remove(&format!("anthropic-ratelimit-unified-{claim}-{suffix}"));
        }
    }
}

pub fn clear_mock_early_warning() {
    if !internal_mock_capability() {
        return;
    }
    clear_early_warning_locked(&mut MOCK_STATE.lock().expect("mock limits lock poisoned"));
}

/// Maps to CC `setMockRateLimitScenario(...)`.
pub fn set_mock_rate_limit_scenario(scenario: MockScenario) {
    if !internal_mock_capability() {
        return;
    }
    let mut state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    if scenario == MockScenario::Clear {
        state.headers.clear();
        state.headerless_429_message = None;
        state.enabled = false;
        return;
    }

    let preserved_exceeded = if matches!(
        scenario,
        MockScenario::OverageActive | MockScenario::OverageWarning | MockScenario::OverageExhausted
    ) {
        std::mem::take(&mut state.exceeded_limits)
    } else {
        Vec::new()
    };
    let subscription_type = state.subscription_type.take();
    let billing_access_override = state.billing_access_override;
    let fast_mode_duration_ms = state.fast_mode_duration_ms;
    let fast_mode_expires_at_ms = state.fast_mode_expires_at_ms;
    *state = MockState {
        enabled: true,
        subscription_type,
        billing_access_override,
        fast_mode_duration_ms,
        fast_mode_expires_at_ms,
        exceeded_limits: preserved_exceeded,
        ..MockState::default()
    };
    let five_hours = now_seconds() + 5 * 3600;
    let seven_days = now_seconds() + 7 * 24 * 3600;

    let ensure_exceeded = |state: &mut MockState, kind, reset| {
        if state.exceeded_limits.is_empty() {
            state.exceeded_limits.push(ExceededLimit {
                kind,
                resets_at: reset,
            });
        }
        update_representative_claim(state);
    };

    match scenario {
        MockScenario::Normal => {
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "allowed".into(),
            );
            state.headers.insert(
                "anthropic-ratelimit-unified-reset".into(),
                five_hours.to_string(),
            );
        }
        MockScenario::SessionLimitReached => {
            state.exceeded_limits.push(ExceededLimit {
                kind: ExceededLimitType::FiveHour,
                resets_at: five_hours,
            });
            update_representative_claim(&mut state);
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "rejected".into(),
            );
        }
        MockScenario::ApproachingWeeklyLimit => {
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "allowed_warning".into(),
            );
            state.headers.insert(
                "anthropic-ratelimit-unified-reset".into(),
                seven_days.to_string(),
            );
            state.headers.insert(
                "anthropic-ratelimit-unified-representative-claim".into(),
                "seven_day".into(),
            );
        }
        MockScenario::WeeklyLimitReached => {
            state.exceeded_limits.push(ExceededLimit {
                kind: ExceededLimitType::SevenDay,
                resets_at: seven_days,
            });
            update_representative_claim(&mut state);
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "rejected".into(),
            );
        }
        MockScenario::OverageActive
        | MockScenario::OverageWarning
        | MockScenario::OverageExhausted
        | MockScenario::OutOfCredits
        | MockScenario::OrgZeroCreditLimit
        | MockScenario::OrgSpendCapHit
        | MockScenario::MemberZeroCreditLimit
        | MockScenario::SeatTierZeroCreditLimit => {
            ensure_exceeded(&mut state, ExceededLimitType::FiveHour, five_hours);
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "rejected".into(),
            );
            let overage = match scenario {
                MockScenario::OverageActive => "allowed",
                MockScenario::OverageWarning => "allowed_warning",
                _ => "rejected",
            };
            state.headers.insert(
                "anthropic-ratelimit-unified-overage-status".into(),
                overage.into(),
            );
            state.headers.insert(
                "anthropic-ratelimit-unified-overage-reset".into(),
                end_of_current_month_seconds().to_string(),
            );
            let reason = match scenario {
                MockScenario::OutOfCredits => Some("out_of_credits"),
                MockScenario::OrgZeroCreditLimit => Some("org_service_zero_credit_limit"),
                MockScenario::OrgSpendCapHit => Some("org_level_disabled_until"),
                MockScenario::MemberZeroCreditLimit => Some("member_zero_credit_limit"),
                MockScenario::SeatTierZeroCreditLimit => Some("seat_tier_zero_credit_limit"),
                _ => None,
            };
            if let Some(reason) = reason {
                state.headers.insert(
                    "anthropic-ratelimit-unified-overage-disabled-reason".into(),
                    reason.into(),
                );
            }
        }
        MockScenario::OpusLimit | MockScenario::SonnetLimit => {
            let kind = if scenario == MockScenario::OpusLimit {
                ExceededLimitType::SevenDayOpus
            } else {
                ExceededLimitType::SevenDaySonnet
            };
            state.exceeded_limits.push(ExceededLimit {
                kind,
                resets_at: seven_days,
            });
            update_representative_claim(&mut state);
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "rejected".into(),
            );
        }
        MockScenario::OpusWarning | MockScenario::SonnetWarning => {
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "allowed_warning".into(),
            );
            state.headers.insert(
                "anthropic-ratelimit-unified-reset".into(),
                seven_days.to_string(),
            );
            state.headers.insert(
                "anthropic-ratelimit-unified-representative-claim".into(),
                if scenario == MockScenario::OpusWarning {
                    "seven_day_opus"
                } else {
                    "seven_day_sonnet"
                }
                .into(),
            );
        }
        MockScenario::FastModeLimit | MockScenario::FastModeShortLimit => {
            state.headers.insert(
                "anthropic-ratelimit-unified-status".into(),
                "rejected".into(),
            );
            state.fast_mode_duration_ms = Some(if scenario == MockScenario::FastModeLimit {
                10 * 60 * 1000
            } else {
                10 * 1000
            });
        }
        MockScenario::ExtraUsageRequired => {
            state.headerless_429_message =
                Some("Extra usage is required for long context requests.".to_string());
        }
        MockScenario::Clear => unreachable!(),
    }
}

pub fn get_mock_headerless_429_message() -> Option<String> {
    if !internal_mock_capability() {
        return None;
    }
    if let Some(value) = crate::utils::process_env::var("CLAUDE_MOCK_HEADERLESS_429").truthy() {
        return Some(value);
    }
    let state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state
        .enabled
        .then(|| state.headerless_429_message.clone())
        .flatten()
}

pub fn get_mock_headers() -> Option<MockHeaders> {
    if !internal_mock_capability() {
        return None;
    }
    let state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    (state.enabled && !state.headers.is_empty()).then(|| state.headers.clone())
}

pub fn apply_mock_headers(headers: &MockHeaders) -> MockHeaders {
    let Some(mock) = get_mock_headers() else {
        return headers.clone();
    };
    let mut merged = headers.clone();
    merged.extend(mock);
    merged
}

pub fn should_process_mock_limits() -> bool {
    if !internal_mock_capability() {
        return false;
    }
    let env_enabled = crate::utils::process_env::var("CLAUDE_MOCK_HEADERLESS_429")
        .truthy()
        .is_some();
    MOCK_STATE
        .lock()
        .expect("mock limits lock poisoned")
        .enabled
        || env_enabled
}

pub fn set_mock_subscription_type(subscription_type: Option<String>) {
    if !internal_mock_capability() {
        return;
    }
    let mut state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state.enabled = true;
    state.subscription_type = subscription_type;
}

pub fn get_mock_subscription_type() -> Option<String> {
    if !internal_mock_capability() {
        return None;
    }
    let state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state.enabled.then(|| {
        state
            .subscription_type
            .clone()
            .unwrap_or_else(|| "max".to_string())
    })
}

pub fn should_use_mock_subscription() -> bool {
    if !internal_mock_capability() {
        return false;
    }
    let state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state.enabled && state.subscription_type.is_some()
}

pub fn set_mock_billing_access(value: Option<bool>) {
    if !internal_mock_capability() {
        return;
    }
    let mut state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    state.enabled = true;
    state.billing_access_override = value;
}

pub fn mock_billing_access_override() -> Option<bool> {
    if !internal_mock_capability() {
        return None;
    }
    MOCK_STATE
        .lock()
        .expect("mock limits lock poisoned")
        .billing_access_override
}

pub fn is_mock_fast_mode_rate_limit_scenario() -> bool {
    internal_mock_capability()
        && MOCK_STATE
            .lock()
            .expect("mock limits lock poisoned")
            .fast_mode_duration_ms
            .is_some()
}

pub fn check_mock_fast_mode_rate_limit(is_fast_mode_active: bool) -> Option<MockHeaders> {
    if !internal_mock_capability() || !is_fast_mode_active {
        return None;
    }
    let mut state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    let duration = state.fast_mode_duration_ms?;
    let now = now_millis();
    if state
        .fast_mode_expires_at_ms
        .is_some_and(|expiry| now >= expiry)
    {
        *state = MockState::default();
        return None;
    }
    let expiry = *state
        .fast_mode_expires_at_ms
        .get_or_insert_with(|| now.saturating_add(duration));
    let mut headers = state.headers.clone();
    headers.insert(
        "retry-after".to_string(),
        ((expiry - now).max(1) as f64 / 1000.0).ceil().to_string(),
    );
    Some(headers)
}

/// Maps to CC `getCurrentMockScenario()`.
pub fn get_current_mock_scenario() -> Option<MockScenario> {
    if !internal_mock_capability() {
        return None;
    }
    let state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    if !state.enabled {
        return None;
    }
    let status = state
        .headers
        .get("anthropic-ratelimit-unified-status")
        .map(String::as_str);
    let overage = state
        .headers
        .get("anthropic-ratelimit-unified-overage-status")
        .map(String::as_str);
    let claim = state
        .headers
        .get("anthropic-ratelimit-unified-representative-claim")
        .map(String::as_str);
    match (status, overage, claim) {
        (Some("rejected"), _, Some("seven_day_opus")) => Some(MockScenario::OpusLimit),
        (_, _, Some("seven_day_opus")) => Some(MockScenario::OpusWarning),
        (Some("rejected"), _, Some("seven_day_sonnet")) => Some(MockScenario::SonnetLimit),
        (_, _, Some("seven_day_sonnet")) => Some(MockScenario::SonnetWarning),
        (_, Some("rejected"), _) => Some(MockScenario::OverageExhausted),
        (_, Some("allowed_warning"), _) => Some(MockScenario::OverageWarning),
        (_, Some("allowed"), _) => Some(MockScenario::OverageActive),
        (Some("rejected"), _, Some("five_hour")) => Some(MockScenario::SessionLimitReached),
        (Some("rejected"), _, Some("seven_day")) => Some(MockScenario::WeeklyLimitReached),
        (Some("allowed_warning"), _, Some("seven_day")) => {
            Some(MockScenario::ApproachingWeeklyLimit)
        }
        (Some("allowed"), _, _) => Some(MockScenario::Normal),
        _ => None,
    }
}

pub fn get_scenario_description(scenario: MockScenario) -> &'static str {
    match scenario {
        MockScenario::Normal => "Normal usage, no limits",
        MockScenario::SessionLimitReached => "Session rate limit exceeded",
        MockScenario::ApproachingWeeklyLimit => "Approaching weekly aggregate limit",
        MockScenario::WeeklyLimitReached => "Weekly aggregate limit exceeded",
        MockScenario::OverageActive => "Using extra usage (overage active)",
        MockScenario::OverageWarning => "Approaching extra usage limit",
        MockScenario::OverageExhausted => "Both subscription and extra usage limits exhausted",
        MockScenario::OutOfCredits => "Out of extra usage credits (wallet empty)",
        MockScenario::OrgZeroCreditLimit => "Org spend cap is zero (no extra usage budget)",
        MockScenario::OrgSpendCapHit => "Org spend cap hit for the month",
        MockScenario::MemberZeroCreditLimit => "Member limit is zero (admin can allocate more)",
        MockScenario::SeatTierZeroCreditLimit => {
            "Seat tier limit is zero (admin can allocate more)"
        }
        MockScenario::OpusLimit => "Opus limit reached",
        MockScenario::OpusWarning => "Approaching Opus limit",
        MockScenario::SonnetLimit => "Sonnet limit reached",
        MockScenario::SonnetWarning => "Approaching Sonnet limit",
        MockScenario::FastModeLimit => "Fast mode rate limit",
        MockScenario::FastModeShortLimit => "Fast mode rate limit (short)",
        MockScenario::ExtraUsageRequired => "Headerless 429: Extra usage required for 1M context",
        MockScenario::Clear => "Clear mock headers (use real limits)",
    }
}

/// Human-readable status consumed by the source-backed `/mock-limits` service
/// boundary. The command renderer remains `no-source` in rebuild 2.1.88.
pub fn get_mock_status() -> String {
    if !internal_mock_capability() {
        return "No mock headers active (using real limits)".to_string();
    }
    let state = MOCK_STATE.lock().expect("mock limits lock poisoned");
    if !state.enabled || (state.headers.is_empty() && state.subscription_type.is_none()) {
        return "No mock headers active (using real limits)".to_string();
    }
    let mut lines = vec!["Active mock headers:".to_string()];
    let subscription = state.subscription_type.as_deref().unwrap_or("max");
    let suffix = if state.subscription_type.is_some() {
        "explicitly set"
    } else {
        "default"
    };
    lines.push(format!("  Subscription Type: {subscription} ({suffix})"));
    let mut headers = state.headers.iter().collect::<Vec<_>>();
    headers.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in headers {
        let key = key
            .strip_prefix("anthropic-ratelimit-unified-")
            .unwrap_or(key);
        lines.push(format!("  {key}: {value}"));
    }
    lines.join("\n")
}

pub fn clear_mock_headers() {
    if internal_mock_capability() {
        *MOCK_STATE.lock().expect("mock limits lock poisoned") = MockState::default();
    }
}

#[cfg(test)]
pub fn reset_for_test() {
    *MOCK_STATE.lock().expect("mock limits lock poisoned") = MockState::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn mock_limit_activation_respects_compile_time_internal_capability() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        set_mock_rate_limit_scenario(MockScenario::SessionLimitReached);
        if internal_mock_capability() {
            let headers = get_mock_headers().expect("internal mock headers");
            assert_eq!(
                headers
                    .get("anthropic-ratelimit-unified-status")
                    .map(String::as_str),
                Some("rejected")
            );
            assert!(!headers.contains_key("retry-after"));
        } else {
            assert!(get_mock_headers().is_none());
            assert!(!should_process_mock_limits());
        }
        reset_for_test();
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn mock_header_overlay_and_opus_model_gate_match_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        set_mock_rate_limit_scenario(MockScenario::OpusLimit);
        let original = [("server".to_string(), "real".to_string())]
            .into_iter()
            .collect();
        let overlaid = apply_mock_headers(&original);
        assert_eq!(overlaid.get("server").map(String::as_str), Some("real"));
        assert_eq!(
            overlaid
                .get("anthropic-ratelimit-unified-representative-claim")
                .map(String::as_str),
            Some("seven_day_opus")
        );
        reset_for_test();
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn fast_mode_mock_starts_countdown_on_first_active_request() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_for_test();
        set_mock_rate_limit_scenario(MockScenario::FastModeShortLimit);
        assert!(check_mock_fast_mode_rate_limit(false).is_none());
        let headers = check_mock_fast_mode_rate_limit(true).expect("active fast mock");
        let retry_after = headers["retry-after"].parse::<u64>().unwrap();
        assert!((1..=10).contains(&retry_after));
        reset_for_test();
    }
}
