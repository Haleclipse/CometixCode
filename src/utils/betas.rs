//! Beta-header helpers.
//! Maps to CC `utils/betas.ts`.

pub use crate::constants::betas::{
    ADVISOR_BETA_HEADER, CLAUDE_CODE_20250219_BETA_HEADER, CLI_INTERNAL_BETA_HEADER,
    CONTEXT_1M_BETA_HEADER, CONTEXT_MANAGEMENT_BETA_HEADER, EFFORT_BETA_HEADER,
    FAST_MODE_BETA_HEADER, INTERLEAVED_THINKING_BETA_HEADER, PROMPT_CACHING_SCOPE_BETA_HEADER,
    REDACT_THINKING_BETA_HEADER, STRUCTURED_OUTPUTS_BETA_HEADER, TASK_BUDGETS_BETA_HEADER,
    TOOL_SEARCH_BETA_HEADER_1P, TOOL_SEARCH_BETA_HEADER_3P, WEB_SEARCH_BETA_HEADER,
};
use crate::utils::model::model::get_canonical_name;
use crate::utils::model::model_support_overrides::{
    ModelCapabilityOverride, get_3p_model_capability_override,
};
use crate::utils::model::providers::{ApiProvider, get_api_provider};
use crate::utils::process_env::JsTruthy;
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
/// Rust's public owner reads process/build/provider/cache globals and delegates
/// here so both compiled audiences and provider branches have a deterministic
/// test seam; this helper carries no separate policy or state.
fn model_supports_auto_mode_for(
    model: &str,
    provider: ApiProvider,
    internal_build: bool,
    transcript_classifier_enabled: bool,
    allow_models: &[String],
) -> bool {
    if !transcript_classifier_enabled {
        return false;
    }

    let canonical = get_canonical_name(model);
    // External Auto mode is first-party only. This guard intentionally runs
    // before allowModels so a cached override cannot enable another provider.
    if !internal_build && provider != ApiProvider::FirstParty {
        return false;
    }

    let raw_lower = model.to_ascii_lowercase();
    if allow_models.iter().any(|allowed| {
        let allowed = allowed.to_ascii_lowercase();
        allowed == raw_lower || allowed == canonical
    }) {
        return true;
    }

    if internal_build {
        if canonical.contains("claude-3-") {
            return false;
        }
        for family in ["opus", "sonnet", "haiku"] {
            let prefix = format!("claude-{family}-4");
            if let Some(suffix) = canonical.strip_prefix(&prefix) {
                let supported_version = suffix
                    .strip_prefix('-')
                    .and_then(|suffix| suffix.chars().next())
                    .is_some_and(|version| ('6'..='9').contains(&version));
                if !supported_version {
                    return false;
                }
            }
        }
        return true;
    }

    canonical.starts_with("claude-opus-4-6") || canonical.starts_with("claude-sonnet-4-6")
}

/// Maps to: CC `utils/betas.ts:160-193#modelSupportsAutoMode`.
pub fn model_supports_auto_mode(model: &str) -> bool {
    let internal_build = crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Permissions,
    );
    let transcript_classifier_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
    );
    let auto_mode_config_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::AutoModeConfig,
    );
    let allow_models = (transcript_classifier_enabled && auto_mode_config_enabled)
        .then(|| AutoModeConfig::default().allow_models)
        .unwrap_or_default();
    model_supports_auto_mode_for(
        model,
        get_api_provider(),
        internal_build,
        transcript_classifier_enabled,
        &allow_models,
    )
}

fn has_1m_context(model: &str) -> bool {
    !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_1M_CONTEXT")
            .as_deref(),
    ) && model.to_ascii_lowercase().contains("[1m]")
}

/// Maps to CC `utils/betas.ts:92-111` `modelSupportsISP(...)`.
pub fn model_supports_isp(model: &str) -> bool {
    if let Some(supported_3p) =
        get_3p_model_capability_override(model, ModelCapabilityOverride::InterleavedThinking)
    {
        return supported_3p;
    }
    let canonical = canonical_model(model);
    let provider = get_api_provider();
    // Foundry supports interleaved thinking for all models
    if provider == ApiProvider::Foundry {
        return true;
    }
    if provider == ApiProvider::FirstParty {
        return !canonical.contains("claude-3-");
    }
    canonical.contains("claude-opus-4") || canonical.contains("claude-sonnet-4")
}

