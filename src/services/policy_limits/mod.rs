//! Organization-level policy restrictions.
//!
//! Maps to: CC `services/policyLimits/index.ts`.
//!
//! Read-only port: the cache file CC writes is consulted, but fetching,
//! ETag negotiation, and background polling are not ported. Absent cache means
//! fail open, exactly like the official cache-miss path.

/// Maps to: CC `services/policyLimits/index.ts:55` `CACHE_FILENAME`.
const CACHE_FILENAME: &str = "policy-limits.json";

/// Maps to: CC `services/policyLimits/index.ts:502`
/// `ESSENTIAL_TRAFFIC_DENY_ON_MISS`. Without this a cache miss or network
/// timeout would silently re-enable these features for HIPAA orgs.
const ESSENTIAL_TRAFFIC_DENY_ON_MISS: &[&str] = &["allow_product_feedback"];

/// Maps to: CC `services/policyLimits/types.ts:8-12`
/// `PolicyLimitsResponseSchema`. Only blocked policies are present; an absent
/// key is allowed.
#[derive(Debug, serde::Deserialize)]
struct PolicyLimitsResponse {
    restrictions: std::collections::HashMap<String, PolicyRestriction>,
}

#[derive(Debug, serde::Deserialize)]
struct PolicyRestriction {
    allowed: bool,
}

/// Maps to: CC `services/policyLimits/index.ts:119-121` `getCachePath`.
fn cache_path() -> std::path::PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir().join(CACHE_FILENAME)
}

/// Maps to: CC `services/policyLimits/index.ts:392-405`
/// `loadCachedRestrictions` plus the `sessionCache` read in
/// `getRestrictionsFromCache` (:531-549).
fn restrictions_from_cache() -> Option<&'static std::collections::HashMap<String, PolicyRestriction>>
{
    static SESSION_CACHE: std::sync::LazyLock<
        Option<std::collections::HashMap<String, PolicyRestriction>>,
    > = std::sync::LazyLock::new(|| {
        let content = std::fs::read_to_string(cache_path()).ok()?;
        serde_json::from_str::<PolicyLimitsResponse>(&content)
            .ok()
            .map(|response| response.restrictions)
    });

    SESSION_CACHE.as_ref()
}

/// Maps to: CC `services/policyLimits/index.ts:510-526` `isPolicyAllowed`.
/// Unknown, unavailable, or explicitly allowed policies return true; the
/// essential-traffic set fails closed on a cache miss.
pub fn is_policy_allowed(policy: &str) -> bool {
    let Some(restrictions) = restrictions_from_cache() else {
        if crate::utils::privacy_level::is_essential_traffic_only()
            && ESSENTIAL_TRAFFIC_DENY_ON_MISS.contains(&policy)
        {
            return false;
        }
        return true;
    };
    restrictions
        .get(policy)
        .is_none_or(|restriction| restriction.allowed)
}
