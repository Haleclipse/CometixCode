//! Anthropic prompt-cache policy: cache-control objects, the 1h TTL gate, and
//! the sticky cache-TTL usage record the cache warmer reads.
//!
//! @nyan: relocated from `services/api/claude.rs` (provider redesign PR 4).
//! Cache markers are an Anthropic wire concept; nothing outside the Anthropic
//! adapter should build them.

use serde::{Deserialize, Serialize};

pub use crate::utils::api::CacheScope;
use crate::utils::model::model::{
    get_default_opus_model, get_default_sonnet_model, get_small_fast_model,
};

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
/// @nyan offset: first-party `ttl: "1h"` is gated by `PromptCache1hConfig`.
/// The query-source allowlist is settings.json `promptCache1h.allowlist`.
/// Vertex / Foundry / Bedrock keep `should1hCacheTTL`.
pub fn get_cache_control(scope: Option<CacheScope>, query_source: Option<&str>) -> CacheControl {
    get_cache_control_with_retention(scope, query_source, None)
}

/// Computes cache-control with an explicit or profile-resolved cache retention TTL.
pub fn get_cache_control_with_retention(
    scope: Option<CacheScope>,
    query_source: Option<&str>,
    retention: Option<crate::utils::settings::nyan::CacheRetention>,
) -> CacheControl {
    let effective_retention = retention
        .or_else(|| {
            super::target::current_main_target()
                .ok()
                .map(|t| t.cache_retention)
        })
        .unwrap_or_default();

    let ttl = match effective_retention {
        crate::utils::settings::nyan::CacheRetention::Long => Some("1h".to_string()),
        crate::utils::settings::nyan::CacheRetention::None => None,
        crate::utils::settings::nyan::CacheRetention::Short => {
            should_1h_cache_ttl(query_source).then(|| "1h".to_string())
        }
    };

    CacheControl {
        cache_type: "ephemeral".to_string(),
        ttl,
        scope: (scope == Some(CacheScope::Global)).then_some(CacheScope::Global),
    }
}

/// Returns whether cache retention allows prompt caching (i.e. not `CacheRetention::None`).
pub fn is_cache_retention_enabled(
    retention: Option<crate::utils::settings::nyan::CacheRetention>,
) -> bool {
    let effective = retention
        .or_else(|| {
            super::target::current_main_target()
                .ok()
                .map(|t| t.cache_retention)
        })
        .unwrap_or_default();
    effective != crate::utils::settings::nyan::CacheRetention::None
}

fn should_1h_cache_ttl(query_source: Option<&str>) -> bool {
    // Maps to CC `services/api/claude.ts:393-434` `should1hCacheTTL(...)`.
    let provider = crate::utils::model::providers::get_api_provider();
    let bedrock_opt_in = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("ENABLE_PROMPT_CACHING_1H_BEDROCK").as_deref(),
    );
    should_1h_cache_ttl_with_inputs(
        provider,
        bedrock_opt_in,
        prompt_cache_1h_user_eligible(),
        &prompt_cache_1h_allowlist(),
        query_source,
        crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::PromptCache1hConfig,
        ),
    )
}

/// Maps to CC `should1hCacheTTL` eligibility:
/// `USER_TYPE === 'ant' || (isClaudeAISubscriber() && !currentLimits.isUsingOverage)`.
/// There is no claude.ai subscriber, so only the internal audience qualifies.
fn prompt_cache_1h_user_eligible() -> bool {
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    )
}

/// Maps to CC `getFeatureValue_CACHED_MAY_BE_STALE('tengu_prompt_cache_1h_config', {})`
/// then `config.allowlist ?? []`.
///
/// @nyan offset: GrowthBook is not ported; the same `{ allowlist }` payload
/// is read from settings.json via `get_prompt_cache_1h_allowlist`.
fn prompt_cache_1h_allowlist() -> Vec<String> {
    // @nyan offset: official reads GrowthBook `tengu_prompt_cache_1h_config`.
    // Users set the same `{ allowlist }` payload in settings.json.
    crate::utils::settings::get_prompt_cache_1h_allowlist()
}

fn prompt_cache_1h_allowlist_from_feature_value(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.get("allowlist"))
        .and_then(|value| value.as_array())
        .map(|allowlist| {
            allowlist
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn prompt_cache_1h_allowlist_matches(allowlist: &[String], query_source: &str) -> bool {
    allowlist.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            query_source.starts_with(prefix)
        } else {
            query_source == pattern
        }
    })
}

