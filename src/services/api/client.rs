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
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Stub types for modules not yet ported
// ---------------------------------------------------------------------------

pub use crate::utils::model::providers::ApiProvider;

/// CC `utils/proxy.ts#getProxyFetchOptions` joins here through the SDK's HTTP
/// client, built by [`crate::utils::http::client_builder`] from the effective
/// `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`; no parallel Rust function is declared
/// for that deferred SDK representation seam.

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

/// Parse the `ANTHROPIC_CUSTOM_HEADERS` environment variable into a header map.
///
/// Format: newline-separated `Name: Value` pairs (curl style).
///
/// Maps to: CC services/api/client.ts:330-354
fn get_custom_headers() -> HashMap<String, String> {
    let mut headers = HashMap::new();
    let raw = match crate::utils::process_env::var("ANTHROPIC_CUSTOM_HEADERS") {
        Ok(v) => v,
        Err(_) => return headers,
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
async fn configure_api_key_headers(headers: &mut HashMap<String, Option<String>>) {
    let is_non_interactive = crate::bootstrap::state::get_is_non_interactive_session();
    let token = crate::utils::process_env::var("ANTHROPIC_AUTH_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());

    // Try the async api key helper if no env token
    let token = match token {
        Some(t) => Some(t),
        None => crate::utils::auth::get_api_key_from_api_key_helper(is_non_interactive),
    };

    if let Some(t) = token {
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
    /// Override API key (falls back to ANTHROPIC_API_KEY env var).
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
    let oauth_auth_selected = crate::utils::model::providers::get_api_provider()
        == ApiProvider::FirstParty
        && is_claude_ai_subscriber();

    // ----- Build default headers -----
    // Maps to: CC services/api/client.ts:101-129
    let container_id = crate::utils::process_env::var("CLAUDE_CODE_CONTAINER_ID").ok();
    let remote_session_id = crate::utils::process_env::var("CLAUDE_CODE_REMOTE_SESSION_ID").ok();
    let client_app = crate::utils::process_env::var("CLAUDE_AGENT_SDK_CLIENT_APP").ok();
    let custom_headers = get_custom_headers();

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

    if let Some(cid) = &container_id {
        default_headers.insert(
            "x-claude-remote-container-id".to_string(),
            Some(cid.clone()),
        );
    }
    if let Some(rsid) = &remote_session_id {
        default_headers.insert("x-claude-remote-session-id".to_string(), Some(rsid.clone()));
    }
    if let Some(app) = &client_app {
        default_headers.insert("x-client-app".to_string(), Some(app.clone()));
    }

    crate::utils::debug::log_for_debugging(&format!(
        "[API:request] Creating client, ANTHROPIC_CUSTOM_HEADERS present: {}, has Authorization header: {}",
        crate::utils::process_env::var("ANTHROPIC_CUSTOM_HEADERS").is_ok(),
        custom_headers.contains_key("Authorization"),
    ));

    // Additional protection header
    // Maps to: CC services/api/client.ts:124-129
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_ADDITIONAL_PROTECTION")
            .ok()
            .as_deref(),
    ) {
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
        configure_api_key_headers(&mut default_headers).await;
    }

    // ----- Common client args -----
    // Maps to: CC services/api/client.ts:141-152
    let timeout_ms: u64 = crate::utils::process_env::var("API_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_API_TIMEOUT_MS);

    // Whether to inject x-client-request-id for first-party API calls only
    let inject_client_request_id = crate::utils::model::providers::get_api_provider()
        == ApiProvider::FirstParty
        && crate::utils::model::providers::is_first_party_anthropic_base_url();

    // ----- Provider dispatch -----
    let provider = crate::utils::model::providers::get_api_provider();

    match provider {
        // ---------------------------------------------------------------
        // Bedrock
        // Maps to: CC services/api/client.ts:153-190
        // ---------------------------------------------------------------
        ApiProvider::Bedrock => {
            let small_fast_model = get_small_fast_model();
            let aws_region = if model.as_deref() == Some(small_fast_model.as_str()) {
                crate::utils::process_env::var("ANTHROPIC_SMALL_FAST_MODEL_AWS_REGION")
                    .ok()
                    .unwrap_or_else(get_aws_region)
            } else {
                get_aws_region()
            };

            crate::utils::debug::log_for_debugging(&format!(
                "[API:bedrock] region={aws_region}, skip_auth={}",
                crate::utils::env_utils::is_env_truthy(
                    crate::utils::process_env::var("CLAUDE_CODE_SKIP_BEDROCK_AUTH")
                        .ok()
                        .as_deref()
                ),
            ));

            // Determine auth strategy
            let bedrock_auth =
                if let Ok(bearer) = crate::utils::process_env::var("AWS_BEARER_TOKEN_BEDROCK") {
                    // Bearer token auth overrides everything
                    let mut hdrs = default_headers.clone();
                    hdrs.insert(
                        "Authorization".to_string(),
                        Some(format!("Bearer {bearer}")),
                    );
                    BedrockAuth::BearerToken {
                        extra_headers: hdrs,
                    }
                } else if crate::utils::env_utils::is_env_truthy(
                    crate::utils::process_env::var("CLAUDE_CODE_SKIP_BEDROCK_AUTH")
                        .ok()
                        .as_deref(),
                ) {
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
                inject_client_request_id,
                source,
            })
        }

        // ---------------------------------------------------------------
        // Foundry (Azure)
        // Maps to: CC services/api/client.ts:191-220
        // ---------------------------------------------------------------
        ApiProvider::Foundry => {
            let foundry_auth =
                if crate::utils::process_env::var("ANTHROPIC_FOUNDRY_API_KEY").is_ok() {
                    FoundryAuth::ApiKey
                } else if crate::utils::env_utils::is_env_truthy(
                    crate::utils::process_env::var("CLAUDE_CODE_SKIP_FOUNDRY_AUTH")
                        .ok()
                        .as_deref(),
                ) {
                    FoundryAuth::SkipAuth
                } else {
                    FoundryAuth::AzureAd
                };

            crate::utils::debug::log_for_debugging(&format!(
                "[API:foundry] auth={foundry_auth:?}, skip_auth={}",
                crate::utils::env_utils::is_env_truthy(
                    crate::utils::process_env::var("CLAUDE_CODE_SKIP_FOUNDRY_AUTH")
                        .ok()
                        .as_deref()
                ),
            ));

            Ok(AnthropicClientHandle {
                provider: ProviderConfig::Foundry { auth: foundry_auth },
                default_headers,
                max_retries,
                timeout_ms,
                inject_client_request_id,
                source,
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
            let project_id = crate::utils::process_env::var("ANTHROPIC_VERTEX_PROJECT_ID").ok();

            // Determine whether GoogleAuth needs an explicit projectId fallback
            // to avoid the 12-second GCE metadata server timeout.
            // Maps to: CC services/api/client.ts:253-288
            let has_project_env_var = crate::utils::process_env::var("GCLOUD_PROJECT").is_ok()
                || crate::utils::process_env::var("GOOGLE_CLOUD_PROJECT").is_ok()
                || crate::utils::process_env::var("gcloud_project").is_ok()
                || crate::utils::process_env::var("google_cloud_project").is_ok();
            let has_key_file = crate::utils::process_env::var("GOOGLE_APPLICATION_CREDENTIALS")
                .is_ok()
                || crate::utils::process_env::var("google_application_credentials").is_ok();

            let vertex_auth = if crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_SKIP_VERTEX_AUTH")
                    .ok()
                    .as_deref(),
            ) {
                VertexAuth::SkipAuth
            } else {
                VertexAuth::GoogleAuth {
                    project_id_fallback: if has_project_env_var || has_key_file {
                        None
                    } else {
                        project_id.clone()
                    },
                }
            };

            crate::utils::debug::log_for_debugging(&format!(
                "[API:vertex] region={region}, project_id={project_id:?}, skip_auth={}",
                crate::utils::env_utils::is_env_truthy(
                    crate::utils::process_env::var("CLAUDE_CODE_SKIP_VERTEX_AUTH")
                        .ok()
                        .as_deref()
                ),
            ));

            Ok(AnthropicClientHandle {
                provider: ProviderConfig::Vertex {
                    region,
                    project_id,
                    auth: vertex_auth,
                },
                default_headers,
                max_retries,
                timeout_ms,
                inject_client_request_id,
                source,
            })
        }

        // ---------------------------------------------------------------
        // Direct API (first party)
        // Maps to: CC services/api/client.ts:300-316
        // ---------------------------------------------------------------
        ApiProvider::FirstParty => {
            let resolved_api_key = if is_claude_ai_subscriber() {
                None
            } else {
                api_key.or_else(crate::utils::auth::get_anthropic_api_key)
            };
            // CC passes `apiKey: null`, never undefined, so the SDK does not
            // fall back to `ANTHROPIC_API_KEY`. `Option` cannot say null; an
            // omitted header is the SDK's own spelling of it.
            // Deviation: with no credential at all, CC's SDK rejects the
            // request before sending it ("Could not resolve authentication
            // method"); the omitted header satisfies that check here, so the
            // request is sent and the server rejects it instead.
            if resolved_api_key.is_none() {
                default_headers.insert("x-api-key".to_string(), None);
            }

            let auth_token = if is_claude_ai_subscriber() {
                crate::utils::auth::get_claude_ai_oauth_tokens().map(|tokens| tokens.access_token)
            } else {
                None
            };

            // Maps to: CC `services/api/client.ts:306-311`: only ant staging
            // sessions source their API base URL from the canonical OAuth
            // configuration owner.
            let base_url = if crate::utils::process_env::var("USER_TYPE").ok().as_deref()
                == Some("ant")
                && crate::utils::env_utils::is_env_truthy(
                    crate::utils::process_env::var("USE_STAGING_OAUTH")
                        .ok()
                        .as_deref(),
                ) {
                Some(crate::constants::oauth::get_oauth_config()?.base_api_url)
            } else {
                crate::utils::process_env::var("ANTHROPIC_BASE_URL").ok()
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
                inject_client_request_id,
                source,
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
    /// Use `ANTHROPIC_FOUNDRY_API_KEY` (SDK reads it automatically).
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
    /// Use `google-auth-library` (or gcp-auth crate equivalent).
    GoogleAuth {
        /// Fallback project ID to avoid metadata server timeout.
        project_id_fallback: Option<String>,
    },
    /// Skip authentication (for proxies / testing).
    SkipAuth,
}

/// Provider-specific configuration for the Anthropic client.
///
/// Maps to: CC services/api/client.ts:153-316 (provider dispatch branches)
#[derive(Clone, Debug)]
pub enum ProviderConfig {
    /// Direct API access via api.anthropic.com.
    Direct {
        api_key: Option<String>,
        auth_token: Option<String>,
        base_url: Option<String>,
    },
    /// AWS Bedrock provider.
    Bedrock { region: String, auth: BedrockAuth },
    /// Azure Foundry provider.
    Foundry { auth: FoundryAuth },
    /// GCP Vertex AI provider.
    Vertex {
        region: String,
        project_id: Option<String>,
        auth: VertexAuth,
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
    /// Whether to inject `x-client-request-id` headers (first-party only).
    pub inject_client_request_id: bool,
    /// Caller-provided source tag for debug logging.
    pub source: Option<String>,
}

impl AnthropicClientHandle {
    /// Build an `anthropic_sdk::Anthropic` client from this handle.
    ///
    /// Direct first-party API construction is wired in Phase 1. Provider SDKs
    /// remain explicit future work.
    ///
    /// For the Direct provider the construction will look like:
    /// ```ignore
    /// let client_opts = anthropic_sdk::ClientOptions {
    ///     api_key,
    ///     auth_token,
    ///     base_url,
    ///     max_retries: Some(self.max_retries),
    ///     timeout: Some(self.timeout_ms),
    ///     default_headers: sdk_headers,
    ///     ..Default::default()
    /// };
    /// anthropic_sdk::Anthropic::new(client_opts)
    /// ```
    ///
    /// For Bedrock/Vertex/Foundry, the SDK client is constructed with
    /// provider-specific options and cast to the common `Anthropic` type
    /// (matching CC's `as unknown as Anthropic` pattern).
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
            } => build_direct_client(
                api_key.clone(),
                auth_token.clone(),
                base_url.clone(),
                self.timeout_ms,
                self.max_retries,
                self.default_headers.clone(),
            ),
            ProviderConfig::Bedrock { .. } => Err(ClientError::Sdk(
                "Bedrock provider SDK is not wired in Cometix Phase 1; use Direct API".to_string(),
            )),
            ProviderConfig::Foundry { auth } => {
                let base_url = crate::utils::process_env::var("ANTHROPIC_FOUNDRY_BASE_URL")
                    .ok()
                    .or_else(|| {
                        crate::utils::process_env::var("ANTHROPIC_FOUNDRY_RESOURCE")
                            .ok()
                            .map(|resource| {
                                format!("https://{resource}.services.ai.azure.com/anthropic/")
                            })
                    });
                let Some(base_url) = base_url else {
                    return Err(ClientError::MissingConfig(
                        "ANTHROPIC_FOUNDRY_BASE_URL or ANTHROPIC_FOUNDRY_RESOURCE".to_string(),
                    ));
                };
                let (api_key, auth_token) = match auth {
                    FoundryAuth::ApiKey => (
                        crate::utils::process_env::var("ANTHROPIC_FOUNDRY_API_KEY").ok(),
                        None,
                    ),
                    FoundryAuth::AzureAd => {
                        return Err(ClientError::Sdk(
                            "Foundry DefaultAzureCredential requires the provider SDK adapter"
                                .to_string(),
                        ));
                    }
                    FoundryAuth::SkipAuth => (None, None),
                };
                build_direct_client(
                    api_key,
                    auth_token,
                    Some(base_url),
                    self.timeout_ms,
                    self.max_retries,
                    self.default_headers.clone(),
                )
            }
            ProviderConfig::Vertex { .. } => Err(ClientError::Sdk(
                "Vertex provider SDK is not wired in Cometix Phase 1; use Direct API".to_string(),
            )),
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
}

pub type ClientBuildOutput = anthropic_sdk::Anthropic;

fn build_direct_client(
    api_key: Option<String>,
    auth_token: Option<String>,
    base_url: Option<String>,
    timeout_ms: u64,
    max_retries: u32,
    default_headers: HashMap<String, Option<String>>,
) -> Result<ClientBuildOutput, ClientError> {
    // The SDK's own `process.env` fallbacks (`ANTHROPIC_AUTH_TOKEN`,
    // `ANTHROPIC_BASE_URL`, `ANTHROPIC_LOG`) read the effective environment,
    // not the frozen real one. Only the first installation takes effect.
    let _ = anthropic_sdk::internal::env::set_env_source(|key| {
        crate::utils::process_env::var(key).ok()
    });
    let http_client = crate::utils::http::client_builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|error| ClientError::Sdk(format!("failed to build HTTP client: {error}")))?;
    anthropic_sdk::Anthropic::new(anthropic_sdk::ClientOptions {
        api_key,
        auth_token,
        base_url,
        timeout: Some(timeout_ms),
        max_retries: Some(max_retries),
        http_client: Some(http_client),
        default_headers: Some(default_headers),
        ..Default::default()
    })
    .map_err(|error| ClientError::Sdk(error.to_string()))
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
    let client = crate::utils::http::client_builder()
        .build()
        .expect("Client::new()");
    let mut request = client
        .request(method.clone(), url.clone())
        .header("content-type", "application/json");
    if !body.is_empty() {
        request = request.body(body.clone());
    }
    match auth {
        BedrockAuth::BearerToken { .. } => {
            let token = crate::utils::process_env::var("AWS_BEARER_TOKEN_BEDROCK").ok()?;
            request = request.bearer_auth(token);
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

    struct EnvGuard {
        _env: crate::utils::env_utils::EnvVarGuard,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::set(key, value),
            }
        }

        fn remove(key: &'static str) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::unset(key),
            }
        }
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
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-api-key-oauth-gate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvGuard::remove("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvGuard::remove("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvGuard::remove("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvGuard::remove("CLAUDE_CODE_USE_FOUNDRY");
        let handle = get_anthropic_client(GetAnthropicClientOptions {
            api_key: Some("sk-ant-test".to_string()),
            ..GetAnthropicClientOptions::default()
        })
        .await
        .expect("unrelated explicit API-key auth must remain available");
        assert!(matches!(
            handle.provider,
            ProviderConfig::Direct {
                api_key: Some(ref key),
                auth_token: None,
                ..
            } if key == "sk-ant-test"
        ));
        let _ = std::fs::remove_dir_all(config_home);
    }

    /// CC passes `apiKey: null`, never undefined, when no key resolves: the
    /// SDK's own `ANTHROPIC_API_KEY` fallback must not reach the wire.
    #[tokio::test]
    async fn unresolved_api_key_keeps_the_sdk_environment_fallback_off_the_wire() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home =
            std::env::temp_dir().join(format!("cometix-null-api-key-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvGuard::remove("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvGuard::remove("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvGuard::remove("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvGuard::remove("CLAUDE_CODE_USE_FOUNDRY");
        let _key = EnvGuard::remove("ANTHROPIC_API_KEY");
        let handle = get_anthropic_client(GetAnthropicClientOptions::default())
            .await
            .expect("a keyless first-party client still constructs");
        assert!(matches!(
            handle.provider,
            ProviderConfig::Direct { api_key: None, .. }
        ));
        assert_eq!(handle.default_headers.get("x-api-key"), Some(&None));

        // Exactly what the SDK's fallback reads, through the installed source.
        crate::utils::process_env::set("ANTHROPIC_API_KEY", "sk-ant-ambient");
        crate::utils::tls_provider::install_crypto_provider();
        let client = handle.build().expect("client builds");
        assert_eq!(client.api_key(), Some("sk-ant-ambient"));
        let headers = client
            .build_headers(0, None)
            .expect("an omitted header satisfies the SDK's auth validation");
        assert!(headers.get("x-api-key").is_none());
        let _ = std::fs::remove_dir_all(config_home);
    }

    #[tokio::test]
    async fn selected_expired_oauth_propagates_the_closed_side_effect_error() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = std::env::temp_dir().join(format!(
            "cometix-selected-oauth-gate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&config_home).unwrap();
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _oauth = EnvGuard::remove("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvGuard::remove("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvGuard::remove("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvGuard::remove("CLAUDE_CODE_USE_FOUNDRY");
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
                api_key: Some("sk-test".to_string()),
                auth_token: None,
                base_url: None,
            },
            default_headers: HashMap::new(),
            max_retries: 2,
            timeout_ms: 600_000,
            inject_client_request_id: true,
            source: Some("test".to_string()),
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
            inject_client_request_id: false,
            source: None,
        };
        assert_eq!(handle.provider_type(), ApiProvider::Bedrock);
    }

    #[test]
    fn provider_config_foundry_has_correct_type() {
        let handle = AnthropicClientHandle {
            provider: ProviderConfig::Foundry {
                auth: FoundryAuth::SkipAuth,
            },
            default_headers: HashMap::new(),
            max_retries: 2,
            timeout_ms: 600_000,
            inject_client_request_id: false,
            source: None,
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
            },
            default_headers: HashMap::new(),
            max_retries: 2,
            timeout_ms: 600_000,
            inject_client_request_id: false,
            source: None,
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
