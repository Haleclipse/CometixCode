//! 1M-context entitlement checks.
//! Maps to CC `utils/model/check1mAccess.ts`.
//!
//! Every input is local: the opt-out env var, the cached OAuth scopes, and the
//! `cachedExtraUsageDisabledReason` snapshot last written by a rate-limit
//! header. Nothing here re-fetches an entitlement.

use crate::utils::config::CachedExtraUsageDisabledReason;

/// Maps to: CC `utils/model/check1mAccess.ts:11-43` `isExtraUsageEnabled`.
fn is_extra_usage_enabled() -> bool {
    match crate::utils::config::load_global_config().cached_extra_usage_disabled_reason {
        // No cache yet: treat as not enabled (conservative).
        CachedExtraUsageDisabledReason::Uncached => false,
        // No disabled reason from the API: extra usage is enabled.
        CachedExtraUsageDisabledReason::Enabled => true,
        // Provisioned but credits depleted still counts as enabled; every
        // other reason (and any unrecognized one) does not.
        CachedExtraUsageDisabledReason::Disabled(reason) => reason == "out_of_credits",
    }
}

/// Maps to: CC `utils/model/check1mAccess.ts:46-58` `checkOpus1mAccess`.
pub fn check_opus_1m_access() -> bool {
    if crate::utils::context::is_1m_context_disabled() {
        return false;
    }
    if crate::utils::auth::is_claude_ai_subscriber() {
        return is_extra_usage_enabled();
    }
    true
}

/// Maps to: CC `utils/model/check1mAccess.ts:60-72` `checkSonnet1mAccess`.
pub fn check_sonnet_1m_access() -> bool {
    if crate::utils::context::is_1m_context_disabled() {
        return false;
    }
    if crate::utils::auth::is_claude_ai_subscriber() {
        return is_extra_usage_enabled();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn with_cached_reason(reason: CachedExtraUsageDisabledReason) {
        crate::utils::config::set_test_global_config(Some(crate::utils::config::GlobalConfig {
            cached_extra_usage_disabled_reason: reason,
            ..Default::default()
        }));
    }

    /// Maps to: CC `utils/model/check1mAccess.ts:14-42`. The three cached
    /// states and the `out_of_credits` carve-out are the whole reason the
    /// config field is tri-state, so exercise each one.
    #[test]
    fn extra_usage_gate_matches_the_official_tristate_and_reason_switch() {
        with_cached_reason(CachedExtraUsageDisabledReason::Uncached);
        assert!(!is_extra_usage_enabled());

        with_cached_reason(CachedExtraUsageDisabledReason::Enabled);
        assert!(is_extra_usage_enabled());

        with_cached_reason(CachedExtraUsageDisabledReason::Disabled(
            "out_of_credits".to_string(),
        ));
        assert!(is_extra_usage_enabled());

        for reason in [
            "overage_not_provisioned",
            "org_level_disabled",
            "org_level_disabled_until",
            "seat_tier_level_disabled",
            "member_level_disabled",
            "seat_tier_zero_credit_limit",
            "group_zero_credit_limit",
            "member_zero_credit_limit",
            "org_service_level_disabled",
            "org_service_zero_credit_limit",
            "no_limits_configured",
            "unknown",
            // CC's `switch` has a `default: return false`, so an unrecognized
            // reason is treated as disabled rather than as "provisioned".
            "a_reason_the_client_does_not_know",
        ] {
            with_cached_reason(CachedExtraUsageDisabledReason::Disabled(reason.to_string()));
            assert!(!is_extra_usage_enabled(), "{reason}");
        }

        crate::utils::config::set_test_global_config(None);
    }

    /// Maps to: CC `utils/model/check1mAccess.ts:47-57` — the opt-out closes
    /// the gate for everyone, and non-subscribers pass without an entitlement.
    #[test]
    fn access_checks_honor_the_official_opt_out_and_non_subscriber_pass() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_1M_CONTEXT", "1");
        assert!(!check_opus_1m_access());
        assert!(!check_sonnet_1m_access());
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");

        // Bare mode disables Anthropic auth, so `isClaudeAISubscriber()` is
        // false and both checks take the non-subscriber pass.
        crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "1");
        with_cached_reason(CachedExtraUsageDisabledReason::Uncached);
        assert!(check_opus_1m_access());
        assert!(check_sonnet_1m_access());
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        crate::utils::config::set_test_global_config(None);
    }
}
