//! Maps to: CC `hooks/useApiKeyVerification.ts`.
//!
//! @nyan: CC verifies an Anthropic API key online. nyan checks, without a
//! request, that the provider of the session's profile can resolve its
//! `credential` (`services::provider::credential`); a rejected key surfaces as
//! the request's own error.

use crate::utils::provider_auth::store::AuthStore;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CredentialStatus {
    #[default]
    Ready,
    /// The provider whose credential cannot be resolved.
    Missing { provider: String },
}

impl CredentialStatus {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing { .. })
    }
}

/// The credential status of the session's profile, recomputed when the
/// settings or the profile change.
pub fn use_credential_status(hooks: &mut Hooks<'_, '_>) -> CredentialStatus {
    let (settings, profile) = crate::state::app_state::use_app_state(hooks, |state| {
        (state.settings.clone(), state.main_loop_profile.clone())
    });
    hooks.use_memo(
        || match crate::services::provider::credential::missing_credential(
            &settings,
            profile.as_deref(),
            &AuthStore::user(),
        ) {
            Some(provider) => CredentialStatus::Missing { provider },
            None => CredentialStatus::Ready,
        },
        (std::sync::Arc::as_ptr(&settings) as usize, profile.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[component]
    fn CredentialStatusHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let status = match use_credential_status(&mut hooks) {
            CredentialStatus::Ready => "ready".to_string(),
            CredentialStatus::Missing { provider } => format!("missing:{provider}"),
        };
        element! { Text(content: status) }
    }

    fn render(settings: serde_json::Value, profile: Option<&str>) -> String {
        let state = crate::state::app_state_store::AppState {
            settings: std::sync::Arc::new(serde_json::from_value(settings).unwrap()),
            main_loop_profile: profile.map(str::to_string),
            ..Default::default()
        };
        let store = crate::state::store::AppStore::new(state, None);
        element! {
            crate::state::app_state::AppStateProvider(
                prebuilt_store: Some(store),
                children: crate::state::app_state::ProviderChildren::new(|| element! {
                    CredentialStatusHarness
                }.into_any()),
            )
        }
        .render(Some(80))
        .to_string()
        .trim()
        .to_string()
    }

    #[test]
    fn status_follows_the_session_profiles_provider_credential() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _set = EnvVarGuard::set("NYAN_FAKE_STATUS_KEY", "sk-fake-status-0001");
        let _unset = EnvVarGuard::unset("NYAN_FAKE_STATUS_UNSET");
        let settings = serde_json::json!({"nyan": {
            "providers": {
                "deepseek": {"kind": "openai", "protocol": "openaiChat",
                    "baseUrl": "https://api.deepseek.example",
                    "credential": {"env": "NYAN_FAKE_STATUS_KEY"}},
                "cpa": {"kind": "anthropic", "protocol": "anthropicMessages",
                    "credential": {"env": "NYAN_FAKE_STATUS_UNSET"}}
            },
            "models": {
                "main": {"provider": "deepseek", "model": "deepseek-v4",
                    "capabilities": {"contextWindow": 128000, "maxOutputTokens": 8192}},
                "heavy": {"provider": "cpa", "model": "claude-opus-5-5",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}}
            },
            "selection": {"main": "main"}
        }});
        assert_eq!(render(settings.clone(), None), "ready");
        assert_eq!(render(settings, Some("heavy")), "missing:cpa");
    }
}
