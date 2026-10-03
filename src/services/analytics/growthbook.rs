//! Cached GrowthBook feature reads.
//!
//! Maps to: CC `services/analytics/growthbook.ts`. Network initialization and
//! exposure logging are owned by the future analytics runtime; this owner keeps
//! the synchronous override/disk-cache behavior.
//!
//! Agent `runAgent` reads the source-defined `tengu_slim_subagent_claudemd`
//! kill switch through this cached reader. Other gates still use the
//! source-controlled switch table in `utils/feature_flags.rs`; this does not
//! restore remote delivery, refresh, or exposure registration.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::utils::process_env::JsTruthy;

#[derive(Default)]
struct EnvOverridesCache {
    parsed: bool,
    overrides: Option<Map<String, Value>>,
}

static ENV_OVERRIDES: LazyLock<Mutex<EnvOverridesCache>> =
    LazyLock::new(|| Mutex::new(EnvOverridesCache::default()));

/// Maps to: CC `services/analytics/growthbook.ts:159-192` `getEnvOverrides`.
///
/// The environment object is parsed once and remains frozen until
/// `reset_growth_book`, while config overrides remain live on every read.
fn get_env_overrides() -> Option<Map<String, Value>> {
    let mut cache = ENV_OVERRIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cache.parsed {
        cache.parsed = true;
        if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Api,
        ) {
            cache.overrides = crate::utils::process_env::var("CLAUDE_INTERNAL_FC_OVERRIDES")
                .truthy()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| value.as_object().cloned());
        }
    }
    cache.overrides.clone()
}

/// Maps to: CC `services/analytics/growthbook.ts:211-220`
/// `getConfigOverrides`.
fn get_config_overrides() -> Option<HashMap<String, Value>> {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    ) {
        return None;
    }
    crate::utils::config::load_global_config().growth_book_overrides
}