pub(crate) fn should_1h_cache_ttl_with_inputs(
    provider: crate::utils::model::providers::ApiProvider,
    bedrock_opt_in: bool,
    user_eligible: bool,
    allowlist: &[String],
    query_source: Option<&str>,
    first_party_config_enabled: bool,
) -> bool {
    use crate::utils::model::providers::ApiProvider;

    // Official: Bedrock users opt in via env and skip GrowthBook.
    if provider == ApiProvider::Bedrock && bedrock_opt_in {
        return true;
    }

    if provider != ApiProvider::FirstParty {
        // Official remaining path for Vertex / Foundry / Bedrock-without-opt-in:
        // eligible user + `tengu_prompt_cache_1h_config.allowlist` match.
        if !user_eligible {
            return false;
        }
        let Some(query_source) = query_source else {
            return false;
        };
        return prompt_cache_1h_allowlist_matches(allowlist, query_source);
    }

    // @nyan offset: first-party uses `PromptCache1hConfig` as the final
    // control. An empty settings allowlist means "any querySource"; a
    // non-empty list uses the official prefix-match rules.
    if !first_party_config_enabled {
        return false;
    }
    let Some(query_source) = query_source else {
        return false;
    };
    if allowlist.is_empty() {
        return true;
    }
    prompt_cache_1h_allowlist_matches(allowlist, query_source)
}

// ---------------------------------------------------------------------------
// Task budget param

