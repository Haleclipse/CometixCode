//! Maps to: CC `services/api/overageCreditGrant.ts` cached grant helpers.
//!
//! Official code can fetch `/overage_credit_grant` and update
//! `GlobalConfig.overageCreditGrantCache`. Nyan keeps this slice read-only:
//! it only consumes existing cache entries and never performs OAuth/network
//! calls, invalidation writes, or analytics.

use crate::components::logo_v2::overage_credit_upsell::OverageCreditGrantInfo as UiGrantInfo;
use crate::utils::config::GlobalConfig;
use serde::Deserialize;

pub const CACHE_TTL_MS: i64 = 60 * 60 * 1000;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OverageCreditGrantInfo {
    pub available: bool,
    pub eligible: bool,
    pub granted: bool,
    pub amount_minor_units: Option<i64>,
    pub currency: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OverageCreditGrantCacheEntry {
    pub info: OverageCreditGrantInfo,
    pub timestamp: i64,
}

/// Maps to: CC `services/api/overageCreditGrant.ts` `getCachedOverageCreditGrant`.
///
/// Overage credit grants belong to a claude.ai OAuth account. There is no such
/// account, so a leftover `oauthAccount` or cache entry is never read.
pub fn get_cached_overage_credit_grant_readonly(
    _config: &GlobalConfig,
    _now_ms: i64,
) -> Option<OverageCreditGrantInfo> {
    None
}

/// Maps to: CC `services/api/overageCreditGrant.ts` `formatGrantAmount`.
pub fn format_grant_amount(info: &OverageCreditGrantInfo) -> Option<String> {
    let amount_minor_units = info.amount_minor_units?;
    let currency = info.currency.as_deref()?;
    if !currency.eq_ignore_ascii_case("USD") {
        return None;
    }
    let dollars = amount_minor_units as f64 / 100.0;
    if dollars.fract() == 0.0 {
        Some(format!("${}", dollars as i64))
    } else {
        Some(format!("${dollars:.2}"))
    }
}

pub fn ui_grant_info_from_cached_readonly(
    config: &GlobalConfig,
    now_ms: i64,
) -> Option<UiGrantInfo> {
    let info = get_cached_overage_credit_grant_readonly(config, now_ms)?;
    let amount = format_grant_amount(&info);
    Some(UiGrantInfo {
        available: info.available,
        granted: info.granted,
        amount,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::{AccountInfo, GlobalConfig};

    fn config_with_cache(entry: serde_json::Value) -> GlobalConfig {
        GlobalConfig {
            oauth_account: Some(AccountInfo {
                organization_uuid: Some("org-1".to_string()),
                ..Default::default()
            }),
            overage_credit_grant_cache: Some(serde_json::json!({ "org-1": entry })),
            ..Default::default()
        }
    }

    #[test]
    fn format_grant_amount_matches_official_usd_only_shape() {
        assert_eq!(
            format_grant_amount(&OverageCreditGrantInfo {
                amount_minor_units: Some(1_000),
                currency: Some("USD".to_string()),
                ..Default::default()
            })
            .as_deref(),
            Some("$10")
        );
        assert_eq!(
            format_grant_amount(&OverageCreditGrantInfo {
                amount_minor_units: Some(1_025),
                currency: Some("usd".to_string()),
                ..Default::default()
            })
            .as_deref(),
            Some("$10.25")
        );
        assert!(
            format_grant_amount(&OverageCreditGrantInfo {
                amount_minor_units: Some(1_000),
                currency: Some("EUR".to_string()),
                ..Default::default()
            })
            .is_none()
        );
    }

    #[test]
    fn cached_overage_grant_is_never_read_without_a_claude_ai_account() {
        // A populated cache for the org of a leftover `oauthAccount` is ignored:
        // there is no claude.ai OAuth account, so no grant is ever surfaced.
        let config = config_with_cache(serde_json::json!({
            "timestamp": 1_000,
            "info": {
                "available": true,
                "eligible": true,
                "granted": false,
                "amount_minor_units": 1_000,
                "currency": "USD"
            }
        }));

        assert!(get_cached_overage_credit_grant_readonly(&config, 2_000).is_none());
        assert!(ui_grant_info_from_cached_readonly(&config, 2_000).is_none());
    }
}
