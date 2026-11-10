//! Beta-header helpers.
//! Maps to CC `utils/betas.ts`.

pub use crate::constants::betas::{
    ADVISOR_BETA_HEADER, CLAUDE_CODE_20250219_BETA_HEADER, CONTEXT_1M_BETA_HEADER,
    CONTEXT_MANAGEMENT_BETA_HEADER, EFFORT_BETA_HEADER, INTERLEAVED_THINKING_BETA_HEADER,
    PROMPT_CACHING_SCOPE_BETA_HEADER, REDACT_THINKING_BETA_HEADER, STRUCTURED_OUTPUTS_BETA_HEADER,
    TASK_BUDGETS_BETA_HEADER, TOOL_SEARCH_BETA_HEADER_1P,
};
use crate::utils::model::model::get_canonical_name;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

fn canonical_model(model: &str) -> String {
    get_canonical_name(model)
}

fn is_haiku(model: &str) -> bool {
    canonical_model(model).contains("haiku")
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoModeConfig {
    #[serde(default)]
    allow_models: Vec<String>,
}

/// Parameterized core of CC `utils/betas.ts:160-193#modelSupportsAutoMode`.
/// Rust's public owner reads the feature/cache globals and delegates here so
/// the gate has a deterministic test seam; this helper carries no separate
/// policy or state.
fn model_supports_auto_mode_for(
    model: &str,
    transcript_classifier_enabled: bool,
    allow_models: &[String],
) -> bool {
    if !transcript_classifier_enabled {
        return false;
    }

    let canonical = get_canonical_name(model);
    let raw_lower = model.to_ascii_lowercase();
    if allow_models.iter().any(|allowed| {
        let allowed = allowed.to_ascii_lowercase();
        allowed == raw_lower || allowed == canonical
    }) {
        return true;
    }

    canonical.starts_with("claude-opus-4-6") || canonical.starts_with("claude-sonnet-4-6")
}

/// Maps to: CC `utils/betas.ts:160-193#modelSupportsAutoMode`.
pub fn model_supports_auto_mode(model: &str) -> bool {
    let transcript_classifier_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
    );
    let auto_mode_config_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::AutoModeConfig,
    );
    let allow_models = if transcript_classifier_enabled && auto_mode_config_enabled {
        AutoModeConfig::default().allow_models
    } else {
        Default::default()
    };
    model_supports_auto_mode_for(model, transcript_classifier_enabled, &allow_models)
}

fn has_1m_context(model: &str) -> bool {
    !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_DISABLE_1M_CONTEXT").as_deref(),
    ) && model.to_ascii_lowercase().contains("[1m]")
}

/// Maps to CC `utils/betas.ts:92-111` `modelSupportsISP(...)`.
pub fn model_supports_isp(model: &str) -> bool {
    let canonical = canonical_model(model);
    !canonical.contains("claude-3-")
}

/// Maps to CC `utils/betas.ts` `modelSupportsContextManagement(...)`.
pub fn model_supports_context_management(model: &str) -> bool {
    let canonical = canonical_model(model);
    !canonical.contains("claude-3-")
}

/// Maps to CC `utils/betas.ts` `shouldIncludeFirstPartyOnlyBetas(...)`.
pub fn should_include_first_party_only_betas() -> bool {
    !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_DISABLE_EXPERIMENTAL_BETAS").as_deref(),
    )
}

/// Maps to CC `utils/betas.ts` `shouldUseGlobalCacheScope(...)`.
pub fn should_use_global_cache_scope() -> bool {
    !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_DISABLE_EXPERIMENTAL_BETAS").as_deref(),
    )
}

/// Maps to CC `utils/betas.ts` `getToolSearchBetaHeader(...)`.
pub fn get_tool_search_beta_header() -> &'static str {
    TOOL_SEARCH_BETA_HEADER_1P
}

/// Maps to CC `utils/betas.ts` `modelSupportsStructuredOutputs(...)`.
pub fn model_supports_structured_outputs(model: &str) -> bool {
    let canonical = canonical_model(model);
    canonical.contains("claude-sonnet-4-6")
        || canonical.contains("claude-sonnet-4-5")
        || canonical.contains("claude-opus-4-1")
        || canonical.contains("claude-opus-4-5")
        || canonical.contains("claude-opus-4-6")
        || canonical.contains("claude-haiku-4-5")
}

