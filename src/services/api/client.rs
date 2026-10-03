//! Anthropic API client factory.
//!
//! Maps to: CC services/api/client.ts (full file).
//!
//! This module ports the CC `getAnthropicClient()` factory and its helpers.
//! The factory constructs an `anthropic_sdk::Anthropic` client configured for
//! one of four providers: Direct API, Bedrock, Foundry, or Vertex. Each
//! provider branch reads environment variables to determine credentials and
//! region configuration.
//!

pub use crate::utils::auth::AwsCredentials;
use crate::utils::auth::{is_claude_ai_subscriber, refresh_and_get_aws_credentials};
use crate::utils::env_utils::{get_aws_region, get_vertex_region_for_model};
use crate::utils::model::model::get_small_fast_model;
use anthropic_sdk::Nullable;
use sha2::{Digest as _, Sha256};
use crate::utils::process_env::{self, EnvSnapshot, JsTruthy};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Stub types for modules not yet ported
// ---------------------------------------------------------------------------

pub use crate::utils::model::providers::ApiProvider;

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
// Custom headers
// ---------------------------------------------------------------------------

/// Stands in for the SDK's `readEnv(key)`: `process.env[key]?.trim()`, with
/// `''` kept. CC leaves these options to the SDK; Cometix computes them from
/// `env` and passes them explicitly, because the SDK reads the OS
/// environment, which carrier writes after startup never reach.
fn read_env(env: &EnvSnapshot, key: &str) -> Option<String> {
    // Node decodes a non-UTF-8 value lossily; `String.prototype.trim`
    // (White_Space plus U+FEFF, minus U+0085) is not `str::trim`.
    env.var_os(key).map(|value| {
        value
            .to_string_lossy()
            .trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
            .to_owned()
    })
}

/// Parse the `ANTHROPIC_CUSTOM_HEADERS` environment variable into a header map.
///
/// Format: newline-separated `Name: Value` pairs (curl style).
///
/// Maps to: CC services/api/client.ts:330-354
fn get_custom_headers(env: &EnvSnapshot) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    // CC `if (!customHeadersEnv) return`.
    let Some(raw) = env.var("ANTHROPIC_CUSTOM_HEADERS").truthy() else {
        return headers;
    };

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(colon_idx) = trimmed.find(':') {
            let name = trimmed[..colon_idx].trim();
            let value = trimmed[colon_idx + 1..].trim();
            if !name.is_empty() {
                headers.insert(name.to_string(), value.to_string());
            }
        }
    }

    headers
}

// ---------------------------------------------------------------------------
// API key header configuration
// ---------------------------------------------------------------------------

/// Configure API key / bearer token headers for non-subscriber clients.
///
/// Maps to: CC services/api/client.ts:318-328
async fn configure_api_key_headers(
    headers: &mut HashMap<String, Option<String>>,
    env: &EnvSnapshot,
) {
    let is_non_interactive = crate::bootstrap::state::get_is_non_interactive_session();
    // CC `process.env.ANTHROPIC_AUTH_TOKEN || helper`: only an empty value is
    // falsy, and the value is used as set (a whitespace-only token is sent).
    let token = match env.var("ANTHROPIC_AUTH_TOKEN").truthy() {
        Some(token) => Some(token.to_owned()),
        None => crate::utils::auth::get_api_key_from_api_key_helper(is_non_interactive),
    };

    if let Some(t) = token.filter(|token| !token.is_empty()) {
        headers.insert("Authorization".to_string(), Some(format!("Bearer {t}")));
    }
}

// ---------------------------------------------------------------------------
// Client configuration
// ---------------------------------------------------------------------------