/// Maps to CC `utils/betas.ts` `modelSupportsContextManagement(...)`.
pub fn model_supports_context_management(model: &str) -> bool {
    let provider = get_api_provider();
    if provider == ApiProvider::Foundry {
        return true;
    }
    let canonical = canonical_model(model);
    if provider == ApiProvider::FirstParty {
        return !canonical.contains("claude-3-");
    }
    canonical.contains("claude-opus-4")
        || canonical.contains("claude-sonnet-4")
        || canonical.contains("claude-haiku-4")
}

/// Maps to CC `utils/betas.ts` `shouldIncludeFirstPartyOnlyBetas(...)`.
pub fn should_include_first_party_only_betas() -> bool {
    matches!(
        get_api_provider(),
        ApiProvider::FirstParty | ApiProvider::Foundry
    ) && !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS")
            .as_deref(),
    )
}

/// Maps to CC `utils/betas.ts` `shouldUseGlobalCacheScope(...)`.
pub fn should_use_global_cache_scope() -> bool {
    get_api_provider() == ApiProvider::FirstParty
        && !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS")
                .as_deref(),
        )
}

/// Maps to CC `utils/betas.ts` `getToolSearchBetaHeader(...)`.
pub fn get_tool_search_beta_header() -> &'static str {
    match get_api_provider() {
        ApiProvider::Bedrock | ApiProvider::Vertex => TOOL_SEARCH_BETA_HEADER_3P,
        ApiProvider::FirstParty | ApiProvider::Foundry => TOOL_SEARCH_BETA_HEADER_1P,
    }
}

/// Maps to CC `utils/betas.ts` `modelSupportsStructuredOutputs(...)`.
pub fn model_supports_structured_outputs(model: &str) -> bool {
    if !matches!(
        get_api_provider(),
        ApiProvider::FirstParty | ApiProvider::Foundry
    ) {
        return false;
    }
    let canonical = canonical_model(model);
    canonical.contains("claude-sonnet-4-6")
        || canonical.contains("claude-sonnet-4-5")
        || canonical.contains("claude-opus-4-1")
        || canonical.contains("claude-opus-4-5")
        || canonical.contains("claude-opus-4-6")
        || canonical.contains("claude-haiku-4-5")
}

fn vertex_model_supports_web_search(model: &str) -> bool {
    let canonical = canonical_model(model);
    canonical.contains("claude-opus-4")
        || canonical.contains("claude-sonnet-4")
        || canonical.contains("claude-haiku-4")
}

static ALL_MODEL_BETAS_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MODEL_BETAS_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static BEDROCK_EXTRA_BODY_BETAS_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
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
    let provider = get_api_provider();
    let include_first_party_only_betas = should_include_first_party_only_betas();

    if !is_haiku(model) {
        beta_headers.push(CLAUDE_CODE_20250219_BETA_HEADER.to_string());
        if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Api,
        ) && crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() == Some("cli")
            && !CLI_INTERNAL_BETA_HEADER.is_empty()
        {
            beta_headers.push(CLI_INTERNAL_BETA_HEADER.to_string());
        }
    }

    if has_1m_context(model) {
        beta_headers.push(CONTEXT_1M_BETA_HEADER.to_string());
    }

    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_INTERLEAVED_THINKING")
            .as_deref(),
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

    if provider == ApiProvider::Vertex && vertex_model_supports_web_search(model) {
        beta_headers.push(WEB_SEARCH_BETA_HEADER.to_string());
    }
    if provider == ApiProvider::Foundry {
        beta_headers.push(WEB_SEARCH_BETA_HEADER.to_string());
    }

    if include_first_party_only_betas {
        beta_headers.push(PROMPT_CACHING_SCOPE_BETA_HEADER.to_string());
    }

    if let Some(extra_betas) = crate::utils::process_env::var("ANTHROPIC_BETAS").truthy() {
        beta_headers.extend(
            extra_betas
                .split(',')
                .map(str::trim)
                .filter(|beta| !beta.is_empty())
                .map(str::to_string),
        );
    }

    cache.insert(model.to_string(), beta_headers.clone());
    beta_headers
}

