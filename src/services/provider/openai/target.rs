//! Where an OpenAI request goes and with which credential, derived from the
//! selected profile only.
//!
//! @nyan: no ambient `OPENAI_API_KEY` or `OPENAI_BASE_URL` is read. A profile
//! names its provider's credential reference (PR 3) and optionally a base URL.
//! The OpenAI Codex sign-in (`oauth: "openai-codex"`) talks to the ChatGPT
//! backend, which only serves the Responses protocol.

use std::fmt;

use crate::utils::model::profile::ResolvedModelProfile;
use crate::utils::provider_auth::oauth::openai_codex;
use crate::utils::provider_auth::reference::{
    CredentialSource, Secret, resolve_env_secret, resolve_file_secret,
};
use crate::utils::provider_auth::resolve::{ResolveError, Resolved, Resolver, validate_reference};
use crate::utils::provider_auth::store::AuthStore;
use crate::utils::settings::nyan::ProviderProtocol;

pub const API_BASE: &str = "https://api.openai.com/v1";
pub const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Debug, Clone)]
pub enum OpenAiAuth {
    /// `none`, or an unset `optionalEnv`: send no credential.
    None,
    ApiKey(Secret),
    /// ChatGPT sign-in: bearer token plus the account the backend wants named.
    Codex {
        access: Secret,
        account_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct OpenAiTarget {
    pub profile: String,
    pub provider: String,
    pub protocol: ProviderProtocol,
    /// No trailing slash. The protocol path is appended to it.
    pub base_url: String,
    pub auth: OpenAiAuth,
}

#[derive(Debug)]
pub enum TargetError {
    /// The profile speaks another protocol.
    NotOpenAi {
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
    /// The ChatGPT backend does not serve Chat Completions.
    CodexNeedsResponses {
        profile: String,
    },
    /// The access token carries no ChatGPT account id.
    MissingAccountId {
        provider: String,
    },
    Resolver(String),
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOpenAi { profile, protocol } => write!(
                f,
                "Model profile {profile:?} uses protocol {protocol:?}; it cannot be served by the OpenAI client."
            ),
            Self::NotConfigured { provider } => write!(
                f,
                "Provider {provider:?} has no credential. Set settings.nyan.providers.{provider}.credential to {{\"env\": \"OPENAI_API_KEY\"}}, another environment variable, or {{\"oauth\": \"openai-codex\"}}."
            ),
            Self::Credential { provider, source } => {
                write!(f, "Provider {provider:?} credential: {source}")
            }
            Self::CodexNeedsResponses { profile } => write!(
                f,
                "Model profile {profile:?}: the OpenAI Codex sign-in only serves the Responses API; set the profile's protocol to openaiResponses."
            ),
            Self::MissingAccountId { provider } => write!(
                f,
                "Provider {provider:?}: the OpenAI Codex access token carries no ChatGPT account id; sign in again."
            ),
            Self::Resolver(reason) => write!(f, "credential resolver unavailable: {reason}"),
        }
    }
}

impl std::error::Error for TargetError {}

fn clean_base_url(base_url: Option<&str>) -> Option<String> {
    base_url
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

/// The target for a resolved profile, reading the user's auth store when the
/// credential is an OAuth sign-in.
pub async fn target_for_profile(
    profile: &ResolvedModelProfile,
) -> Result<OpenAiTarget, TargetError> {
    let wants_oauth = profile
        .credential
        .as_ref()
        .is_some_and(|credential| credential.oauth.is_some());
    if wants_oauth {
        let resolver = Resolver::new(AuthStore::user())
            .map_err(|error| TargetError::Resolver(error.to_string()))?;
        target_with_resolver(profile, &resolver).await
    } else {
        // Nothing here touches the network or the auth store.
        let resolver = None::<&Resolver>;
        resolve(profile, resolver).await
    }
}

pub(crate) async fn target_with_resolver(
    profile: &ResolvedModelProfile,
    resolver: &Resolver,
) -> Result<OpenAiTarget, TargetError> {
    resolve(profile, Some(resolver)).await
}

async fn resolve(
    profile: &ResolvedModelProfile,
    resolver: Option<&Resolver>,
) -> Result<OpenAiTarget, TargetError> {
    if !matches!(
        profile.protocol,
        ProviderProtocol::OpenaiChat | ProviderProtocol::OpenaiResponses
    ) {
        return Err(TargetError::NotOpenAi {
            profile: profile.profile.clone(),
            protocol: profile.protocol,
        });
    }
    let provider = profile.provider.clone();
    let credential_error = |source| TargetError::Credential {
        provider: provider.clone(),
        source,
    };
    let reference = profile
        .credential
        .as_ref()
        .ok_or_else(|| TargetError::NotConfigured {
            provider: provider.clone(),
        })?;
    let source = validate_reference(profile.provider_kind, reference).map_err(credential_error)?;

    let auth = match source {
        CredentialSource::Env { name, required } => {
            match resolve_env_secret(&name, required)
                .map_err(|e| credential_error(ResolveError::Credential(e)))?
            {
                Some(secret) => OpenAiAuth::ApiKey(secret),
                None => OpenAiAuth::None,
            }
        }
        CredentialSource::File { path } => OpenAiAuth::ApiKey(
            resolve_file_secret(&path)
                .map_err(|e| credential_error(ResolveError::Credential(e)))?,
        ),
        CredentialSource::None => OpenAiAuth::None,
        CredentialSource::OAuth { .. } => {
            if profile.protocol != ProviderProtocol::OpenaiResponses {
                return Err(TargetError::CodexNeedsResponses {
                    profile: profile.profile.clone(),
                });
            }
            let resolver = resolver.ok_or_else(|| {
                TargetError::Resolver("no credential resolver for an OAuth profile".into())
            })?;
            match resolver
                .resolve(profile.provider_kind, reference, None)
                .await
                .map_err(credential_error)?
            {
                Resolved::Secret(access) => {
                    let account_id = openai_codex::account_id_from_token(access.expose())
                        .ok_or_else(|| TargetError::MissingAccountId {
                            provider: provider.clone(),
                        })?;
                    OpenAiAuth::Codex { access, account_id }
                }
                Resolved::Absent => OpenAiAuth::None,
            }
        }
    };

    let default_base = match auth {
        OpenAiAuth::Codex { .. } => CODEX_BASE,
        _ => API_BASE,
    };
    Ok(OpenAiTarget {
        profile: profile.profile.clone(),
        provider,
        protocol: profile.protocol,
        base_url: clean_base_url(profile.base_url.as_deref())
            .unwrap_or_else(|| default_base.into()),
        auth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::model::profile::ResolvedCapabilities;
    use crate::utils::settings::nyan::{CredentialReference, ProviderKind};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn profile(
        credential: Option<CredentialReference>,
        protocol: ProviderProtocol,
    ) -> ResolvedModelProfile {
        ResolvedModelProfile {
            profile: "p".into(),
            provider: "openai".into(),
            provider_kind: ProviderKind::Openai,
            protocol,
            model: "gpt-5".into(),
            context_window: 400_000,
            max_output_tokens: 128_000,
            capabilities: ResolvedCapabilities::default(),
            base_url: None,
            credential,
        }
    }

    fn env_ref(name: &str) -> CredentialReference {
        CredentialReference {
            env: Some(name.into()),
            ..Default::default()
        }
    }

    fn lock() -> crate::utils::test_env::TestEnvGuard<'static> {
        TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[tokio::test]
    async fn key_and_default_endpoint_come_from_the_profile_not_the_environment() {
        let _lock = lock();
        let _guards = [
            EnvVarGuard::set("NYAN_OPENAI_TEST_KEY", "sk-profile"),
            EnvVarGuard::set("OPENAI_API_KEY", "sk-ambient"),
            EnvVarGuard::set("OPENAI_BASE_URL", "https://ambient.example"),
        ];
        let target = target_for_profile(&profile(
            Some(env_ref("NYAN_OPENAI_TEST_KEY")),
            ProviderProtocol::OpenaiChat,
        ))
        .await
        .unwrap();
        let OpenAiAuth::ApiKey(key) = &target.auth else {
            panic!("{:?}", target.auth)
        };
        assert_eq!(key.expose(), "sk-profile");
        assert_eq!(target.base_url, API_BASE);
    }

    #[tokio::test]
    async fn a_missing_credential_is_an_error_not_an_ambient_fallback() {
        let _lock = lock();
        let _ambient = EnvVarGuard::set("OPENAI_API_KEY", "sk-ambient");
        let error = target_for_profile(&profile(None, ProviderProtocol::OpenaiChat))
            .await
            .unwrap_err();
        assert!(
            matches!(error, TargetError::NotConfigured { .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_unset_optional_key_means_no_credential_and_a_set_one_is_used() {
        let _lock = lock();
        let _unset = EnvVarGuard::unset("NYAN_OPENAI_TEST_OPTIONAL");
        let optional = CredentialReference {
            optional_env: Some("NYAN_OPENAI_TEST_OPTIONAL".into()),
            ..Default::default()
        };
        let target = target_for_profile(&profile(
            Some(optional.clone()),
            ProviderProtocol::OpenaiChat,
        ))
        .await
        .unwrap();
        assert!(matches!(target.auth, OpenAiAuth::None));
    }

    #[tokio::test]
    async fn an_anthropic_protocol_profile_is_refused() {
        let error = target_for_profile(&profile(None, ProviderProtocol::AnthropicMessages))
            .await
            .unwrap_err();
        assert!(matches!(error, TargetError::NotOpenAi { .. }));
    }

    #[tokio::test]
    async fn the_codex_sign_in_is_refused_for_chat_completions() {
        let oauth = CredentialReference {
            oauth: Some("openai-codex".into()),
            ..Default::default()
        };
        let error = target_for_profile(&profile(Some(oauth), ProviderProtocol::OpenaiChat))
            .await
            .unwrap_err();
        assert!(
            matches!(error, TargetError::CodexNeedsResponses { .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn base_url_is_trimmed_and_blank_means_the_default() {
        let _lock = lock();
        let _key = EnvVarGuard::set("NYAN_OPENAI_TEST_KEY2", "k");
        let mut p = profile(
            Some(env_ref("NYAN_OPENAI_TEST_KEY2")),
            ProviderProtocol::OpenaiChat,
        );
        p.base_url = Some(" https://gw.example/v1/ ".into());
        assert_eq!(
            target_for_profile(&p).await.unwrap().base_url,
            "https://gw.example/v1"
        );
        p.base_url = Some("  ".into());
        assert_eq!(target_for_profile(&p).await.unwrap().base_url, API_BASE);
    }
}
