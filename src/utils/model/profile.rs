//! Profile selection for the provider-neutral settings shape.
//!
//! PR 1 only has the temporary Anthropic request compatibility path. Other
//! provider kinds are intentionally rejected until their adapters land.

use std::fmt;

use crate::utils::settings::SettingsJson;
use crate::utils::settings::nyan::{
    ModelProfileSettings, NyanSettings, ProviderKind, ProviderProtocol, ProviderSettings,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelProfile {
    pub profile: String,
    pub provider: String,
    pub provider_kind: ProviderKind,
    pub model: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileResolutionError {
    MissingSettings,
    MissingSelection,
    MissingProfile(String),
    MissingProvider(String),
    MissingProviderKind(String),
    MissingProviderProtocol(String),
    UnsupportedProvider(ProviderKind),
    MissingModel(String),
    MissingContextWindow(String),
    MissingMaxOutputTokens(String),
    InvalidDecisionProfile(String),
}

impl fmt::Display for ProfileResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSettings => write!(
                f,
                "No provider profiles configured. Configure settings.nyan.providers, settings.nyan.models, and settings.nyan.selection.main, or pass an explicit temporary model."
            ),
            Self::MissingSelection => write!(
                f,
                "No main model profile selected. Set settings.nyan.selection.main to a configured profile name."
            ),
            Self::MissingProfile(profile) => write!(
                f,
                "Model profile {profile:?} is not defined in settings.nyan.models."
            ),
            Self::MissingProvider(provider) => write!(
                f,
                "Provider {provider:?} referenced by the model profile is not defined in settings.nyan.providers."
            ),
            Self::MissingProviderKind(provider) => {
                write!(f, "Provider {provider:?} is missing its built-in kind.")
            }
            Self::MissingProviderProtocol(provider) => write!(
                f,
                "Provider {provider:?} must declare protocol or protocols."
            ),
            Self::UnsupportedProvider(kind) => write!(
                f,
                "Provider kind {kind:?} is not available in PR 1; only the temporary Anthropic compatibility path is implemented."
            ),
            Self::MissingModel(profile) => write!(
                f,
                "Model profile {profile:?} is missing its provider model id."
            ),
            Self::MissingContextWindow(profile) => write!(
                f,
                "Model profile {profile:?} must declare capabilities.contextWindow until the metadata snapshot lands."
            ),
            Self::MissingMaxOutputTokens(profile) => write!(
                f,
                "Model profile {profile:?} must declare capabilities.maxOutputTokens until the metadata snapshot lands."
            ),
            Self::InvalidDecisionProfile(profile) => write!(
                f,
                "Decision profile {profile:?} must be commercial and support structured output."
            ),
        }
    }
}

impl std::error::Error for ProfileResolutionError {}

fn provider_supports_protocol(provider: &ProviderSettings, protocol: ProviderProtocol) -> bool {
    provider.protocol == Some(protocol)
        || provider
            .protocols
            .as_ref()
            .is_some_and(|protocols| protocols.contains(&protocol))
}

pub fn resolve_profile(
    nyan: &NyanSettings,
    profile_name: &str,
) -> Result<ResolvedModelProfile, ProfileResolutionError> {
    let models = nyan
        .models
        .as_ref()
        .ok_or(ProfileResolutionError::MissingSettings)?;
    let model_profile = models
        .get(profile_name)
        .ok_or_else(|| ProfileResolutionError::MissingProfile(profile_name.to_string()))?;
    let provider_name = model_profile
        .provider
        .as_deref()
        .ok_or_else(|| ProfileResolutionError::MissingProvider(profile_name.to_string()))?;
    let providers = nyan
        .providers
        .as_ref()
        .ok_or(ProfileResolutionError::MissingSettings)?;
    let provider = providers
        .get(provider_name)
        .ok_or_else(|| ProfileResolutionError::MissingProvider(provider_name.to_string()))?;
    let kind = provider
        .kind
        .ok_or_else(|| ProfileResolutionError::MissingProviderKind(provider_name.to_string()))?;
    let model = model_profile
        .model
        .clone()
        .ok_or_else(|| ProfileResolutionError::MissingModel(profile_name.to_string()))?;
    let capabilities = model_profile.capabilities.as_ref();
    let context_window = capabilities
        .and_then(|capabilities| capabilities.context_window)
        .ok_or_else(|| ProfileResolutionError::MissingContextWindow(profile_name.to_string()))?;
    let max_output_tokens = capabilities
        .and_then(|capabilities| capabilities.max_output_tokens)
        .ok_or_else(|| ProfileResolutionError::MissingMaxOutputTokens(profile_name.to_string()))?;

    if kind != ProviderKind::Anthropic {
        return Err(ProfileResolutionError::UnsupportedProvider(kind));
    }
    if !provider_supports_protocol(provider, ProviderProtocol::AnthropicMessages) {
        return Err(ProfileResolutionError::MissingProviderProtocol(
            provider_name.to_string(),
        ));
    }

    // @nyan: PR 1's temporary compatibility layer delegates credentials and
    // transport to the existing Anthropic client. PR 4 removes this branch.
    Ok(ResolvedModelProfile {
        profile: profile_name.to_string(),
        provider: provider_name.to_string(),
        provider_kind: kind,
        model,
        context_window,
        max_output_tokens,
    })
}