fn bedrock_extra_params_header(header: &str) -> bool {
    header == INTERLEAVED_THINKING_BETA_HEADER
        || header == CONTEXT_1M_BETA_HEADER
        || header == TOOL_SEARCH_BETA_HEADER_3P
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
    let mut beta_headers = get_all_model_betas(model);
    if get_api_provider() == ApiProvider::Bedrock {
        beta_headers.retain(|header| !bedrock_extra_params_header(header));
    }
    MODEL_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(model.to_string(), beta_headers.clone());
    beta_headers
}

/// Maps to: CC `utils/betas.ts:379-385` memoized
/// `getBedrockExtraBodyParamsBetas`.
pub fn get_bedrock_extra_body_params_betas(model: &str) -> Vec<String> {
    if let Some(betas) = BEDROCK_EXTRA_BODY_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(model)
        .cloned()
    {
        return betas;
    }
    let beta_headers = get_all_model_betas(model)
        .into_iter()
        .filter(|header| bedrock_extra_params_header(header))
        .collect::<Vec<_>>();
    BEDROCK_EXTRA_BODY_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(model.to_string(), beta_headers.clone());
    beta_headers
}

/// Maps to: CC `utils/betas.ts:397-428` `getMergedBetas`.
pub fn get_merged_betas(model: &str, is_agentic_query: bool) -> Vec<String> {
    let mut beta_headers = get_model_betas(model);
    if is_agentic_query {
        if !beta_headers
            .iter()
            .any(|beta| beta == CLAUDE_CODE_20250219_BETA_HEADER)
        {
            beta_headers.push(CLAUDE_CODE_20250219_BETA_HEADER.to_string());
        }
        if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Api,
        ) && crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() == Some("cli")
            && !CLI_INTERNAL_BETA_HEADER.is_empty()
            && !beta_headers
                .iter()
                .any(|beta| beta == CLI_INTERNAL_BETA_HEADER)
        {
            beta_headers.push(CLI_INTERNAL_BETA_HEADER.to_string());
        }
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
    BEDROCK_EXTRA_BODY_BETAS_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn auto_mode_model_gate_matches_official_provider_audience_and_allowlist_order() {
        assert!(!model_supports_auto_mode_for(
            "claude-sonnet-4-6",
            ApiProvider::FirstParty,
            false,
            false,
            &["claude-sonnet-4-6".to_string()],
        ));
        for provider in [
            ApiProvider::Bedrock,
            ApiProvider::Vertex,
            ApiProvider::Foundry,
        ] {
            assert!(!model_supports_auto_mode_for(
                "claude-sonnet-4-6",
                provider,
                false,
                true,
                &["claude-sonnet-4-6".to_string()],
            ));
        }
        for model in ["claude-opus-4-6", "claude-sonnet-4-6"] {
            assert!(model_supports_auto_mode_for(
                model,
                ApiProvider::FirstParty,
                false,
                true,
                &[],
            ));
        }
        assert!(!model_supports_auto_mode_for(
            "claude-haiku-4-5",
            ApiProvider::FirstParty,
            false,
            true,
            &[],
        ));
        assert!(model_supports_auto_mode_for(
            "CLAUDE-HAIKU-4-5",
            ApiProvider::FirstParty,
            false,
            true,
            &["claude-haiku-4-5".to_string()],
        ));
        assert!(model_supports_auto_mode_for(
            "any-internal-model",
            ApiProvider::Bedrock,
            true,
            true,
            &[],
        ));
        assert!(!model_supports_auto_mode_for(
            "claude-3-5-sonnet",
            ApiProvider::FirstParty,
            true,
            true,
            &[],
        ));
        assert!(!model_supports_auto_mode_for(
            "claude-opus-4-5",
            ApiProvider::FirstParty,
            true,
            true,
            &[],
        ));
        assert!(model_supports_auto_mode_for(
            "claude-3-5-sonnet",
            ApiProvider::FirstParty,
            true,
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
    fn public_auto_mode_gate_ignores_cached_config_and_matches_provider_rules() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let provider_vars = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ];
        let _providers = provider_vars.map(EnvVarGuard::unset);
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_auto_mode_config".to_string(),
            serde_json::json!({"allowModels": ["internal-cached-model"]}),
        )]));
        crate::utils::config::set_test_global_config(Some(config));

        assert!(model_supports_auto_mode("claude-sonnet-4-6"));
        // The cached allowModels entry is inert: eligibility now comes from the
        // ported family rules alone.
        if cfg!(feature = "anthropic_internal") {
            assert!(model_supports_auto_mode("internal-cached-model"));
        } else {
            assert!(!model_supports_auto_mode("internal-cached-model"));
        }
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        if cfg!(feature = "anthropic_internal") {
            assert!(model_supports_auto_mode("internal-cached-model"));
        } else {
            assert!(!model_supports_auto_mode("internal-cached-model"));
            assert!(!model_supports_auto_mode("claude-sonnet-4-6"));
        }
        drop(_providers);
        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn get_model_betas_matches_official_first_party_agentic_headers() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        crate::utils::process_env::remove("ANTHROPIC_BETAS");
        crate::utils::process_env::remove("CLAUDE_CODE_ENTRYPOINT");
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
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        assert!(model_supports_structured_outputs("claude-sonnet-4-6"));
        assert!(!model_supports_structured_outputs(
            "claude-sonnet-4-20250514"
        ));
        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        assert!(!model_supports_structured_outputs("claude-sonnet-4-6"));
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
    }

    #[test]
    fn bedrock_model_betas_are_split_between_header_and_extra_body_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        crate::utils::process_env::remove("DISABLE_INTERLEAVED_THINKING");
        crate::utils::process_env::set("ANTHROPIC_BETAS", STRUCTURED_OUTPUTS_BETA_HEADER);
        clear_betas_caches();

        let header_betas = get_model_betas("claude-sonnet-4-20250514[1m]");
        let body_betas = get_bedrock_extra_body_params_betas("claude-sonnet-4-20250514[1m]");

        assert!(!header_betas.contains(&CONTEXT_1M_BETA_HEADER.to_string()));
        assert!(!header_betas.contains(&INTERLEAVED_THINKING_BETA_HEADER.to_string()));
        assert!(body_betas.contains(&CONTEXT_1M_BETA_HEADER.to_string()));
        assert!(body_betas.contains(&INTERLEAVED_THINKING_BETA_HEADER.to_string()));
        assert!(header_betas.contains(&STRUCTURED_OUTPUTS_BETA_HEADER.to_string()));
        assert!(!body_betas.contains(&STRUCTURED_OUTPUTS_BETA_HEADER.to_string()));

        crate::utils::process_env::remove("ANTHROPIC_BETAS");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        clear_betas_caches();
    }

    #[test]
    fn model_and_bedrock_split_memos_preserve_raw_key_chronology_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "DISABLE_INTERLEAVED_THINKING",
            "ANTHROPIC_BETAS",
        ] {
            crate::utils::process_env::remove(key);
        }
        clear_betas_caches();
        let raw = "claude-sonnet-4-20250514[1m]";
        let first_party_header = get_model_betas(raw);
        assert!(first_party_header.contains(&CONTEXT_1M_BETA_HEADER.to_string()));
        assert!(first_party_header.contains(&INTERLEAVED_THINKING_BETA_HEADER.to_string()));

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert_eq!(get_model_betas(raw), first_party_header);
        let body_after_flip = get_bedrock_extra_body_params_betas(raw);
        assert!(body_after_flip.contains(&CONTEXT_1M_BETA_HEADER.to_string()));
        assert!(body_after_flip.contains(&INTERLEAVED_THINKING_BETA_HEADER.to_string()));

        clear_betas_caches();
        let recomputed_header = get_model_betas(raw);
        let recomputed_body = get_bedrock_extra_body_params_betas(raw);
        assert!(!recomputed_header.contains(&CONTEXT_1M_BETA_HEADER.to_string()));
        assert!(!recomputed_header.contains(&INTERLEAVED_THINKING_BETA_HEADER.to_string()));
        assert!(recomputed_body.contains(&CONTEXT_1M_BETA_HEADER.to_string()));
        assert!(recomputed_body.contains(&INTERLEAVED_THINKING_BETA_HEADER.to_string()));

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        clear_betas_caches();
    }

    #[test]
    fn structured_outputs_model_provider_matrix_matches_official_canonical_policy() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _providers = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ]
        .map(EnvVarGuard::unset);
        let supported_models = [
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-1",
            "claude-opus-4-5",
            "claude-opus-4-6-20260101[1m]",
            "CLAUDE-HAIKU-4-5",
        ];
        for model in supported_models {
            assert!(model_supports_structured_outputs(model), "model={model}");
        }
        for model in [
            "claude-3-5-sonnet",
            "claude-sonnet-4-20250514",
            "claude-sonnet-4-1",
            "claude-opus-4",
            "custom-model",
        ] {
            assert!(!model_supports_structured_outputs(model), "model={model}");
        }

        let config_dir = std::env::temp_dir().join(format!(
            "cometix-structured-output-model-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("settings.json"),
            r#"{"modelOverrides":{"claude-sonnet-4-6":"provider-deployment-id"}}"#,
        )
        .unwrap();
        let config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        // `CLAUDE_CONFIG_DIR` selects the User settings file, so it is an INPUT
        // to the merged-settings cache that `get_settings_with_errors`
        // memoizes. CC reads it once during startup and never changes it, so
        // the source has no invalidation for it; a test that repoints it
        // mid-run must invalidate explicitly, both on the way in and on the way
        // back out.
        crate::utils::settings::settings_cache::reset_settings_cache();
        assert!(model_supports_structured_outputs("provider-deployment-id"));
        drop(config);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(config_dir);

        crate::utils::process_env::set("CLAUDE_CODE_USE_FOUNDRY", "1");
        for model in supported_models {
            assert!(model_supports_structured_outputs(model), "foundry={model}");
        }
        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        for model in supported_models {
            assert!(!model_supports_structured_outputs(model), "vertex={model}");
        }
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        for model in supported_models {
            assert!(!model_supports_structured_outputs(model), "bedrock={model}");
        }
        for key in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            crate::utils::process_env::remove(key);
        }
    }

    #[test]
    fn beta_header_raw_model_memo_and_clear_lifecycle_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "ANTHROPIC_BETAS",
        ] {
            crate::utils::process_env::remove(key);
        }
        clear_betas_caches();
        let mut cached_gate_on = crate::utils::config::GlobalConfig::default();
        cached_gate_on.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(cached_gate_on));

        let raw = "claude-sonnet-4-6";
        let first = get_all_model_betas(raw);
        // The strict-tools gate is source-controlled and off, so a cached
        // GrowthBook entry cannot add the structured-outputs header.
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

        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
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

        for key in [
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
        ] {
            crate::utils::process_env::remove(key);
        }
        assert!(
            !get_all_model_betas(raw)
                .iter()
                .any(|beta| beta == CONTEXT_MANAGEMENT_BETA_HEADER)
        );

        crate::utils::config::set_test_global_config(None);
        clear_betas_caches();
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn foundry_config_override_no_longer_reaches_strict_tools_gate() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ] {
            crate::utils::process_env::remove(key);
        }
        crate::utils::process_env::set("CLAUDE_CODE_USE_FOUNDRY", "1");
        clear_betas_caches();
        let mut config = crate::utils::config::GlobalConfig::default();
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));

        assert!(
            !get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );

        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::config::set_test_global_config(None);
        clear_betas_caches();
    }

    #[test]
    fn anthropic_betas_duplicates_and_raw_alias_memo_keys_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ] {
            crate::utils::process_env::remove(key);
        }
        crate::services::analytics::growthbook::reset_growth_book();
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));
        clear_betas_caches();
        crate::utils::process_env::set(
            "ANTHROPIC_BETAS",
            format!("{0},{0}", STRUCTURED_OUTPUTS_BETA_HEADER),
        );
        let duplicated = get_all_model_betas("claude-sonnet-4-6");
        assert_eq!(
            duplicated
                .iter()
                .filter(|beta| beta.as_str() == STRUCTURED_OUTPUTS_BETA_HEADER)
                .count(),
            2
        );

        crate::utils::process_env::remove("ANTHROPIC_BETAS");
        assert_eq!(get_all_model_betas("claude-sonnet-4-6"), duplicated);
        assert!(
            !get_all_model_betas("CLAUDE-SONNET-4-6")
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );

        crate::utils::config::set_test_global_config(None);
        clear_betas_caches();
    }
}
