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
    CacheRetention, MaxTokensField, OpenaiCompatSettings, PromptCacheKeyMode, ProviderKind,
    ProviderProtocol,
};

pub const API_BASE: &str = "https://api.openai.com/v1";
pub const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";

/// Who serves an OpenAI-protocol endpoint, read from the base URL's host
/// (after Pi's `detectCompat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vendor {
    /// `api.openai.com` and its subdomains, or the ChatGPT backend.
    OpenAi,
    DeepSeek,
    /// Moonshot's Kimi APIs.
    Moonshot,
    OpenRouter,
    Other,
}

impl Vendor {
    fn of(base_url: &str) -> Self {
        let Ok(url) = url::Url::parse(base_url) else {
            return Self::Other;
        };
        let Some(host) = url.host_str() else {
            return Self::Other;
        };
        let within = |domain: &str| {
            host.strip_suffix(domain)
                .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
        };
        if within("api.openai.com") || within("chatgpt.com") {
            Self::OpenAi
        } else if within("deepseek.com") {
            Self::DeepSeek
        } else if within("moonshot.cn") || within("moonshot.ai") || within("kimi.com") {
            Self::Moonshot
        } else if within("openrouter.ai") {
            Self::OpenRouter
        } else {
            Self::Other
        }
    }
}

/// Dialect options for an OpenAI-protocol endpoint, with the defaults for its
/// kind and host applied. Settings say what the server takes; nothing is
/// probed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compat {
    pub max_tokens_field: MaxTokensField,
    pub include_usage: bool,
    pub cache_control_format: Option<String>,
    pub requires_reasoning_content: bool,
    /// Whether `prompt_cache_key` is sent while caching is on.
    pub prompt_cache_key: bool,
    /// Whether long retention asks for OpenAI's long TTL
    /// (`prompt_cache_retention` or `prompt_cache_options`). Only read when
    /// the key is sent.
    pub long_cache_retention: bool,
    /// The model bills prompt-cache writes (catalog `cacheWrite > 0`, GPT-5.6
    /// on), which is exactly the set that takes `prompt_cache_options` and
    /// rejects `prompt_cache_retention`. Follows Pi's `openai.ts` catalog.
    pub explicit_prompt_cache_mode: bool,
}

impl Compat {
    /// The kind's defaults on the OpenAI API, for a model the catalog does not
    /// know.
    pub fn resolve(kind: ProviderKind, settings: Option<&OpenaiCompatSettings>) -> Self {
        Self::resolve_full(kind, API_BASE, None, settings)
    }