/// Rust projection of JavaScript `Boolean(value)` used by the Statsig
/// migration helper at CC `services/analytics/growthbook.ts:809-814,829-835`.
fn javascript_boolean(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Maps to: CC `services/analytics/growthbook.ts:734-775`
/// `getFeatureValue_CACHED_MAY_BE_STALE`.
pub fn get_feature_value_cached_may_be_stale<T>(feature: &str, default_value: T) -> T
where
    T: Clone + DeserializeOwned,
{
    if let Some(value) = get_env_overrides().and_then(|overrides| overrides.get(feature).cloned()) {
        if let Ok(value) = serde_json::from_value(value) {
            return value;
        }
    }
    if let Some(value) =
        get_config_overrides().and_then(|overrides| overrides.get(feature).cloned())
    {
        if let Ok(value) = serde_json::from_value(value) {
            return value;
        }
    }

    if crate::services::analytics::config::is_analytics_disabled() {
        return default_value;
    }

    crate::utils::config::load_global_config()
        .cached_growth_book_features
        .as_ref()
        .and_then(|features| features.get(feature))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or(default_value)
}

/// Maps to: CC `services/analytics/growthbook.ts:792-837`
/// `checkStatsigFeatureGate_CACHED_MAY_BE_STALE`.
///
/// GrowthBook networking and exposure registration remain absent analytics
/// seams. Locally observable override, analytics-disable, GrowthBook-cache, and
/// Statsig-fallback precedence is preserved exactly.
pub fn check_statsig_feature_gate_cached_may_be_stale(gate: &str) -> bool {
    if let Some(value) = get_env_overrides().and_then(|overrides| overrides.get(gate).cloned()) {
        return javascript_boolean(&value);
    }
    if let Some(value) = get_config_overrides().and_then(|overrides| overrides.get(gate).cloned()) {
        return javascript_boolean(&value);
    }

    if crate::services::analytics::config::is_analytics_disabled() {
        return false;
    }

    let config = crate::utils::config::load_global_config();
    if let Some(value) = config
        .cached_growth_book_features
        .as_ref()
        .and_then(|features| features.get(gate))
    {
        return javascript_boolean(value);
    }
    config
        .cached_statsig_gates
        .as_ref()
        .and_then(|gates| gates.get(gate))
        .copied()
        .unwrap_or(false)
}

/// Maps to: CC `services/analytics/growthbook.ts:851-894`.
/// Unlike the synchronous migration reader, security restrictions prefer the
/// Statsig cache. The shared remote reinitialization runtime remains unavailable.
pub async fn check_security_restriction_gate(gate: &str) -> bool {
    if let Some(value) = get_env_overrides().and_then(|v| v.get(gate).cloned()) {
        return javascript_boolean(&value);
    }
    if let Some(value) = get_config_overrides().and_then(|v| v.get(gate).cloned()) {
        return javascript_boolean(&value);
    }
    if crate::services::analytics::config::is_analytics_disabled() {
        return false;
    }
    let config = crate::utils::config::load_global_config();
    if let Some(value) = config
        .cached_statsig_gates
        .as_ref()
        .and_then(|v| v.get(gate))
    {
        return *value;
    }
    config
        .cached_growth_book_features
        .as_ref()
        .and_then(|v| v.get(gate))
        .is_some_and(javascript_boolean)
}

/// Maps to: CC `growthbook.ts:1136-1141` and `getFeatureValueInternal:672-693`.
/// Explicit no-client seam: overrides precede the source's null-client default.
/// Disk cache is NOT a fresh initialized response. Remote initialization and
/// exposure delivery are still missing from the shared analytics runtime.
pub async fn get_dynamic_config_blocks_on_init<T>(name: &str, default: T) -> T
where
    T: Clone + DeserializeOwned,
{
    if let Some(value) = get_env_overrides().and_then(|v| v.get(name).cloned()) {
        return serde_json::from_value(value).unwrap_or(default);
    }
    if let Some(value) = get_config_overrides().and_then(|v| v.get(name).cloned()) {
        return serde_json::from_value(value).unwrap_or(default);
    }
    default
}

/// Maps to: CC `services/analytics/growthbook.ts:1005-1027` `resetGrowthBook`.
///
/// Cometix has no GrowthBook client/network/exposure runtime to destroy. This
/// source-shaped reset clears the locally implemented parse-once override state.
pub fn reset_growth_book() {
    *ENV_OVERRIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = EnvOverridesCache::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use std::collections::HashMap;

    fn reset_test_state() {
        reset_growth_book();
        crate::utils::config::set_test_global_config(None);
        for key in [
            "CLAUDE_INTERNAL_FC_OVERRIDES",
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ] {
            crate::utils::process_env::remove(key);
        }
    }

    fn cached_config(
        growth_book: Option<Value>,
        statsig: Option<bool>,
    ) -> crate::utils::config::GlobalConfig {
        let mut config = crate::utils::config::GlobalConfig::default();
        if let Some(value) = growth_book {
            config.cached_growth_book_features = Some(HashMap::from([("gate".to_string(), value)]));
        }
        if let Some(value) = statsig {
            config.cached_statsig_gates = Some(HashMap::from([("gate".to_string(), value)]));
        }
        config
    }

    #[test]
    fn statsig_migration_cache_precedence_matches_official_growthbook_then_statsig() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_test_state();

        crate::utils::config::set_test_global_config(Some(cached_config(None, None)));
        assert!(!check_statsig_feature_gate_cached_may_be_stale("gate"));

        crate::utils::config::set_test_global_config(Some(cached_config(
            Some(Value::Bool(true)),
            Some(false),
        )));
        assert!(check_statsig_feature_gate_cached_may_be_stale("gate"));
        crate::utils::config::set_test_global_config(Some(cached_config(
            Some(Value::Bool(false)),
            Some(true),
        )));
        assert!(!check_statsig_feature_gate_cached_may_be_stale("gate"));

        crate::utils::config::set_test_global_config(Some(cached_config(None, Some(true))));
        assert!(check_statsig_feature_gate_cached_may_be_stale("gate"));
        crate::utils::config::set_test_global_config(Some(cached_config(None, Some(false))));
        assert!(!check_statsig_feature_gate_cached_may_be_stale("gate"));
        reset_test_state();
    }

    #[test]
    fn statsig_migration_boolean_coercion_matches_official_javascript_boolean() {
        for (value, expected) in [
            (serde_json::json!(false), false),
            (serde_json::json!(true), true),
            (serde_json::json!(0), false),
            (serde_json::json!(1), true),
            (serde_json::json!(""), false),
            (serde_json::json!("false"), true),
            (Value::Null, false),
            (serde_json::json!([]), true),
            (serde_json::json!({}), true),
        ] {
            assert_eq!(javascript_boolean(&value), expected, "value={value}");
        }
    }

    #[test]
    fn statsig_migration_analytics_disable_precedes_disk_like_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_test_state();
        crate::utils::config::set_test_global_config(Some(cached_config(
            Some(Value::Bool(true)),
            Some(true),
        )));

        for (key, value) in [
            ("NODE_ENV", "test"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CLAUDE_CODE_USE_VERTEX", "1"),
            ("CLAUDE_CODE_USE_FOUNDRY", "1"),
            ("DISABLE_TELEMETRY", "1"),
        ] {
            crate::utils::process_env::set(key, value);
            assert!(
                !check_statsig_feature_gate_cached_may_be_stale("gate"),
                "disable={key}"
            );
            crate::utils::process_env::remove(key);
        }
        reset_test_state();
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn statsig_migration_overrides_and_env_reset_lifecycle_match_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_test_state();
        let mut config = cached_config(Some(Value::Bool(false)), Some(false));
        config.growth_book_overrides = Some(HashMap::from([(
            "gate".to_string(),
            Value::String("config".to_string()),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        crate::utils::process_env::set("NODE_ENV", "test");
        crate::utils::process_env::set("CLAUDE_INTERNAL_FC_OVERRIDES", r#"{"gate":0}"#);
        assert!(!check_statsig_feature_gate_cached_may_be_stale("gate"));

        crate::utils::process_env::set("CLAUDE_INTERNAL_FC_OVERRIDES", r#"{"gate":1}"#);
        assert!(!check_statsig_feature_gate_cached_may_be_stale("gate"));
        reset_growth_book();
        assert!(check_statsig_feature_gate_cached_may_be_stale("gate"));

        crate::utils::process_env::set("CLAUDE_INTERNAL_FC_OVERRIDES", "not-json");
        reset_growth_book();
        assert!(check_statsig_feature_gate_cached_may_be_stale("gate"));

        let mut updated = cached_config(Some(Value::Bool(true)), Some(true));
        updated.growth_book_overrides = Some(HashMap::from([(
            "gate".to_string(),
            Value::String(String::new()),
        )]));
        crate::utils::config::set_test_global_config(Some(updated));
        assert!(!check_statsig_feature_gate_cached_may_be_stale("gate"));
        reset_test_state();
    }
    #[tokio::test]
    async fn security_gate_matches_official_statsig_first_and_dynamic_no_client_default() {
        // growthbook.ts:851-894 vs :672-693: security fallback is not cached dynamic config.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_test_state();
        crate::utils::config::set_test_global_config(Some(cached_config(
            Some(Value::Bool(true)),
            Some(false),
        )));
        assert!(!check_security_restriction_gate("gate").await);
        assert_eq!(
            get_dynamic_config_blocks_on_init("gate", Value::Null).await,
            Value::Null
        );
        crate::utils::config::set_test_global_config(Some(cached_config(
            Some(Value::Bool(false)),
            Some(true),
        )));
        assert!(check_security_restriction_gate("gate").await);
        crate::utils::process_env::set("DISABLE_TELEMETRY", "1");
        assert!(!check_security_restriction_gate("gate").await);
        reset_test_state();
    }
}
