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
use crate::utils::settings::nyan::{
    CacheRetention, MaxTokensField, OpenaiCompatSettings, ProviderKind, ProviderProtocol,
};

pub const API_BASE: &str = "https://api.openai.com/v1";
pub const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";

/// Dialect options for an `openaiChat` endpoint with the kind's defaults
/// applied. Settings say what the server takes; nothing is probed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compat {
    pub max_tokens_field: MaxTokensField,
    pub include_usage: bool,
    pub cache_control_format: Option<String>,
    pub requires_reasoning_content: bool,
}

impl Compat {
    pub fn resolve(kind: ProviderKind, settings: Option<&OpenaiCompatSettings>) -> Self {
        Self::resolve_full(kind, None, None, settings)
    }

    pub fn resolve_full(
        kind: ProviderKind,
        provider: Option<&str>,
        model: Option<&str>,
        settings: Option<&OpenaiCompatSettings>,
    ) -> Self {
        // Local servers take `max_tokens`, the one cap name both vLLM and Ollama
        // accept; the OpenAI kind keeps its own `max_completion_tokens`.
        let default_field = if kind.is_local() {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        };
        let cache_control_format = settings
            .and_then(|s| s.cache_control_format.clone())
            .or_else(|| {
                if let (Some(p), Some(m)) = (provider, model) {
                    if p.eq_ignore_ascii_case("openrouter")
                        && (m.starts_with("anthropic/") || m.starts_with("google/"))
                    {
                        return Some("anthropic".to_string());
                    }
                }
                None
            });
        let requires_reasoning_content = settings
            .and_then(|s| s.requires_reasoning_content)
            .unwrap_or_else(|| {
                let p = provider.unwrap_or_default().to_lowercase();
                let m = model.unwrap_or_default().to_lowercase();
                p.contains("deepseek")
                    || m.contains("deepseek")
                    || p.contains("kimi")
                    || m.contains("kimi")
                    || p.contains("moonshot")
                    || m.contains("moonshot")
            });

        Self {
            max_tokens_field: settings
                .and_then(|s| s.max_tokens_field)
                .unwrap_or(default_field),
            include_usage: settings.and_then(|s| s.include_usage).unwrap_or(true),
            cache_control_format,
            requires_reasoning_content,
        }
    }
}

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
    pub provider_kind: ProviderKind,
    pub protocol: ProviderProtocol,
    /// No trailing slash. The protocol path is appended to it.
    pub base_url: String,
    pub auth: OpenAiAuth,
    /// The `optionalEnv` variable this credential reads, so an unset one can be
    /// named when the server answers 401.
    pub optional_env: Option<String>,
    pub compat: Compat,
    pub cache_retention: CacheRetention,
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
    /// A local server with no endpoint. It must never reach a commercial one.
    MissingBaseUrl {
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
            Self::MissingBaseUrl { provider } => write!(
                f,
                "Provider {provider:?} is a local server and must set baseUrl; it never falls back to a commercial endpoint."
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
    // Before any credential is read: a local profile must not fall through to
    // the commercial default endpoint below.
    if profile.provider_kind.is_local() && clean_base_url(profile.base_url.as_deref()).is_none() {
        return Err(TargetError::MissingBaseUrl {
            provider: profile.provider.clone(),
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
    let optional_env = match &source {
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
        provider_kind: profile.provider_kind,
        protocol: profile.protocol,
        base_url: clean_base_url(profile.base_url.as_deref())
            .unwrap_or_else(|| default_base.into()),
        auth,
        optional_env,
        compat: Compat::resolve_full(
            profile.provider_kind,
            Some(&profile.provider),
            Some(&profile.model),
            profile.compat.as_ref(),
        ),
        cache_retention: profile.cache_retention,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::model::profile::ResolvedCapabilities;
    use crate::utils::settings::nyan::{CredentialReference, MaxTokensField, ProviderKind};
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
            compat: None,
            cache_retention: CacheRetention::default(),
            effort: None,
            extra: None,
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
    async fn commercial_openai_profile_rejects_optional_env() {
        let optional = CredentialReference {
            optional_env: Some("NYAN_OPENAI_TEST_OPTIONAL".into()),
            ..Default::default()
        };
        let error = target_for_profile(&profile(Some(optional), ProviderProtocol::OpenaiChat))
            .await
            .unwrap_err();
        assert!(matches!(error, TargetError::Credential { .. }), "{error}");
        assert!(error.to_string().contains("optionalEnv"), "{error}");
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
    async fn a_local_profile_with_no_optional_key_sends_none_and_names_the_variable() {
        let _lock = lock();
        let _unset = EnvVarGuard::unset("NYAN_OPENAI_TEST_LOCAL_KEY");
        let mut p = profile(
            Some(CredentialReference {
                optional_env: Some("NYAN_OPENAI_TEST_LOCAL_KEY".into()),
                ..Default::default()
            }),
            ProviderProtocol::OpenaiChat,
        );
        p.provider_kind = ProviderKind::Vllm;
        p.base_url = Some("http://127.0.0.1:8000/v1".into());
        let target = target_for_profile(&p).await.unwrap();
        assert!(matches!(target.auth, OpenAiAuth::None));
        assert_eq!(
            target.optional_env.as_deref(),
            Some("NYAN_OPENAI_TEST_LOCAL_KEY")
        );
        assert_eq!(target.base_url, "http://127.0.0.1:8000/v1");
        assert_eq!(target.compat.max_tokens_field, MaxTokensField::MaxTokens);
        assert!(target.compat.include_usage);
    }

    #[tokio::test]
    async fn a_local_profile_with_optional_key_uses_key_when_set() {
        let _lock = lock();
        let _set = EnvVarGuard::set("NYAN_OPENAI_TEST_LOCAL_KEY_SET", "sk-local-secret");
        let mut p = profile(
            Some(CredentialReference {
                optional_env: Some("NYAN_OPENAI_TEST_LOCAL_KEY_SET".into()),
                ..Default::default()
            }),
            ProviderProtocol::OpenaiChat,
        );
        p.provider_kind = ProviderKind::Vllm;
        p.base_url = Some("http://127.0.0.1:8000/v1".into());
        let target = target_for_profile(&p).await.unwrap();
        let OpenAiAuth::ApiKey(key) = &target.auth else {
            panic!("{:?}", target.auth)
        };
        assert_eq!(key.expose(), "sk-local-secret");
    }

    #[tokio::test]
    async fn a_local_profile_without_a_base_url_never_reaches_the_commercial_default() {
        let _lock = lock();
        let _key = EnvVarGuard::set("NYAN_OPENAI_TEST_LOCAL_KEY2", "sk-must-not-leave");
        let mut p = profile(
            Some(env_ref("NYAN_OPENAI_TEST_LOCAL_KEY2")),
            ProviderProtocol::OpenaiChat,
        );
        p.provider_kind = ProviderKind::Ollama;
        p.base_url = Some("   ".into());
        let error = target_for_profile(&p).await.unwrap_err();
        assert!(
            matches!(error, TargetError::MissingBaseUrl { .. }),
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
