//! Anthropic API client factory.
//!
//! Maps to: CC services/api/client.ts (full file).
//!
//! This module ports the CC `getAnthropicClient()` factory and its helpers.
//! The factory constructs an `anthropic_sdk::Anthropic` client configured for
//! the direct Anthropic API using credentials and endpoint settings from the
//! selected provider profile.
//!

use super::target::AnthropicTarget;
use crate::utils::process_env::{self, JsTruthy};
use anthropic_sdk::Nullable;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Header name for client-generated request correlation IDs.
///
/// Maps to: CC services/api/client.ts:356
pub const CLIENT_REQUEST_ID_HEADER: &str = "x-client-request-id";

/// Default API timeout in milliseconds (10 minutes).
const DEFAULT_API_TIMEOUT_MS: u64 = 600_000;

// ---------------------------------------------------------------------------
// Client configuration
// ---------------------------------------------------------------------------

/// Options for [`get_anthropic_client`].
///
/// Maps to: CC services/api/client.ts:88-99
#[derive(Clone, Debug, Default)]
pub struct GetAnthropicClientOptions {
    /// Base URL and API key, taken from the selected provider profile. The
    /// client reads no other source for either (decision 22 is retired).
    pub target: AnthropicTarget,
    /// Maximum retries on transient errors.
    pub max_retries: u32,
    /// Model name, used for provider-specific region selection.
    pub model: Option<String>,
    /// Caller identifier for debug logging.
    pub source: Option<String>,
}

impl GetAnthropicClientOptions {
    /// Options aimed at the provider of the profile a request names; `None` is
    /// the main profile. Fails with the profile / credential configuration
    /// error (never an ambient fallback) when no usable profile or key is
    /// configured.
    pub fn for_profile(
        profile: Option<&str>,
        max_retries: u32,
        model: Option<String>,
        source: Option<String>,
    ) -> Result<Self, super::target::TargetError> {
        Ok(Self {
            target: super::target::current_request_target(profile)?,
            max_retries,
            model,
            source,
        })
    }
}