    /// `base_url` is the endpoint requests go to, after the default is applied.
    pub fn resolve_full(
        kind: ProviderKind,
        base_url: &str,
        model: Option<&str>,
        settings: Option<&OpenaiCompatSettings>,
    ) -> Self {
        let vendor = Vendor::of(base_url);
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
                let model = model?;
                (vendor == Vendor::OpenRouter
                    && (model.starts_with("anthropic/") || model.starts_with("google/")))
                .then(|| "anthropic".to_string())
            });
        let requires_reasoning_content = settings
            .and_then(|s| s.requires_reasoning_content)
            .unwrap_or_else(|| {
                // The model name still counts: DeepSeek and Kimi models are
                // also served through gateways and local servers.
                let model = model.unwrap_or_default().to_lowercase();
                matches!(vendor, Vendor::DeepSeek | Vendor::Moonshot)
                    || ["deepseek", "kimi", "moonshot"]
                        .iter()
                        .any(|name| model.contains(name))
            });
        // @nyan: stricter than Pi, which also sends the key to every host it
        // believes takes long retention. Only OpenAI's own hosts get OpenAI's
        // cache parameters unless the provider's compat asks for them.
        let openai_host = kind == ProviderKind::Openai && vendor == Vendor::OpenAi;
        let prompt_cache_key = match settings.and_then(|s| s.prompt_cache_key) {
            None | Some(PromptCacheKeyMode::Auto) => openai_host,
            Some(PromptCacheKeyMode::Enabled) => true,
            Some(PromptCacheKeyMode::Disabled) => false,
        };
        let long_cache_retention = settings
            .and_then(|s| s.long_cache_retention)
            .unwrap_or(openai_host);
        let explicit_prompt_cache_mode = model
            .and_then(|model| {
                crate::utils::model::metadata::metadata_provider_for_kind(kind).and_then(
                    |snapshot| crate::utils::model::metadata::find_model_metadata(snapshot, model),
                )
            })
            .and_then(|entry| entry.cost.as_ref()?.cache_write)
            .is_some_and(|price| price > 0.0);

        Self {
            max_tokens_field: settings
                .and_then(|s| s.max_tokens_field)
                .unwrap_or(default_field),
            include_usage: settings.and_then(|s| s.include_usage).unwrap_or(true),
            cache_control_format,
            requires_reasoning_content,
            prompt_cache_key,
            long_cache_retention,
            explicit_prompt_cache_mode,
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
    let base_url =
        clean_base_url(profile.base_url.as_deref()).unwrap_or_else(|| default_base.into());
    // The ChatGPT sign-in only ever reaches the ChatGPT backend, directly or
    // through a proxy, so it takes that host's defaults whatever the base URL.
    let compat_base = match auth {
        OpenAiAuth::Codex { .. } => CODEX_BASE,
        _ => base_url.as_str(),
    };
    let compat = Compat::resolve_full(
        profile.provider_kind,
        compat_base,
        Some(&profile.model),
        profile.compat.as_ref(),
    );
    Ok(OpenAiTarget {
        profile: profile.profile.clone(),
        provider,
        provider_kind: profile.provider_kind,
        protocol: profile.protocol,
        base_url,
        auth,
        optional_env,
        compat,
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

    #[test]
    fn the_vendor_is_read_from_the_host_not_the_whole_url() {
        for (url, vendor) in [
            ("https://api.openai.com/v1", Vendor::OpenAi),
            ("https://us.api.openai.com/v1", Vendor::OpenAi),
            ("https://API.OpenAI.com/v1/", Vendor::OpenAi),
            (CODEX_BASE, Vendor::OpenAi),
            ("https://api.deepseek.com", Vendor::DeepSeek),
            ("https://api.deepseek.com/v1", Vendor::DeepSeek),
            ("https://api.moonshot.cn/v1", Vendor::Moonshot),
            ("https://api.moonshot.ai/v1", Vendor::Moonshot),
            ("https://api.kimi.com/coding/v1", Vendor::Moonshot),
            ("https://openrouter.ai/api/v1", Vendor::OpenRouter),
            // Look-alikes and a vendor named only in the path or query.
            ("https://api.openai.com.evil.example/v1", Vendor::Other),
            ("https://notapi.openai.com/v1", Vendor::Other),
            ("https://mydeepseek.com/v1", Vendor::Other),
            ("https://gw.example/api.openai.com/v1", Vendor::Other),
            ("https://gw.example/v1?upstream=deepseek.com", Vendor::Other),
            ("https://api.openai.com@gw.example/v1", Vendor::Other),
            ("http://127.0.0.1:8317/v1", Vendor::Other),
            ("not a url", Vendor::Other),
        ] {
            assert_eq!(Vendor::of(url), vendor, "{url}");
        }
    }

    #[test]
    fn only_openai_hosts_of_the_openai_kind_get_openai_cache_parameters() {
        let compat = |kind, url: &str| Compat::resolve_full(kind, url, Some("gpt-5"), None);
        for url in [API_BASE, "https://us.api.openai.com/v1", CODEX_BASE] {
            let c = compat(ProviderKind::Openai, url);
            assert!(c.prompt_cache_key && c.long_cache_retention, "{url}");
        }
        for (kind, url) in [
            (ProviderKind::Openai, "https://api.deepseek.com"),
            (ProviderKind::Openai, "https://api.moonshot.cn/v1"),
            (ProviderKind::Openai, "https://openrouter.ai/api/v1"),
            (ProviderKind::Openai, "http://127.0.0.1:8317/v1"),
            (ProviderKind::Gateway, API_BASE),
            (ProviderKind::Openrouter, "https://openrouter.ai/api/v1"),
            (ProviderKind::Vllm, "http://127.0.0.1:8000/v1"),
        ] {
            let c = compat(kind, url);
            assert!(
                !c.prompt_cache_key && !c.long_cache_retention,
                "{kind:?} {url}"
            );
        }
    }

    #[test]
    fn reasoning_content_follows_the_host_then_the_model_name_and_compat_wins() {
        let requires = |kind, url: &str, model: &str, explicit: Option<bool>| {
            let settings = OpenaiCompatSettings {
                requires_reasoning_content: explicit,
                ..Default::default()
            };
            Compat::resolve_full(kind, url, Some(model), Some(&settings)).requires_reasoning_content
        };
        // The host alone decides for DeepSeek and Kimi, whatever the model id.
        for url in [
            "https://api.deepseek.com",
            "https://api.moonshot.cn/v1",
            "https://api.moonshot.ai/v1",
            "https://api.kimi.com/coding/v1",
        ] {
            assert!(requires(ProviderKind::Openai, url, "chat", None), "{url}");
        }
        // Through a gateway or a local server, the model name still counts.
        for (kind, url, model) in [
            (
                ProviderKind::Openai,
                "https://openrouter.ai/api/v1",
                "deepseek/deepseek-reasoner",
            ),
            (
                ProviderKind::Openrouter,
                "https://openrouter.ai/api/v1",
                "moonshotai/kimi-k2-thinking",
            ),
            (
                ProviderKind::Gateway,
                "http://127.0.0.1:8317/v1",
                "DeepSeek-V4-Flash",
            ),
            (ProviderKind::Vllm, "http://127.0.0.1:8000/v1", "kimi-k2.6"),
        ] {
            assert!(requires(kind, url, model, None), "{kind:?} {model}");
        }
        assert!(!requires(ProviderKind::Openai, API_BASE, "gpt-5", None));
        assert!(!requires(
            ProviderKind::Openai,
            "https://openrouter.ai/api/v1",
            "openai/gpt-5",
            None
        ));
        // Explicit compat beats both.
        assert!(!requires(
            ProviderKind::Openai,
            "https://api.deepseek.com",
            "deepseek-reasoner",
            Some(false)
        ));
        assert!(requires(
            ProviderKind::Openai,
            API_BASE,
            "gpt-5",
            Some(true)
        ));
    }

    #[test]
    fn openrouter_cache_control_follows_the_host() {
        let format = |kind, url: &str, model: &str| {
            Compat::resolve_full(kind, url, Some(model), None).cache_control_format
        };
        let openrouter = "https://openrouter.ai/api/v1";
        assert_eq!(
            format(
                ProviderKind::Openai,
                openrouter,
                "anthropic/claude-opus-5.5"
            )
            .as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            format(ProviderKind::Openrouter, openrouter, "google/gemini-3-pro").as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            format(ProviderKind::Openai, openrouter, "openai/gpt-5"),
            None
        );
        assert_eq!(
            format(ProviderKind::Openai, API_BASE, "anthropic/claude-opus-5.5"),
            None
        );
    }

    /// The ChatGPT sign-in keeps its key, at the default backend and through a
    /// proxy alike; the Responses body builder drops the TTL fields it refuses.
    #[tokio::test]
    async fn the_codex_sign_in_keeps_the_prompt_cache_key_at_any_base_url() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        use crate::utils::provider_auth::store::{OAuthCredential, ProviderAuth};

        let root = crate::utils::node_os::tmpdir().join(format!(
            "nyan-openai-target-codex-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let store = AuthStore::at(root.join("auth"));
        let claims = serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_1"}
        });
        let access = format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut auth = ProviderAuth::default();
        auth.set_account(
            "main",
            OAuthCredential {
                access,
                refresh: "r".into(),
                expires_at_ms: i64::MAX / 2,
                account_id: None,
                metadata: Default::default(),
            },
        );
        store
            .lock(openai_codex::PROVIDER_ID)
            .unwrap()
            .save(&auth)
            .unwrap();
        let resolver = Resolver::for_test(
            store,
            crate::utils::provider_auth::oauth::build_client(
                crate::utils::proxy::client_builder().no_proxy(),
            )
            .unwrap(),
            openai_codex::Endpoints::at("http://127.0.0.1:1"),
        );
        let oauth = CredentialReference {
            oauth: Some(openai_codex::PROVIDER_ID.into()),
            ..Default::default()
        };

        let mut p = profile(Some(oauth), ProviderProtocol::OpenaiResponses);
        let mut targets = Vec::new();
        for base_url in [None, Some("http://127.0.0.1:8317/backend-api/codex")] {
            p.base_url = base_url.map(str::to_string);
            targets.push(target_with_resolver(&p, &resolver).await);
        }
        let _ = std::fs::remove_dir_all(&root);
        let [default, proxied] = <[_; 2]>::try_from(targets).unwrap().map(Result::unwrap);
        assert_eq!(default.base_url, CODEX_BASE);
        assert_eq!(proxied.base_url, "http://127.0.0.1:8317/backend-api/codex");
        for target in [default, proxied] {
            assert!(matches!(target.auth, OpenAiAuth::Codex { .. }));
            assert!(target.compat.prompt_cache_key, "{}", target.base_url);
        }
    }
}