pub fn resolve_main_profile(
    settings: &SettingsJson,
) -> Result<ResolvedModelProfile, ProfileResolutionError> {
    let nyan = settings
        .nyan
        .as_ref()
        .ok_or(ProfileResolutionError::MissingSettings)?;
    let profile_name = nyan
        .selection
        .as_ref()
        .and_then(|selection| selection.main.as_deref())
        .ok_or(ProfileResolutionError::MissingSelection)?;
    resolve_profile(nyan, profile_name)
}

pub fn resolve_model_input(
    settings: &SettingsJson,
    input: &str,
) -> Result<String, ProfileResolutionError> {
    let Some(nyan) = settings.nyan.as_ref() else {
        return Ok(input.to_string());
    };
    let Some(models) = nyan.models.as_ref() else {
        return Ok(input.to_string());
    };
    if !models.contains_key(input) {
        return Ok(input.to_string());
    }
    resolve_profile(nyan, input).map(|profile| profile.model)
}

pub fn profile_names(nyan: Option<&NyanSettings>) -> impl Iterator<Item = &str> {
    nyan.and_then(|nyan| nyan.models.as_ref())
        .into_iter()
        .flat_map(|models| models.keys().map(String::as_str))
}

pub fn model_profile<'a>(nyan: &'a NyanSettings, name: &str) -> Option<&'a ModelProfileSettings> {
    nyan.models.as_ref()?.get(name)
}

pub fn validate_decision_profile(
    nyan: &NyanSettings,
    profile_name: &str,
) -> Result<(), ProfileResolutionError> {
    let resolved = resolve_profile(nyan, profile_name)?;
    let profile = model_profile(nyan, profile_name).expect("resolved model profile exists");
    if !resolved.provider_kind.is_commercial()
        || profile
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.structured_output)
            != Some(true)
    {
        return Err(ProfileResolutionError::InvalidDecisionProfile(
            profile_name.to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> SettingsJson {
        serde_json::from_value(serde_json::json!({
            "nyan": {
                "providers": {
                    "anthropic": {
                        "kind": "anthropic",
                        "protocol": "anthropicMessages"
                    },
                    "local": {
                        "kind": "vllm",
                        "protocol": "openaiChat"
                    }
                },
                "models": {
                    "main": {
                        "provider": "anthropic",
                        "model": "claude-sonnet-4-6",
                        "capabilities": {
                            "contextWindow": 200000,
                            "maxOutputTokens": 128000,
                            "structuredOutput": true
                        }
                    },
                    "local": {
                        "provider": "local",
                        "model": "qwen",
                        "capabilities": {
                            "contextWindow": 32768,
                            "maxOutputTokens": 4096
                        }
                    }
                },
                "selection": {"main": "main"}
            }
        }))
        .unwrap()
    }

    #[test]
    fn resolves_main_profile_and_profile_name_inputs() {
        let settings = settings();
        let resolved = resolve_main_profile(&settings).unwrap();
        assert_eq!(resolved.profile, "main");
        assert_eq!(resolved.model, "claude-sonnet-4-6");
        assert_eq!(
            resolve_model_input(&settings, "main").unwrap(),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            resolve_model_input(&settings, "provider/model").unwrap(),
            "provider/model"
        );
        assert!(validate_decision_profile(settings.nyan.as_ref().unwrap(), "main").is_ok());
    }

    #[test]
    fn decision_profile_rejects_local_and_profiles_without_structured_output() {
        let mut settings = settings();
        let nyan = settings.nyan.as_mut().unwrap();
        nyan.models
            .as_mut()
            .unwrap()
            .get_mut("main")
            .unwrap()
            .capabilities
            .as_mut()
            .unwrap()
            .structured_output = None;
        assert_eq!(
            validate_decision_profile(nyan, "main").unwrap_err(),
            ProfileResolutionError::InvalidDecisionProfile("main".to_string())
        );

        nyan.models
            .as_mut()
            .unwrap()
            .get_mut("local")
            .unwrap()
            .capabilities
            .as_mut()
            .unwrap()
            .structured_output = Some(true);
        assert_eq!(
            validate_decision_profile(nyan, "local").unwrap_err(),
            ProfileResolutionError::UnsupportedProvider(ProviderKind::Vllm)
        );
    }

    #[test]
    fn rejects_unsupported_and_incomplete_profiles_without_defaults() {
        let settings = settings();
        let nyan = settings.nyan.as_ref().unwrap();
        assert_eq!(
            resolve_profile(nyan, "local").unwrap_err(),
            ProfileResolutionError::UnsupportedProvider(ProviderKind::Vllm)
        );
        assert_eq!(
            resolve_main_profile(&SettingsJson::default()).unwrap_err(),
            ProfileResolutionError::MissingSettings
        );
        assert_eq!(
            resolve_profile(
                &NyanSettings {
                    models: Some(std::collections::BTreeMap::from([(
                        "missing".to_string(),
                        ModelProfileSettings {
                            provider: Some("anthropic".to_string()),
                            model: Some("claude".to_string()),
                            capabilities: None,
                            ..Default::default()
                        },
                    )])),
                    providers: Some(std::collections::BTreeMap::from([(
                        "anthropic".to_string(),
                        ProviderSettings {
                            kind: Some(ProviderKind::Anthropic),
                            protocol: Some(ProviderProtocol::AnthropicMessages),
                            ..Default::default()
                        },
                    )])),
                    ..Default::default()
                },
                "missing"
            )
            .unwrap_err(),
            ProfileResolutionError::MissingContextWindow("missing".to_string())
        );
    }
}