static ALL_MODEL_BETAS_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MODEL_BETAS_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maps to: CC `utils/betas.ts:234-369` memoized `getAllModelBetas`.
///
/// The memo key is the exact raw model string. Provider, gate, kill switch,
/// environment betas, and canonical model identity are deliberately absent.
pub fn get_all_model_betas(model: &str) -> Vec<String> {
    let mut cache = ALL_MODEL_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(betas) = cache.get(model) {
        return betas.clone();
    }

    let mut beta_headers = Vec::new();
    let include_first_party_only_betas = should_include_first_party_only_betas();

    if !is_haiku(model) {
        beta_headers.push(CLAUDE_CODE_20250219_BETA_HEADER.to_string());
    }

    if has_1m_context(model) {
        beta_headers.push(CONTEXT_1M_BETA_HEADER.to_string());
    }

    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_INTERLEAVED_THINKING").as_deref(),
    ) && model_supports_isp(model)
    {
        beta_headers.push(INTERLEAVED_THINKING_BETA_HEADER.to_string());
    }

    // Maps to CC `utils/betas.ts:264-277`: skip the API-side Haiku thinking
    // summarizer in interactive sessions only. SDK / print-mode keep summaries
    // because callers may iterate over thinking content; users opt back in via
    // settings.json `showThinkingSummaries`. The session gate is
    // `getIsNonInteractiveSession()` (bootstrap state), not an env var.
    if include_first_party_only_betas
        && model_supports_isp(model)
        && !crate::bootstrap::state::get_is_non_interactive_session()
        && crate::utils::settings::get_initial_settings().show_thinking_summaries != Some(true)
    {
        beta_headers.push(REDACT_THINKING_BETA_HEADER.to_string());
    }

    if include_first_party_only_betas && model_supports_context_management(model) {
        beta_headers.push(CONTEXT_MANAGEMENT_BETA_HEADER.to_string());
    }

    let strict_tools_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::StrictToolSchemas,
    );
    if include_first_party_only_betas
        && model_supports_structured_outputs(model)
        && strict_tools_enabled
    {
        beta_headers.push(STRUCTURED_OUTPUTS_BETA_HEADER.to_string());
    }

    if include_first_party_only_betas {
        beta_headers.push(PROMPT_CACHING_SCOPE_BETA_HEADER.to_string());
    }

    cache.insert(model.to_string(), beta_headers.clone());
    beta_headers
}

/// Maps to: CC `utils/betas.ts:371-377` memoized `getModelBetas`.
pub fn get_model_betas(model: &str) -> Vec<String> {
    if let Some(betas) = MODEL_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(model)
        .cloned()
    {
        return betas;
    }
    let beta_headers = get_all_model_betas(model);
    MODEL_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(model.to_string(), beta_headers.clone());
    beta_headers
}

/// Maps to: CC `utils/betas.ts:397-428` `getMergedBetas`.
pub fn get_merged_betas(model: &str, is_agentic_query: bool) -> Vec<String> {
    let mut beta_headers = get_model_betas(model);
    if is_agentic_query
        && !beta_headers
            .iter()
            .any(|beta| beta == CLAUDE_CODE_20250219_BETA_HEADER)
    {
        beta_headers.push(CLAUDE_CODE_20250219_BETA_HEADER.to_string());
    }
    beta_headers
}

