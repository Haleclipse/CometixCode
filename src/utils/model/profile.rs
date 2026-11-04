//! Profile selection for the provider-neutral settings shape.
//!
//! Only provider kinds whose adapter has landed resolve; the others are
//! rejected with a clear error until their adapter PR.

use std::fmt;

use crate::utils::settings::SettingsJson;
use crate::utils::settings::nyan::{
    CacheRetention, CredentialReference, ModelProfileSettings, NyanSettings, OpenaiCompatSettings,
    ProviderKind, ProviderProtocol, ProviderSettings,
};

/// Capabilities after merging the metadata snapshot under the profile's
/// explicit fields. A capability neither declares is "not supported" (§3.2);
/// nothing here is probed or guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResolvedCapabilities {
    pub tools: bool,
    pub images: bool,
    pub thinking: bool,
    pub structured_output: bool,
    pub streaming: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModelProfile {
    pub profile: String,
    pub provider: String,
    pub provider_kind: ProviderKind,
    /// The one wire protocol this profile speaks.
    pub protocol: ProviderProtocol,
    pub model: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub capabilities: ResolvedCapabilities,
    pub base_url: Option<String>,
    pub credential: Option<CredentialReference>,
    pub compat: Option<OpenaiCompatSettings>,
    pub cache_retention: CacheRetention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileResolutionError {
    MissingSettings,
    MissingSelection,
    MissingProfile(String),
    MissingProvider(String),
    MissingProviderKind(String),
    MissingProviderProtocol(String),
    /// The provider offers several protocols and the profile names none.
    AmbiguousProtocol {
        profile: String,
        provider: String,
    },
    /// The profile names a protocol its provider does not declare, or one the
    /// provider kind cannot speak.
    ProtocolNotOffered {
        profile: String,
        provider: String,
        protocol: ProviderProtocol,
    },
    UnsupportedProvider(ProviderKind),
    /// A local server kind without its own endpoint. Never defaults to a
    /// commercial base URL.
    MissingBaseUrl(String),
    /// `compat` is set on a provider whose profile speaks another protocol.
    CompatNotForProtocol {
        profile: String,
        protocol: ProviderProtocol,
    },
    MissingModel(String),
    MissingContextWindow(String),
    MissingMaxOutputTokens(String),
    InvalidDecisionProfile(String),
    /// A decision tier named a profile on a local server. Decision providers
    /// accept commercial profiles only (design §5.3).
    LocalDecisionProfile(String),
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
            Self::AmbiguousProtocol { profile, provider } => write!(
                f,
                "Provider {provider:?} offers several protocols; set settings.nyan.models.{profile}.protocol to one of them (openaiChat or openaiResponses)."
            ),
            Self::ProtocolNotOffered {
                profile,
                provider,
                protocol,
            } => write!(
                f,
                "Model profile {profile:?} asks for protocol {protocol:?}, which provider {provider:?} does not offer."
            ),
            Self::UnsupportedProvider(kind) => write!(
                f,
                "Provider kind {kind:?} has no adapter yet; only anthropic, openai, gemini, vllm and ollama profiles can send requests."
            ),
            Self::MissingBaseUrl(provider) => write!(
                f,
                "Provider {provider:?} is a local server and must set baseUrl; a local profile never falls back to a commercial endpoint."
            ),
            Self::CompatNotForProtocol { profile, protocol } => write!(
                f,
                "Model profile {profile:?} speaks {protocol:?}, but its provider sets compat; compat options only apply to openaiChat."
            ),
            Self::MissingModel(profile) => write!(
                f,
                "Model profile {profile:?} is missing its provider model id."
            ),
            Self::MissingContextWindow(profile) => write!(
                f,
                "Model profile {profile:?} must declare capabilities.contextWindow when not provided by the metadata snapshot."
            ),
            Self::MissingMaxOutputTokens(profile) => write!(
                f,
                "Model profile {profile:?} must declare capabilities.maxOutputTokens when not provided by the metadata snapshot."
            ),
            Self::InvalidDecisionProfile(profile) => write!(
                f,
                "Decision profile {profile:?} must be commercial and support structured output."
            ),
            Self::LocalDecisionProfile(profile) => write!(
                f,
                "Decision profile {profile:?} is served by a local provider; decision tiers accept commercial profiles only."
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

fn declared_protocols(provider: &ProviderSettings) -> Vec<ProviderProtocol> {
    let mut all: Vec<ProviderProtocol> = provider.protocol.into_iter().collect();
    for protocol in provider.protocols.iter().flatten() {
        if !all.contains(protocol) {
            all.push(*protocol);
        }
    }
    all
}

/// Protocols a provider kind has an adapter for. A kind outside this table
/// cannot send requests yet (Gemini is PR 6). Local servers speak only the
/// OpenAI-compatible Chat Completions protocol; there is no Ollama native one.
fn kind_protocols(kind: ProviderKind) -> &'static [ProviderProtocol] {
    match kind {
        ProviderKind::Anthropic => &[ProviderProtocol::AnthropicMessages],
        ProviderKind::Openai => &[
            ProviderProtocol::OpenaiChat,
            ProviderProtocol::OpenaiResponses,
        ],
        ProviderKind::Gemini => &[ProviderProtocol::Gemini],
        ProviderKind::Vllm | ProviderKind::Ollama => &[ProviderProtocol::OpenaiChat],
        _ => &[],
    }
}

/// The one protocol a profile speaks. A profile never mixes protocols: the
/// choice is made here once and every later stage reads it from the result.
fn select_protocol(
    profile_name: &str,
    provider_name: &str,
    provider: &ProviderSettings,
    kind: ProviderKind,
    requested: Option<ProviderProtocol>,
) -> Result<ProviderProtocol, ProfileResolutionError> {
    let declared = declared_protocols(provider);
    let offered = kind_protocols(kind);
    let not_offered = |protocol| ProfileResolutionError::ProtocolNotOffered {
        profile: profile_name.to_string(),
        provider: provider_name.to_string(),
        protocol,
    };
    let chosen = match requested {
        Some(protocol) => protocol,
        None => match declared.as_slice() {
            [] => {
                return Err(ProfileResolutionError::MissingProviderProtocol(
                    provider_name.to_string(),
                ));
            }
            [only] => *only,
            _ => {
                return Err(ProfileResolutionError::AmbiguousProtocol {
                    profile: profile_name.to_string(),
                    provider: provider_name.to_string(),
                });
            }
        },
    };
    if !provider_supports_protocol(provider, chosen) || !offered.contains(&chosen) {
        return Err(not_offered(chosen));
    }
    Ok(chosen)
}

fn flag(value: Option<&str>, on: &str) -> Option<bool> {
    value.map(|value| value == on)
}

/// The profile's own value, else the snapshot's.
fn pick_str<'a>(own: Option<&'a String>, snapshot: Option<&'a String>) -> Option<&'a str> {
    own.or(snapshot).map(String::as_str)
}

fn resolve_capabilities(
    profile: &ModelProfileSettings,
    metadata: Option<&crate::utils::model::metadata::ModelMetadataEntry>,
) -> ResolvedCapabilities {
    let explicit = profile.capabilities.as_ref();
    let snapshot = metadata.map(|entry| &entry.capabilities);
    ResolvedCapabilities {
        tools: flag(
            pick_str(
                explicit.and_then(|c| c.tools.as_ref()),
                snapshot.and_then(|c| c.tools.as_ref()),
            ),
            "native",
        )
        .unwrap_or(false),
        images: flag(
            pick_str(
                explicit.and_then(|c| c.images.as_ref()),
                snapshot.and_then(|c| c.images.as_ref()),
            ),
            "input",
        )
        .unwrap_or(false),
        thinking: pick_str(
            explicit.and_then(|c| c.thinking.as_ref()),
            snapshot.and_then(|c| c.thinking.as_ref()),
        )
        .is_some_and(|value| value != "none"),
        structured_output: explicit
            .and_then(|c| c.structured_output)
            .or(snapshot.and_then(|c| c.structured_output))
            .unwrap_or(false),
        streaming: explicit
            .and_then(|c| c.streaming)
            .or(snapshot.and_then(|c| c.streaming))
            .unwrap_or(false),
    }
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
    let metadata = crate::utils::model::metadata::metadata_provider_for_kind(kind).and_then(
        |snapshot_provider| {
            crate::utils::model::metadata::find_model_metadata(snapshot_provider, &model)
        },
    );
    let capabilities = model_profile.capabilities.as_ref();
    let context_window = capabilities
        .and_then(|capabilities| capabilities.context_window)
        .or_else(|| metadata.map(|m| m.context_window))
        .ok_or_else(|| ProfileResolutionError::MissingContextWindow(profile_name.to_string()))?;
    let max_output_tokens = capabilities
        .and_then(|capabilities| capabilities.max_output_tokens)
        .or_else(|| metadata.map(|m| m.max_output_tokens))
        .ok_or_else(|| ProfileResolutionError::MissingMaxOutputTokens(profile_name.to_string()))?;

    if kind_protocols(kind).is_empty() {
        return Err(ProfileResolutionError::UnsupportedProvider(kind));
    }
    let protocol = select_protocol(
        profile_name,
        provider_name,
        provider,
        kind,
        model_profile.protocol,
    )?;
    if kind.is_local()
        && provider
            .base_url
            .as_deref()
            .is_none_or(|url| url.trim().is_empty())
    {
        return Err(ProfileResolutionError::MissingBaseUrl(
            provider_name.to_string(),
        ));
    }
    if provider.compat.is_some() && protocol != ProviderProtocol::OpenaiChat {
        return Err(ProfileResolutionError::CompatNotForProtocol {
            profile: profile_name.to_string(),
            protocol,
        });
    }

    Ok(ResolvedModelProfile {
        profile: profile_name.to_string(),
        provider: provider_name.to_string(),
        provider_kind: kind,
        protocol,
        model,
        context_window,
        max_output_tokens,
        capabilities: resolve_capabilities(model_profile, metadata),
        base_url: provider.base_url.clone(),
        credential: provider.credential.clone(),
        compat: provider.compat.clone(),
        cache_retention: model_profile.cache_retention(),
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

/// The built-in kind of the provider a profile names, if both exist.
fn profile_provider_kind(nyan: &NyanSettings, profile_name: &str) -> Option<ProviderKind> {
    let provider = model_profile(nyan, profile_name)?.provider.as_deref()?;
    nyan.providers.as_ref()?.get(provider)?.kind
}

pub fn validate_decision_profile(
    nyan: &NyanSettings,
    profile_name: &str,
) -> Result<(), ProfileResolutionError> {
    // Checked before resolution so a local profile is refused for being local,
    // whatever else is wrong with it.
    if profile_provider_kind(nyan, profile_name).is_some_and(ProviderKind::is_local) {
        return Err(ProfileResolutionError::LocalDecisionProfile(
            profile_name.to_string(),
        ));
    }
    if !profile_provider_kind(nyan, profile_name).is_some_and(ProviderKind::is_commercial) {
        return Err(ProfileResolutionError::InvalidDecisionProfile(
            profile_name.to_string(),
        ));
    }
    resolve_profile(nyan, profile_name)?;
    let profile = model_profile(nyan, profile_name).expect("resolved model profile exists");
    if profile
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFieldDiagnosis {
    pub field_name: &'static str,
    pub value: String,
    pub is_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDiagnosis {
    pub profile_name: String,
    pub provider: String,
    pub model: String,
    pub fields: Vec<ProfileFieldDiagnosis>,
}

fn profile_metadata(
    nyan: &NyanSettings,
    profile: &ModelProfileSettings,
) -> Option<&'static crate::utils::model::metadata::ModelMetadataEntry> {
    let provider = profile.provider.as_deref()?;
    let kind = nyan.providers.as_ref()?.get(provider)?.kind?;
    let snapshot_provider = crate::utils::model::metadata::metadata_provider_for_kind(kind)?;
    crate::utils::model::metadata::find_model_metadata(snapshot_provider, profile.model.as_deref()?)
}

fn push_cost_field(
    fields: &mut Vec<ProfileFieldDiagnosis>,
    field_name: &'static str,
    override_value: Option<f64>,
    snapshot_value: Option<f64>,
) {
    if let Some(value) = override_value {
        fields.push(ProfileFieldDiagnosis {
            field_name,
            value: value.to_string(),
            is_override: true,
        });
    } else if let Some(value) = snapshot_value {
        fields.push(ProfileFieldDiagnosis {
            field_name,
            value: value.to_string(),
            is_override: false,
        });
    }
}

pub fn diagnose_profile(
    profile_name: &str,
    profile: &ModelProfileSettings,
    metadata: Option<&crate::utils::model::metadata::ModelMetadataEntry>,
) -> ProfileDiagnosis {
    let provider = profile.provider.as_deref().unwrap_or("unknown");
    let model = profile.model.as_deref().unwrap_or("unknown");

    let mut fields = Vec::new();

    if let Some(display_name) = &profile.display_name {
        fields.push(ProfileFieldDiagnosis {
            field_name: "displayName",
            value: display_name.clone(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        fields.push(ProfileFieldDiagnosis {
            field_name: "displayName",
            value: m.display_name.clone(),
            is_override: false,
        });
    }

    let caps = profile.capabilities.as_ref();

    if let Some(cw) = caps.and_then(|c| c.context_window) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "contextWindow",
            value: cw.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        fields.push(ProfileFieldDiagnosis {
            field_name: "contextWindow",
            value: m.context_window.to_string(),
            is_override: false,
        });
    }

    if let Some(mo) = caps.and_then(|c| c.max_output_tokens) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "maxOutputTokens",
            value: mo.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        fields.push(ProfileFieldDiagnosis {
            field_name: "maxOutputTokens",
            value: m.max_output_tokens.to_string(),
            is_override: false,
        });
    }

    if let Some(tools) = caps.and_then(|c| c.tools.as_deref()) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "tools",
            value: tools.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        if let Some(tools) = &m.capabilities.tools {
            fields.push(ProfileFieldDiagnosis {
                field_name: "tools",
                value: tools.clone(),
                is_override: false,
            });
        }
    }

    if let Some(thinking) = caps.and_then(|c| c.thinking.as_deref()) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "thinking",
            value: thinking.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        if let Some(thinking) = &m.capabilities.thinking {
            fields.push(ProfileFieldDiagnosis {
                field_name: "thinking",
                value: thinking.clone(),
                is_override: false,
            });
        }
    }

    if let Some(images) = caps.and_then(|c| c.images.as_deref()) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "images",
            value: images.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        if let Some(images) = &m.capabilities.images {
            fields.push(ProfileFieldDiagnosis {
                field_name: "images",
                value: images.clone(),
                is_override: false,
            });
        }
    }

    if let Some(so) = caps.and_then(|c| c.structured_output) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "structuredOutput",
            value: so.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        if let Some(so) = m.capabilities.structured_output {
            fields.push(ProfileFieldDiagnosis {
                field_name: "structuredOutput",
                value: so.to_string(),
                is_override: false,
            });
        }
    }

    if let Some(streaming) = caps.and_then(|c| c.streaming) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "streaming",
            value: streaming.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        if let Some(streaming) = m.capabilities.streaming {
            fields.push(ProfileFieldDiagnosis {
                field_name: "streaming",
                value: streaming.to_string(),
                is_override: false,
            });
        }
    }

    if let Some(pc) = caps.and_then(|c| c.prompt_cache.as_deref()) {
        fields.push(ProfileFieldDiagnosis {
            field_name: "promptCache",
            value: pc.to_string(),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        if let Some(pc) = &m.capabilities.prompt_cache {
            fields.push(ProfileFieldDiagnosis {
                field_name: "promptCache",
                value: pc.clone(),
                is_override: false,
            });
        }
    }

    if let Some(input) = &profile.input {
        fields.push(ProfileFieldDiagnosis {
            field_name: "input",
            value: input.join(","),
            is_override: true,
        });
    } else if let Some(m) = metadata {
        fields.push(ProfileFieldDiagnosis {
            field_name: "input",
            value: m.input.join(","),
            is_override: false,
        });
    }

    let cost = profile.cost.as_ref();
    let snapshot_cost = metadata.and_then(|m| m.cost.as_ref());
    push_cost_field(
        &mut fields,
        "cost.input",
        cost.and_then(|c| c.input),
        snapshot_cost.map(|c| c.input),
    );
    push_cost_field(
        &mut fields,
        "cost.output",
        cost.and_then(|c| c.output),
        snapshot_cost.map(|c| c.output),
    );
    push_cost_field(
        &mut fields,
        "cost.cacheRead",
        cost.and_then(|c| c.cache_read),
        snapshot_cost.and_then(|c| c.cache_read),
    );
    push_cost_field(
        &mut fields,
        "cost.cacheWrite",
        cost.and_then(|c| c.cache_write),
        snapshot_cost.and_then(|c| c.cache_write),
    );

    ProfileDiagnosis {
        profile_name: profile_name.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        fields,
    }
}

pub fn diagnose_profiles(nyan: &NyanSettings) -> Vec<ProfileDiagnosis> {
    let Some(models) = &nyan.models else {
        return Vec::new();
    };
    models
        .iter()
        .map(|(name, profile)| diagnose_profile(name, profile, profile_metadata(nyan, profile)))
        .collect()
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
            ProfileResolutionError::LocalDecisionProfile("local".to_string())
        );
    }

    #[test]
    fn rejects_unsupported_and_incomplete_profiles_without_defaults() {
        let settings = settings();
        let nyan = settings.nyan.as_ref().unwrap();
        // The fixture's local provider has no baseUrl; it must not borrow one.
        assert_eq!(
            resolve_profile(nyan, "local").unwrap_err(),
            ProfileResolutionError::MissingBaseUrl("local".to_string())
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

    fn openai_nyan(model_profile: serde_json::Value) -> NyanSettings {
        serde_json::from_value(serde_json::json!({
            "providers": {
                "openai": {
                    "kind": "openai",
                    "protocols": ["openaiChat", "openaiResponses"],
                    "credential": {"env": "OPENAI_API_KEY"}
                },
                "chat-only": {
                    "kind": "openai",
                    "protocol": "openaiChat"
                }
            },
            "models": { "p": model_profile },
            "selection": {"main": "p"}
        }))
        .unwrap()
    }

    #[test]
    fn a_profile_speaks_exactly_the_protocol_it_names() {
        for (name, expected) in [
            ("openaiChat", ProviderProtocol::OpenaiChat),
            ("openaiResponses", ProviderProtocol::OpenaiResponses),
        ] {
            let nyan = openai_nyan(serde_json::json!({
                "provider": "openai", "model": "gpt-5", "protocol": name
            }));
            assert_eq!(resolve_profile(&nyan, "p").unwrap().protocol, expected);
        }
    }

    #[test]
    fn a_provider_with_two_protocols_needs_the_profile_to_choose() {
        let nyan = openai_nyan(serde_json::json!({"provider": "openai", "model": "gpt-5"}));
        assert_eq!(
            resolve_profile(&nyan, "p").unwrap_err(),
            ProfileResolutionError::AmbiguousProtocol {
                profile: "p".into(),
                provider: "openai".into()
            }
        );
    }

    #[test]
    fn a_single_protocol_provider_needs_no_choice_but_refuses_another() {
        let nyan = openai_nyan(serde_json::json!({"provider": "chat-only", "model": "gpt-5"}));
        assert_eq!(
            resolve_profile(&nyan, "p").unwrap().protocol,
            ProviderProtocol::OpenaiChat
        );
        let nyan = openai_nyan(serde_json::json!({
            "provider": "chat-only", "model": "gpt-5", "protocol": "openaiResponses"
        }));
        assert!(matches!(
            resolve_profile(&nyan, "p").unwrap_err(),
            ProfileResolutionError::ProtocolNotOffered { .. }
        ));
    }

    #[test]
    fn local_kinds_speak_only_openai_chat_and_must_name_an_endpoint() {
        let local = |protocol: &str, base_url: Option<&str>| -> NyanSettings {
            let mut provider = serde_json::json!({"kind": "ollama", "protocol": protocol});
            if let Some(url) = base_url {
                provider["baseUrl"] = serde_json::json!(url);
            }
            serde_json::from_value(serde_json::json!({
                "providers": {"local": provider},
                "models": {"p": {
                    "provider": "local", "model": "qwen3",
                    "capabilities": {"contextWindow": 32768, "maxOutputTokens": 4096}
                }}
            }))
            .unwrap()
        };
        let ok =
            resolve_profile(&local("openaiChat", Some("http://127.0.0.1:11434/v1")), "p").unwrap();
        assert_eq!(ok.protocol, ProviderProtocol::OpenaiChat);
        assert_eq!(ok.provider_kind, ProviderKind::Ollama);
        assert!(!ok.provider_kind.is_commercial());

        assert!(matches!(
            resolve_profile(&local("openaiResponses", Some("http://x/v1")), "p").unwrap_err(),
            ProfileResolutionError::ProtocolNotOffered { .. }
        ));
        assert_eq!(
            resolve_profile(&local("openaiChat", None), "p").unwrap_err(),
            ProfileResolutionError::MissingBaseUrl("local".to_string())
        );
        assert_eq!(
            resolve_profile(&local("openaiChat", Some("  ")), "p").unwrap_err(),
            ProfileResolutionError::MissingBaseUrl("local".to_string())
        );
    }

    #[test]
    fn compat_is_only_meaningful_on_openai_chat() {
        let nyan: NyanSettings = serde_json::from_value(serde_json::json!({
            "providers": {"openai": {
                "kind": "openai",
                "protocols": ["openaiChat", "openaiResponses"],
                "compat": {"includeUsage": false}
            }},
            "models": {"p": {"provider": "openai", "model": "gpt-5", "protocol": "openaiResponses"}}
        }))
        .unwrap();
        assert!(matches!(
            resolve_profile(&nyan, "p").unwrap_err(),
            ProfileResolutionError::CompatNotForProtocol { .. }
        ));
    }

    #[test]
    fn a_decision_tier_refuses_a_local_profile_before_anything_else_is_checked() {
        let nyan: NyanSettings = serde_json::from_value(serde_json::json!({
            "providers": {"local": {"kind": "vllm", "protocol": "openaiChat"}},
            "models": {"p": {"provider": "local", "model": "qwen"}}
        }))
        .unwrap();
        assert_eq!(
            validate_decision_profile(&nyan, "p").unwrap_err(),
            ProfileResolutionError::LocalDecisionProfile("p".to_string())
        );
    }

    #[test]
    fn a_kind_cannot_speak_a_protocol_it_has_no_adapter_for() {
        let nyan: NyanSettings = serde_json::from_value(serde_json::json!({
            "providers": {
                "anthropic": {
                    "kind": "anthropic",
                    "protocols": ["anthropicMessages", "openaiChat"]
                }
            },
            "models": {"p": {
                "provider": "anthropic", "model": "claude-sonnet-4-6", "protocol": "openaiChat"
            }}
        }))
        .unwrap();
        assert!(matches!(
            resolve_profile(&nyan, "p").unwrap_err(),
            ProfileResolutionError::ProtocolNotOffered { .. }
        ));
    }

    #[test]
    fn undeclared_capabilities_are_not_supported_and_declared_ones_override_the_snapshot() {
        // gpt-5 is in the snapshot with tools/images/thinking/structured output.
        let from_snapshot = resolve_profile(
            &openai_nyan(serde_json::json!({
                "provider": "openai", "model": "gpt-5", "protocol": "openaiChat"
            })),
            "p",
        )
        .unwrap();
        assert!(from_snapshot.capabilities.tools);
        assert!(from_snapshot.capabilities.images);
        assert!(from_snapshot.capabilities.thinking);
        assert!(from_snapshot.capabilities.structured_output);

        let overridden = resolve_profile(
            &openai_nyan(serde_json::json!({
                "provider": "openai", "model": "gpt-5", "protocol": "openaiChat",
                "capabilities": {"tools": "none", "images": "none", "thinking": "none"}
            })),
            "p",
        )
        .unwrap();
        assert!(!overridden.capabilities.tools);
        assert!(!overridden.capabilities.images);
        assert!(!overridden.capabilities.thinking);
        assert!(overridden.capabilities.structured_output);

        // Not in the snapshot and declared nowhere: nothing is assumed.
        let unknown = resolve_profile(
            &openai_nyan(serde_json::json!({
                "provider": "openai", "model": "no-such-model", "protocol": "openaiChat",
                "capabilities": {"contextWindow": 8000, "maxOutputTokens": 1000}
            })),
            "p",
        )
        .unwrap();
        assert_eq!(unknown.capabilities, ResolvedCapabilities::default());
    }

    #[test]
    fn inherits_capabilities_from_metadata_snapshot_when_omitted() {
        let nyan: NyanSettings = serde_json::from_value(serde_json::json!({
            "providers": {
                "anthropic": {
                    "kind": "anthropic",
                    "protocol": "anthropicMessages"
                }
            },
            "models": {
                "sonnet": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-6"
                }
            },
            "selection": {"main": "sonnet"}
        }))
        .unwrap();

        let resolved = resolve_profile(&nyan, "sonnet").unwrap();
        assert_eq!(resolved.profile, "sonnet");
        assert_eq!(resolved.model, "claude-sonnet-4-6");
        // Must come from metadata snapshot (1000000 / 128000)
        assert_eq!(resolved.context_window, 1_000_000);
        assert_eq!(resolved.max_output_tokens, 128_000);
    }

    #[test]
    fn profile_capabilities_override_metadata_snapshot() {
        let nyan: NyanSettings = serde_json::from_value(serde_json::json!({
            "providers": {
                "anthropic": {
                    "kind": "anthropic",
                    "protocol": "anthropicMessages"
                }
            },
            "models": {
                "sonnet": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-6",
                    "capabilities": {
                        "contextWindow": 500000,
                        "maxOutputTokens": 64000
                    }
                }
            },
            "selection": {"main": "sonnet"}
        }))
        .unwrap();

        let resolved = resolve_profile(&nyan, "sonnet").unwrap();
        assert_eq!(resolved.context_window, 500_000);
        assert_eq!(resolved.max_output_tokens, 64_000);
    }

    #[test]
    fn diagnose_profiles_detects_overrides_and_snapshot_sources() {
        let nyan: NyanSettings = serde_json::from_value(serde_json::json!({
            "providers": {
                "anthropic": {
                    "kind": "anthropic",
                    "protocol": "anthropicMessages"
                }
            },
            "models": {
                "sonnet-overridden": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-6",
                    "displayName": "My Custom Sonnet",
                    "capabilities": {
                        "contextWindow": 250000
                    }
                },
                "sonnet-pure": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-6"
                }
            }
        }))
        .unwrap();

        let diagnoses = diagnose_profiles(&nyan);
        assert_eq!(diagnoses.len(), 2);

        let overridden = diagnoses
            .iter()
            .find(|d| d.profile_name == "sonnet-overridden")
            .unwrap();
        let cw_field = overridden
            .fields
            .iter()
            .find(|f| f.field_name == "contextWindow")
            .unwrap();
        assert_eq!(cw_field.value, "250000");
        assert!(cw_field.is_override);

        let dn_field = overridden
            .fields
            .iter()
            .find(|f| f.field_name == "displayName")
            .unwrap();
        assert_eq!(dn_field.value, "My Custom Sonnet");
        assert!(dn_field.is_override);

        let pure = diagnoses
            .iter()
            .find(|d| d.profile_name == "sonnet-pure")
            .unwrap();
        let pure_cw = pure
            .fields
            .iter()
            .find(|f| f.field_name == "contextWindow")
            .unwrap();
        assert_eq!(pure_cw.value, "1000000");
        assert!(!pure_cw.is_override);
    }

    #[test]
    fn local_profile_does_not_inherit_snapshot_metadata_for_anthropic_model_id() {
        let mut settings = settings();
        let nyan = settings.nyan.as_mut().unwrap();
        nyan.models.as_mut().unwrap().insert(
            "local-claude".to_string(),
            ModelProfileSettings {
                provider: Some("local".to_string()),
                model: Some("claude-sonnet-4-6".to_string()),
                ..Default::default()
            },
        );
        let err = resolve_profile(nyan, "local-claude").unwrap_err();
        assert_eq!(
            err,
            ProfileResolutionError::MissingContextWindow("local-claude".to_string())
        );
    }

    #[test]
    fn cost_and_input_overrides_are_reported_with_their_source() {
        let nyan: NyanSettings = serde_json::from_value(serde_json::json!({
            "providers": {
                "anthropic": {"kind": "anthropic", "protocol": "anthropicMessages"}
            },
            "models": {
                "sonnet": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-6",
                    "input": ["text"],
                    "cost": {"input": 9.5}
                }
            }
        }))
        .unwrap();

        let diagnoses = diagnose_profiles(&nyan);
        let fields = &diagnoses[0].fields;
        let find = |name: &str| fields.iter().find(|f| f.field_name == name).unwrap();
        assert_eq!(find("input").value, "text");
        assert!(find("input").is_override);
        assert_eq!(find("cost.input").value, "9.5");
        assert!(find("cost.input").is_override);
        assert!(!find("cost.output").is_override);
    }
}
