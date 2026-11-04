//! Where an Anthropic request goes and with which key, derived from the
//! selected provider profile only.
//!
//! @nyan: replaces PR 1's temporary compatibility layer (decision 22), which
//! let the client read `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
//! `ANTHROPIC_BASE_URL`, claude.ai OAuth tokens and `apiKeyHelper` from the
//! ambient environment. A profile now names its own base URL and credential
//! reference, and nothing else supplies them.

use std::fmt;

use super::credential::{ApiKey, CredentialGateError, resolve_api_key};
use crate::utils::model::profile::{ProfileResolutionError, ResolvedModelProfile};
use crate::utils::settings::SettingsJson;
use crate::utils::settings::nyan::ProviderProtocol;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnthropicTarget {
    pub profile: String,
    pub provider: String,
    pub base_url: Option<String>,
    pub api_key: Option<ApiKey>,
    pub cache_retention: crate::utils::settings::nyan::CacheRetention,
}

#[derive(Debug)]
pub enum TargetError {
    Profile(ProfileResolutionError),
    Credential(CredentialGateError),
    /// The profile speaks another protocol. Sending it an Anthropic request
    /// would put another provider's key and endpoint behind Anthropic's wire
    /// format, so the Anthropic client refuses it.
    NotAnthropic {
        profile: String,
        protocol: ProviderProtocol,
    },
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => error.fmt(f),
            Self::Credential(error) => error.fmt(f),
            Self::NotAnthropic { profile, protocol } => write!(
                f,
                "Model profile {profile:?} uses protocol {protocol:?}; it cannot be served by the Anthropic client."
            ),
        }
    }
}

impl std::error::Error for TargetError {}

impl From<ProfileResolutionError> for TargetError {
    fn from(error: ProfileResolutionError) -> Self {
        Self::Profile(error)
    }
}

impl From<CredentialGateError> for TargetError {
    fn from(error: CredentialGateError) -> Self {
        Self::Credential(error)
    }
}

