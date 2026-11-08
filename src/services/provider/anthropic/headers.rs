//! Outbound model-request headers, split into a common layer and the Anthropic
//! adapter. No new provider is introduced here.
//!
//! Settings `httpHeaders` override either layer by header name. The existing
//! `ANTHROPIC_CUSTOM_HEADERS` environment escape hatch is applied by the client
//! factory before this override, so settings win.

use std::collections::HashMap;

use crate::utils::process_env::{EnvSnapshot, JsTruthy};

/// Headers sent for every model request, regardless of provider.
pub fn common_headers() -> HashMap<String, Option<String>> {
    HashMap::from([
        (
            "User-Agent".to_string(),
            Some(crate::utils::http::user_agent()),
        ),
        ("x-app".to_string(), Some("cli".to_string())),
    ])
}

/// Anthropic-shaped adapter headers. Session and remote ids are nyan's; the
/// additional-protection flag keeps Anthropic's own header name because that
/// is the provider wire field, not our product name.
pub fn anthropic_adapter_headers(env: &EnvSnapshot) -> HashMap<String, Option<String>> {
    let mut headers = HashMap::from([(
        "X-Nyan-Session-Id".to_string(),
        Some(crate::bootstrap::state::get_session_id()),
    )]);
    if let Some(container_id) = env.var("CLAUDE_CODE_CONTAINER_ID").truthy() {
        headers.insert(
            "x-nyan-remote-container-id".to_string(),
            Some(container_id.to_string()),
        );
    }
    if let Some(remote_session_id) = env.var("CLAUDE_CODE_REMOTE_SESSION_ID").truthy() {
        headers.insert(
            "x-nyan-remote-session-id".to_string(),
            Some(remote_session_id.to_string()),
        );
    }
    if crate::utils::env_utils::is_env_truthy(env.var("NYAN_ADDITIONAL_PROTECTION")) {
        headers.insert(
            "x-anthropic-additional-protection".to_string(),
            Some("true".to_string()),
        );
    }
    headers
}

/// `anthropic-beta` belongs to the Anthropic request, not the common layer.
pub fn anthropic_beta_header(betas: &[String]) -> Option<(String, String)> {
    if betas.is_empty() {
        return None;
    }
    Some(("anthropic-beta".to_string(), betas.join(",")))
}

fn configured_headers() -> Vec<(String, String)> {
    crate::utils::settings::get_initial_settings()
        .http_headers
        .map(|headers| headers.into_iter().collect())
        .unwrap_or_default()
}

/// Settings `httpHeaders` replace any built-in header of the same name.
pub fn apply_configured_overrides(headers: &mut HashMap<String, Option<String>>) {
    for (name, value) in configured_headers() {
        headers.insert(name, Some(value));
    }
}

/// Replace one header when settings name it. Matching is case-insensitive.
pub fn override_header_if_configured(headers: &mut HashMap<String, Option<String>>, name: &str) {
    if let Some((_, value)) = configured_headers()
        .into_iter()
        .find(|(configured, _)| configured.eq_ignore_ascii_case(name))
    {
        headers.insert(name.to_string(), Some(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::settings_cache::{
        reset_settings_cache, set_session_settings_cache,
    };
    use crate::utils::settings::types::SettingsJson;
    use crate::utils::settings::validation::SettingsWithErrors;
    use indexmap::IndexMap;

    struct CacheGuard;
    impl Drop for CacheGuard {
        fn drop(&mut self) {
            reset_settings_cache();
        }
    }

    #[test]
    fn common_layer_uses_the_shared_nyan_user_agent() {
        let headers = common_headers();
        assert_eq!(
            headers.get("User-Agent").and_then(Option::as_deref),
            Some(crate::utils::http::user_agent()).as_deref()
        );
        assert_eq!(headers.get("x-app").and_then(Option::as_deref), Some("cli"));
        assert!(!headers.contains_key("anthropic-beta"));
        assert!(!headers.contains_key("X-Nyan-Session-Id"));
    }

    #[test]
    fn anthropic_adapter_keeps_provider_header_names_and_renames_ours() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _container =
            crate::utils::test_env::EnvVarGuard::set("CLAUDE_CODE_CONTAINER_ID", "box");
        let _protection =
            crate::utils::test_env::EnvVarGuard::set("NYAN_ADDITIONAL_PROTECTION", "1");
        let env = crate::utils::process_env::snapshot();
        let headers = anthropic_adapter_headers(&env);
        assert!(headers.contains_key("X-Nyan-Session-Id"));
        assert_eq!(
            headers
                .get("x-nyan-remote-container-id")
                .and_then(Option::as_deref),
            Some("box")
        );
        assert_eq!(
            headers
                .get("x-anthropic-additional-protection")
                .and_then(Option::as_deref),
            Some("true")
        );
        assert!(!headers.contains_key("x-claude-remote-container-id"));
    }

    #[test]
    fn settings_override_replaces_a_provider_header() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _guard = CacheGuard;
        let mut http_headers = IndexMap::new();
        http_headers.insert("anthropic-beta".to_string(), "from-settings".to_string());
        set_session_settings_cache(SettingsWithErrors {
            settings: SettingsJson {
                http_headers: Some(http_headers),
                ..SettingsJson::default()
            },
            errors: Vec::new(),
        });
        let mut headers =
            HashMap::from([("anthropic-beta".to_string(), Some("computed".to_string()))]);
        override_header_if_configured(&mut headers, "anthropic-beta");
        assert_eq!(
            headers.get("anthropic-beta").and_then(Option::as_deref),
            Some("from-settings")
        );
    }
}