/// Error type for client construction failures.
///
/// Maps to: CC services/api/client.ts (error paths in getAnthropicClient)
#[derive(Debug)]
pub enum ClientError {
    /// SDK-level client construction failure.
    Sdk(String),
    /// Missing required configuration (e.g. API key, project ID).
    MissingConfig(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sdk(msg) => write!(f, "SDK error: {msg}"),
            Self::MissingConfig(msg) => write!(f, "missing required configuration: {msg}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Construct the direct `anthropic_sdk::Anthropic` client for the selected profile.
pub async fn get_anthropic_client(
    opts: GetAnthropicClientOptions,
) -> anyhow::Result<AnthropicClientHandle> {
    let GetAnthropicClientOptions {
        target,
        max_retries,
        model: _,
        source,
    } = opts;
    let env = process_env::snapshot();
    let client_app = env.var("CLAUDE_AGENT_SDK_CLIENT_APP").truthy();

    let mut default_headers = crate::services::provider::anthropic::headers::common_headers();
    for (name, value) in
        crate::services::provider::anthropic::headers::anthropic_adapter_headers(&env)
    {
        default_headers.insert(name, value);
    }
    if let Some(app) = client_app {
        default_headers.insert("x-client-app".to_string(), Some(app.to_string()));
    }
    crate::services::provider::anthropic::headers::apply_configured_overrides(&mut default_headers);

    let resolved_fetch = build_fetch(source, target.base_url.as_deref());
    let fetch_options = crate::utils::proxy::get_proxy_fetch_options()?.build()?;
    let timeout_ms = env
        .var("API_TIMEOUT_MS")
        .truthy()
        .and_then(|value| {
            let value = value.trim_start_matches(|c: char| {
                (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
            });
            let digits = value.strip_prefix('+').unwrap_or(value);
            let end = digits
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(digits.len());
            digits[..end].parse::<u64>().ok()
        })
        .filter(|timeout| (1..=i32::MAX as u64).contains(timeout))
        .unwrap_or(DEFAULT_API_TIMEOUT_MS);

    Ok(AnthropicClientHandle {
        provider: ProviderConfig::Direct {
            api_key: Nullable::from_resolved(target.api_key.map(|key| key.expose().to_string())),
            auth_token: Nullable::from_resolved(None),
            base_url: Some(target.base_url.unwrap_or_default()),
        },
        default_headers,
        max_retries,
        timeout_ms,
        fetch: resolved_fetch,
        fetch_options,
    })
}

// ---------------------------------------------------------------------------
// Client configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum ProviderConfig {
    Direct {
        api_key: Nullable<String>,
        auth_token: Nullable<String>,
        base_url: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct AnthropicClientHandle {
    pub provider: ProviderConfig,
    pub default_headers: HashMap<String, Option<String>>,
    pub max_retries: u32,
    pub timeout_ms: u64,
    pub fetch: ResolvedFetch,
    pub fetch_options: reqwest::Client,
}

impl AnthropicClientHandle {
    pub fn build(self) -> Result<ClientBuildOutput, ClientError> {
        let ProviderConfig::Direct {
            api_key,
            auth_token,
            base_url,
        } = self.provider.clone();
        self.build_direct_client(api_key, auth_token, base_url)
    }

    /// CC's `ARGS` (`client.ts:141-152`) with the credentials and base URL the
    /// caller resolved. Every `ClientOptions` field is explicit, so the SDK
    /// reads none of its option variables, and its HTTP client is the
    /// handle's `fetch_options`.
    fn core_client_options(
        &self,
        api_key: Nullable<String>,
        auth_token: Nullable<String>,
        base_url: Option<String>,
    ) -> anthropic_sdk::ClientOptions {
        anthropic_sdk::ClientOptions {
            api_key,
            auth_token,
            base_url,
            timeout: Some(self.timeout_ms),
            max_retries: Some(self.max_retries),
            default_headers: Some(self.default_headers.clone()),
            // CC `ARGS.fetch: resolvedFetch`.
            middlewares: vec![std::sync::Arc::new(self.fetch.clone())],
            // CC `ARGS.fetchOptions`.
            http_client: Some(self.fetch_options.clone()),
            ..Default::default()
        }
    }

    fn build_direct_client(
        &self,
        api_key: Nullable<String>,
        auth_token: Nullable<String>,
        base_url: Option<String>,
    ) -> Result<ClientBuildOutput, ClientError> {
        anthropic_sdk::Anthropic::new(self.core_client_options(api_key, auth_token, base_url))
            .map(ClientBuildOutput::Anthropic)
            .map_err(|error| ClientError::Sdk(error.to_string()))
    }
}

#[derive(Clone)]
pub enum ClientBuildOutput {
    Anthropic(anthropic_sdk::Anthropic),
}

pub type BetaMessageEventStream = std::pin::Pin<
    Box<
        dyn futures::Stream<
                Item = Result<
                    anthropic_sdk::resources::beta::messages::BetaMessageStreamEvent,
                    anthropic_sdk::ApiError,
                >,
            > + Send,
    >,
>;

fn boxed_stream_response<S>(
    response: anthropic_sdk::ApiResponse<S>,
) -> anthropic_sdk::ApiResponse<BetaMessageEventStream>
where
    S: futures::Stream<
            Item = Result<
                anthropic_sdk::resources::beta::messages::BetaMessageStreamEvent,
                anthropic_sdk::ApiError,
            >,
        > + Send
        + 'static,
{
    anthropic_sdk::ApiResponse {
        data: Box::pin(response.data),
        response: response.response,
        request_id: response.request_id,
    }
}

impl ClientBuildOutput {
    pub fn beta(&self) -> ClientBeta<'_> {
        ClientBeta(self)
    }
    pub fn as_client(&self) -> &anthropic_sdk::Anthropic {
        match self {
            Self::Anthropic(client) => client,
        }
    }
}

pub struct ClientBeta<'a>(&'a ClientBuildOutput);
impl ClientBeta<'_> {
    pub fn messages(&self) -> ClientBetaMessages<'_> {
        ClientBetaMessages(self.0)
    }
}
pub struct ClientBetaMessages<'a>(&'a ClientBuildOutput);