fn clean_base_url(base_url: Option<&str>) -> Option<String> {
    base_url
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

/// Build the target for an already-resolved profile and explicit settings.
pub fn target_for_profile_with_settings(
    profile: &ResolvedModelProfile,
    settings: &SettingsJson,
) -> Result<AnthropicTarget, TargetError> {
    if profile.protocol != ProviderProtocol::AnthropicMessages {
        return Err(TargetError::NotAnthropic {
            profile: profile.profile.clone(),
            protocol: profile.protocol,
        });
    }
    let api_key = resolve_api_key(&profile.provider, profile.credential.as_ref())?;
    let cache_retention = settings
        .nyan
        .as_ref()
        .and_then(|nyan| nyan.models.as_ref())
        .and_then(|models| models.get(&profile.profile))
        .map(|model_profile| model_profile.cache_retention())
        .unwrap_or_default();
    Ok(AnthropicTarget {
        profile: profile.profile.clone(),
        provider: profile.provider.clone(),
        base_url: clean_base_url(profile.base_url.as_deref()),
        api_key,
        cache_retention,
    })
}

/// Build the target for an already-resolved profile.
pub fn target_for_profile(profile: &ResolvedModelProfile) -> Result<AnthropicTarget, TargetError> {
    target_for_profile_with_settings(profile, &crate::utils::settings::get_initial_settings())
}

/// The target of the selected main profile. Auxiliary requests (titles,
/// classifiers, quota checks) use the main profile's provider too: there is no
/// second source of Anthropic credentials.
pub fn resolve_main_target(settings: &SettingsJson) -> Result<AnthropicTarget, TargetError> {
    let profile = crate::utils::model::profile::resolve_main_profile(settings)?;
    target_for_profile_with_settings(&profile, settings)
}

/// Same as [`resolve_main_target`] against the live settings.
pub fn current_main_target() -> Result<AnthropicTarget, TargetError> {
    resolve_main_target(&crate::utils::settings::get_initial_settings())
}

/// Test support: install an Anthropic main profile in the session settings
/// cache, with the key held in `env_name` (which the caller sets). Every test
/// that sends a real request goes through this one path, so none of them can
/// lean on ambient Anthropic variables again.
#[cfg(test)]
pub(crate) fn install_test_main_profile(base_url: &str, env_name: &str) {
    install_test_main_profile_with_retention(base_url, env_name, None);
}

#[cfg(test)]
pub(crate) fn install_test_main_profile_with_retention(
    base_url: &str,
    env_name: &str,
    cache_retention: Option<crate::utils::settings::nyan::CacheRetention>,
) {
    use crate::utils::settings::nyan::{
        CredentialReference, ModelCapabilities, ModelProfileSettings, NyanSettings, ProviderKind,
        ProviderProtocol, ProviderSettings, SelectionSettings,
    };
    use crate::utils::settings::settings_cache::set_session_settings_cache;
    use crate::utils::settings::validation::SettingsWithErrors;
    use std::collections::BTreeMap;

    let nyan = NyanSettings {
        providers: Some(BTreeMap::from([(
            "anthropic".to_string(),
            ProviderSettings {
                kind: Some(ProviderKind::Anthropic),
                protocol: Some(ProviderProtocol::AnthropicMessages),
                base_url: Some(base_url.to_string()),
                credential: Some(CredentialReference {
                    env: Some(env_name.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )])),
        models: Some(BTreeMap::from([(
            "main".to_string(),
            ModelProfileSettings {
                provider: Some("anthropic".to_string()),
                model: Some("claude-sonnet-4-6".to_string()),
                capabilities: Some(ModelCapabilities {
                    context_window: Some(200_000),
                    max_output_tokens: Some(64_000),
                    ..Default::default()
                }),
                cache_retention,
                ..Default::default()
            },
        )])),
        selection: Some(SelectionSettings {
            main: Some("main".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    set_session_settings_cache(SettingsWithErrors {
        settings: SettingsJson {
            nyan: Some(Box::new(nyan)),
            ..Default::default()
        },
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::nyan::{CredentialReference, ProviderKind};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn lock() -> crate::utils::test_env::TestEnvGuard<'static> {
        TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn profile(
        credential: Option<CredentialReference>,
        base_url: Option<&str>,
    ) -> ResolvedModelProfile {
        ResolvedModelProfile {
            profile: "main".into(),
            provider: "anthropic".into(),
            provider_kind: ProviderKind::Anthropic,
            protocol: crate::utils::settings::nyan::ProviderProtocol::AnthropicMessages,
            model: "claude-sonnet-4-6".into(),
            context_window: 200_000,
            max_output_tokens: 64_000,
            capabilities: Default::default(),
            base_url: base_url.map(str::to_string),
            credential,
            compat: None,
        }
    }

    fn env_ref(name: &str) -> CredentialReference {
        CredentialReference {
            env: Some(name.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn target_takes_base_url_and_key_from_the_profile_only() {
        let _lock = lock();
        // The ambient variables the compat layer used to read are really
        // present in the process and must be ignored.
        let _guards = [
            EnvVarGuard::set("NYAN_TARGET_TEST_KEY", "sk-profile"),
            EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ambient"),
            EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", "tok-ambient"),
            EnvVarGuard::set("ANTHROPIC_BASE_URL", "https://ambient.example"),
        ];
        let target = target_for_profile(&profile(
            Some(env_ref("NYAN_TARGET_TEST_KEY")),
            Some(" https://gw.example/ "),
        ))
        .unwrap();
        assert_eq!(target.api_key.unwrap().expose(), "sk-profile");
        assert_eq!(target.base_url.as_deref(), Some("https://gw.example"));
        assert_eq!(
            target.cache_retention,
            crate::utils::settings::nyan::CacheRetention::Short
        );
    }

    #[test]
    fn target_resolves_cache_retention_from_profile_settings() {
        let _lock = lock();
        let _guards = [EnvVarGuard::set("NYAN_TARGET_TEST_KEY", "sk-profile")];
        install_test_main_profile_with_retention(
            "https://api.anthropic.com",
            "NYAN_TARGET_TEST_KEY",
            Some(crate::utils::settings::nyan::CacheRetention::Long),
        );
        let target = current_main_target().unwrap();
        assert_eq!(
            target.cache_retention,
            crate::utils::settings::nyan::CacheRetention::Long
        );

        install_test_main_profile_with_retention(
            "https://api.anthropic.com",
            "NYAN_TARGET_TEST_KEY",
            None,
        );
        let default_target = current_main_target().unwrap();
        assert_eq!(
            default_target.cache_retention,
            crate::utils::settings::nyan::CacheRetention::Short
        );
    }

    #[test]
    fn an_openai_profile_is_never_turned_into_an_anthropic_target() {
        let _lock = lock();
        let _key = EnvVarGuard::set("NYAN_TARGET_TEST_OPENAI", "sk-openai");
        let mut openai = profile(Some(env_ref("NYAN_TARGET_TEST_OPENAI")), None);
        openai.provider_kind = ProviderKind::Openai;
        openai.protocol = ProviderProtocol::OpenaiResponses;
        assert!(matches!(
            target_for_profile(&openai).unwrap_err(),
            TargetError::NotAnthropic {
                protocol: ProviderProtocol::OpenaiResponses,
                ..
            }
        ));
    }

    #[test]
    fn missing_credential_is_a_clear_error_not_an_ambient_fallback() {
        let _lock = lock();
        let _ambient = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ambient");
        let error = target_for_profile(&profile(None, None)).unwrap_err();
        assert!(matches!(
            error,
            TargetError::Credential(CredentialGateError::NotConfigured { .. })
        ));
        assert!(error.to_string().contains("credential"));
    }

    #[test]
    fn unset_key_variable_names_the_variable() {
        let _lock = lock();
        let _unset = EnvVarGuard::unset("NYAN_TARGET_TEST_UNSET");
        let error = target_for_profile(&profile(Some(env_ref("NYAN_TARGET_TEST_UNSET")), None))
            .unwrap_err();
        assert!(error.to_string().contains("NYAN_TARGET_TEST_UNSET"));
    }

    #[test]
    fn blank_base_url_means_the_sdk_default() {
        let _lock = lock();
        let _key = EnvVarGuard::set("NYAN_TARGET_TEST_KEY2", "x");
        let target =
            target_for_profile(&profile(Some(env_ref("NYAN_TARGET_TEST_KEY2")), Some("  ")))
                .unwrap();
        assert_eq!(target.base_url, None);
    }

    #[test]
    fn no_profile_is_the_resolver_error() {
        let error = resolve_main_target(&SettingsJson::default()).unwrap_err();
        assert!(matches!(
            error,
            TargetError::Profile(ProfileResolutionError::MissingSettings)
        ));
    }
}