/// Maps to: CC services/api/claude.ts:333-356
///
/// Returns whether prompt caching is enabled for the given model.
/// Respects DISABLE_PROMPT_CACHING, DISABLE_PROMPT_CACHING_HAIKU,
/// DISABLE_PROMPT_CACHING_SONNET, and DISABLE_PROMPT_CACHING_OPUS env vars.
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

    // Check if we should disable for default Sonnet
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING_SONNET").as_deref(),
    ) {
        let default_sonnet = get_default_sonnet_model();
        if model == default_sonnet {
            return false;
        }
    }

    // Check if we should disable for default Opus
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING_OPUS").as_deref(),
    ) {
        let default_opus = get_default_opus_model();
        if model == default_opus {
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
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
        let cc = get_cache_control(None, None);
        assert_eq!(cc.cache_type, "ephemeral");
        assert!(cc.ttl.is_none());
        assert!(cc.scope.is_none());
    }

    #[test]
    fn cache_control_global_scope() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
        let cc = get_cache_control(Some(CacheScope::Global), None);
        assert_eq!(cc.scope, Some(CacheScope::Global));
        assert_eq!(
            serde_json::to_value(&cc).unwrap()["scope"],
            serde_json::json!("global")
        );
        // CC `...(scope === 'global' && { scope })`: org is the API default
        // and never goes on the wire.
        assert!(
            get_cache_control(Some(CacheScope::Org), None)
                .scope
                .is_none()
        );
    }

    #[test]
    fn cache_control_with_retention_honors_explicit_retention() {
        use crate::utils::settings::nyan::CacheRetention;

        let cc_long = get_cache_control_with_retention(None, None, Some(CacheRetention::Long));
        assert_eq!(cc_long.cache_type, "ephemeral");
        assert_eq!(cc_long.ttl.as_deref(), Some("1h"));

        let cc_short = get_cache_control_with_retention(None, None, Some(CacheRetention::Short));
        assert_eq!(cc_short.cache_type, "ephemeral");
        assert!(cc_short.ttl.is_none());

        assert!(is_cache_retention_enabled(Some(CacheRetention::Long)));
        assert!(is_cache_retention_enabled(Some(CacheRetention::Short)));
        assert!(!is_cache_retention_enabled(Some(CacheRetention::None)));
    }

    #[test]
    fn cache_control_1h_ttl_matches_official_bedrock_opt_in_and_allowlist_gate() {
        use crate::utils::model::providers::ApiProvider;

        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::Bedrock,
            true,
            false,
            &[],
            None,
            false,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::Bedrock,
            false,
            false,
            &["*".to_string()],
            Some("sdk"),
            false,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::Vertex,
            false,
            false,
            &["sdk".to_string()],
            Some("sdk"),
            true,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::Vertex,
            false,
            true,
            &["repl_main_thread*".to_string(), "sdk".to_string()],
            Some("repl_main_thread"),
            false,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::Foundry,
            false,
            true,
            &["sdk".to_string()],
            Some("sdk"),
            false,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::Foundry,
            false,
            true,
            &["agent:*".to_string()],
            Some("sdk"),
            true,
        ));
        // @nyan offset: first-party uses PromptCache1hConfig as the final
        // control. Empty allowlist + flag on = any querySource; a non-empty
        // list uses the official prefix-match rules.
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            true,
            &["*".to_string()],
            Some("sdk"),
            false,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            false,
            &[],
            Some("repl_main_thread"),
            true,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            true,
            &["*".to_string()],
            None,
            true,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            false,
            &["repl_main_thread*".to_string()],
            Some("repl_main_thread"),
            true,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            false,
            &["repl_main_thread*".to_string()],
            Some("sdk"),
            true,
        ));
    }

    #[test]
    fn cache_control_sets_1h_ttl_for_bedrock_official_env_opt_in() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::set("ENABLE_PROMPT_CACHING_1H_BEDROCK", "1");

        let cc = get_cache_control(None, Some("compact"));
        assert_eq!(cc.ttl.as_deref(), Some("1h"));

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
    }

    #[test]
    fn cache_control_1h_allowlist_parses_official_payload_shape() {
        assert_eq!(
            prompt_cache_1h_allowlist_from_feature_value(Some(&serde_json::json!({
                "allowlist": ["repl_main_thread*", "sdk", 7]
            }))),
            ["repl_main_thread*", "sdk"]
        );
        assert!(
            prompt_cache_1h_allowlist_from_feature_value(Some(&serde_json::json!({}))).is_empty()
        );
        assert!(prompt_cache_1h_allowlist_from_feature_value(None).is_empty());
    }

    #[test]
    fn cache_control_1h_allowlist_ignores_growthbook_delivery() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("nyan-prompt-cache-1h-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp config dir");
        std::fs::write(
            root.join(".claude.json"),
            r#"{
                "cachedGrowthBookFeatures": {
                    "tengu_prompt_cache_1h_config": {
                        "allowlist": ["repl_main_thread*", "sdk"]
                    }
                },
                "growthBookOverrides": {
                    "tengu_prompt_cache_1h_config": {"allowlist": ["agent:*"]}
                }
            }"#,
        )
        .expect("write temp global config");

        crate::utils::process_env::set("CLAUDE_CONFIG_DIR", &root);
        crate::utils::process_env::set(
            "CLAUDE_INTERNAL_FC_OVERRIDES",
            r#"{"tengu_prompt_cache_1h_config":{"allowlist":["sdk"]}}"#,
        );
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");

        crate::utils::settings::settings_cache::reset_settings_cache();
        for source in [
            crate::utils::settings::SettingSource::User,
            crate::utils::settings::SettingSource::Project,
            crate::utils::settings::SettingSource::Local,
            crate::utils::settings::SettingSource::Flag,
            crate::utils::settings::SettingSource::Policy,
        ] {
            crate::utils::settings::settings_cache::set_cached_settings_for_source(source, None);
        }

        // Official fallback `{}` keeps the non-1P allowlist empty. GrowthBook
        // disk/env payloads stay inert. First-party follows PromptCache1hConfig
        // (currently off), so ttl stays omitted here.
        assert!(prompt_cache_1h_allowlist().is_empty());
        assert!(
            get_cache_control(None, Some("repl_main_thread:main"))
                .ttl
                .is_none()
        );
        assert!(get_cache_control(None, Some("sdk")).ttl.is_none());
        assert!(
            get_cache_control(None, Some("agent:general-purpose"))
                .ttl
                .is_none()
        );

        // Official Bedrock env opt-in still drives 1h for that provider.
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::set("ENABLE_PROMPT_CACHING_1H_BEDROCK", "1");
        assert_eq!(
            get_cache_control(None, Some("sdk")).ttl.as_deref(),
            Some("1h")
        );

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CONFIG_DIR");
        crate::utils::process_env::remove("CLAUDE_INTERNAL_FC_OVERRIDES");
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cache_control_1h_allowlist_reads_settings_not_growthbook() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
        struct ResetCache;
        impl Drop for ResetCache {
            fn drop(&mut self) {
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let _reset = ResetCache;
        for source in [
            crate::utils::settings::SettingSource::User,
            crate::utils::settings::SettingSource::Project,
            crate::utils::settings::SettingSource::Local,
            crate::utils::settings::SettingSource::Flag,
            crate::utils::settings::SettingSource::Policy,
        ] {
            crate::utils::settings::settings_cache::set_cached_settings_for_source(source, None);
        }
        crate::utils::settings::settings_cache::set_cached_settings_for_source(
            crate::utils::settings::SettingSource::User,
            Some(crate::utils::settings::types::SettingsJson {
                prompt_cache_1h: Some(crate::utils::settings::types::PromptCache1hSettings {
                    allowlist: Some(vec!["sdk".into(), "repl_main_thread*".into()]),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            prompt_cache_1h_allowlist(),
            vec!["sdk".to_string(), "repl_main_thread*".to_string()]
        );
    }

    // -- configure_effort_params --
}
