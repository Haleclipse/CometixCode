//! Anthropic prompt-cache policy: cache-control objects, the profile TTL, and
//! the sticky cache-TTL usage record the cache warmer reads.
//!
//! @nyan: relocated from `services/api/request.rs` (provider redesign PR 4).
//! Cache markers are an Anthropic wire concept; nothing outside the Anthropic
//! adapter should build them.

use serde::{Deserialize, Serialize};

use crate::utils::settings::nyan::CacheRetention;

pub use crate::utils::api::CacheScope;
use crate::utils::model::model::get_small_fast_model;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheTtlUsage {
    pub ephemeral_1h_input_tokens: u64,
    pub ephemeral_5m_input_tokens: u64,
}

impl CacheTtlUsage {
    pub fn is_known(self) -> bool {
        self.ephemeral_1h_input_tokens > 0 || self.ephemeral_5m_input_tokens > 0
    }
}

/// Last cache TTL breakdown advertised by a main-thread request, keyed by the
/// model it belongs to: prompt caches are per-model, so a model change must not
/// inherit the previous model's TTL. The value is sticky because Anthropic only
/// reports the `ephemeral_*` split on requests that write cache; a later
/// all-hits turn reports the cache read but no breakdown, and the cache that
/// turn read was written with the recorded TTL.
static LAST_CACHE_TTL_USAGE: std::sync::LazyLock<
    std::sync::Mutex<Option<(String, CacheTtlUsage)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

pub fn last_cache_ttl_usage_for(model: &str) -> Option<CacheTtlUsage> {
    let canonical = crate::utils::model::model::get_canonical_name(model);
    LAST_CACHE_TTL_USAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .filter(|(recorded_model, ttl)| *recorded_model == canonical && ttl.is_known())
        .map(|(_, ttl)| *ttl)
}

pub(crate) fn record_cache_ttl_usage<T: serde::Serialize>(
    query_source: &str,
    model: &str,
    usage: &T,
) {
    // Only the main thread's own requests describe the cache the warmer will
    // refresh; sub-agents and forks must not overwrite it.
    if !matches!(query_source, "repl_main_thread" | "sdk") {
        return;
    }
    let Ok(raw) = serde_json::to_value(usage) else {
        return;
    };
    let Some(cache_creation) = raw.get("cache_creation").filter(|value| value.is_object()) else {
        return;
    };
    let field = |name: &str| -> u64 {
        cache_creation
            .get(name)
            .and_then(serde_json::Value::as_i64)
            .map_or(0, |value| value.max(0) as u64)
    };
    let ttl = CacheTtlUsage {
        ephemeral_1h_input_tokens: field("ephemeral_1h_input_tokens"),
        ephemeral_5m_input_tokens: field("ephemeral_5m_input_tokens"),
    };
    if !ttl.is_known() {
        return;
    }
    *LAST_CACHE_TTL_USAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some((crate::utils::model::model::get_canonical_name(model), ttl));
}

/// Maps to: CC services/api/logging.ts:GlobalCacheStrategy
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalCacheStrategy {
    None,
    SystemPrompt,
}

// ---------------------------------------------------------------------------
// Stream event types

/// Maps to: CC services/api/claude.ts:358-374 cache-control object shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<CacheScope>,
}

/// Maps to: CC services/api/claude.ts:358-374 `getCacheControl({scope, querySource})`.
///
/// `scope` is only emitted when it is `global` (`...(scope === 'global' && { scope })`);
/// `org` is the API default and stays off the wire.
///
/// @nyan: the TTL comes only from the profile's `cacheRetention` (default
/// `long`), not CC's `should1hCacheTTL` subscriber/allowlist gate — the
/// session profile's, since that is where the main loop's requests go.
pub fn get_cache_control(scope: Option<CacheScope>) -> CacheControl {
    let retention = super::target::current_request_target(
        crate::bootstrap::state::get_session_profile().as_deref(),
    )
    .map(|target| target.cache_retention)
    .unwrap_or_default();
    get_cache_control_with_retention(scope, retention)
}

/// [`get_cache_control`] with the TTL of the profile the request goes through.
pub fn get_cache_control_with_retention(
    scope: Option<CacheScope>,
    retention: CacheRetention,
) -> CacheControl {
    let ttl = match retention {
        CacheRetention::Long => Some("1h".to_string()),
        CacheRetention::None => None,
        // @nyan: explicit "5m" rather than CC's unmarked default, so gateways
        // that upgrade unmarked breakpoints to 1h keep the requested TTL.
        CacheRetention::Short => Some("5m".to_string()),
    };

    CacheControl {
        cache_type: "ephemeral".to_string(),
        ttl,
        scope: (scope == Some(CacheScope::Global)).then_some(CacheScope::Global),
    }
}

// ---------------------------------------------------------------------------
// Task budget param

/// Maps to: CC services/api/claude.ts:333-356
///
/// Returns whether prompt caching is enabled for the given model.
/// Respects DISABLE_PROMPT_CACHING and DISABLE_PROMPT_CACHING_HAIKU.
/// @nyan: CC's `_SONNET` / `_OPUS` switches named the Anthropic default
/// models, which nyan no longer has.
pub fn get_prompt_caching_enabled(model: &str) -> bool {
    // Global disable takes precedence
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING").as_deref(),
    ) {
        return false;
    }

    // Check if we should disable for small/fast model
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING_HAIKU").as_deref(),
    ) {
        let small_fast_model = get_small_fast_model();
        if model == small_fast_model {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// getCacheControl (already defined above)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// configureEffortParams

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn prompt_caching_enabled_by_default() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("DISABLE_PROMPT_CACHING");
        assert!(get_prompt_caching_enabled("claude-sonnet-4-20250514"));
    }

    #[test]
    fn prompt_caching_disabled_globally() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("DISABLE_PROMPT_CACHING", "1");
        assert!(!get_prompt_caching_enabled("claude-sonnet-4-20250514"));
        crate::utils::process_env::remove("DISABLE_PROMPT_CACHING");
    }

    // -- get_cache_control --

    #[test]
    fn cache_control_default_is_ephemeral() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let cc = get_cache_control(None);
        assert_eq!(cc.cache_type, "ephemeral");
        // Default retention is long: explicit 1h on every breakpoint.
        assert_eq!(cc.ttl.as_deref(), Some("1h"));
        assert!(cc.scope.is_none());
    }

    #[test]
    fn cache_control_global_scope() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let cc = get_cache_control(Some(CacheScope::Global));
        assert_eq!(cc.scope, Some(CacheScope::Global));
        assert_eq!(
            serde_json::to_value(&cc).unwrap()["scope"],
            serde_json::json!("global")
        );
        // CC `...(scope === 'global' && { scope })`: org is the API default
        // and never goes on the wire.
        assert!(get_cache_control(Some(CacheScope::Org)).scope.is_none());
    }

    #[test]
    fn cache_control_with_retention_honors_explicit_retention() {
        let cc_long = get_cache_control_with_retention(None, CacheRetention::Long);
        assert_eq!(cc_long.cache_type, "ephemeral");
        assert_eq!(cc_long.ttl.as_deref(), Some("1h"));

        let cc_short = get_cache_control_with_retention(None, CacheRetention::Short);
        assert_eq!(cc_short.cache_type, "ephemeral");
        assert_eq!(cc_short.ttl.as_deref(), Some("5m"));

        let cc_none = get_cache_control_with_retention(None, CacheRetention::None);
        assert!(cc_none.ttl.is_none());
    }

    // -- configure_effort_params --
}
