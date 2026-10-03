//! Maps to: CC `services/api/referral.ts` cached referral helpers.
//!
//! Official code can fetch referral eligibility and write it to
//! `GlobalConfig.passesEligibilityCache`. Cometix keeps this slice read-only:
//! it only inspects existing config/credential snapshots and never performs
//! OAuth/network calls, writes config, or logs analytics. That restriction is
//! stated here rather than carried in a `_readonly` suffix on every name — the
//! source's names are `shouldCheckForPasses`, `checkCachedPassesEligibility`,
//! `getCachedReferrerReward`, `getCachedRemainingPasses`, and the mapping keeps
//! them.
//!
//! Two shape deviations, recorded rather than hidden:
//! - CC's functions take NO arguments and read `getGlobalConfig()` internally
//!   (`referral.ts:71/83/150/162`). The Rust versions take `&GlobalConfig` so
//!   the read-only slice cannot reach for process state on its own. That
//!   parameter is genuinely used; a `get_env` parameter that was NOT used has
//!   been removed, because it made callers look like they could steer a result
//!   that actually comes from `.credentials.json`.
//! - `get_cached_referrer_reward_text` returns the formatted string where CC's
//!   `getCachedReferrerReward` returns `ReferrerRewardInfo | null`; the `_text`
//!   in the name marks that, and `format_credit_amount` is the shared formatter
//!   either shape uses.

use crate::components::logo_v2::guest_passes_upsell::GuestPassesUpsellSnapshot;
use crate::utils::auth::{get_subscription_type, is_claude_ai_subscriber};
use crate::utils::config::GlobalConfig;
use serde::Deserialize;
use std::collections::HashMap;