/// Maps to: CC `utils/betas.ts:430-434` `clearBetasCaches`.
pub fn clear_betas_caches() {
    ALL_MODEL_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    MODEL_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn auto_mode_model_gate_matches_official_allowlist_order() {
        assert!(!model_supports_auto_mode_for(
            "claude-sonnet-4-6",
            false,
            &["claude-sonnet-4-6".to_string()],
        ));
        for model in ["claude-opus-4-6", "claude-sonnet-4-6"] {
            assert!(model_supports_auto_mode_for(model, true, &[]));
        }
        assert!(!model_supports_auto_mode_for("claude-haiku-4-5", true, &[]));
        assert!(!model_supports_auto_mode_for("claude-opus-4-5", true, &[]));
        assert!(model_supports_auto_mode_for(
            "CLAUDE-HAIKU-4-5",
            true,
            &["claude-haiku-4-5".to_string()],
        ));
        assert!(model_supports_auto_mode_for(
            "claude-3-5-sonnet",
            true,
            &["CLAUDE-3-5-SONNET".to_string()],
        ));
    }

    #[test]
    fn auto_mode_config_reads_camel_case_allow_models_from_cached_shape() {
        let config: AutoModeConfig = serde_json::from_value(serde_json::json!({
            "allowModels": ["internal-a", "internal-b"]
        }))
        .unwrap();
        assert_eq!(config.allow_models, ["internal-a", "internal-b"]);
    }

    #[test]
    fn public_auto_mode_gate_matches_family_rules() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let config = crate::utils::config::GlobalConfig::default();
        crate::utils::config::set_test_global_config(Some(config));

        assert!(model_supports_auto_mode("claude-sonnet-4-6"));
        // There is no allowModels payload: eligibility comes from the ported
        // family rules alone.
        assert!(!model_supports_auto_mode("internal-cached-model"));
        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn get_model_betas_matches_official_first_party_agentic_headers() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("NYAN_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("NYAN_DISABLE_1M_CONTEXT");
        crate::utils::process_env::remove("NYAN_ENTRYPOINT");
        clear_betas_caches();

        let betas = get_merged_betas("claude-haiku-4-5-20251001[1m]", true);
        assert!(betas.contains(&CLAUDE_CODE_20250219_BETA_HEADER.to_string()));
        assert!(betas.contains(&CONTEXT_1M_BETA_HEADER.to_string()));
        assert!(betas.contains(&INTERLEAVED_THINKING_BETA_HEADER.to_string()));
        assert!(betas.contains(&CONTEXT_MANAGEMENT_BETA_HEADER.to_string()));
        assert!(betas.contains(&PROMPT_CACHING_SCOPE_BETA_HEADER.to_string()));
    }

    #[test]
    fn model_supports_structured_outputs_matches_current_official_allowlist_shape() {
        assert!(model_supports_structured_outputs("claude-sonnet-4-6"));
        assert!(!model_supports_structured_outputs(
            "claude-sonnet-4-20250514"
        ));
    }

    #[test]
    fn beta_header_raw_model_memo_and_clear_lifecycle_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "NYAN_DISABLE_EXPERIMENTAL_BETAS",
            "NYAN_DISABLE_NONESSENTIAL_TRAFFIC",
        ] {
            crate::utils::process_env::remove(key);
        }
        clear_betas_caches();
        crate::utils::config::set_test_global_config(Some(Default::default()));

        let raw = "claude-sonnet-4-6";
        let first = get_all_model_betas(raw);
        // The strict-tools gate is source-controlled and off, so nothing adds
        // the structured-outputs header.
        assert!(
            !first
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );
        let context_index = first
            .iter()
            .position(|beta| beta == CONTEXT_MANAGEMENT_BETA_HEADER)
            .expect("context management header");
        let cache_scope_index = first
            .iter()
            .position(|beta| beta == PROMPT_CACHING_SCOPE_BETA_HEADER)
            .expect("prompt caching scope header");
        assert!(context_index < cache_scope_index);

        crate::utils::process_env::set("NYAN_DISABLE_EXPERIMENTAL_BETAS", "1");
        assert!(
            get_all_model_betas(raw)
                .iter()
                .any(|beta| beta == CONTEXT_MANAGEMENT_BETA_HEADER)
        );
        assert!(
            !get_all_model_betas("claude-sonnet-4-6-new-raw-key")
                .iter()
                .any(|beta| beta == CONTEXT_MANAGEMENT_BETA_HEADER)
        );

        clear_betas_caches();
        assert!(
            !get_all_model_betas(raw)
                .iter()
                .any(|beta| beta == CONTEXT_MANAGEMENT_BETA_HEADER)
        );

        crate::utils::process_env::remove("NYAN_DISABLE_EXPERIMENTAL_BETAS");
        assert!(
            !get_all_model_betas(raw)
                .iter()
                .any(|beta| beta == CONTEXT_MANAGEMENT_BETA_HEADER)
        );

        crate::utils::config::set_test_global_config(None);
        clear_betas_caches();
    }
}