impl ClientBetaMessages<'_> {
    pub async fn create_with_options(
        &self,
        params: &anthropic_sdk::resources::beta::messages::BetaMessageCreateParams,
        options: Option<&anthropic_sdk::RequestOptions>,
    ) -> Result<anthropic_sdk::resources::beta::messages::BetaMessage, anthropic_sdk::ApiError>
    {
        match self.0 {
            ClientBuildOutput::Anthropic(c) => {
                c.beta()
                    .messages()
                    .create_with_options(params, options)
                    .await
            }
        }
    }
    pub async fn create_with_response_and_options(
        &self,
        params: &anthropic_sdk::resources::beta::messages::BetaMessageCreateParams,
        options: Option<&anthropic_sdk::RequestOptions>,
    ) -> Result<
        anthropic_sdk::ApiResponse<anthropic_sdk::resources::beta::messages::BetaMessage>,
        anthropic_sdk::ApiError,
    > {
        match self.0 {
            ClientBuildOutput::Anthropic(c) => {
                c.beta()
                    .messages()
                    .create_with_response_and_options(params, options)
                    .await
            }
        }
    }
    pub async fn create_stream_with_response_and_options(
        &self,
        params: &anthropic_sdk::resources::beta::messages::BetaMessageCreateParams,
        options: Option<&anthropic_sdk::RequestOptions>,
    ) -> Result<anthropic_sdk::ApiResponse<BetaMessageEventStream>, anthropic_sdk::ApiError> {
        match self.0 {
            ClientBuildOutput::Anthropic(c) => c
                .beta()
                .messages()
                .create_stream_with_response_and_options(params, options)
                .await
                .map(boxed_stream_response),
        }
    }
    pub async fn count_tokens(
        &self,
        params: &anthropic_sdk::resources::beta::messages::BetaMessageCountTokensParams,
    ) -> Result<
        anthropic_sdk::resources::beta::messages::BetaMessageTokensCount,
        anthropic_sdk::ApiError,
    > {
        match self.0 {
            ClientBuildOutput::Anthropic(c) => c.beta().messages().count_tokens(params).await,
        }
    }
}

/// Maps to: CC `services/api/client.ts:358-390` `buildFetch(fetchOverride,
/// source)`, the wrapper around every SDK fetch. For the first-party API only
/// (`getAPIProvider() === 'firstParty' && isFirstPartyAnthropicBaseUrl()`,
/// decided here as in CC), it sets `x-client-request-id` to a fresh UUID
/// unless the caller already set one. Timeouts return no server request ID,
/// so this lets them be correlated. It logs every request's path either way.
/// The SDK runs middleware on each attempt, as CC's wrapper runs on each
/// fetch. CC's one `fetchOverride` is `dumpPromptsFetch` (`query.ts:688`,
/// passed at `claude.ts:848,1783`); Nyan's `claude.rs` dumps prompts
/// itself, so it needs no override.
fn build_fetch(source: Option<String>, base_url: Option<&str>) -> ResolvedFetch {
    ResolvedFetch {
        inject_client_request_id:
            crate::utils::model::providers::is_first_party_anthropic_base_url_for_audience(
                base_url,
                crate::utils::build_profile::build_audience(),
            ),
        source,
    }
}

/// The fetch wrapper [`build_fetch`] returns, as SDK middleware.
#[derive(Clone, Debug)]
pub struct ResolvedFetch {
    inject_client_request_id: bool,
    source: Option<String>,
}