pub const CACHE_EXPIRATION_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReferrerRewardInfo {
    pub currency: String,
    pub amount_minor_units: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReferralEligibilityCacheEntry {
    pub eligible: bool,
    pub timestamp: i64,
    pub referrer_reward: Option<ReferrerRewardInfo>,
    pub remaining_passes: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedPassesEligibility {
    pub eligible: bool,
    pub needs_refresh: bool,
    pub has_cache: bool,
    pub reward_text: Option<String>,
    pub remaining_passes: Option<u32>,
}

/// Maps to: CC `services/api/referral.ts` `checkCachedPassesEligibility`.
pub fn check_cached_passes_eligibility(
    config: &GlobalConfig,
    now_ms: i64,
) -> CachedPassesEligibility {
    if !should_check_for_passes(config) {
        return CachedPassesEligibility::default();
    }

    let Some(org_id) = config
        .oauth_account
        .as_ref()
        .and_then(|account| account.organization_uuid.as_deref())
        .filter(|org_id| !org_id.is_empty())
    else {
        return CachedPassesEligibility::default();
    };

    let Some(entry) = cached_passes_entry(config, org_id) else {
        return CachedPassesEligibility {
            eligible: false,
            needs_refresh: true,
            has_cache: false,
            reward_text: None,
            remaining_passes: None,
        };
    };

    CachedPassesEligibility {
        eligible: entry.eligible,
        needs_refresh: now_ms.saturating_sub(entry.timestamp) > CACHE_EXPIRATION_MS,
        has_cache: true,
        reward_text: entry.referrer_reward.as_ref().map(format_credit_amount),
        remaining_passes: entry.remaining_passes,
    }
}

/// Maps to: CC `services/api/referral.ts` `getCachedReferrerReward`.
pub fn get_cached_referrer_reward_text(config: &GlobalConfig) -> Option<String> {
    if !should_check_for_passes(config) {
        return None;
    }
    let org_id = config
        .oauth_account
        .as_ref()
        .and_then(|account| account.organization_uuid.as_deref())
        .filter(|org_id| !org_id.is_empty())?;
    cached_passes_entry(config, org_id)
        .and_then(|entry| entry.referrer_reward)
        .as_ref()
        .map(format_credit_amount)
}

/// Maps to: CC `services/api/referral.ts` `getCachedRemainingPasses`.
pub fn get_cached_remaining_passes(config: &GlobalConfig) -> Option<u32> {
    if !should_check_for_passes(config) {
        return None;
    }
    let org_id = config
        .oauth_account
        .as_ref()
        .and_then(|account| account.organization_uuid.as_deref())
        .filter(|org_id| !org_id.is_empty())?;
    cached_passes_entry(config, org_id).and_then(|entry| entry.remaining_passes)
}

/// Maps to: CC `services/api/referral.ts` `formatCreditAmount`.
pub fn format_credit_amount(reward: &ReferrerRewardInfo) -> String {
    let symbol = match reward.currency.as_str() {
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "BRL" => "R$",
        "CAD" => "CA$",
        "AUD" => "A$",
        "NZD" => "NZ$",
        "SGD" => "S$",
        currency => {
            return format!(
                "{} {}",
                currency,
                formatted_minor_units(reward.amount_minor_units)
            );
        }
    };
    format!(
        "{symbol}{}",
        formatted_minor_units(reward.amount_minor_units)
    )
}

/// **Rust-only, no CC counterpart symbol.** CC's `LogoV2` reads
/// `checkCachedPassesEligibility()` and the upsell counters from
/// `getGlobalConfig()` at its own call site; this bundles the same reads so the
/// snapshot can be built once, off the render path.
pub fn guest_passes_snapshot_from_readonly_config(
    config: &GlobalConfig,
    now_ms: i64,
) -> (GuestPassesUpsellSnapshot, Option<String>) {
    let eligibility = check_cached_passes_eligibility(config, now_ms);
    let snapshot = GuestPassesUpsellSnapshot {
        eligible: eligibility.eligible,
        has_cache: eligibility.has_cache,
        remaining_passes: eligibility.remaining_passes,
        passes_last_seen_remaining: config.passes_last_seen_remaining.unwrap_or(0),
        passes_upsell_seen_count: config.passes_upsell_seen_count.unwrap_or(0),
        has_visited_passes: config.has_visited_passes.unwrap_or(false),
    };
    (snapshot, eligibility.reward_text)
}

/// Maps to: CC `services/api/referral.ts#shouldCheckForPasses` — a
/// zero-argument predicate.
///
/// It used to take a `get_env` the body never read: `is_claude_ai_subscriber`
/// and `get_subscription_type` resolve from process state (ultimately
/// `.credentials.json` under `get_claude_config_home_dir()`), exactly as at the source.
/// The parameter made callers look like they could steer the result, which is
/// how a test came to assert the no-subscription outcome while silently
/// depending on whichever account the developer was logged into. Removed rather
/// than wired up — CC takes no arguments here, so giving it real ones would be
/// the deviation.
fn should_check_for_passes(config: &GlobalConfig) -> bool {
    let has_org = config
        .oauth_account
        .as_ref()
        .and_then(|account| account.organization_uuid.as_deref())
        .is_some_and(|org_id| !org_id.is_empty());
    has_org && is_claude_ai_subscriber() && get_subscription_type().as_deref() == Some("max")
}

fn cached_passes_entry(
    config: &GlobalConfig,
    org_id: &str,
) -> Option<ReferralEligibilityCacheEntry> {
    let cache = config.passes_eligibility_cache.as_ref()?;
    let by_org =
        serde_json::from_value::<HashMap<String, ReferralEligibilityCacheEntry>>(cache.clone())
            .ok()?;
    by_org.get(org_id).cloned()
}

fn formatted_minor_units(amount_minor_units: i64) -> String {
    let amount = amount_minor_units as f64 / 100.0;
    if amount.fract() == 0.0 {
        format!("{}", amount as i64)
    } else {
        format!("{amount:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::{AccountInfo, GlobalConfig};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn config_with_cache(entry: serde_json::Value) -> GlobalConfig {
        GlobalConfig {
            oauth_account: Some(AccountInfo {
                organization_uuid: Some("org-1".to_string()),
                ..Default::default()
            }),
            passes_eligibility_cache: Some(serde_json::json!({ "org-1": entry })),
            ..Default::default()
        }
    }

    #[test]
    fn format_credit_amount_matches_official_currency_table() {
        assert_eq!(
            format_credit_amount(&ReferrerRewardInfo {
                currency: "USD".to_string(),
                amount_minor_units: 500,
            }),
            "$5"
        );
        assert_eq!(
            format_credit_amount(&ReferrerRewardInfo {
                currency: "EUR".to_string(),
                amount_minor_units: 125,
            }),
            "€1.25"
        );
        assert_eq!(
            format_credit_amount(&ReferrerRewardInfo {
                currency: "JPY".to_string(),
                amount_minor_units: 300,
            }),
            "JPY 3"
        );
    }

    #[test]
    fn cached_passes_eligibility_requires_official_precheck_and_reports_cache_state() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let config = config_with_cache(serde_json::json!({
            "eligible": true,
            "timestamp": 1_000,
            "remaining_passes": 2,
            "referrer_reward": { "currency": "USD", "amount_minor_units": 500 }
        }));

        // Asserting the NO-subscription outcome means owning that premise: the
        // subscription lookup falls back to `.credentials.json` under
        // `get_claude_config_home_dir()`, where the harness seeds a logged-in identity.
        // The sibling test below does the same thing in reverse (it writes a
        // max-subscriber file); this one needs the directory empty.
        let dir = std::env::temp_dir().join(format!(
            "cometix-referral-nosub-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);

        let without_subscription = check_cached_passes_eligibility(&config, 2_000);

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(without_subscription, CachedPassesEligibility::default());
    }

    #[test]
    fn cached_passes_eligibility_reads_fresh_max_subscriber_cache() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let config = config_with_cache(serde_json::json!({
            "eligible": true,
            "timestamp": 1_000,
            "remaining_passes": 2,
            "referrer_reward": { "currency": "USD", "amount_minor_units": 500 }
        }));
        let dir =
            std::env::temp_dir().join(format!("cometix-referral-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".credentials.json"),
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "token",
                    "scopes": ["user:inference"],
                    "subscriptionType": "max"
                }
            })
            .to_string(),
        )
        .unwrap();

        // The credentials file only counts if the process actually looks there.
        // This used to pass a `CLAUDE_CONFIG_DIR` closure that nothing read, so
        // the test was green purely because the harness seeds a max-subscriber
        // identity at the real config home — it would have passed with an empty
        // `dir` too, and failed the moment the harness stopped seeding.
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);

        let eligibility = check_cached_passes_eligibility(&config, 2_000);

        let _ = std::fs::remove_dir_all(dir);

        assert!(eligibility.eligible);
        assert!(eligibility.has_cache);
        assert!(!eligibility.needs_refresh);
        assert_eq!(eligibility.reward_text.as_deref(), Some("$5"));
        assert_eq!(eligibility.remaining_passes, Some(2));
    }

    #[test]
    fn guest_passes_snapshot_uses_config_impression_fields_without_writes() {
        let mut config = GlobalConfig::default();
        config.passes_last_seen_remaining = Some(1);
        config.passes_upsell_seen_count = Some(2);
        config.has_visited_passes = Some(true);

        let (snapshot, reward) = guest_passes_snapshot_from_readonly_config(&config, 0);

        assert_eq!(snapshot.passes_last_seen_remaining, 1);
        assert_eq!(snapshot.passes_upsell_seen_count, 2);
        assert!(snapshot.has_visited_passes);
        assert!(reward.is_none());
    }
}
