//! Maps to: CC `services/api/referral.ts` cached referral helpers.
//!
//! Official code fetches referral eligibility for the claude.ai organization
//! and caches it in `GlobalConfig.passesEligibilityCache`. Guest passes are a
//! claude.ai Max benefit and there is no claude.ai account, so no organization
//! is known and nothing is ever read from that cache; the lookups below return
//! the no-passes answer. The source's names `checkCachedPassesEligibility`,
//! `getCachedReferrerReward`, and `getCachedRemainingPasses` are kept.
//!
//! Two shape deviations, recorded rather than hidden:
//! - CC's functions take NO arguments and read `getGlobalConfig()` internally
//!   (`referral.ts:71/83/150/162`). The Rust versions take `&GlobalConfig`,
//!   as `guest_passes_snapshot_from_readonly_config` reads its impression
//!   counters from it.
//! - `get_cached_referrer_reward_text` returns the formatted string where CC's
//!   `getCachedReferrerReward` returns `ReferrerRewardInfo | null`; the `_text`
//!   in the name marks that, and `format_credit_amount` is the shared formatter
//!   either shape uses.

use crate::components::logo_v2::guest_passes_upsell::GuestPassesUpsellSnapshot;
use crate::utils::config::GlobalConfig;
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReferrerRewardInfo {
    pub currency: String,
    pub amount_minor_units: i64,
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
    _config: &GlobalConfig,
    _now_ms: i64,
) -> CachedPassesEligibility {
    CachedPassesEligibility::default()
}

/// Maps to: CC `services/api/referral.ts` `getCachedReferrerReward`.
pub fn get_cached_referrer_reward_text(_config: &GlobalConfig) -> Option<String> {
    None
}

/// Maps to: CC `services/api/referral.ts` `getCachedRemainingPasses`.
pub fn get_cached_remaining_passes(_config: &GlobalConfig) -> Option<u32> {
    None
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
    use crate::utils::config::GlobalConfig;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

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
    fn cached_passes_eligibility_ignores_a_max_subscriber_credentials_file() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        // A leftover eligibility cache and claude.ai account from an older
        // build stay in the config's catch-all and are never read.
        let config: GlobalConfig = serde_json::from_value(serde_json::json!({
            "oauthAccount": { "organizationUuid": "org-1" },
            "passesEligibilityCache": {
                "org-1": {
                    "eligible": true,
                    "timestamp": 1_000,
                    "remaining_passes": 2,
                    "referrer_reward": { "currency": "USD", "amount_minor_units": 500 }
                }
            }
        }))
        .unwrap();
        // Guest passes are a claude.ai Max benefit. A leftover credentials file
        // naming a Max subscription must not bring them back.
        let dir =
            std::env::temp_dir().join(format!("nyan-referral-cache-{}", uuid::Uuid::new_v4()));
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
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        let _oauth = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "ambient-oauth-token");

        let eligibility = check_cached_passes_eligibility(&config, 2_000);

        let _ = std::fs::remove_dir_all(dir);

        assert_eq!(eligibility, CachedPassesEligibility::default());
        assert_eq!(get_cached_referrer_reward_text(&config), None);
        assert_eq!(get_cached_remaining_passes(&config), None);
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
