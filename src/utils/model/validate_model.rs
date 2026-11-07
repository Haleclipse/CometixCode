//! Model validation for commands that accept a concrete model.
//!
//! Maps to CC `utils/model/validateModel.ts`.  Empty models reject locally;
//! `ANTHROPIC_CUSTOM_MODEL_OPTION` returns before any request; unknown models
//! use the existing `side_query` owner so the
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

    // CC `:39-47` — the user-supplied custom option skips the cache and the
    // live probe. @nyan: CC's model aliases are gone.
    if crate::utils::process_env::var("ANTHROPIC_CUSTOM_MODEL_OPTION").as_deref()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[tokio::test]
    async fn custom_option_and_empty_models_match_official_local_validation_paths() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        assert_eq!(
            validate_model("  ").await.unwrap_err(),
            "Model name cannot be empty"
        );
        let _allowlist = EnvVarGuard::set("ANTHROPIC_CUSTOM_MODEL_OPTION", "custom-model");
        assert!(validate_model("custom-model").await.is_ok());
    }
}
