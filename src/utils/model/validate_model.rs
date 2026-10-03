//! Model validation for commands that accept a concrete model.
//!
//! Maps to CC `utils/model/validateModel.ts`.  Empty and allowlist gates
//! reject locally; alias and `ANTHROPIC_CUSTOM_MODEL_OPTION` return before
//! any request; unknown models use the existing `side_query` owner so the
//! validation request has the same client, attribution, beta and retry path
//! as the source.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

static VALID_MODEL_CACHE: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Maps to CC `validateModel(model)` (`validateModel.ts:17-95`).
pub async fn validate_model(model: &str) -> Result<(), String> {
    let normalized = model.trim();
    if normalized.is_empty() {
        return Err("Model name cannot be empty".to_string());
    }

    if !crate::utils::model::model_allowlist::is_model_allowed(normalized) {
        return Err(format!(
            "Model '{normalized}' is not in the list of available models"
        ));
    }

    // CC `:39-47` — known aliases and the user-supplied custom option skip
    // the cache and the live probe.
    let lower = normalized.to_ascii_lowercase();
    if crate::utils::model::aliases::is_model_alias(&lower)
        || crate::utils::process_env::var("ANTHROPIC_CUSTOM_MODEL_OPTION")
            .as_deref()
            == Some(normalized)
    {
        return Ok(());
    }

    if VALID_MODEL_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(normalized)
    {
        return Ok(());
    }

    let result = crate::utils::side_query::side_query(crate::utils::side_query::SideQueryOptions {
        model: normalized.to_string(),
        messages: vec![crate::utils::side_query::user_text_message("Hi")],
        max_tokens: Some(1),
        max_retries: Some(0),
        query_source: "model_validation".to_string(),
        ..Default::default()
    })
    .await;

    match result {
        Ok(_) => {
            VALID_MODEL_CACHE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(normalized.to_string());
            Ok(())
        }
        Err(error) => Err(handle_validation_error(&error, normalized)),
    }
}

/// Maps to CC `handleValidationError` (`validateModel.ts:98-147`).
fn handle_validation_error(error: &anyhow::Error, model: &str) -> String {
    let api_error = error.downcast_ref::<anthropic_sdk::ApiError>();
    if let Some(anthropic_sdk::ApiError::NotFound { body, .. }) = api_error {
        let fallback = get_three_p_fallback_suggestion(model);
        let suggestion = fallback
            .map(|fallback| format!(". Try '{fallback}' instead"))
            .unwrap_or_default();
        // The SDK keeps the API error body as data; preserve the source's
        // model-specific not-found wording when the body identifies a model.
        if body.as_deref().is_some_and(|body| {
            body.get("error")
                .and_then(|error| error.get("type"))
                .and_then(serde_json::Value::as_str)
                == Some("not_found_error")
        }) {
            return format!("Model '{model}' not found{suggestion}");
        }
        return format!("Model '{model}' not found{suggestion}");
    }

    match api_error {
        Some(anthropic_sdk::ApiError::Authentication { .. }) => {
            "Authentication failed. Please check your API credentials.".to_string()
        }
        Some(anthropic_sdk::ApiError::Connection { .. })
        | Some(anthropic_sdk::ApiError::ConnectionTimeout { .. }) => {
            "Network error. Please check your internet connection.".to_string()
        }
        Some(anthropic_sdk::ApiError::BadRequest { message, .. })
        | Some(anthropic_sdk::ApiError::PermissionDenied { message, .. })
        | Some(anthropic_sdk::ApiError::Conflict { message, .. })
        | Some(anthropic_sdk::ApiError::UnprocessableEntity { message, .. })
        | Some(anthropic_sdk::ApiError::RateLimitError { message, .. })
        | Some(anthropic_sdk::ApiError::InternalServerError { message, .. })
        | Some(anthropic_sdk::ApiError::Other { message, .. }) => {
            format!("API error: {message}")
        }
        Some(anthropic_sdk::ApiError::NotFound { .. }) => {
            format!("Model '{model}' not found")
        }
        Some(anthropic_sdk::ApiError::UserAbort { message }) => {
            format!("Unable to validate model: {message}")
        }
        Some(anthropic_sdk::ApiError::Sdk(message)) => {
            format!("Unable to validate model: {message}")
        }
        None => format!("Unable to validate model: {error}"),
    }
}

/// Maps to CC `get3PFallbackSuggestion` (`validateModel.ts:150-168`).
fn get_three_p_fallback_suggestion(model: &str) -> Option<String> {
    if crate::utils::model::providers::get_api_provider()
        == crate::utils::model::providers::ApiProvider::FirstParty
    {
        return None;
    }
    let lower = model.to_ascii_lowercase();
    let models = crate::utils::model::model_strings::get_model_strings();
    if lower.contains("opus-4-6") || lower.contains("opus_4_6") {
        return Some(models.opus41);
    }
    if lower.contains("sonnet-4-6") || lower.contains("sonnet_4_6") {
        return Some(models.sonnet45);
    }
    if lower.contains("sonnet-4-5") || lower.contains("sonnet_4_5") {
        return Some(models.sonnet40);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[tokio::test]
    async fn aliases_and_empty_models_match_official_local_validation_paths() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        assert!(validate_model(" opus ").await.is_ok());
        // CC `:39-42` — every alias returns before the probe, after the trim
        // (`:23`) and the lowercase (`:39`).
        for alias in crate::utils::model::aliases::MODEL_ALIASES {
            assert!(validate_model(alias).await.is_ok(), "{alias}");
        }
        assert!(validate_model("  OpusPlan  ").await.is_ok());
        assert_eq!(
            validate_model("  ").await.unwrap_err(),
            "Model name cannot be empty"
        );
        let _allowlist = EnvVarGuard::set("ANTHROPIC_CUSTOM_MODEL_OPTION", "custom-model");
        assert!(validate_model("custom-model").await.is_ok());
    }

    #[test]
    fn fallback_suggestion_is_only_for_third_party_providers() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        assert!(get_three_p_fallback_suggestion("opus-4-6").is_none());
        let _provider = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert_eq!(
            get_three_p_fallback_suggestion("opus-4-6").as_deref(),
            Some("us.anthropic.claude-opus-4-1-20250805-v1:0")
        );
    }
}