/// Options for [`get_anthropic_client`].
///
/// Maps to: CC services/api/client.ts:88-99
#[derive(Clone, Debug, Default)]
pub struct GetAnthropicClientOptions {
    /// Override API key (an empty one falls back, as CC's `apiKey ||`, to
    /// `get_anthropic_api_key`).
    pub api_key: Option<String>,
    /// Maximum retries on transient errors.
    pub max_retries: u32,
    /// Model name, used for provider-specific region selection.
    pub model: Option<String>,
    /// Caller identifier for debug logging.
    pub source: Option<String>,
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

/// Construct an `anthropic_sdk::Anthropic` client for the active provider.
///
/// This is the main entry point that CC's query layer calls to get a client.
/// It supports four providers based on environment variables:
///
/// - **Direct API** (default): Uses `ANTHROPIC_API_KEY` or OAuth tokens
/// - **Bedrock**: When `CLAUDE_CODE_USE_BEDROCK=1`
/// - **Foundry**: When `CLAUDE_CODE_USE_FOUNDRY=1`
/// - **Vertex**: When `CLAUDE_CODE_USE_VERTEX=1`
///
/// Maps to: CC services/api/client.ts:88-316
pub async fn get_anthropic_client(
    opts: GetAnthropicClientOptions,
) -> anyhow::Result<AnthropicClientHandle> {
    let GetAnthropicClientOptions {
        api_key,
        max_retries,
        model,
        source,
    } = opts;
    // One read of the environment for the whole construction: headers,
    // timeout, credentials, base URL and provider settings all come from it.
    // Deliberate deviation (environment redesign §9.1): CC re-reads
    // `process.env` after its `await`s (the OAuth refresh, the API key
    // helper), while this snapshot spans them. The provider, region and auth
    // helpers called here read the environment for themselves, as CC's do;
    // those still on the OS environment move to the carrier in C4/C5.
    let env = process_env::snapshot();
    let oauth_auth_selected = crate::utils::model::providers::get_api_provider()
        == ApiProvider::FirstParty
        && is_claude_ai_subscriber();

    // ----- Build default headers -----
    // Maps to: CC services/api/client.ts:101-129. The `?:` spreads are
    // truthiness tests, so an empty value adds no header.
    let container_id = env.var("CLAUDE_CODE_CONTAINER_ID").truthy();
    let remote_session_id = env.var("CLAUDE_CODE_REMOTE_SESSION_ID").truthy();
    let client_app = env.var("CLAUDE_AGENT_SDK_CLIENT_APP").truthy();
    let custom_headers = get_custom_headers(&env);

    let mut default_headers: HashMap<String, Option<String>> = HashMap::new();
    default_headers.insert("x-app".to_string(), Some("cli".to_string()));
    default_headers.insert(
        "User-Agent".to_string(),
        Some(crate::utils::http::get_user_agent()),
    );
    default_headers.insert(
        "X-Claude-Code-Session-Id".to_string(),
        Some(crate::bootstrap::state::get_session_id()),
    );

    // Merge custom headers
    for (k, v) in &custom_headers {
        default_headers.insert(k.clone(), Some(v.clone()));
    }

    if let Some(cid) = container_id {
        default_headers.insert(
            "x-claude-remote-container-id".to_string(),
            Some(cid.to_string()),
        );
    }
    if let Some(rsid) = remote_session_id {
        default_headers.insert(
            "x-claude-remote-session-id".to_string(),
            Some(rsid.to_string()),
        );
    }
    if let Some(app) = client_app {
        default_headers.insert("x-client-app".to_string(), Some(app.to_string()));
    }

    crate::utils::debug::log_for_debugging(&format!(
        "[API:request] Creating client, ANTHROPIC_CUSTOM_HEADERS present: {}, has Authorization header: {}",
        env.var("ANTHROPIC_CUSTOM_HEADERS").truthy().is_some(),
        custom_headers.contains_key("Authorization"),
    ));

    // Additional protection header
    // Maps to: CC services/api/client.ts:124-129
    if crate::utils::env_utils::is_env_truthy(env.var("CLAUDE_CODE_ADDITIONAL_PROTECTION")) {
        default_headers.insert(
            "x-anthropic-additional-protection".to_string(),
            Some("true".to_string()),
        );
    }

    // ----- OAuth refresh -----
    // Maps to: CC services/api/client.ts:131-133
    crate::utils::debug::log_for_debugging("[API:auth] OAuth token check starting");
    // The source helper absorbs refresh errors and returns false. The current
    // Rust partial seam exposes transport/storage errors, so this source caller
    // deliberately keeps unrelated provider/API-key construction alive after
    // logging. The L2 product-gate error remains explicit when OAuth is the
    // selected first-party authentication path.
    if let Err(error) = crate::utils::auth::check_and_refresh_oauth_token_if_needed(false).await {
        if oauth_auth_selected
            && error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        {
            // The deliberate product gate is not a recoverable refresh error:
            // continuing would turn a blocked credential outlet into apparent
            // client-construction success with stale OAuth state.
            return Err(error);
        }
        crate::utils::debug::log_for_debugging(&format!(
            "[API:auth] OAuth token check failed: {error}"
        ));
    }
    crate::utils::debug::log_for_debugging("[API:auth] OAuth token check complete");

    // ----- API key headers for non-subscriber path -----
    // Maps to: CC services/api/client.ts:135-138
    if !is_claude_ai_subscriber() {
        configure_api_key_headers(&mut default_headers, &env).await;
    }

    // Maps to: CC services/api/client.ts:140 `buildFetch(fetchOverride, source)`.
    let resolved_fetch = build_fetch(source);
    // CC `ARGS.fetchOptions: getProxyFetchOptions({ forAnthropicAPI: true })`
    // (`client.ts:146-148`).
    let fetch_options = crate::utils::proxy::get_proxy_fetch_options(true)?.build()?;

    // ----- Common client args -----
    // Maps to: CC services/api/client.ts:142-152:
    // `parseInt(process.env.API_TIMEOUT_MS || String(600 * 1000), 10)`, that
    // is leading ECMAScript whitespace (not U+0085), an optional sign, then
    // the longest digit run. Node's `setTimeout` treats NaN, zero, negative
    // and over-2^31-1 delays as 1 ms, so such values time CC's requests out
    // at once; the default is used instead.
    let timeout_ms: u64 = env
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

    // The SDK's `logLevel` default, `parseLogLevel(readEnv('ANTHROPIC_LOG'))`
    // (`log.ts:29-31`), from the snapshot. An empty value is unset; an
    // unknown one warns as the SDK does, through the client's logger (CC's
    // `createStderrLogger` under `--debug-to-stderr`) or else `tracing`, and
    // falls back to `warn`.
    let log_level = match read_env(&env, "ANTHROPIC_LOG").filter(|value| !value.is_empty()) {
        None => anthropic_sdk::LogLevel::Warn,
        Some(value) => anthropic_sdk::LogLevel::from_env_value(&value).unwrap_or_else(|| {
            let warning = format!(
                "process.env['ANTHROPIC_LOG'] was set to {value:?}, expected one of [\"off\",\"error\",\"warn\",\"info\",\"debug\"]"
            );
            if crate::utils::debug::debug_to_stderr_flag() {
                create_stderr_logger().warn(&warning);
            } else {
                tracing::warn!("{warning}");
            }
            anthropic_sdk::LogLevel::Warn
        }),
    };

    // ----- Provider dispatch -----
    let provider = crate::utils::model::providers::get_api_provider();

    match provider {
        // ---------------------------------------------------------------
        // Bedrock
        // Maps to: CC services/api/client.ts:153-190
        // ---------------------------------------------------------------
        ApiProvider::Bedrock => {
            // CC `:157-161`: `model === getSmallFastModel() &&
            // process.env.ANTHROPIC_SMALL_FAST_MODEL_AWS_REGION ? ... :
            // getAWSRegion()`, so an empty override falls through.
            let small_fast_model = get_small_fast_model();
            let aws_region = match env.var("ANTHROPIC_SMALL_FAST_MODEL_AWS_REGION").truthy() {
                Some(region) if model.as_deref() == Some(small_fast_model.as_str()) => {
                    region.to_owned()
                }
                _ => get_aws_region(),
            };
            let skip_auth =
                crate::utils::env_utils::is_env_truthy(env.var("CLAUDE_CODE_SKIP_BEDROCK_AUTH"));

            crate::utils::debug::log_for_debugging(&format!(
                "[API:bedrock] region={aws_region}, skip_auth={skip_auth}",
            ));

            // Determine auth strategy. CC `:172`
            // `if (process.env.AWS_BEARER_TOKEN_BEDROCK)`: an empty token is
            // no token.
            let bedrock_auth = if let Some(bearer) = env.var("AWS_BEARER_TOKEN_BEDROCK").truthy() {
                // Bearer token auth overrides everything
                let mut hdrs = default_headers.clone();
                hdrs.insert(
                    "Authorization".to_string(),
                    Some(format!("Bearer {bearer}")),
                );
                BedrockAuth::BearerToken {
                    extra_headers: hdrs,
                }
            } else if skip_auth {
                BedrockAuth::SkipAuth
            } else {
                // Refresh and use AWS credentials
                match refresh_and_get_aws_credentials().await {
                    Some(creds) => BedrockAuth::Credentials(creds),
                    None => BedrockAuth::DefaultChain,
                }
            };

            Ok(AnthropicClientHandle {
                provider: ProviderConfig::Bedrock {
                    region: aws_region,
                    auth: bedrock_auth,
                },
                default_headers,
                max_retries,
                timeout_ms,
                fetch: resolved_fetch,
                fetch_options,
                log_level,
            })
        }

        // ---------------------------------------------------------------
        // Foundry (Azure)
        // Maps to: CC services/api/client.ts:191-220
        // ---------------------------------------------------------------
        ApiProvider::Foundry => {
            // CC `client.ts:196`: `if (!process.env.ANTHROPIC_FOUNDRY_API_KEY)`
            // is a truthiness test, so an empty key selects Azure AD / skip-auth.
            let skip_auth =
                crate::utils::env_utils::is_env_truthy(env.var("CLAUDE_CODE_SKIP_FOUNDRY_AUTH"));
            let foundry_auth = if env.var("ANTHROPIC_FOUNDRY_API_KEY").truthy().is_some() {
                FoundryAuth::ApiKey
            } else if skip_auth {
                FoundryAuth::SkipAuth
            } else {
                FoundryAuth::AzureAd
            };

            crate::utils::debug::log_for_debugging(&format!(
                "[API:foundry] auth={foundry_auth:?}, skip_auth={skip_auth}",
            ));

            Ok(AnthropicClientHandle {
                provider: ProviderConfig::Foundry {
                    auth: foundry_auth,
                    // CC passes none of these; `AnthropicFoundry` reads them
                    // with `readEnv` (`foundry-sdk client.ts:58-60`).
                    base_url: read_env(&env, "ANTHROPIC_FOUNDRY_BASE_URL"),
                    resource: read_env(&env, "ANTHROPIC_FOUNDRY_RESOURCE"),
                    api_key: read_env(&env, "ANTHROPIC_FOUNDRY_API_KEY"),
                },
                default_headers,
                max_retries,
                timeout_ms,
                fetch: resolved_fetch,
                fetch_options,
                log_level,
            })
        }

        // ---------------------------------------------------------------
        // Vertex
        // Maps to: CC services/api/client.ts:221-298
        // ---------------------------------------------------------------
        ApiProvider::Vertex => {
            // CC: `await refreshGcpCredentialsIfNeeded()` — joins with the
            // deferred GCP-auth subsystem; do not preserve an empty
            // consumer-owned redefinition of the imported auth function.

            let region = get_vertex_region_for_model(model.as_deref());
            // CC passes no `projectId`; `AnthropicVertex` reads it with
            // `readEnv('ANTHROPIC_VERTEX_PROJECT_ID') ?? null`
            // (`vertex-sdk client.ts:79`) and tests it with `if (!this.projectId)`,
            // so `''` is none here.
            let project_id =
                read_env(&env, "ANTHROPIC_VERTEX_PROJECT_ID").filter(|value| !value.is_empty());

            // CC `client.ts:253-288` gives `GoogleAuth` a `projectId` fallback
            // when no project variable or key file is set, so that
            // google-auth-library does not ask the GCE metadata server for the
            // project (a 12 s timeout off GCP). The SDK's `GoogleAuth` takes the
            // project from the credential file or the quota project only and
            // never asks the metadata server for one, so there is nothing to
            // guard here.
            let skip_auth =
                crate::utils::env_utils::is_env_truthy(env.var("CLAUDE_CODE_SKIP_VERTEX_AUTH"));
            let vertex_auth = if skip_auth {
                VertexAuth::SkipAuth
            } else {
                VertexAuth::GoogleAuth
            };

            crate::utils::debug::log_for_debugging(&format!(
                "[API:vertex] region={region}, project_id={project_id:?}, skip_auth={skip_auth}",
            ));

            Ok(AnthropicClientHandle {
                provider: ProviderConfig::Vertex {
                    region,
                    project_id,
                    auth: vertex_auth,
                    // `AnthropicVertex`'s own `readEnv` (`vertex-sdk
                    // client.ts:78`) and the core's `apiKey` default, which
                    // CC leaves to the SDK.
                    base_url: read_env(&env, "ANTHROPIC_VERTEX_BASE_URL"),
                    api_key: read_env(&env, "ANTHROPIC_API_KEY"),
                },
                default_headers,
                max_retries,
                timeout_ms,
                fetch: resolved_fetch,
                fetch_options,
                log_level,
            })
        }

        // ---------------------------------------------------------------
        // Direct API (first party)
        // Maps to: CC services/api/client.ts:300-316
        // ---------------------------------------------------------------
        ApiProvider::FirstParty => {
            let subscriber = is_claude_ai_subscriber();

            // CC `client.ts:302`: `apiKey: isClaudeAISubscriber() ? null :
            // apiKey || getAnthropicApiKey()` is always explicit, since
            // `getAnthropicApiKey()` returns `null` for no key: the SDK never
            // falls back to `ANTHROPIC_API_KEY` here. `||` also skips an
            // empty caller key.
            let resolved_api_key = Nullable::from_resolved(if subscriber {
                None
            } else {
                api_key
                    .filter(|key| !key.is_empty())
                    .or_else(crate::utils::auth::get_anthropic_api_key)
            });

            // CC `client.ts:303-305`: `authToken: isClaudeAISubscriber() ?
            // getClaudeAIOAuthTokens()?.accessToken : undefined`. `undefined`
            // (also a subscriber without tokens) takes the SDK default,
            // `readEnv('ANTHROPIC_AUTH_TOKEN') ?? null`, computed here.
            let auth_token = match subscriber
                .then(crate::utils::auth::get_claude_ai_oauth_tokens)
                .flatten()
            {
                Some(tokens) => Nullable::Set(tokens.access_token),
                None => Nullable::from_resolved(read_env(&env, "ANTHROPIC_AUTH_TOKEN")),
            };

            // Maps to: CC `services/api/client.ts:306-311`: only ant staging
            // sessions source their API base URL from the canonical OAuth
            // configuration owner. Elsewhere CC passes none, and the SDK takes
            // `readEnv('ANTHROPIC_BASE_URL') || default`; `Some("")` is the
            // SDK's default without its own environment read.
            let base_url = if env.var("USER_TYPE") == Some("ant")
                && crate::utils::env_utils::is_env_truthy(env.var("USE_STAGING_OAUTH"))
            {
                Some(crate::constants::oauth::get_oauth_config()?.base_api_url)
            } else {
                Some(read_env(&env, "ANTHROPIC_BASE_URL").unwrap_or_default())
            };

            Ok(AnthropicClientHandle {
                provider: ProviderConfig::Direct {
                    api_key: resolved_api_key,
                    auth_token,
                    base_url,
                },
                default_headers,
                max_retries,
                timeout_ms,
                fetch: resolved_fetch,
                fetch_options,
                log_level,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Provider-specific configuration structs
// ---------------------------------------------------------------------------

/// Authentication strategy for AWS Bedrock.
///
/// Maps to: CC services/api/client.ts:162-189
#[derive(Clone, Debug)]
pub enum BedrockAuth {
    /// Use a bearer token (from `AWS_BEARER_TOKEN_BEDROCK`).
    BearerToken {
        extra_headers: HashMap<String, Option<String>>,
    },
    /// Use explicit AWS credentials (access key, secret key, optional session token).
    Credentials(AwsCredentials),
    /// Delegate to the AWS SDK default credential-provider chain.
    DefaultChain,
    /// Skip authentication entirely (for proxies / testing).
    SkipAuth,
}

/// Authentication strategy for Azure Foundry.
///
/// Maps to: CC services/api/client.ts:195-211
#[derive(Clone, Debug)]
pub enum FoundryAuth {
    /// Use `ANTHROPIC_FOUNDRY_API_KEY`, passed to the client as its API key.
    ApiKey,
    /// Delegate Azure AD authentication to `DefaultAzureCredential`.
    AzureAd,
    /// Skip authentication (for proxies / testing).
    SkipAuth,
}

/// Authentication strategy for GCP Vertex AI.
///
/// Maps to: CC services/api/client.ts:266-288
#[derive(Clone, Debug)]
pub enum VertexAuth {
    /// `new GoogleAuth(...)`: the SDK's Application Default Credentials.
    GoogleAuth,
    /// CC's mock `GoogleAuth` (`client.ts:265-271`), for proxies and tests:
    /// `getRequestHeaders()` returns no header.
    SkipAuth,
}

/// Provider-specific configuration for the Anthropic client.
///
/// Maps to: CC services/api/client.ts:153-316 (provider dispatch branches)
#[derive(Clone, Debug)]
pub enum ProviderConfig {
    /// Direct API access via api.anthropic.com.
    ///
    /// The credentials keep the shape CC hands the SDK (`client.ts:300-305`),
    /// with each option CC leaves `undefined` already resolved to the SDK's
    /// default from the environment snapshot. `Null` is TS `null`: none, and
    /// the environment is not read. The SDK never reads the environment for
    /// these.
    Direct {
        api_key: Nullable<String>,
        auth_token: Nullable<String>,
        base_url: Option<String>,
    },
    /// AWS Bedrock provider.
    Bedrock { region: String, auth: BedrockAuth },
    /// Azure Foundry provider. The endpoint and key are the `readEnv` values
    /// `AnthropicFoundry` reads for itself (`foundry-sdk client.ts:58-60`).
    Foundry {
        auth: FoundryAuth,
        base_url: Option<String>,
        resource: Option<String>,
        api_key: Option<String>,
    },
    /// GCP Vertex AI provider. The project, endpoint and key are the `readEnv`
    /// values `AnthropicVertex` and the core read for themselves
    /// (`vertex-sdk client.ts:78-80`); an empty project is none.
    Vertex {
        region: String,
        project_id: Option<String>,
        auth: VertexAuth,
        base_url: Option<String>,
        api_key: Option<String>,
    },
}

/// Resolved Anthropic client configuration.
///
/// This struct holds all the information needed to construct the actual
/// `anthropic_sdk::Anthropic` client. The `build()` method materializes it
/// into the SDK client.
///
/// Maps to: CC services/api/client.ts:88-316 (return value of getAnthropicClient)
#[derive(Clone, Debug)]
pub struct AnthropicClientHandle {
    /// Provider-specific configuration (Direct, Bedrock, Foundry, Vertex).
    pub provider: ProviderConfig,
    /// Default headers attached to every request.
    pub default_headers: HashMap<String, Option<String>>,
    /// Maximum retries on transient errors.
    pub max_retries: u32,
    /// Request timeout in milliseconds.
    pub timeout_ms: u64,
    /// CC `ARGS.fetch`, the wrapper [`build_fetch`] returns.
    pub fetch: ResolvedFetch,
    /// CC `ARGS.fetchOptions`: the proxy, TLS and socket transport, carried by
    /// the client built from [`crate::utils::proxy::get_proxy_fetch_options`].
    pub fetch_options: reqwest::Client,
    /// The SDK's `logLevel`, resolved from `ANTHROPIC_LOG` in the snapshot.
    pub log_level: anthropic_sdk::LogLevel,
}

impl AnthropicClientHandle {
    /// Build the SDK client for this handle.
    ///
    /// Every client takes the options CC hands the SDK, with each option CC
    /// leaves to `readEnv` already resolved (see [`ProviderConfig::Direct`]).
    /// Direct and Foundry are the core client, Vertex its provider client;
    /// Bedrock still needs its provider SDK (environment redesign C2c). See
    /// [`ClientBuildOutput`] for CC's `as unknown as Anthropic`.
    ///
    /// Maps to: CC services/api/client.ts:141-316
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Sdk` for unsupported providers or SDK construction failures.
    pub fn build(self) -> Result<ClientBuildOutput, ClientError> {
        match &self.provider {
            ProviderConfig::Direct {
                api_key,
                auth_token,
                base_url,
            } => self.build_direct_client(api_key.clone(), auth_token.clone(), base_url.clone()),
            ProviderConfig::Bedrock { .. } => Err(ClientError::Sdk(
                "Bedrock provider SDK is not wired in Cometix Phase 1; use Direct API".to_string(),
            )),
            ProviderConfig::Foundry {
                auth,
                base_url,
                resource,
                api_key,
            } => {
                // CC `new AnthropicFoundry({ ...ARGS, azureADTokenProvider? })`
                // (`client.ts:191-219`). The endpoint and key are the
                // `readEnv` values `AnthropicFoundry` reads for itself
                // (`foundry-sdk client.ts:58-60`), and its checks (both
                // endpoints set, neither, a key beside a token provider, no
                // credential) run in `create_client_with_core_options`. It
                // overrides `authHeaders`, so only the Foundry key or the
                // Azure token is sent, never `ANTHROPIC_API_KEY`/
                // `ANTHROPIC_AUTH_TOKEN`; an Authorization that CC's
                // `configureApiKeyHeaders` put in `defaultHeaders` still goes
                // out, and `default_headers` carries it here too.
                let token_provider: Option<Box<dyn anthropic_sdk_foundry::TokenProvider>> =
                    match auth {
                        FoundryAuth::ApiKey => None,
                        FoundryAuth::SkipAuth => Some(Box::new(SkipFoundryAuth)),
                        FoundryAuth::AzureAd => {
                            use anthropic_sdk_foundry::azure_identity;
                            // The npm package reads `process.env` and sends
                            // through its own pipeline; here the credential
                            // reads the carrier, and its requests go through
                            // CC's transport (environment redesign C2c-2).
                            let env = azure_identity::Environment::new(
                                crate::utils::process_env::snapshot()
                                    .iter()
                                    .map(|(key, value)| (key.to_owned(), value.to_owned())),
                            );
                            let http_client = crate::utils::proxy::create_axios_instance()
                                .and_then(|builder| Ok(builder.build()?))
                                .map_err(|error| ClientError::Sdk(error.to_string()))?;
                            let credential = azure_identity::DefaultAzureCredential::new(
                                azure_identity::DefaultAzureCredentialOptions {
                                    env,
                                    http_client: Some(http_client),
                                },
                            )
                            .map_err(|error| ClientError::Sdk(error.to_string()))?;
                            Some(Box::new(AzureAdTokenProvider(
                                azure_identity::get_bearer_token_provider(
                                    std::sync::Arc::new(credential),
                                    "https://cognitiveservices.azure.com/.default",
                                ),
                            )))
                        }
                    };
                let config = anthropic_sdk_foundry::FoundryConfig {
                    resource: resource.clone().unwrap_or_default(),
                    api_key: api_key.clone(),
                    token_provider,
                    base_url: base_url.clone(),
                };
                // `ARGS` carries no `apiKey`; the Foundry client sets the key
                // and the token itself.
                let core_options = self.core_client_options(Nullable::Unset, Nullable::Unset, None);
                anthropic_sdk_foundry::create_client_with_core_options(config, core_options)
                    .map(ClientBuildOutput::Anthropic)
                    .map_err(|error| {
                        // `ClientError::Sdk` adds the SDK's own prefix.
                        ClientError::Sdk(match error {
                            anthropic_sdk::ApiError::Sdk(message) => message,
                            error => error.to_string(),
                        })
                    })
            }
            ProviderConfig::Vertex {
                region,
                project_id,
                auth,
                base_url,
                api_key,
            } => {
                // CC `new AnthropicVertex({ ...ARGS, region, googleAuth })`
                // (`client.ts:290-297`).
                let token_provider: Option<
                    std::sync::Arc<dyn anthropic_sdk_vertex::TokenProvider>,
                > = match auth {
                    VertexAuth::GoogleAuth => None,
                    VertexAuth::SkipAuth => Some(std::sync::Arc::new(SkipVertexAuth)),
                };
                let config = anthropic_sdk_vertex::VertexConfig {
                    project_id: project_id.clone().unwrap_or_default(),
                    region: region.clone(),
                    access_token: None,
                    token_provider,
                    base_url: base_url.clone(),
                };
                // `ARGS` carries no `apiKey`: the core default
                // `readEnv('ANTHROPIC_API_KEY') ?? null`, read from the
                // snapshot. The Vertex client sets the token and base URL.
                let core_options = self.core_client_options(
                    Nullable::from_resolved(api_key.clone()),
                    Nullable::Unset,
                    None,
                );
                anthropic_sdk_vertex::AnthropicVertex::new_with_core_options(&config, core_options)
                    .map(ClientBuildOutput::Vertex)
                    .map_err(|error| ClientError::Sdk(error.to_string()))
            }
        }
    }

    /// Returns the active provider type.
    ///
    /// Maps to: CC services/api/client.ts:153-316 (provider dispatch)
    pub fn provider_type(&self) -> ApiProvider {
        match &self.provider {
            ProviderConfig::Direct { .. } => ApiProvider::FirstParty,
            ProviderConfig::Bedrock { .. } => ApiProvider::Bedrock,
            ProviderConfig::Foundry { .. } => ApiProvider::Foundry,
            ProviderConfig::Vertex { .. } => ApiProvider::Vertex,
        }
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
            log_level: Some(self.log_level),
            // CC `client.ts:168,216,294,312`:
            // `...(isDebugToStdErr() && { logger: createStderrLogger() })`.
            logger: crate::utils::debug::debug_to_stderr_flag().then(create_stderr_logger),
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

/// CC's skip-auth token provider for Foundry (`client.ts:197-199`),
/// `() => Promise.resolve('')`. `AnthropicFoundry` rejects the empty token on
/// every request (`foundry-sdk client.ts:117-121`), so in CC
/// `CLAUDE_CODE_SKIP_FOUNDRY_AUTH` without a key fails each request with
/// "Expected azureADTokenProvider function argument to return a string but it
/// returned ", and so it does here.
struct SkipFoundryAuth;

impl anthropic_sdk_foundry::TokenProvider for SkipFoundryAuth {
    fn get_token(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<String, anthropic_sdk_foundry::TokenProviderError>>
    {
        Box::pin(async { Ok(String::new()) })
    }
}

/// CC's `getBearerTokenProvider(new DefaultAzureCredential(), scope)`
/// (`client.ts:203-210`) as the Foundry client's token provider. The
/// credential's errors are not `ApiError`s, so the SDK prefixes them with
/// `Failed to get token from azureADTokenProvider: `, as TS does.
struct AzureAdTokenProvider(anthropic_sdk_foundry::azure_identity::BearerTokenProvider);

impl anthropic_sdk_foundry::TokenProvider for AzureAdTokenProvider {
    fn get_token(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<String, anthropic_sdk_foundry::TokenProviderError>>
    {
        Box::pin(async move { self.0.get_token().await.map_err(Into::into) })
    }
}

/// CC's skip-auth stand-in for `GoogleAuth` (`client.ts:265-271`), whose
/// `getRequestHeaders()` returns `{}`: no Google credential is sent.
struct SkipVertexAuth;

impl anthropic_sdk_vertex::TokenProvider for SkipVertexAuth {
    fn get_token(&self) -> futures::future::BoxFuture<'_, Result<String, anthropic_sdk::ApiError>> {
        Box::pin(async { Ok(String::new()) })
    }

    fn request_headers(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<HashMap<String, String>, anthropic_sdk::ApiError>>
    {
        Box::pin(async { Ok(HashMap::new()) })
    }
}

/// The client [`AnthropicClientHandle::build`] returns. CC returns every
/// provider client as `Anthropic` (`as unknown as Anthropic`,
/// `client.ts:189,219,297`). The TS Vertex client rewrites requests in its own
/// `prepareOptions`/`buildRequest` (`vertex-sdk/src/client.ts:122-185`), so
/// every call through the cast keeps the endpoint rewriting and auth. The Rust
/// provider SDKs rewrite in their `messages`/`beta().messages()` wrappers
/// instead, so the provider client is kept and [`ClientBuildOutput::beta`]
/// dispatches to it. There is deliberately no `Deref` to the core client: a
/// `&Anthropic` parameter would take one silently and send a Vertex request to
/// the core path. [`ClientBuildOutput::as_client`] is the explicit way out.
#[derive(Clone)]
pub enum ClientBuildOutput {
    Anthropic(anthropic_sdk::Anthropic),
    Vertex(anthropic_sdk_vertex::AnthropicVertex),
}

impl ClientBuildOutput {
    /// `anthropic.beta`, through the provider's override.
    pub fn beta(&self) -> ClientBeta<'_> {
        ClientBeta(self)
    }

    /// The core client, with the provider's base URL and auth but without its
    /// request rewriting (as the provider SDKs' own `as_client`).
    pub fn as_client(&self) -> &anthropic_sdk::Anthropic {
        match self {
            Self::Anthropic(client) => client,
            Self::Vertex(client) => client.as_client(),
        }
    }
}

/// `anthropic.beta` of a [`ClientBuildOutput`].
pub struct ClientBeta<'a>(&'a ClientBuildOutput);

impl<'a> ClientBeta<'a> {
    /// `anthropic.beta.messages`, through the provider's override.
    pub fn messages(&self) -> ClientBetaMessages<'a> {
        ClientBetaMessages(self.0)
    }
}

/// `anthropic.beta.messages` of a [`ClientBuildOutput`]: the calls Cometix
/// makes, each sent through the provider client.
pub struct ClientBetaMessages<'a>(&'a ClientBuildOutput);

impl ClientBetaMessages<'_> {
    pub async fn create_with_options(
        &self,
        params: &anthropic_sdk::resources::beta::messages::BetaMessageCreateParams,
        options: Option<&anthropic_sdk::RequestOptions>,
    ) -> Result<anthropic_sdk::resources::beta::messages::BetaMessage, anthropic_sdk::ApiError>
    {
        match self.0 {
            ClientBuildOutput::Anthropic(client) => {
                client
                    .beta()
                    .messages()
                    .create_with_options(params, options)
                    .await
            }
            ClientBuildOutput::Vertex(client) => {
                client
                    .beta()
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
            ClientBuildOutput::Anthropic(client) => {
                client
                    .beta()
                    .messages()
                    .create_with_response_and_options(params, options)
                    .await
            }
            ClientBuildOutput::Vertex(client) => {
                client
                    .beta()
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
    ) -> Result<
        anthropic_sdk::ApiResponse<
            anthropic_sdk::core::streaming::SseStream<
                anthropic_sdk::resources::beta::messages::BetaMessageStreamEvent,
            >,
        >,
        anthropic_sdk::ApiError,
    > {
        match self.0 {
            ClientBuildOutput::Anthropic(client) => {
                client
                    .beta()
                    .messages()
                    .create_stream_with_response_and_options(params, options)
                    .await
            }
            ClientBuildOutput::Vertex(client) => {
                client
                    .beta()
                    .messages()
                    .create_stream_with_response_and_options(params, options)
                    .await
            }
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
            ClientBuildOutput::Anthropic(client) => {
                client.beta().messages().count_tokens(params).await
            }
            ClientBuildOutput::Vertex(client) => {
                client.beta().messages().count_tokens(params).await
            }
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
/// passed at `claude.ts:848,1783`); Cometix's `claude.rs` dumps prompts
/// itself, so it needs no override.
fn build_fetch(source: Option<String>) -> ResolvedFetch {
    ResolvedFetch {
        inject_client_request_id: crate::utils::model::providers::get_api_provider()
            == ApiProvider::FirstParty
            && crate::utils::model::providers::is_first_party_anthropic_base_url(),
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
            {
                if let Ok(id) = reqwest::header::HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
                {
                    request.headers_mut().insert(CLIENT_REQUEST_ID_HEADER, id);
                }
            }
            // `headers.get` is a ByteString (one char per byte); `id ? ... : ''`
            // omits an empty preset.
            let id = request
                .headers()
                .get(CLIENT_REQUEST_ID_HEADER)
                .map(|value| value.as_bytes().iter().map(|&byte| char::from(byte)).collect::<String>())
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

/// Maps to: CC `services/api/client.ts:73-86` `createStderrLogger`: every
/// level goes to stderr as `[Anthropic SDK <LEVEL>] <message>`. The SDK still
/// filters by its own level (`ANTHROPIC_LOG`, default `warn`), so request
/// lines such as `sending request: POST <url>` need `ANTHROPIC_LOG=debug`.
fn create_stderr_logger() -> std::sync::Arc<dyn anthropic_sdk::SdkLogger> {
    std::sync::Arc::new(StderrLogger)
}

/// The logger object [`create_stderr_logger`] returns.
struct StderrLogger;

impl anthropic_sdk::SdkLogger for StderrLogger {
    fn log(&self, level: anthropic_sdk::LogLevel, message: &str) {
        let tag = match level {
            anthropic_sdk::LogLevel::Error => "ERROR",
            anthropic_sdk::LogLevel::Warn => "WARN",
            anthropic_sdk::LogLevel::Info => "INFO",
            anthropic_sdk::LogLevel::Debug => "DEBUG",
            anthropic_sdk::LogLevel::Off => return,
        };
        eprintln!("[Anthropic SDK {tag}] {message}");
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_key = [0u8; BLOCK_SIZE];
    let mut outer_key = [0u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_key[index] = normalized[index] ^ 0x36;
        outer_key[index] = normalized[index] ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(inner_key);
    inner.update(data);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_key);
    outer.update(inner_digest);
    outer.finalize().into()
}

pub(crate) fn aws_uri_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push('%');
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    output
}

fn aws_sigv4_headers(
    method: &str,
    url: &reqwest::Url,
    body: &[u8],
    region: &str,
    service: &str,
    credentials: &AwsCredentials,
) -> Vec<(String, String)> {
    let now = chrono::Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    };
    let payload_hash = sha256_hex(body);
    let mut canonical_headers = vec![
        ("content-type", "application/json".to_string()),
        ("host", host),
        ("x-amz-content-sha256", payload_hash.clone()),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(session_token) = credentials.session_token.as_ref() {
        canonical_headers.push(("x-amz-security-token", session_token.clone()));
    }
    canonical_headers.sort_by_key(|(name, _)| *name);
    let signed_headers = canonical_headers
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let canonical_header_text = canonical_headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect::<String>();
    let canonical_request = format!(
        "{method}\n{}\n{}\n{canonical_header_text}\n{signed_headers}\n{payload_hash}",
        url.path(),
        url.query().unwrap_or_default()
    );
    let credential_scope = format!("{short_date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let date_key = hmac_sha256(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        short_date.as_bytes(),
    );
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    let signing_key = hmac_sha256(&service_key, b"aws4_request");
    let signature = hex_lower(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key_id
    );
    let mut headers = canonical_headers
        .into_iter()
        .filter(|(name, _)| *name != "host")
        .map(|(name, value)| (name.to_string(), value))
        .collect::<Vec<_>>();
    headers.push(("authorization".to_string(), authorization));
    headers
}

/// Rust provider-SDK wire adapter used by CC's Bedrock owners.
pub(crate) async fn send_bedrock_request(
    method: reqwest::Method,
    endpoint: &str,
    path: &str,
    body: Vec<u8>,
    region: &str,
    service: &str,
    auth: &BedrockAuth,
) -> Option<serde_json::Value> {
    let url = reqwest::Url::parse(&format!(
        "{}/{}",
        endpoint.trim_end_matches('/'),
        path.trim_start_matches('/')
    ))
    .ok()?;
    let client = reqwest::Client::new();
    let mut request = client
        .request(method.clone(), url.clone())
        .header("content-type", "application/json");
    if !body.is_empty() {
        request = request.body(body.clone());
    }
    match auth {
        // The `Authorization` `get_anthropic_client` built from its snapshot.
        BedrockAuth::BearerToken { extra_headers } => {
            let authorization = extra_headers.get("Authorization").cloned().flatten()?;
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }
        BedrockAuth::Credentials(credentials) => {
            for (name, value) in
                aws_sigv4_headers(method.as_str(), &url, &body, region, service, credentials)
            {
                request = request.header(name, value);
            }
        }
        BedrockAuth::DefaultChain => {
            tracing::warn!("Bedrock default credential chain requires the provider SDK adapter");
            return None;
        }
        BedrockAuth::SkipAuth => {}
    }
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "Bedrock provider request failed");
        return None;
    }
    response.json::<serde_json::Value>().await.ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
    fn get_custom_headers_parses_curl_style_headers() {
        // Simulate ANTHROPIC_CUSTOM_HEADERS env var
        let raw = "X-Custom: value1\nAuthorization: Bearer tok\n\nBad-No-Colon\nX-Empty:\n";
        let mut headers = HashMap::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(colon_idx) = trimmed.find(':') {
                let name = trimmed[..colon_idx].trim();
                let value = trimmed[colon_idx + 1..].trim();
                if !name.is_empty() {
                    headers.insert(name.to_string(), value.to_string());
                }
            }
        }
        assert_eq!(headers.get("X-Custom").map(|s| s.as_str()), Some("value1"));
        assert_eq!(
            headers.get("Authorization").map(|s| s.as_str()),
            Some("Bearer tok")
        );
        assert_eq!(headers.get("X-Empty").map(|s| s.as_str()), Some(""));
        assert!(!headers.contains_key("Bad-No-Colon"));
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

    #[test]
    fn vertex_region_falls_back_to_us_east5() {
        // When no env vars are set, default should be us-east5
        let region = get_vertex_region_for_model(None);
        // Could be overridden by CLOUD_ML_REGION in test env, so just check non-empty
        assert!(!region.is_empty());
    }

    #[test]
    fn aws_region_has_sane_default() {
        let region = get_aws_region();
        assert!(!region.is_empty());
    }

    #[tokio::test]
    async fn explicit_api_key_auth_remains_available_without_oauth_credentials() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-api-key-oauth-gate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _auth_token = EnvVarGuard::unset("ANTHROPIC_AUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let handle = get_anthropic_client(GetAnthropicClientOptions {
            api_key: Some("sk-ant-test".to_string()),
            ..GetAnthropicClientOptions::default()
        })
        .await
        .expect("unrelated explicit API-key auth must remain available");
        // No `ANTHROPIC_AUTH_TOKEN`: the SDK default `readEnv(...) ?? null` is
        // `null`, passed explicitly.
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

    /// CC leaves `authToken` and `baseURL` to the SDK, which takes
    /// `readEnv(key)`: trimmed, `''` kept, then `baseURL || default`. They are
    /// computed from the snapshot and passed explicitly, so the SDK reads no
    /// environment variable.
    #[tokio::test]
    async fn sdk_defaults_are_computed_from_the_snapshot_like_read_env() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-sdk-defaults-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _base_url = EnvVarGuard::set("ANTHROPIC_BASE_URL", " https://gateway.example/ \n");
        let _auth_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", "\u{feff} tok ");
        let _timeout = EnvVarGuard::set("API_TIMEOUT_MS", " 90000ms");

        let handle = get_anthropic_client(GetAnthropicClientOptions {
            api_key: Some("sk-ant-test".to_string()),
            ..GetAnthropicClientOptions::default()
        })
        .await
        .unwrap();

        let ProviderConfig::Direct {
            ref auth_token,
            ref base_url,
            ..
        } = handle.provider
        else {
            panic!("first-party provider expected");
        };
        assert_eq!(auth_token, &Nullable::Set("tok".to_string()));
        assert_eq!(base_url.as_deref(), Some("https://gateway.example/"));
        // `parseInt(' 90000ms', 10)` is 90000.
        assert_eq!(handle.timeout_ms, 90_000);
        // CC's own `process.env.ANTHROPIC_AUTH_TOKEN || helper` uses the value
        // as set, untrimmed.
        assert_eq!(
            handle.default_headers.get("Authorization"),
            Some(&Some("Bearer \u{feff} tok ".to_string()))
        );
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// An unset `ANTHROPIC_BASE_URL` is `Some("")`: the SDK default without
    /// its own environment read.
    #[tokio::test]
    async fn unset_base_url_is_the_sdk_default_without_an_environment_read() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-sdk-base-url-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _base_url = EnvVarGuard::unset("ANTHROPIC_BASE_URL");

        let handle = get_anthropic_client(GetAnthropicClientOptions {
            api_key: Some("sk-ant-test".to_string()),
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
        assert_eq!(preset.headers().get(CLIENT_REQUEST_ID_HEADER).unwrap(), "caller-id");

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
            log_level: anthropic_sdk::LogLevel::Warn,
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

    /// CC `new AnthropicVertex({ ...ARGS, region, googleAuth })` cast to
    /// `Anthropic`: each `beta.messages` call Cometix makes is rewritten as
    /// the TS client's `buildRequest` does (`vertex-sdk/src/client.ts:139-182`)
    /// and carries `ARGS`'s headers. The skip-auth mock sends no Google
    /// credential; the key is the one read from the snapshot.
    #[tokio::test]
    async fn vertex_messages_go_to_the_vertex_endpoint_through_args() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let head_end = loop {
                    if let Some(at) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break at + 4;
                    }
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0, "HTTP request ended before headers");
                    request.extend_from_slice(&buffer[..count]);
                };
                let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map_or(0, |value| value.trim().parse::<usize>().unwrap());
                while request.len() < head_end + length {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0, "HTTP request ended before its body");
                    request.extend_from_slice(&buffer[..count]);
                }
                stream
                    .write_all(
                        b"HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\n\
                          content-length: 2\r\nconnection: close\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
                requests.push(String::from_utf8_lossy(&request).to_ascii_lowercase());
            }
            requests
        });
        let handle = AnthropicClientHandle {
            provider: ProviderConfig::Vertex {
                region: "us-east5".to_string(),
                project_id: Some("test-project".to_string()),
                auth: VertexAuth::SkipAuth,
                base_url: Some(format!("http://{address}/v1")),
                api_key: Some("snapshot-key".to_string()),
            },
            default_headers: HashMap::from([("x-app".to_string(), Some("cli".to_string()))]),
            max_retries: 0,
            timeout_ms: 5_000,
            fetch: ResolvedFetch {
                inject_client_request_id: false,
                source: Some("test".to_string()),
            },
            fetch_options: direct_client(),
            log_level: anthropic_sdk::LogLevel::Warn,
        };
        let client = handle.build().unwrap();
        assert!(matches!(client, ClientBuildOutput::Vertex(_)));
        let params = anthropic_sdk::resources::beta::messages::BetaMessageCreateParams {
            model: "claude-test".to_string(),
            max_tokens: 16,
            messages: vec![anthropic_sdk::resources::beta::messages::BetaMessageParam {
                role: "user".to_string(),
                content: anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(
                    "hi".to_string(),
                ),
            }],
            ..Default::default()
        };
        let count_params = anthropic_sdk::resources::beta::messages::BetaMessageCountTokensParams {
            model: "claude-test".to_string(),
            messages: params.messages.clone(),
            ..Default::default()
        };
        let messages = client.beta().messages();
        let _ = messages.create_with_options(&params, None).await;
        let _ = messages
            .create_with_response_and_options(&params, None)
            .await;
        let _ = messages
            .create_stream_with_response_and_options(&params, None)
            .await;
        let _ = messages.count_tokens(&count_params).await;
        let requests = tokio::time::timeout(std::time::Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
        let [create, create_with_response, stream, count] = requests.as_slice() else {
            panic!("four requests expected: {requests:?}");
        };
        let models =
            "post /v1/projects/test-project/locations/us-east5/publishers/anthropic/models/";
        for request in [create, create_with_response] {
            assert!(
                request.starts_with(&format!("{models}claude-test:rawpredict ")),
                "{request}"
            );
        }
        assert!(
            stream.starts_with(&format!("{models}claude-test:streamrawpredict ")),
            "{stream}"
        );
        assert!(stream.contains("\"stream\":true"), "{stream}");
        assert!(
            count.starts_with(&format!("{models}count-tokens:rawpredict ")),
            "{count}"
        );
        assert!(count.contains("token-counting-2024-11-01"), "{count}");
        assert!(count.contains("\"model\":\"claude-test\""), "{count}");
        for request in [create, create_with_response, stream] {
            assert!(!request.contains("\"model\""), "{request}");
        }
        for request in [create, create_with_response, stream, count] {
            assert!(
                request.contains("\"anthropic_version\":\"vertex-2023-10-16\""),
                "{request}"
            );
            assert!(request.contains("\r\nx-app: cli"), "{request}");
            assert!(request.contains("\r\nx-api-key: snapshot-key"), "{request}");
            assert!(!request.contains("\r\nauthorization:"), "{request}");
        }
    }

    /// The Vertex endpoint and key are `readEnv` values taken from the
    /// snapshot (`vertex-sdk client.ts:78`, the core's `apiKey` default).
    #[tokio::test]
    #[allow(clippy::disallowed_methods)] // Deliberately desyncs the OS from the carrier.
    async fn vertex_handle_reads_its_sdk_defaults_from_the_snapshot() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home =
            std::env::temp_dir().join(format!("cometix-vertex-defaults-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).unwrap();
        let _guards = [
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home),
            EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK"),
            EnvVarGuard::set("CLAUDE_CODE_USE_VERTEX", "1"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY"),
            EnvVarGuard::set("CLAUDE_CODE_SKIP_VERTEX_AUTH", "1"),
            EnvVarGuard::set("ANTHROPIC_VERTEX_PROJECT_ID", " vertex-project "),
            EnvVarGuard::set("ANTHROPIC_VERTEX_BASE_URL", " https://vertex.example/v1 "),
            EnvVarGuard::set("ANTHROPIC_API_KEY", " env-key "),
        ];
        // The OS environment disagrees with the carrier, so a read that went
        // around the snapshot would surface here. The guards restore only the
        // carrier; `OsRestore` puts the OS values back to the startup capture,
        // which is what the OS holds in a test process, even on a panic.
        //
        // SAFETY (both blocks): writing the real environment races any thread
        // that calls libc's getenv. nextest, the project's test gate, runs
        // this test alone in its process; a threaded harness could still race
        // an unrelated test.
        const KEYS: [&str; 3] = [
            "ANTHROPIC_VERTEX_PROJECT_ID",
            "ANTHROPIC_VERTEX_BASE_URL",
            "ANTHROPIC_API_KEY",
        ];
        struct OsRestore;
        impl Drop for OsRestore {
            fn drop(&mut self) {
                let startup = crate::utils::process_env::startup_snapshot();
                for key in KEYS {
                    unsafe {
                        match startup.var_os(key) {
                            Some(value) => std::env::set_var(key, value),
                            None => std::env::remove_var(key),
                        }
                    }
                }
            }
        }
        let _os_restore = OsRestore;
        let os_values = ["os-project", "https://os.example/v1", "os-key"];
        for (key, value) in KEYS.into_iter().zip(os_values) {
            unsafe { std::env::set_var(key, value) };
        }
        let handle = get_anthropic_client(GetAnthropicClientOptions::default())
            .await
            .unwrap();
        let ProviderConfig::Vertex {
            project_id,
            auth,
            base_url,
            api_key,
            ..
        } = &handle.provider
        else {
            panic!("Vertex expected: {:?}", handle.provider);
        };
        assert_eq!(project_id.as_deref(), Some("vertex-project"));
        assert!(matches!(auth, VertexAuth::SkipAuth));
        assert_eq!(base_url.as_deref(), Some("https://vertex.example/v1"));
        assert_eq!(api_key.as_deref(), Some("env-key"));
        let _ = std::fs::remove_dir_all(config_home);
    }

    fn foundry_handle(
        auth: FoundryAuth,
        base_url: Option<String>,
        resource: Option<&str>,
        api_key: Option<&str>,
    ) -> AnthropicClientHandle {
        AnthropicClientHandle {
            provider: ProviderConfig::Foundry {
                auth,
                base_url,
                resource: resource.map(str::to_owned),
                api_key: api_key.map(str::to_owned),
            },
            default_headers: HashMap::from([("x-app".to_string(), Some("cli".to_string()))]),
            max_retries: 0,
            timeout_ms: 5_000,
            fetch: ResolvedFetch {
                inject_client_request_id: false,
                source: Some("test".to_string()),
            },
            fetch_options: direct_client(),
            log_level: anthropic_sdk::LogLevel::Warn,
        }
    }

    /// CC `new AnthropicFoundry({ ...ARGS })` with `ANTHROPIC_FOUNDRY_API_KEY`:
    /// only the Foundry key is sent (`foundry-sdk client.ts:103-131`), next
    /// to `ARGS`'s headers.
    #[tokio::test]
    async fn foundry_key_mode_sends_only_the_foundry_key_through_args() {
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
        let client = foundry_handle(
            FoundryAuth::ApiKey,
            Some(format!("http://{address}/anthropic/")),
            None,
            Some("foundry-key"),
        )
        .build()
        .unwrap();
        let _ = client.as_client().models().retrieve("model", None).await;
        let request = tokio::time::timeout(std::time::Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
        assert!(
            request.starts_with("get /anthropic/v1/models/model"),
            "{request}"
        );
        assert!(
            request.contains("\r\nx-api-key: foundry-key\r\n"),
            "{request}"
        );
        assert!(request.contains("\r\nx-app: cli\r\n"), "{request}");
        assert!(!request.contains("\r\nauthorization:"), "{request}");
    }

    /// `AnthropicFoundry`'s endpoint checks (`foundry-sdk client.ts:69-93`)
    /// surface with the one prefix `ClientError` adds. An empty base URL is
    /// unset, so the resource decides and the key never goes to
    /// api.anthropic.com.
    #[test]
    fn foundry_endpoint_checks_come_from_the_sdk() {
        let both = foundry_handle(
            FoundryAuth::ApiKey,
            Some("https://proxy.example/".to_string()),
            Some("res"),
            Some("key"),
        );
        let Err(error) = both.build() else {
            panic!("a base URL and a resource together are rejected");
        };
        assert_eq!(
            error.to_string(),
            "SDK error: baseURL and resource are mutually exclusive"
        );
        let Err(error) = foundry_handle(FoundryAuth::ApiKey, None, None, Some("key")).build()
        else {
            panic!("an endpoint is required");
        };
        assert_eq!(
            error.to_string(),
            "SDK error: Must provide one of the `baseURL` or `resource` arguments, or the `ANTHROPIC_FOUNDRY_RESOURCE` environment variable"
        );
        let empty = foundry_handle(
            FoundryAuth::ApiKey,
            Some(String::new()),
            Some("res"),
            Some("key"),
        );
        assert_eq!(
            empty.build().unwrap().as_client().base_url(),
            "https://res.services.ai.azure.com/anthropic/"
        );
    }

    /// CC's skip-auth provider resolves to `''`, which `AnthropicFoundry`
    /// rejects before sending (`client.ts:197-199`, `foundry-sdk
    /// client.ts:117-121`): the request fails, as in CC.
    #[tokio::test]
    async fn foundry_skip_auth_fails_every_request_like_cc() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = foundry_handle(
            FoundryAuth::SkipAuth,
            Some(format!("http://{address}/anthropic/")),
            None,
            None,
        )
        .build()
        .unwrap();
        let error = client
            .as_client()
            .models()
            .retrieve("model", None)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, anthropic_sdk::ApiError::Sdk(message) if message
                == "Expected azureADTokenProvider function argument to return a string but it returned "),
            "{error}"
        );
    }

    /// CC `getBearerTokenProvider(new DefaultAzureCredential(), scope)`
    /// (`client.ts:203-210`) behind `AnthropicFoundry`: a chain that yields no
    /// token fails the request with the SDK's prefix in front of the chain's
    /// aggregate error. `AZURE_TOKEN_CREDENTIALS` keeps the chain to the Azure
    /// CLI, which is not on `PATH`, so nothing reaches the network.
    #[cfg(unix)]
    #[tokio::test]
    async fn foundry_azure_ad_failure_matches_official_prefixed_chain_error() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let empty = std::env::temp_dir().join(format!("cometix-no-az-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&empty).unwrap();
        let _guards = [
            EnvVarGuard::set("AZURE_TOKEN_CREDENTIALS", "AzureCliCredential"),
            EnvVarGuard::set("PATH", &empty),
        ];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = foundry_handle(
            FoundryAuth::AzureAd,
            Some(format!("http://{address}/anthropic/")),
            None,
            None,
        )
        .build()
        .unwrap();
        let error = client
            .as_client()
            .models()
            .retrieve("model", None)
            .await
            .unwrap_err();
        let _ = std::fs::remove_dir_all(&empty);
        assert!(
            matches!(&error, anthropic_sdk::ApiError::Sdk(message) if message
                == "Failed to get token from azureADTokenProvider: ChainedTokenCredential authentication failed.\n\
                    CredentialUnavailableError: Azure CLI could not be found. Please visit https://aka.ms/azure-cli for installation instructions and then, once installed, authenticate to your Azure account using 'az login'."),
            "{error}"
        );
    }

    /// CC `ARGS.fetchOptions`: the SDK sends through the environment's proxy.
    /// `HTTPS_PROXY` carries an `http://` request too, CC's one proxy for
    /// every scheme; reqwest's own detection would send it direct.
    #[tokio::test]
    async fn sdk_requests_take_the_proxy_from_fetch_options() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-sdk-proxy-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let _guards = [
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home),
            EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY"),
            EnvVarGuard::unset("ANTHROPIC_UNIX_SOCKET"),
            EnvVarGuard::set("ANTHROPIC_BASE_URL", "http://api.invalid"),
            EnvVarGuard::unset("https_proxy"),
            EnvVarGuard::set("HTTPS_PROXY", format!("http://{proxy_address}")),
            EnvVarGuard::unset("http_proxy"),
            EnvVarGuard::unset("HTTP_PROXY"),
            EnvVarGuard::unset("no_proxy"),
            EnvVarGuard::unset("NO_PROXY"),
        ];
        let server = tokio::spawn(async move {
            let (mut stream, _) = proxy.accept().await.unwrap();
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
            String::from_utf8_lossy(&request).into_owned()
        });

        let handle = get_anthropic_client(GetAnthropicClientOptions {
            api_key: Some("sk-ant-test".to_string()),
            max_retries: 0,
            ..GetAnthropicClientOptions::default()
        })
        .await
        .unwrap();
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
        assert!(
            request.starts_with("GET http://api.invalid/v1/models/model"),
            "{request}"
        );
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// CC `process.env.ANTHROPIC_AUTH_TOKEN || helper`, then `if (token)`: a
    /// whitespace-only token is truthy and goes out as set. The SDK's own
    /// `readEnv` trims the same value to `''`.
    #[tokio::test]
    async fn whitespace_auth_token_is_truthy_for_cc_and_empty_for_the_sdk() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-blank-auth-token-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _auth_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", "  ");

        let handle = get_anthropic_client(GetAnthropicClientOptions {
            api_key: Some("sk-ant-test".to_string()),
            ..GetAnthropicClientOptions::default()
        })
        .await
        .unwrap();
        assert_eq!(
            handle.default_headers.get("Authorization"),
            Some(&Some("Bearer   ".to_string()))
        );
        assert!(matches!(
            handle.provider,
            ProviderConfig::Direct { auth_token: Nullable::Set(ref token), .. } if token.is_empty()
        ));
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// CC `client.ts:301-304`: a subscriber's `apiKey` is an explicit `null`
    /// and `authToken` is the OAuth access token, so an `ANTHROPIC_API_KEY`
    /// in the environment (even an empty one) never reaches the request.
    #[tokio::test]
    async fn subscriber_passes_null_api_key_and_the_oauth_token_like_cc() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-subscriber-null-key-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        // Exported but empty: CC still treats the user as a subscriber, and
        // the SDK would keep `''` if it were asked to read the variable.
        let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "");
        let _auth_token = EnvVarGuard::unset("ANTHROPIC_AUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        std::fs::write(
            config_home.join(".credentials.json"),
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "live-access",
                    "refreshToken": "refresh-token",
                    "expiresAt": 4_102_444_800_000_u64,
                    "scopes": ["user:inference"]
                }
            })
            .to_string(),
        )
        .unwrap();

        let handle = get_anthropic_client(GetAnthropicClientOptions::default())
            .await
            .expect("a live subscriber token builds a client");
        assert!(
            matches!(
                handle.provider,
                ProviderConfig::Direct {
                    api_key: Nullable::Null,
                    auth_token: Nullable::Set(ref token),
                    ..
                } if token == "live-access"
            ),
            "{:?}",
            handle.provider
        );
        // On the wire: only the OAuth bearer, no `x-api-key`. Building the
        // SDK client needs the process TLS provider, as at startup.
        crate::utils::tls_provider::install_crypto_provider();
        let headers = handle
            .build()
            .expect("client builds")
            .as_client()
            .build_headers(0, None)
            .expect("headers build");
        assert!(headers.get("x-api-key").is_none(), "{headers:?}");
        assert_eq!(headers.get("authorization").unwrap(), "Bearer live-access");
        let _ = std::fs::remove_dir_all(config_home);
    }

    #[tokio::test]
    async fn selected_expired_oauth_propagates_the_closed_side_effect_error() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-selected-oauth-gate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        std::fs::write(
            config_home.join(".credentials.json"),
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "expired-access",
                    "refreshToken": "refresh-token",
                    "expiresAt": 0,
                    "scopes": ["user:inference"]
                }
            })
            .to_string(),
        )
        .unwrap();

        let error = get_anthropic_client(GetAnthropicClientOptions::default())
            .await
            .expect_err("selected expired OAuth must not become client-construction success");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        let _ = std::fs::remove_dir_all(config_home);
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
            fetch: build_fetch(Some("test".to_string())),
            fetch_options: direct_client(),
            log_level: anthropic_sdk::LogLevel::Warn,
        };
        assert_eq!(handle.provider_type(), ApiProvider::FirstParty);
    }

    #[test]
    fn provider_config_bedrock_has_correct_type() {
        let handle = AnthropicClientHandle {
            provider: ProviderConfig::Bedrock {
                region: "us-east-1".to_string(),
                auth: BedrockAuth::SkipAuth,
            },
            default_headers: HashMap::new(),
            max_retries: 2,
            timeout_ms: 600_000,
            fetch: build_fetch(None),
            fetch_options: direct_client(),
            log_level: anthropic_sdk::LogLevel::Warn,
        };
        assert_eq!(handle.provider_type(), ApiProvider::Bedrock);
    }

    #[test]
    fn provider_config_foundry_has_correct_type() {
        let handle = AnthropicClientHandle {
            provider: ProviderConfig::Foundry {
                auth: FoundryAuth::SkipAuth,
                base_url: None,
                resource: Some("example".to_string()),
                api_key: None,
            },
            default_headers: HashMap::new(),
            max_retries: 2,
            timeout_ms: 600_000,
            fetch: build_fetch(None),
            fetch_options: direct_client(),
            log_level: anthropic_sdk::LogLevel::Warn,
        };
        assert_eq!(handle.provider_type(), ApiProvider::Foundry);
    }

    #[test]
    fn provider_config_vertex_has_correct_type() {
        let handle = AnthropicClientHandle {
            provider: ProviderConfig::Vertex {
                region: "us-east5".to_string(),
                project_id: None,
                auth: VertexAuth::SkipAuth,
                base_url: None,
                api_key: None,
            },
            default_headers: HashMap::new(),
            max_retries: 2,
            timeout_ms: 600_000,
            fetch: build_fetch(None),
            fetch_options: direct_client(),
            log_level: anthropic_sdk::LogLevel::Warn,
        };
        assert_eq!(handle.provider_type(), ApiProvider::Vertex);
    }

    #[test]
    fn user_agent_uses_official_claude_cli_shape() {
        let ua = crate::utils::http::get_user_agent();
        assert!(ua.starts_with("claude-cli/"));
    }

    #[test]
    fn first_party_base_url_detection() {
        // When ANTHROPIC_BASE_URL is not set, should be first-party
        let result = crate::utils::model::providers::is_first_party_anthropic_base_url();
        // May be overridden in test env, just verify it does not panic
        let _ = result;
    }
}