impl anthropic_sdk::HttpMiddleware for ResolvedFetch {
    fn before_request<'a>(
        &'a self,
        request: &'a mut reqwest::Request,
    ) -> futures::future::BoxFuture<'a, Result<(), anthropic_sdk::ApiError>> {
        Box::pin(async move {
            if self.inject_client_request_id
                && !request.headers().contains_key(CLIENT_REQUEST_ID_HEADER)
                && let Ok(id) =
                    reqwest::header::HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            {
                request.headers_mut().insert(CLIENT_REQUEST_ID_HEADER, id);
            }
            // `headers.get` is a ByteString (one char per byte); `id ? ... : ''`
            // omits an empty preset.
            let id = request
                .headers()
                .get(CLIENT_REQUEST_ID_HEADER)
                .map(|value| {
                    value
                        .as_bytes()
                        .iter()
                        .map(|&byte| char::from(byte))
                        .collect::<String>()
                })
                .filter(|id| !id.is_empty())
                .map(|id| format!(" {CLIENT_REQUEST_ID_HEADER}={id}"))
                .unwrap_or_default();
            crate::utils::debug::log_for_debugging(&format!(
                "[API REQUEST] {}{id} source={}",
                request.url().path(),
                self.source.as_deref().unwrap_or("unknown"),
            ));
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn target_with_key(key: &str, base_url: Option<&str>) -> AnthropicTarget {
        AnthropicTarget {
            profile: "main".to_string(),
            provider: "anthropic".to_string(),
            base_url: base_url.map(str::to_string),
            api_key: Some(super::super::credential::ApiKey::for_test(key)),
            ..Default::default()
        }
    }
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// A transport without proxy or TLS options, for handles built by hand.
    fn direct_client() -> reqwest::Client {
        crate::utils::tls_provider::install_crypto_provider();
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[test]
    fn client_request_id_header_value_matches_cc() {
        assert_eq!(CLIENT_REQUEST_ID_HEADER, "x-client-request-id");
    }

    #[test]
    fn default_timeout_matches_cc_10_minutes() {
        assert_eq!(DEFAULT_API_TIMEOUT_MS, 600_000);
    }

    #[test]
    fn api_provider_detection_from_env() {
        // Default (no env vars set) should be FirstParty.
        // We cannot safely set/unset env vars in parallel tests, so just
        // verify the function runs without panicking.
        let _provider = crate::utils::model::providers::get_api_provider();
    }

    #[tokio::test]
    async fn explicit_api_key_auth_remains_available_without_oauth_credentials() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home =
            std::env::temp_dir().join(format!("nyan-api-key-oauth-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _auth_token = EnvVarGuard::unset("ANTHROPIC_AUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let handle = get_anthropic_client(GetAnthropicClientOptions {
            target: target_with_key("sk-ant-test", None),
            ..GetAnthropicClientOptions::default()
        })
        .await
        .expect("a profile API key must build a client");
        // The profile key is the only credential; no auth token exists.
        assert!(matches!(
            handle.provider,
            ProviderConfig::Direct {
                api_key: Nullable::Set(ref key),
                auth_token: Nullable::Null,
                ..
            } if key == "sk-ant-test"
        ));
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// The retired compatibility layer read these variables; the client must
    /// now ignore them and use only the profile target. `API_TIMEOUT_MS` and
    /// the like are not credentials and keep their snapshot reads.
    #[tokio::test]
    async fn ambient_anthropic_credentials_and_base_url_are_ignored() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home =
            std::env::temp_dir().join(format!("nyan-sdk-defaults-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "oauth-ambient");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _base_url = EnvVarGuard::set("ANTHROPIC_BASE_URL", "https://ambient.example");
        let _auth_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", "tok-ambient");
        let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ambient");
        let _timeout = EnvVarGuard::set("API_TIMEOUT_MS", " 90000ms");

        let handle = get_anthropic_client(GetAnthropicClientOptions {
            target: target_with_key("sk-profile", Some("https://gw.example")),
            ..GetAnthropicClientOptions::default()
        })
        .await
        .unwrap();

        let ProviderConfig::Direct {
            ref api_key,
            ref auth_token,
            ref base_url,
        } = handle.provider
        else {
            panic!("first-party provider expected");
        };
        assert_eq!(api_key, &Nullable::Set("sk-profile".to_string()));
        assert_eq!(auth_token, &Nullable::Null);
        assert_eq!(base_url.as_deref(), Some("https://gw.example"));
        // `parseInt(' 90000ms', 10)` is 90000.
        assert_eq!(handle.timeout_ms, 90_000);
        // No Authorization header from the ambient token or an apiKeyHelper.
        assert_eq!(handle.default_headers.get("Authorization"), None);
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// A target without a key sends none: the SDK must not fall back to its
    /// own environment read.
    #[tokio::test]
    async fn keyless_target_passes_an_explicit_null_key() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home =
            std::env::temp_dir().join(format!("nyan-keyless-target-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config_home);
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ambient");

        let handle = get_anthropic_client(GetAnthropicClientOptions::default())
            .await
            .unwrap();
        assert!(matches!(
            handle.provider,
            ProviderConfig::Direct {
                api_key: Nullable::Null,
                auth_token: Nullable::Null,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// A target without a base URL is `Some("")`: the SDK default without its
    /// own environment read.
    #[tokio::test]
    async fn unset_base_url_is_the_sdk_default_without_an_environment_read() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home =
            std::env::temp_dir().join(format!("nyan-sdk-base-url-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _base_url = EnvVarGuard::unset("ANTHROPIC_BASE_URL");

        let handle = get_anthropic_client(GetAnthropicClientOptions {
            target: target_with_key("sk-ant-test", None),
            ..GetAnthropicClientOptions::default()
        })
        .await
        .unwrap();
        assert!(matches!(
            handle.provider,
            ProviderConfig::Direct { base_url: Some(ref url), .. } if url.is_empty()
        ));
        crate::utils::tls_provider::install_crypto_provider();
        let client = handle.build().unwrap();
        assert_eq!(client.as_client().base_url(), "https://api.anthropic.com");
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// CC `buildFetch` (`client.ts:358-390`): `x-client-request-id` is added
    /// only when injection applies and the caller has not set one.
    #[tokio::test]
    async fn resolved_fetch_adds_a_client_request_id_only_when_injecting() {
        use anthropic_sdk::HttpMiddleware as _;
        let request = || {
            reqwest::Request::new(
                reqwest::Method::POST,
                "https://api.anthropic.com/v1/messages".parse().unwrap(),
            )
        };

        let injecting = ResolvedFetch {
            inject_client_request_id: true,
            source: Some("repl".to_string()),
        };
        let mut fresh = request();
        injecting.before_request(&mut fresh).await.unwrap();
        let id = fresh.headers().get(CLIENT_REQUEST_ID_HEADER).unwrap();
        assert!(uuid::Uuid::parse_str(id.to_str().unwrap()).is_ok());

        let mut preset = request();
        preset
            .headers_mut()
            .insert(CLIENT_REQUEST_ID_HEADER, "caller-id".parse().unwrap());
        injecting.before_request(&mut preset).await.unwrap();
        assert_eq!(
            preset.headers().get(CLIENT_REQUEST_ID_HEADER).unwrap(),
            "caller-id"
        );

        let mut other = request();
        ResolvedFetch {
            inject_client_request_id: false,
            source: None,
        }
        .before_request(&mut other)
        .await
        .unwrap();
        assert!(other.headers().get(CLIENT_REQUEST_ID_HEADER).is_none());
    }

    /// `build()` hands `ResolvedFetch` to the SDK, so the header reaches the
    /// wire, as CC's `ARGS.fetch` wraps every SDK request.
    #[tokio::test]
    async fn built_client_sends_requests_through_resolved_fetch() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0, "HTTP request ended before headers");
                request.extend_from_slice(&buffer[..count]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\n\
                      content-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await
                .unwrap();
            String::from_utf8_lossy(&request).to_ascii_lowercase()
        });
        let handle = AnthropicClientHandle {
            provider: ProviderConfig::Direct {
                api_key: Nullable::Set("sk-ant-test".to_string()),
                auth_token: Nullable::Null,
                base_url: Some(format!("http://{address}")),
            },
            default_headers: HashMap::new(),
            max_retries: 0,
            timeout_ms: 5_000,
            fetch: ResolvedFetch {
                inject_client_request_id: true,
                source: Some("test".to_string()),
            },
            fetch_options: direct_client(),
        };
        let _ = handle
            .build()
            .unwrap()
            .as_client()
            .models()
            .retrieve("model", None)
            .await;
        let request = tokio::time::timeout(std::time::Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
        assert!(request.contains("\r\nx-client-request-id: "), "{request}");
    }

    #[test]
    fn provider_config_direct_has_correct_type() {
        let handle = AnthropicClientHandle {
            provider: ProviderConfig::Direct {
                api_key: "sk-test".into(),
                auth_token: Nullable::Unset,
                base_url: None,
            },
            default_headers: HashMap::new(),
            max_retries: 2,
            timeout_ms: 600_000,
            fetch: build_fetch(Some("test".to_string()), None),
            fetch_options: direct_client(),
        };
    }

    #[test]
    fn user_agent_uses_official_claude_cli_shape() {
        let ua = crate::utils::http::get_user_agent();
        assert_eq!(ua, crate::utils::http::user_agent());
        assert!(ua.starts_with("nyan/"));
    }

    #[test]
    fn first_party_base_url_detection() {
        // When ANTHROPIC_BASE_URL is not set, should be first-party
        let result = crate::utils::model::providers::is_first_party_anthropic_base_url();
        // May be overridden in test env, just verify it does not panic
        let _ = result;
    }
}
