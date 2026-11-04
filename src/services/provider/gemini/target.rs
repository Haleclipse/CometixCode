//! Where a Gemini request goes and with which credential, derived from the
//! selected profile only.
//!
//! @nyan: docs/nyan/provider-redesign.md §3.5, §7.3. No ambient Anthropic
//! configuration or beta flags apply. API keys are resolved through `utils::provider_auth`.

use std::fmt;

use crate::utils::model::profile::ResolvedModelProfile;
use crate::utils::provider_auth::reference::{
    CredentialSource, Secret, resolve_env_secret, resolve_file_secret,
};
use crate::utils::provider_auth::resolve::{ResolveError, validate_reference};
use crate::utils::settings::nyan::{ProviderKind, ProviderProtocol};

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

#[derive(Debug, Clone)]
pub enum GeminiAuth {
    None,
    ApiKey(Secret),
}

#[derive(Debug, Clone)]
pub struct GeminiTarget {
    pub profile: String,
    pub provider: String,
    pub provider_kind: ProviderKind,
    pub protocol: ProviderProtocol,
    /// Base URL without trailing slash. Model path is appended to it.
    pub base_url: String,
    pub auth: GeminiAuth,
    pub optional_env: Option<String>,
}

#[derive(Debug)]
pub enum TargetError {
    NotGemini {
        profile: String,
        protocol: ProviderProtocol,
    },
    NotConfigured {
        provider: String,
    },
    Credential {
        provider: String,
        source: ResolveError,
    },
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotGemini { profile, protocol } => write!(
                f,
                "Model profile {profile:?} uses protocol {protocol:?}; it cannot be served by the Gemini client."
            ),
            Self::NotConfigured { provider } => write!(
                f,
                "Provider {provider:?} has no credential. Set settings.nyan.providers.{provider}.credential to {{\"env\": \"GEMINI_API_KEY\"}} or another environment variable / file."
            ),
            Self::Credential { provider, source } => {
                write!(f, "Provider {provider:?} credential: {source}")
            }
        }
    }
}

impl std::error::Error for TargetError {}

fn clean_base_url(base_url: Option<&str>) -> Option<String> {
    base_url
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

/// Derive the GeminiTarget for a resolved profile.
pub async fn target_for_profile(
    profile: &ResolvedModelProfile,
) -> Result<GeminiTarget, TargetError> {
    if profile.protocol != ProviderProtocol::Gemini {
        return Err(TargetError::NotGemini {
            profile: profile.profile.clone(),
            protocol: profile.protocol,
        });
    }

    let base_url =
        clean_base_url(profile.base_url.as_deref()).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let provider = profile.provider.clone();
    let credential_error = |source| TargetError::Credential {
        provider: provider.clone(),
        source,
    };

    let reference = profile.credential.as_ref();
    let (auth, optional_env) = if let Some(reference) = reference {
        let source =
            validate_reference(profile.provider_kind, reference).map_err(credential_error)?;
        let opt_env = match &source {
            CredentialSource::Env {
                name,
                required: false,
            } => Some(name.clone()),
            _ => None,
        };
        let auth = match source {
            CredentialSource::Env { name, required } => {
                match resolve_env_secret(&name, required)
                    .map_err(|e| credential_error(ResolveError::Credential(e)))?
                {
                    Some(secret) => GeminiAuth::ApiKey(secret),
                    None => GeminiAuth::None,
                }
            }
            CredentialSource::File { path } => GeminiAuth::ApiKey(
                resolve_file_secret(&path)
                    .map_err(|e| credential_error(ResolveError::Credential(e)))?,
            ),
            CredentialSource::None => GeminiAuth::None,
            CredentialSource::OAuth {
                provider: oauth_provider,
            } => {
                return Err(credential_error(ResolveError::OAuthNotAllowed {
                    kind: profile.provider_kind,
                    oauth: oauth_provider,
                }));
            }
        };
        (auth, opt_env)
    } else {
        // Fallback: check ambient GEMINI_API_KEY
        if let Ok(secret_opt) = resolve_env_secret("GEMINI_API_KEY", false)
            && let Some(secret) = secret_opt
        {
            (GeminiAuth::ApiKey(secret), None)
        } else {
            return Err(TargetError::NotConfigured { provider });
        }
    };

    Ok(GeminiTarget {
        profile: profile.profile.clone(),
        provider: profile.provider.clone(),
        provider_kind: profile.provider_kind,
        protocol: profile.protocol,
        base_url,
        auth,
        optional_env,
    })
}
