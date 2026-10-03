//! MCP OAuth/authentication service boundary.
//! Maps to: CC `services/mcp/auth.ts`.
//!
//! This module owns MCP OAuth discovery and credential-key helpers. Runtime
//! transport code consumes its results; slash commands and UI components must
//! not implement OAuth protocol details themselves.

use super::oauth_port::{build_redirect_uri, find_available_port};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maps to: CC `services/mcp/auth.ts#getServerKey`.
pub fn get_server_key(
    server_name: &str,
    server_type: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
) -> String {
    let config_json = serde_json::json!({
        "type": server_type,
        "url": url,
        "headers": headers,
    })
    .to_string();
    let digest = Sha256::digest(config_json.as_bytes());
    let hash = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{server_name}|{hash}")
}

/// Maps to: CC `services/mcp/auth.ts:349-379` `hasMcpDiscoveryButNoToken`.
pub fn has_mcp_discovery_but_no_token(
    server_name: &str,
    config: &super::types::ScopedMcpServerConfig,
) -> bool {
    if crate::services::mcp::xaa_idp_login::is_xaa_enabled()
        && config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.get("xaa"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return false;
    }
    let transport_type = match config.transport {
        super::types::Transport::Sse => "sse",
        super::types::Transport::Http => "http",
        _ => return false,
    };
    let Some(server_url) = config
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return false;
    };
    let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
    let Some(entry) = crate::utils::secure_storage::get_secure_storage()
        .read()
        .and_then(|credentials| credentials.get("mcpOAuth")?.get(&server_key).cloned())
    else {
        return false;
    };
    let has_access_token = entry
        .get("accessToken")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.trim().is_empty());
    let has_refresh_token = entry
        .get("refreshToken")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.trim().is_empty());
    !has_access_token && !has_refresh_token
}

/// Read-side projection of OAuth server metadata needed by Claude Code MCP
/// flows. Maps to: CC `AuthorizationServerMetadata` values consumed by
/// `fetchAuthServerMetadata(...)` and `getScopeFromMetadata(...)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpAuthorizationServerMetadata {
    pub issuer: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scopes_supported: Option<Vec<String>>,
    pub scope: Option<String>,
    pub default_scope: Option<String>,
}

/// Maps to: CC `services/mcp/auth.ts#getScopeFromMetadata`.
pub fn get_scope_from_metadata(metadata: &McpAuthorizationServerMetadata) -> Option<String> {
    if let Some(scope) = metadata
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
    {
        return Some(scope.to_string());
    }
    if let Some(default_scope) = metadata
        .default_scope
        .as_deref()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
    {
        return Some(default_scope.to_string());
    }

    metadata
        .scopes_supported
        .as_ref()
        .map(|scopes| {
            scopes
                .iter()
                .map(|scope| scope.trim())
                .filter(|scope| !scope.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|scopes| !scopes.is_empty())
        .map(|scopes| scopes.join(" "))
}

/// Callback invoked as soon as the authorization URL is available.
/// Maps to: CC `performMCPOAuthFlow(..., onAuthorizationUrl, ...)`.
pub type McpAuthorizationUrlCallback = std::sync::Arc<dyn Fn(String) + Send + Sync>;

/// Manual callback URL submitter exposed to UI while OAuth waits for the
/// loopback callback.
/// Maps to: CC `performMCPOAuthFlow(..., { onWaitingForCallback })` submit fn.
#[derive(Clone, Debug)]
pub struct McpManualCallbackSubmit {
    sender: tokio::sync::mpsc::UnboundedSender<String>,
}

impl McpManualCallbackSubmit {
    /// Maps to the callback URL string passed to CC's manual submit function.
    pub fn submit(&self, callback_url: impl Into<String>) {
        let _ = self.sender.send(callback_url.into());
    }
}

/// Callback invoked once the OAuth callback server is waiting and a manual URL
/// can be pasted from browser-based remote environments.
/// Maps to: CC `onWaitingForCallback`.
pub type McpWaitingForCallbackCallback =
    std::sync::Arc<dyn Fn(McpManualCallbackSubmit) + Send + Sync>;

const AUTHENTICATION_CANCELLED_MESSAGE: &str = "Authentication cancelled";

struct McpOAuthAbortState {
    aborted: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

/// Abort controller for MCP OAuth flows.
/// Maps to: CC `AbortController` passed into `performMCPOAuthFlow(...)`.
#[derive(Clone)]
pub struct McpOAuthAbortHandle {
    inner: std::sync::Arc<McpOAuthAbortState>,
}

/// Abort signal for MCP OAuth flows.
/// Maps to: CC `AbortSignal` consumed by `performMCPOAuthFlow(...)`.
#[derive(Clone)]
pub struct McpOAuthAbortSignal {
    inner: std::sync::Arc<McpOAuthAbortState>,
}

impl std::fmt::Debug for McpOAuthAbortHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOAuthAbortHandle")
            .field("aborted", &self.is_aborted())
            .finish()
    }
}

impl std::fmt::Debug for McpOAuthAbortSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOAuthAbortSignal")
            .field("aborted", &self.is_aborted())
            .finish()
    }
}

impl Default for McpOAuthAbortHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl McpOAuthAbortHandle {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(McpOAuthAbortState {
                aborted: std::sync::atomic::AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn signal(&self) -> McpOAuthAbortSignal {
        McpOAuthAbortSignal {
            inner: std::sync::Arc::clone(&self.inner),
        }
    }

    pub fn abort(&self) {
        if !self
            .inner
            .aborted
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.inner.notify.notify_waiters();
        }
    }

    pub fn is_aborted(&self) -> bool {
        self.signal().is_aborted()
    }
}

impl McpOAuthAbortSignal {
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        while !self.is_aborted() {
            self.inner.notify.notified().await;
        }
    }
}

/// Maps to: CC `error instanceof AuthenticationCancelledError`.
pub fn is_authentication_cancelled_error(error: &anyhow::Error) -> bool {
    error.to_string() == AUTHENTICATION_CANCELLED_MESSAGE
}

/// Maps to: CC `services/mcp/auth.ts#performMCPOAuthFlow` options relevant to
/// service-side execution.
#[derive(Clone, Default)]
pub struct McpOAuthFlowOptions {
    pub skip_browser_open: bool,
    pub on_authorization_url: Option<McpAuthorizationUrlCallback>,
    pub on_waiting_for_callback: Option<McpWaitingForCallbackCallback>,
    pub abort_signal: Option<McpOAuthAbortSignal>,
}

impl std::fmt::Debug for McpOAuthFlowOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOAuthFlowOptions")
            .field("skip_browser_open", &self.skip_browser_open)
            .field(
                "on_authorization_url",
                &self.on_authorization_url.as_ref().map(|_| "[callback]"),
            )
            .field(
                "on_waiting_for_callback",
                &self.on_waiting_for_callback.as_ref().map(|_| "[callback]"),
            )
            .field("abort_signal", &self.abort_signal)
            .finish()
    }
}

/// Result of a completed MCP OAuth flow.
/// Maps to: CC `performMCPOAuthFlow(...)` success path plus the authorization
/// URL callback used by `McpAuthTool`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOAuthFlowResult {
    pub authorization_url: String,
    pub redirect_uri: String,
    pub port: u16,
    pub state: String,
    pub server_key: String,
    pub access_token_saved: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Read projection of the MCP SDK `OAuthTokens` value returned by
/// `ClaudeAuthProvider.tokens()`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpOAuthTokens {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<f64>,
    pub scope: Option<String>,
    pub token_type: &'static str,
}

/// Maps to: CC `services/mcp/auth.ts:1376-1405` `ClaudeAuthProvider`.
///
/// This reviewed partial retains the source identity for read/status callers;
/// interactive discovery and refresh remain in the separately gated MCP
/// runtime paths until the full async provider object is ported.
pub struct ClaudeAuthProvider {
    server_name: String,
    server_config: super::types::ScopedMcpServerConfig,
}

impl ClaudeAuthProvider {
    /// Maps to: CC `services/mcp/auth.ts:1393-1405`
    /// `ClaudeAuthProvider.constructor` with default redirect options.
    pub fn new(server_name: &str, server_config: &super::types::ScopedMcpServerConfig) -> Self {
        Self {
            server_name: server_name.to_string(),
            server_config: server_config.clone(),
        }
    }

    /// Maps to: CC `services/mcp/auth.ts:1540-1702`
    /// `ClaudeAuthProvider.tokens`.
    ///
    /// Partial dependency seam: secure-storage `readAsync` and the imported MCP
    /// SDK refresh carrier are not yet available at this synchronous settings
    /// boundary. Current-token projection is source-shaped; a token requiring
    /// proactive refresh reaches the authorized typed closed outlet and cannot
    /// be reported as a successful authenticated state.
    pub fn tokens(&self) -> anyhow::Result<Option<McpOAuthTokens>> {
        let transport_type = match self.server_config.transport {
            super::types::Transport::Sse => "sse",
            super::types::Transport::Http => "http",
            _ => return Ok(None),
        };
        let Some(server_url) = self
            .server_config
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        else {
            return Ok(None);
        };
        let server_key = get_server_key(
            &self.server_name,
            transport_type,
            server_url,
            &self.server_config.headers,
        );
        let entry = crate::utils::secure_storage::get_secure_storage()
            .read()
            .and_then(|credentials| credentials.get("mcpOAuth")?.get(&server_key).cloned())
            .filter(Value::is_object);
        let xaa_configured = self
            .server_config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.get("xaa"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && crate::services::mcp::xaa_idp_login::is_xaa_enabled();
        let should_try_xaa = xaa_configured
            && entry
                .as_ref()
                .map(|entry| {
                    let has_refresh = entry
                        .get("refreshToken")
                        .and_then(Value::as_str)
                        .is_some_and(|token| !token.is_empty());
                    let has_access = entry
                        .get("accessToken")
                        .and_then(Value::as_str)
                        .is_some_and(|token| !token.is_empty());
                    let expires_in = entry
                        .get("expiresAt")
                        .and_then(Value::as_f64)
                        .map(|expires_at| expires_at / 1000.0 - now_ms() as f64 / 1000.0);
                    !has_refresh
                        && (!has_access || expires_in.is_some_and(|seconds| seconds <= 300.0))
                })
                .unwrap_or(true);
        if should_try_xaa {
            if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
                return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
            }
            anyhow::bail!("MCP XAA silent token exchange is unavailable in this partial provider");
        }
        let Some(entry) = entry else {
            return Ok(None);
        };

        let access_token = entry
            .get("accessToken")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let refresh_token = entry
            .get("refreshToken")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let expires_in = entry
            .get("expiresAt")
            .and_then(Value::as_f64)
            .map(|expires_at| expires_at / 1000.0 - now_ms() as f64 / 1000.0);

        if expires_in.is_some_and(|expires_in| expires_in <= 0.0) && refresh_token.is_none() {
            return Ok(None);
        }
        if expires_in.is_some_and(|expires_in| expires_in <= 300.0)
            && refresh_token
                .as_deref()
                .is_some_and(|token| !token.is_empty())
        {
            if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
                return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
            }
            anyhow::bail!("MCP OAuth proactive refresh is unavailable in this partial provider");
        }

        Ok(Some(McpOAuthTokens {
            access_token,
            refresh_token,
            expires_in,
            scope: entry
                .get("scope")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            token_type: "Bearer",
        }))
    }
}

#[cfg(feature = "mcp_runtime")]
mod runtime {
    use super::super::types::{ScopedMcpServerConfig, Transport};
    use super::*;
    use crate::utils::process_env::JsTruthy;
    use rmcp::transport::auth::{
        AuthError, AuthorizationManager, AuthorizationMetadata, CredentialStore,
        InMemoryCredentialStore, OAuthClientConfig, OAuthTokenResponse, StoredCredentials,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener as TokioTcpListener;

    const AUTH_REQUEST_TIMEOUT_MS: u64 = 30_000;
    const AUTH_CALLBACK_TIMEOUT_SECS: u64 = 5 * 60;

    /// rmcp's `ReqwestOAuthHttpClient` limit (`MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES`).
    const MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

    /// The HTTP client rmcp's `AuthorizationManager` sends every OAuth request
    /// with. CC's MCP auth requests run on the global `fetch` (the SDK's
    /// default `fetchFn`, or `createAuthFetch`, `:198-237`), whose dispatcher
    /// carries the proxy and TLS options; rmcp builds plain clients unless it
    /// is handed one. The rest is rmcp's own: one client per redirect policy,
    /// a 30 s timeout, a 1 MiB body limit.
    pub(crate) fn oauth_http_client()
    -> anyhow::Result<Arc<dyn rmcp::transport::auth::OAuthHttpClient>> {
        let build = |redirect: reqwest::redirect::Policy| -> anyhow::Result<reqwest::Client> {
            Ok(crate::utils::proxy::get_proxy_fetch_options(false)?
                .timeout(Duration::from_millis(AUTH_REQUEST_TIMEOUT_MS))
                .redirect(redirect)
                .build()?)
        };
        Ok(Arc::new(FetchOAuthHttpClient {
            follow_redirects: build(reqwest::redirect::Policy::default())?,
            stop_redirects: build(reqwest::redirect::Policy::none())?,
        }))
    }

    struct FetchOAuthHttpClient {
        follow_redirects: reqwest::Client,
        stop_redirects: reqwest::Client,
    }

    impl rmcp::transport::auth::OAuthHttpClient for FetchOAuthHttpClient {
        fn execute(
            &self,
            request: rmcp::transport::auth::OAuthHttpRequest,
        ) -> rmcp::transport::auth::OAuthHttpClientFuture<'_> {
            use futures::StreamExt as _;
            use rmcp::transport::auth::{OAuthHttpClientError, OAuthHttpRedirectPolicy};
            Box::pin(async move {
                // `non_exhaustive`: a policy added later stops, the safer side.
                let client = match request.redirect_policy {
                    OAuthHttpRedirectPolicy::Follow => &self.follow_redirects,
                    _ => &self.stop_redirects,
                };
                let request = reqwest::Request::try_from(request.request)
                    .map_err(|error| OAuthHttpClientError::new(error.to_string()))?;
                let response = client
                    .execute(request)
                    .await
                    .map_err(|error| OAuthHttpClientError::new(error.to_string()))?;
                let mut builder = http::Response::builder()
                    .status(response.status())
                    .version(response.version());
                for (name, value) in response.headers() {
                    builder = builder.header(name, value);
                }
                let mut body = Vec::new();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk =
                        chunk.map_err(|error| OAuthHttpClientError::new(error.to_string()))?;
                    if chunk.len() > MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES - body.len() {
                        return Err(OAuthHttpClientError::new(format!(
                            "OAuth HTTP response body exceeds {MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES} bytes"
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
                builder
                    .body(body)
                    .map_err(|error| OAuthHttpClientError::new(error.to_string()))
            })
        }
    }

    impl From<AuthorizationMetadata> for McpAuthorizationServerMetadata {
        fn from(metadata: AuthorizationMetadata) -> Self {
            let scope = metadata
                .additional_fields
                .get("scope")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let default_scope = metadata
                .additional_fields
                .get("default_scope")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Self {
                issuer: metadata.issuer,
                authorization_endpoint: metadata.authorization_endpoint,
                token_endpoint: metadata.token_endpoint,
                registration_endpoint: metadata.registration_endpoint,
                scopes_supported: metadata.scopes_supported,
                scope,
                default_scope,
            }
        }
    }

    async fn fetch_rmcp_auth_server_metadata(
        server_url: &str,
        configured_metadata_url: Option<&str>,
    ) -> anyhow::Result<Option<AuthorizationMetadata>> {
        if let Some(configured_metadata_url) = configured_metadata_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            if !configured_metadata_url.starts_with("https://") {
                anyhow::bail!(
                    "authServerMetadataUrl must use https:// (got: {configured_metadata_url})"
                );
            }
            // CC `createAuthFetch` (`auth.ts:198-237`) wraps the global
            // `fetch`, whose dispatcher carries the proxy and TLS options.
            let client = crate::utils::proxy::get_proxy_fetch_options(false)?
                .timeout(Duration::from_millis(AUTH_REQUEST_TIMEOUT_MS))
                .build()?;
            let request = client
                .get(configured_metadata_url)
                .header(reqwest::header::ACCEPT, "application/json");
            if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
                return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
            }
            let response = request.send().await?;
            if !response.status().is_success() {
                anyhow::bail!(
                    "HTTP {} fetching configured auth server metadata from {}",
                    response.status(),
                    configured_metadata_url
                );
            }
            let metadata = response.json::<AuthorizationMetadata>().await?;
            return Ok(Some(metadata));
        }

        let manager =
            AuthorizationManager::new_with_oauth_http_client(server_url, oauth_http_client()?)
                .await?;
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        match manager.discover_metadata().await {
            Ok(metadata) => Ok(Some(metadata)),
            Err(AuthError::NoAuthorizationSupport) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Maps to: CC `services/mcp/auth.ts#fetchAuthServerMetadata`.
    ///
    /// Uses rmcp's official `AuthorizationManager::discover_metadata()` for
    /// RFC 9728 protected-resource discovery, RFC 8414/OIDC metadata fallback,
    /// SEP-835 scope selection inputs, SSRF guards, and metadata validation.
    pub async fn fetch_auth_server_metadata(
        server_url: &str,
        configured_metadata_url: Option<&str>,
    ) -> anyhow::Result<Option<McpAuthorizationServerMetadata>> {
        Ok(
            fetch_rmcp_auth_server_metadata(server_url, configured_metadata_url)
                .await?
                .map(Into::into),
        )
    }

    fn oauth_config_string<'a>(config: &'a ScopedMcpServerConfig, key: &str) -> Option<&'a str> {
        let value = config.oauth.as_ref()?.get(key)?.as_str()?.trim();
        (!value.is_empty()).then_some(value)
    }

    fn oauth_configured_metadata_url(config: &ScopedMcpServerConfig) -> Option<&str> {
        oauth_config_string(config, "authServerMetadataUrl")
    }

    fn oauth_configured_client_id(config: &ScopedMcpServerConfig) -> Option<&str> {
        oauth_config_string(config, "clientId")
    }

    fn oauth_configured_callback_port(config: &ScopedMcpServerConfig) -> Option<u16> {
        let raw = config.oauth.as_ref()?.get("callbackPort")?.as_u64()?;
        u16::try_from(raw).ok().filter(|port| *port > 0)
    }

    fn oauth_transport_type(config: &ScopedMcpServerConfig) -> anyhow::Result<&'static str> {
        match config.transport {
            Transport::Http => Ok("http"),
            Transport::Sse => Ok("sse"),
            _ => anyhow::bail!(
                "Server uses {} transport which does not support MCP OAuth",
                config.transport.as_str()
            ),
        }
    }

    fn authorization_state_from_url(authorization_url: &str) -> anyhow::Result<String> {
        let parsed = reqwest::Url::parse(authorization_url)?;
        parsed
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("authorization URL did not include OAuth state"))
    }

    fn scope_from_token_json(value: &Value) -> Option<String> {
        if let Some(scope) = value.get("scope").and_then(Value::as_str) {
            let trimmed = scope.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        value
            .get("scopes")
            .and_then(Value::as_array)
            .map(|scopes| {
                scopes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|scope| !scope.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|scopes| !scopes.is_empty())
            .map(|scopes| scopes.join(" "))
    }

    fn read_credentials_json_for_update() -> Value {
        crate::utils::secure_storage::get_secure_storage()
            .read()
            .unwrap_or_else(|| serde_json::json!({}))
    }

    fn write_credentials_json(value: &Value) -> anyhow::Result<()> {
        let status = crate::utils::secure_storage::get_secure_storage().update(value)?;
        if status.success {
            Ok(())
        } else {
            anyhow::bail!("Failed to update MCP OAuth credentials")
        }
    }

    fn persist_mcp_oauth_tokens(
        server_name: &str,
        server_key: &str,
        server_url: &str,
        client_id: &str,
        client_secret: Option<&str>,
        metadata: &AuthorizationMetadata,
        token_response: &OAuthTokenResponse,
        resource_metadata_url: Option<&str>,
    ) -> anyhow::Result<bool> {
        // Maps to: CC `ClaudeAuthProvider.saveTokens` and discovery-state fields.
        let token_json = serde_json::to_value(token_response)?;
        let Some(access_token) = token_json
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
        else {
            anyhow::bail!("OAuth token response did not include an access_token")
        };
        let refresh_token = token_json
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(ToOwned::to_owned);
        let expires_in_secs = token_json
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(3600);
        let scope = scope_from_token_json(&token_json);

        let mut credentials = read_credentials_json_for_update();
        if !credentials.is_object() {
            credentials = serde_json::json!({});
        }
        let root = credentials.as_object_mut().expect("object checked above");
        let mcp_oauth = root
            .entry("mcpOAuth".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !mcp_oauth.is_object() {
            *mcp_oauth = serde_json::json!({});
        }
        let mut entry = mcp_oauth
            .get(server_key)
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        let entry_obj = entry.as_object_mut().expect("entry object");
        entry_obj.insert(
            "serverName".to_string(),
            Value::String(server_name.to_string()),
        );
        entry_obj.insert(
            "serverUrl".to_string(),
            Value::String(server_url.to_string()),
        );
        entry_obj.insert(
            "accessToken".to_string(),
            Value::String(access_token.to_string()),
        );
        match refresh_token {
            Some(refresh_token) => {
                entry_obj.insert("refreshToken".to_string(), Value::String(refresh_token));
            }
            None => {
                entry_obj.remove("refreshToken");
            }
        }
        entry_obj.insert(
            "expiresAt".to_string(),
            Value::Number(serde_json::Number::from(
                now_ms().saturating_add(expires_in_secs.saturating_mul(1000)),
            )),
        );
        if let Some(scope) = scope {
            entry_obj.insert("scope".to_string(), Value::String(scope));
        }
        entry_obj.insert("clientId".to_string(), Value::String(client_id.to_string()));
        if let Some(client_secret) = client_secret
            .map(str::trim)
            .filter(|secret| !secret.is_empty())
        {
            entry_obj.insert(
                "clientSecret".to_string(),
                Value::String(client_secret.to_string()),
            );
        }
        let resource_metadata_url = resource_metadata_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                entry_obj
                    .get("discoveryState")
                    .and_then(|state| state.get("resourceMetadataUrl"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .map(ToOwned::to_owned)
            });
        let mut discovery_state = serde_json::json!({
            "authorizationServerUrl": metadata.issuer.clone().unwrap_or_else(|| metadata.authorization_endpoint.clone()),
        });
        if let Some(resource_metadata_url) = resource_metadata_url {
            discovery_state
                .as_object_mut()
                .expect("discoveryState object")
                .insert(
                    "resourceMetadataUrl".to_string(),
                    Value::String(resource_metadata_url),
                );
        }
        entry_obj.insert("discoveryState".to_string(), discovery_state);
        mcp_oauth
            .as_object_mut()
            .expect("mcpOAuth object")
            .insert(server_key.to_string(), entry);
        write_credentials_json(&credentials)?;
        Ok(true)
    }

    fn persist_mcp_xaa_tokens(
        server_name: &str,
        server_key: &str,
        server_url: &str,
        client_id: &str,
        client_secret: &str,
        tokens: &crate::services::mcp::xaa::XaaResult,
    ) -> anyhow::Result<()> {
        // Maps to: CC `performMCPXaaAuth` / `ClaudeAuthProvider.xaaRefresh`
        // direct keychain writes.
        let mut credentials = read_credentials_json_for_update();
        if !credentials.is_object() {
            credentials = serde_json::json!({});
        }
        let root = credentials.as_object_mut().expect("object checked above");
        let mcp_oauth = root
            .entry("mcpOAuth".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !mcp_oauth.is_object() {
            *mcp_oauth = serde_json::json!({});
        }
        let mut entry = mcp_oauth
            .get(server_key)
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        let entry_obj = entry.as_object_mut().expect("entry object");
        entry_obj.insert(
            "serverName".to_string(),
            Value::String(server_name.to_string()),
        );
        entry_obj.insert(
            "serverUrl".to_string(),
            Value::String(server_url.to_string()),
        );
        entry_obj.insert(
            "accessToken".to_string(),
            Value::String(tokens.access_token.clone()),
        );
        if let Some(refresh_token) = tokens
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            entry_obj.insert(
                "refreshToken".to_string(),
                Value::String(refresh_token.to_string()),
            );
        }
        entry_obj.insert(
            "expiresAt".to_string(),
            Value::Number(serde_json::Number::from(now_ms().saturating_add(
                tokens.expires_in.unwrap_or(3600).saturating_mul(1000),
            ))),
        );
        if let Some(scope) = tokens
            .scope
            .as_deref()
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
        {
            entry_obj.insert("scope".to_string(), Value::String(scope.to_string()));
        }
        entry_obj.insert("clientId".to_string(), Value::String(client_id.to_string()));
        entry_obj.insert(
            "clientSecret".to_string(),
            Value::String(client_secret.to_string()),
        );
        entry_obj.insert(
            "discoveryState".to_string(),
            serde_json::json!({
                "authorizationServerUrl": tokens.authorization_server_url,
            }),
        );
        mcp_oauth
            .as_object_mut()
            .expect("mcpOAuth object")
            .insert(server_key.to_string(), entry);
        write_credentials_json(&credentials)
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub(super) struct McpOAuthTokenSnapshot {
        pub(super) access_token: Option<String>,
        pub(super) refresh_token: Option<String>,
        pub(super) expires_in_secs: i64,
        pub(super) scope: Option<String>,
        pub(super) client_id: Option<String>,
        pub(super) client_secret: Option<String>,
    }

    fn trimmed_entry_string(entry: &Value, key: &str) -> Option<String> {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    fn mcp_oauth_entry_by_key(server_key: &str) -> Option<Value> {
        read_credentials_json_for_update()
            .get("mcpOAuth")?
            .get(server_key)
            .cloned()
            .filter(Value::is_object)
    }

    fn mcp_oauth_client_secret_by_key(server_key: &str) -> Option<String> {
        read_credentials_json_for_update()
            .get("mcpOAuthClientConfig")?
            .get(server_key)?
            .get("clientSecret")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|secret| !secret.is_empty())
            .map(ToOwned::to_owned)
    }

    /// Maps to: CC `services/mcp/auth.ts#saveMcpClientSecret`.
    pub fn save_mcp_client_secret(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        client_secret: &str,
    ) -> anyhow::Result<()> {
        let transport_type = oauth_transport_type(config)?;
        let server_url = config
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP OAuth server is missing URL"))?;
        let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
        let mut credentials = read_credentials_json_for_update();
        if !credentials.is_object() {
            credentials = serde_json::json!({});
        }
        let root = credentials.as_object_mut().expect("object checked above");
        let client_config = root
            .entry("mcpOAuthClientConfig".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !client_config.is_object() {
            *client_config = serde_json::json!({});
        }
        client_config
            .as_object_mut()
            .expect("mcpOAuthClientConfig object")
            .insert(
                server_key,
                serde_json::json!({ "clientSecret": client_secret }),
            );
        write_credentials_json(&credentials)
    }

    /// Maps to: CC `services/mcp/auth.ts#getMcpClientConfig`.
    pub fn get_mcp_client_config(
        server_name: &str,
        config: &ScopedMcpServerConfig,
    ) -> anyhow::Result<Option<String>> {
        let transport_type = oauth_transport_type(config)?;
        let server_url = config
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP OAuth server is missing URL"))?;
        let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
        Ok(mcp_oauth_client_secret_by_key(&server_key))
    }

    /// Maps to: CC `services/mcp/auth.ts#clearMcpClientConfig`.
    pub fn clear_mcp_client_config(
        server_name: &str,
        config: &ScopedMcpServerConfig,
    ) -> anyhow::Result<()> {
        let transport_type = oauth_transport_type(config)?;
        let server_url = config
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP OAuth server is missing URL"))?;
        let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
        let mut credentials = read_credentials_json_for_update();
        if let Some(client_config) = credentials
            .get_mut("mcpOAuthClientConfig")
            .and_then(Value::as_object_mut)
        {
            client_config.remove(&server_key);
            write_credentials_json(&credentials)?;
        }
        Ok(())
    }

    fn token_snapshot_from_entry(
        entry: &Value,
        config: &ScopedMcpServerConfig,
        server_key: &str,
    ) -> McpOAuthTokenSnapshot {
        let expires_at = entry.get("expiresAt").and_then(Value::as_u64).unwrap_or(0);
        let expires_in_secs = ((expires_at as i128 - now_ms() as i128) / 1000) as i64;
        McpOAuthTokenSnapshot {
            access_token: trimmed_entry_string(entry, "accessToken"),
            refresh_token: trimmed_entry_string(entry, "refreshToken"),
            expires_in_secs,
            scope: trimmed_entry_string(entry, "scope"),
            client_id: trimmed_entry_string(entry, "clientId")
                .or_else(|| oauth_configured_client_id(config).map(ToOwned::to_owned)),
            client_secret: trimmed_entry_string(entry, "clientSecret")
                .or_else(|| mcp_oauth_client_secret_by_key(server_key)),
        }
    }

    pub(super) fn oauth_token_response_from_snapshot(
        snapshot: &McpOAuthTokenSnapshot,
    ) -> Option<OAuthTokenResponse> {
        if snapshot.access_token.is_none() && snapshot.refresh_token.is_none() {
            return None;
        }
        let mut token = serde_json::json!({
            "access_token": snapshot.access_token.clone().unwrap_or_default(),
            "token_type": "Bearer",
            "expires_in": snapshot.expires_in_secs.max(0) as u64,
        });
        let token_obj = token.as_object_mut().expect("token object");
        if let Some(refresh_token) = &snapshot.refresh_token {
            token_obj.insert(
                "refresh_token".to_string(),
                Value::String(refresh_token.clone()),
            );
        }
        if let Some(scope) = &snapshot.scope {
            token_obj.insert("scope".to_string(), Value::String(scope.clone()));
        }
        serde_json::from_value(token).ok()
    }

    fn granted_scopes_from_snapshot(snapshot: &McpOAuthTokenSnapshot) -> Vec<String> {
        snapshot
            .scope
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect()
    }

    fn discovery_state_value(entry: &Value, key: &str) -> Option<Value> {
        entry.get("discoveryState")?.get(key).cloned()
    }

    fn discovery_state_string(entry: &Value, key: &str) -> Option<String> {
        discovery_state_value(entry, key)
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    async fn metadata_for_refresh(
        server_url: &str,
        config: &ScopedMcpServerConfig,
        entry: &Value,
    ) -> anyhow::Result<Option<AuthorizationMetadata>> {
        // Maps to: CC `ClaudeAuthProvider.discoveryState()` + `_doRefresh()`
        // metadata priority: cached metadata, cached authorization server URL,
        // then normal configured/RFC discovery from the MCP server URL.
        if let Some(metadata) = discovery_state_value(entry, "authorizationServerMetadata")
            .and_then(|value| serde_json::from_value::<AuthorizationMetadata>(value).ok())
        {
            return Ok(Some(metadata));
        }
        if let Some(auth_server_url) = discovery_state_string(entry, "authorizationServerUrl") {
            match fetch_rmcp_auth_server_metadata(&auth_server_url, None).await {
                Ok(metadata) if metadata.is_some() => return Ok(metadata),
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(error = %error, "failed to refresh MCP OAuth metadata from cached auth server URL");
                }
            }
        }
        fetch_rmcp_auth_server_metadata(server_url, oauth_configured_metadata_url(config)).await
    }

    fn invalidate_mcp_oauth_tokens(server_key: &str) -> anyhow::Result<()> {
        // Maps to: CC `ClaudeAuthProvider.invalidateCredentials('tokens')`.
        let mut credentials = read_credentials_json_for_update();
        let Some(entry) = credentials
            .get_mut("mcpOAuth")
            .and_then(Value::as_object_mut)
            .and_then(|mcp_oauth| mcp_oauth.get_mut(server_key))
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };
        entry.insert("accessToken".to_string(), Value::String(String::new()));
        entry.remove("refreshToken");
        entry.insert(
            "expiresAt".to_string(),
            Value::Number(serde_json::Number::from(0)),
        );
        write_credentials_json(&credentials)
    }

    fn is_invalid_grant_error(error: &AuthError) -> bool {
        error
            .to_string()
            .to_ascii_lowercase()
            .contains("invalid_grant")
    }

    fn is_retryable_refresh_error(error: &AuthError) -> bool {
        let message = error.to_string().to_ascii_lowercase();
        [
            "timeout",
            "timed out",
            "etimedout",
            "econnreset",
            "temporarily_unavailable",
            "temporarily unavailable",
            "too many requests",
            "429",
            "500",
            "502",
            "503",
            "504",
        ]
        .iter()
        .any(|needle| message.contains(needle))
    }

    fn extract_www_authenticate_param(header: &str, param: &str) -> Option<String> {
        let lower = header.to_ascii_lowercase();
        let needle = format!("{}=", param.to_ascii_lowercase());
        let pos = lower.find(&needle)?;
        let start = pos + needle.len();
        let value = header.get(start..)?;
        if let Some(stripped) = value.strip_prefix('"') {
            let end = stripped.find('"')?;
            Some(stripped[..end].to_string())
        } else {
            let end = value
                .find(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
                .unwrap_or(value.len());
            (end > 0).then(|| value[..end].to_string())
        }
    }

    fn same_origin_resource_metadata_url(server_url: &str, value: &str) -> Option<String> {
        let base = reqwest::Url::parse(server_url).ok()?;
        let candidate = reqwest::Url::parse(value)
            .or_else(|_| base.join(value))
            .ok()?;
        let same_origin = matches!(candidate.scheme(), "http" | "https")
            && base.scheme() == candidate.scheme()
            && base
                .host_str()
                .zip(candidate.host_str())
                .is_some_and(|(base, candidate)| base.eq_ignore_ascii_case(candidate))
            && base.port_or_known_default() == candidate.port_or_known_default();
        same_origin.then(|| candidate.to_string())
    }

    fn disallowed_metadata_ipv4(addr: std::net::Ipv4Addr) -> bool {
        let octets = addr.octets();
        addr.is_private()
            || addr.is_loopback()
            || addr.is_link_local()
            || addr.is_broadcast()
            || addr.is_unspecified()
            || addr.is_multicast()
            || octets[0] == 0
            || (octets[0] == 100 && (64..=127).contains(&octets[1]))
            || (octets[0] == 198 && matches!(octets[1], 18 | 19))
    }

    fn disallowed_metadata_ipv6(addr: std::net::Ipv6Addr) -> bool {
        if let Some(mapped) = addr.to_ipv4_mapped() {
            return disallowed_metadata_ipv4(mapped);
        }
        let segments = addr.segments();
        addr.is_loopback()
            || addr.is_unspecified()
            || addr.is_multicast()
            || (segments[0] & 0xffc0) == 0xfe80
            || (segments[0] & 0xfe00) == 0xfc00
    }

    fn disallowed_metadata_host(host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if matches!(
            host.as_str(),
            "localhost" | "metadata" | "metadata.google.internal" | "metadata.azure.internal"
        ) || host.ends_with(".localhost")
        {
            return true;
        }
        match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(addr)) => disallowed_metadata_ipv4(addr),
            Ok(std::net::IpAddr::V6(addr)) => disallowed_metadata_ipv6(addr),
            Err(_) => false,
        }
    }

    pub(super) fn allowed_authorization_server_metadata_url(value: &str) -> Option<String> {
        // Maps to rmcp/SDK SSRF guards used by CC `discoverOAuthServerInfo`.
        let url = reqwest::Url::parse(value).ok()?;
        let allowed = matches!(url.scheme(), "http" | "https")
            && url
                .host_str()
                .is_some_and(|host| !disallowed_metadata_host(host));
        allowed.then(|| url.to_string())
    }

    /// Maps to: CC `wrapFetchWithStepUpDetection(...)`,
    /// `ClaudeAuthProvider.markStepUpPending(...)`, and persisted discovery
    /// state consumed by `performMCPOAuthFlow(...)` after a 401/403
    /// WWW-Authenticate challenge.
    pub fn record_mcp_www_authenticate_challenge(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        transport_type: &str,
        server_url: &str,
        www_authenticate_header: &str,
        required_scope: Option<&str>,
    ) -> anyhow::Result<bool> {
        let scope = required_scope
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| extract_www_authenticate_param(www_authenticate_header, "scope"));
        let resource_metadata_url =
            extract_www_authenticate_param(www_authenticate_header, "resource_metadata")
                .and_then(|value| same_origin_resource_metadata_url(server_url, &value));

        if scope.is_none() && resource_metadata_url.is_none() {
            return Ok(false);
        }

        let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
        let mut credentials = read_credentials_json_for_update();
        if !credentials.is_object() {
            credentials = serde_json::json!({});
        }
        let root = credentials.as_object_mut().expect("object checked above");
        let mcp_oauth = root
            .entry("mcpOAuth".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !mcp_oauth.is_object() {
            *mcp_oauth = serde_json::json!({});
        }
        let mut entry = mcp_oauth
            .get(&server_key)
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        let entry_obj = entry.as_object_mut().expect("entry object");
        entry_obj.insert(
            "serverName".to_string(),
            Value::String(server_name.to_string()),
        );
        entry_obj.insert(
            "serverUrl".to_string(),
            Value::String(server_url.to_string()),
        );
        entry_obj
            .entry("accessToken".to_string())
            .or_insert_with(|| Value::String(String::new()));
        entry_obj
            .entry("expiresAt".to_string())
            .or_insert_with(|| Value::Number(serde_json::Number::from(0)));
        if let Some(scope) = scope {
            entry_obj.insert("stepUpScope".to_string(), Value::String(scope));
        }
        if let Some(resource_metadata_url) = resource_metadata_url {
            let discovery_state = entry_obj
                .entry("discoveryState".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if !discovery_state.is_object() {
                *discovery_state = serde_json::json!({});
            }
            discovery_state
                .as_object_mut()
                .expect("discoveryState object")
                .insert(
                    "resourceMetadataUrl".to_string(),
                    Value::String(resource_metadata_url),
                );
        }
        mcp_oauth
            .as_object_mut()
            .expect("mcpOAuth object")
            .insert(server_key, entry);
        write_credentials_json(&credentials)?;
        Ok(true)
    }

    async fn refresh_mcp_oauth_token(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        server_key: &str,
        server_url: &str,
        entry: &Value,
        snapshot: &McpOAuthTokenSnapshot,
    ) -> anyhow::Result<Option<String>> {
        // Maps to: CC `ClaudeAuthProvider.refreshAuthorization()` and
        // `_doRefresh()`, but delegates token refresh to rmcp's official
        // `AuthorizationManager::refresh_token()` state machine.
        let Some(client_id) = snapshot.client_id.clone() else {
            return Ok(None);
        };
        let Some(token_response) = oauth_token_response_from_snapshot(snapshot) else {
            return Ok(None);
        };
        let Some(metadata) = metadata_for_refresh(server_url, config, entry).await? else {
            return Ok(None);
        };

        let store = InMemoryCredentialStore::new();
        let granted_scopes = granted_scopes_from_snapshot(snapshot);
        let stored_credentials = StoredCredentials::new(
            client_id.clone(),
            Some(token_response),
            granted_scopes.clone(),
            Some(now_ms() / 1000),
        );
        // Deviation (L2, user-authorized OAuth safety gate): seeding rmcp's
        // mutable credential store is a credential-cache outlet. Metadata and
        // the complete source-shaped stored value are prepared first.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        store.save(stored_credentials).await?;

        let mut manager =
            AuthorizationManager::new_with_oauth_http_client(server_url, oauth_http_client()?)
                .await?;
        manager.set_metadata(metadata.clone());
        manager.set_credential_store(store);
        let redirect_uri = build_redirect_uri(oauth_configured_callback_port(config));
        let mut client_config =
            OAuthClientConfig::new(client_id.clone(), redirect_uri).with_scopes(granted_scopes);
        if let Some(client_secret) = snapshot.client_secret.clone() {
            client_config = client_config.with_client_secret(client_secret);
        }
        manager.configure_client(client_config)?;

        let mut last_error = None;
        for attempt in 1..=3 {
            // Deviation (L2, user-authorized OAuth safety gate): CC
            // `services/mcp/auth.ts:2222-2240` refreshes with the selected
            // metadata/client/token state. All preparation above remains live;
            // only the final SDK credential exchange is default-closed.
            if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
                return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
            }
            match manager.refresh_token().await {
                Ok(token_response) => {
                    persist_mcp_oauth_tokens(
                        server_name,
                        server_key,
                        server_url,
                        &client_id,
                        snapshot.client_secret.as_deref(),
                        &metadata,
                        &token_response,
                        None,
                    )?;
                    let token_json = serde_json::to_value(&token_response)?;
                    return Ok(token_json
                        .get("access_token")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|token| !token.is_empty())
                        .map(ToOwned::to_owned));
                }
                Err(error) if is_invalid_grant_error(&error) => {
                    invalidate_mcp_oauth_tokens(server_key)?;
                    return Ok(None);
                }
                Err(error) if attempt < 3 && is_retryable_refresh_error(&error) => {
                    last_error = Some(error);
                    tokio::time::sleep(Duration::from_millis(1000_u64 << (attempt - 1))).await;
                }
                Err(error) => return Err(error.into()),
            }
        }

        if let Some(error) = last_error {
            return Err(error.into());
        }
        Ok(None)
    }

    async fn perform_mcp_xaa_auth(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        server_key: &str,
        server_url: &str,
        options: &McpOAuthFlowOptions,
    ) -> anyhow::Result<Option<String>> {
        // Maps to: CC `services/mcp/auth.ts#performMCPXaaAuth`.
        let Some(idp) = crate::services::mcp::xaa_idp_login::get_xaa_idp_settings() else {
            anyhow::bail!(
                "XAA: no IdP connection configured. Run 'claude mcp xaa setup --issuer <url> --client-id <id> --client-secret' to configure."
            );
        };
        let Some(client_id) = oauth_configured_client_id(config).map(ToOwned::to_owned) else {
            anyhow::bail!(
                "XAA: server '{server_name}' needs an AS client_id. Re-add with --client-id."
            );
        };
        let Some(client_secret) = mcp_oauth_client_secret_by_key(server_key) else {
            anyhow::bail!(
                "XAA: AS client secret not found for '{server_name}'. Re-add with --client-secret."
            );
        };

        let idp_client_secret =
            crate::services::mcp::xaa_idp_login::get_idp_client_secret(&idp.issuer);
        let observed_authorization_url = Arc::new(Mutex::new(None::<String>));
        let observed_for_callback = Arc::clone(&observed_authorization_url);
        let outer_callback = options.on_authorization_url.clone();
        let authorization_callback: crate::services::mcp::xaa_idp_login::XaaAuthorizationUrlCallback =
            Arc::new(move |url| {
                *observed_for_callback
                    .lock()
                    .expect("XAA authorization URL mutex") = Some(url.clone());
                if let Some(callback) = &outer_callback {
                    callback(url);
                }
            });

        let id_token = crate::services::mcp::xaa_idp_login::acquire_idp_id_token(
            crate::services::mcp::xaa_idp_login::IdpLoginOptions {
                idp_issuer: idp.issuer.clone(),
                idp_client_id: idp.client_id.clone(),
                idp_client_secret: idp_client_secret.clone(),
                callback_port: idp.callback_port,
                on_authorization_url: Some(authorization_callback),
                skip_browser_open: options.skip_browser_open,
            },
        )
        .await?;
        let oidc = crate::services::mcp::xaa_idp_login::discover_oidc(&idp.issuer).await?;
        let result = crate::services::mcp::xaa::perform_cross_app_access(
            server_url,
            crate::services::mcp::xaa::XaaConfig {
                client_id: client_id.clone(),
                client_secret: client_secret.clone(),
                idp_client_id: idp.client_id,
                idp_client_secret,
                idp_id_token: id_token,
                idp_token_endpoint: oidc.token_endpoint,
            },
            server_name,
        )
        .await;
        match result {
            Ok(tokens) => {
                persist_mcp_xaa_tokens(
                    server_name,
                    server_key,
                    server_url,
                    &client_id,
                    &client_secret,
                    &tokens,
                )?;
                Ok(observed_authorization_url
                    .lock()
                    .expect("XAA authorization URL mutex")
                    .clone())
            }
            Err(error) => {
                if error
                    .downcast_ref::<crate::services::mcp::xaa::XaaTokenExchangeError>()
                    .is_some_and(|error| error.should_clear_id_token)
                {
                    let _ = crate::services::mcp::xaa_idp_login::clear_idp_id_token(&idp.issuer);
                    tracing::debug!(
                        server = server_name,
                        "XAA: cleared cached id_token after token-exchange failure"
                    );
                }
                Err(error)
            }
        }
    }

    /// Maps to: CC MCP SDK `auth(provider, ...)` 401 retry path when
    /// `ClaudeAuthProvider.tokens()` exposes a refresh_token.
    ///
    /// A transport-level 401 should force a refresh even when the local expiry
    /// timestamp says the access token is still fresh. This mirrors the JS MCP
    /// SDK's send/start retry semantics while preserving Cometix's service
    /// boundary: transports ask for a refreshed bearer; they do not implement
    /// OAuth protocol logic or open browsers.
    pub async fn refresh_mcp_oauth_access_token_after_auth_failure(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        transport_type: &str,
        server_url: &str,
    ) -> anyhow::Result<Option<String>> {
        let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
        let Some(entry) = mcp_oauth_entry_by_key(&server_key) else {
            return Ok(None);
        };
        let snapshot = token_snapshot_from_entry(&entry, config, &server_key);
        if snapshot.refresh_token.is_none() {
            return Ok(None);
        }
        refresh_mcp_oauth_token(
            server_name,
            config,
            &server_key,
            server_url,
            &entry,
            &snapshot,
        )
        .await
    }

    fn metadata_string_array(metadata: &AuthorizationMetadata, key: &str) -> Vec<String> {
        metadata
            .additional_fields
            .get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn metadata_string(metadata: &AuthorizationMetadata, key: &str) -> Option<String> {
        metadata
            .additional_fields
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    fn form_percent_encode(value: &str) -> String {
        let mut encoded = String::new();
        for byte in value.as_bytes() {
            match *byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    encoded.push(*byte as char)
                }
                b' ' => encoded.push('+'),
                other => encoded.push_str(&format!("%{other:02X}")),
            }
        }
        encoded
    }

    fn form_urlencoded_body(params: &[(String, String)]) -> String {
        params
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    form_percent_encode(key),
                    form_percent_encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    pub(super) async fn revoke_token(
        server_name: &str,
        endpoint: &str,
        token: &str,
        token_type_hint: &str,
        client_id: Option<&str>,
        client_secret: Option<&str>,
        access_token: Option<&str>,
        auth_method: &str,
    ) -> anyhow::Result<()> {
        // Maps to: CC `services/mcp/auth.ts#revokeToken`, an `axios.post`
        // through the global interceptor.
        let client = crate::utils::proxy::create_axios_instance()?
            .timeout(Duration::from_millis(AUTH_REQUEST_TIMEOUT_MS))
            .build()?;
        let mut params = vec![
            ("token".to_string(), token.to_string()),
            ("token_type_hint".to_string(), token_type_hint.to_string()),
        ];
        let mut request = client.post(endpoint);
        if let (Some(client_id), Some(client_secret)) = (client_id, client_secret) {
            if auth_method == "client_secret_post" {
                params.push(("client_id".to_string(), client_id.to_string()));
                params.push(("client_secret".to_string(), client_secret.to_string()));
            } else {
                request = request.basic_auth(client_id, Some(client_secret));
            }
        } else if let Some(client_id) = client_id {
            params.push(("client_id".to_string(), client_id.to_string()));
        }

        let request = request
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form_urlencoded_body(&params));
        // Deviation (L2, user-authorized OAuth safety gate): CC
        // `services/mcp/auth.ts:381-456` sends the prepared RFC 7009 request.
        // Cometix rejects at the final credential HTTP outlet.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        if response.status().is_success() {
            return Ok(());
        }

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            if let Some(access_token) = access_token {
                let request = client
                    .post(endpoint)
                    .header(
                        reqwest::header::AUTHORIZATION,
                        format!("Bearer {access_token}"),
                    )
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(form_urlencoded_body(&[
                        ("token".to_string(), token.to_string()),
                        ("token_type_hint".to_string(), token_type_hint.to_string()),
                    ]));
                // Deviation (L2, same OAuth safety gate): the source's
                // non-compliant Bearer retry is independently guarded so no
                // alternate revocation send bypasses the product switch.
                if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
                    return Err(
                        crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into(),
                    );
                }
                let response = request.send().await?;
                if response.status().is_success() {
                    return Ok(());
                }
                anyhow::bail!("HTTP {} revoking {token_type_hint}", response.status());
            }
        }

        tracing::debug!(server = server_name, status = %response.status(), token_type_hint, "MCP OAuth token revocation failed");
        anyhow::bail!("HTTP {} revoking {token_type_hint}", response.status())
    }

    fn slim_discovery_state_for_preserve(entry: &Value) -> Option<Value> {
        let state = entry.get("discoveryState")?;
        let mut slim = serde_json::Map::new();
        if let Some(url) = state
            .get("authorizationServerUrl")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            slim.insert(
                "authorizationServerUrl".to_string(),
                Value::String(url.to_string()),
            );
        }
        if let Some(url) = state
            .get("resourceMetadataUrl")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            slim.insert(
                "resourceMetadataUrl".to_string(),
                Value::String(url.to_string()),
            );
        }
        (!slim.is_empty()).then(|| Value::Object(slim))
    }

    fn clear_server_tokens_from_local_storage_with_preserved_state(
        server_name: &str,
        server_key: &str,
        server_url: &str,
        previous_entry: Option<&Value>,
        preserve_step_up_state: bool,
    ) -> anyhow::Result<()> {
        // Maps to: CC `clearServerTokensFromLocalStorage(...)` plus
        // `revokeServerTokens(..., { preserveStepUpState })` rehydrate block.
        clear_server_tokens_from_local_storage(server_key)?;
        if !preserve_step_up_state {
            return Ok(());
        }
        let Some(previous_entry) = previous_entry else {
            return Ok(());
        };
        let step_up_scope = trimmed_entry_string(previous_entry, "stepUpScope");
        let discovery_state = slim_discovery_state_for_preserve(previous_entry);
        if step_up_scope.is_none() && discovery_state.is_none() {
            return Ok(());
        }

        let mut credentials = read_credentials_json_for_update();
        if !credentials.is_object() {
            credentials = serde_json::json!({});
        }
        let root = credentials.as_object_mut().expect("object checked above");
        let mcp_oauth = root
            .entry("mcpOAuth".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !mcp_oauth.is_object() {
            *mcp_oauth = serde_json::json!({});
        }
        let mut entry = serde_json::json!({
            "serverName": server_name,
            "serverUrl": server_url,
            "accessToken": "",
            "expiresAt": 0,
        });
        let entry_obj = entry.as_object_mut().expect("entry object");
        if let Some(step_up_scope) = step_up_scope {
            entry_obj.insert("stepUpScope".to_string(), Value::String(step_up_scope));
        }
        if let Some(discovery_state) = discovery_state {
            entry_obj.insert("discoveryState".to_string(), discovery_state);
        }
        mcp_oauth
            .as_object_mut()
            .expect("mcpOAuth object")
            .insert(server_key.to_string(), entry);
        write_credentials_json(&credentials)
    }

    /// Maps to: CC `services/mcp/auth.ts:467-589` `revokeServerTokens`.
    pub async fn revoke_server_tokens(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        preserve_step_up_state: bool,
    ) -> anyhow::Result<()> {
        let transport_type = oauth_transport_type(config)?;
        let server_url = config
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP OAuth server is missing URL"))?;
        let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
        let entry = mcp_oauth_entry_by_key(&server_key);
        let Some(entry_value) = entry.as_ref() else {
            return Ok(());
        };
        let snapshot = token_snapshot_from_entry(entry_value, config, &server_key);

        if snapshot.access_token.is_some() || snapshot.refresh_token.is_some() {
            match metadata_for_refresh(server_url, config, entry_value).await {
                Ok(Some(metadata)) => {
                    if let Some(revocation_endpoint) =
                        metadata_string(&metadata, "revocation_endpoint")
                    {
                        let auth_methods = {
                            let revocation = metadata_string_array(
                                &metadata,
                                "revocation_endpoint_auth_methods_supported",
                            );
                            if revocation.is_empty() {
                                metadata_string_array(
                                    &metadata,
                                    "token_endpoint_auth_methods_supported",
                                )
                            } else {
                                revocation
                            }
                        };
                        let auth_method = if !auth_methods
                            .iter()
                            .any(|method| method == "client_secret_basic")
                            && auth_methods
                                .iter()
                                .any(|method| method == "client_secret_post")
                        {
                            "client_secret_post"
                        } else {
                            "client_secret_basic"
                        };
                        if let Some(refresh_token) = snapshot.refresh_token.as_deref() {
                            if let Err(error) = revoke_token(
                                server_name,
                                &revocation_endpoint,
                                refresh_token,
                                "refresh_token",
                                snapshot.client_id.as_deref(),
                                snapshot.client_secret.as_deref(),
                                snapshot.access_token.as_deref(),
                                auth_method,
                            )
                            .await
                            {
                                tracing::debug!(server = server_name, error = %error, "failed to revoke MCP OAuth refresh token");
                            }
                        }
                        if let Some(access_token) = snapshot.access_token.as_deref() {
                            if let Err(error) = revoke_token(
                                server_name,
                                &revocation_endpoint,
                                access_token,
                                "access_token",
                                snapshot.client_id.as_deref(),
                                snapshot.client_secret.as_deref(),
                                snapshot.access_token.as_deref(),
                                auth_method,
                            )
                            .await
                            {
                                tracing::debug!(server = server_name, error = %error, "failed to revoke MCP OAuth access token");
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(server = server_name, error = %error, "failed to discover MCP OAuth metadata for revocation");
                }
            }
        }

        clear_server_tokens_from_local_storage_with_preserved_state(
            server_name,
            &server_key,
            server_url,
            entry.as_ref(),
            preserve_step_up_state,
        )
    }

    #[derive(Debug)]
    pub(super) struct OAuthCallback {
        pub(super) code: String,
        pub(super) state: String,
        pub(super) issuer: Option<String>,
    }

    fn html_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.as_bytes().len()
        )
    }

    async fn write_html_response(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        body: &str,
    ) -> anyhow::Result<()> {
        stream
            .write_all(html_response(status, body).as_bytes())
            .await?;
        let _ = stream.shutdown().await;
        Ok(())
    }

    fn parse_callback_url(
        parsed: &reqwest::Url,
        require_callback_path: bool,
    ) -> anyhow::Result<OAuthCallback> {
        if require_callback_path && parsed.path() != "/callback" {
            anyhow::bail!("ignoring non-callback OAuth request")
        }
        if let Some(error) = parsed
            .query_pairs()
            .find(|(key, _)| key == "error")
            .map(|(_, value)| value.into_owned())
        {
            let description = parsed
                .query_pairs()
                .find(|(key, _)| key == "error_description")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default();
            anyhow::bail!("OAuth error: {error} - {description}")
        }
        let code = parsed
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("OAuth callback did not include a code"))?;
        let state = parsed
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("OAuth callback did not include a state"))?;
        let issuer = parsed
            .query_pairs()
            .find(|(key, _)| key == "iss")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.trim().is_empty());
        Ok(OAuthCallback {
            code,
            state,
            issuer,
        })
    }

    fn parse_callback_target(target: &str) -> anyhow::Result<OAuthCallback> {
        let parsed = reqwest::Url::parse(&format!("http://localhost{target}"))?;
        parse_callback_url(&parsed, true)
    }

    pub(super) fn parse_manual_callback_url(
        callback_url: &str,
    ) -> anyhow::Result<Option<OAuthCallback>> {
        // Maps to CC `onWaitingForCallback` submit callback: invalid URLs and
        // URLs without a code are ignored so the user can try again.
        let Ok(parsed) = reqwest::Url::parse(callback_url.trim()) else {
            return Ok(None);
        };
        match parse_callback_url(&parsed, false) {
            Ok(callback) => Ok(Some(callback)),
            Err(error)
                if error.to_string() == "OAuth callback did not include a code"
                    || error.to_string() == "OAuth callback did not include a state" =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    async fn handle_oauth_callback_stream(
        mut stream: tokio::net::TcpStream,
        expected_state: &str,
    ) -> anyhow::Result<Option<OAuthCallback>> {
        let mut buffer = vec![0_u8; 8192];
        let read = stream.read(&mut buffer).await?;
        let request = String::from_utf8_lossy(&buffer[..read]);
        let Some(request_line) = request.lines().next() else {
            return Ok(None);
        };
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        if method != "GET" {
            write_html_response(
                &mut stream,
                "405 Method Not Allowed",
                "<h1>Authentication Error</h1><p>Unsupported callback method.</p>",
            )
            .await?;
            return Ok(None);
        }
        match parse_callback_target(target) {
            Ok(callback) if callback.state == expected_state => {
                write_html_response(
                    &mut stream,
                    "200 OK",
                    "<h1>Authentication Successful</h1><p>You can close this window. Return to CometixCode.</p>",
                )
                .await?;
                Ok(Some(callback))
            }
            Ok(_) => {
                write_html_response(
                    &mut stream,
                    "400 Bad Request",
                    "<h1>Authentication Error</h1><p>Invalid state parameter. Please try again.</p>",
                )
                .await?;
                anyhow::bail!("OAuth state mismatch - possible CSRF attack")
            }
            Err(error) => {
                write_html_response(
                    &mut stream,
                    "400 Bad Request",
                    &format!("<h1>Authentication Error</h1><p>{error}</p>"),
                )
                .await?;
                if error.to_string().starts_with("ignoring non-callback") {
                    return Ok(None);
                }
                Err(error)
            }
        }
    }

    pub(super) fn validate_manual_oauth_callback(
        callback_url: &str,
        expected_state: &str,
    ) -> anyhow::Result<Option<OAuthCallback>> {
        match parse_manual_callback_url(callback_url)? {
            Some(callback) if callback.state == expected_state => Ok(Some(callback)),
            Some(_) => anyhow::bail!("OAuth state mismatch - possible CSRF attack"),
            None => Ok(None),
        }
    }

    async fn wait_for_oauth_callback(
        listener: TokioTcpListener,
        expected_state: &str,
        mut manual_callback_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
        abort_signal: Option<McpOAuthAbortSignal>,
    ) -> anyhow::Result<OAuthCallback> {
        let callback = async {
            loop {
                if abort_signal
                    .as_ref()
                    .is_some_and(|signal| signal.is_aborted())
                {
                    anyhow::bail!(AUTHENTICATION_CANCELLED_MESSAGE);
                }

                match (manual_callback_rx.as_mut(), abort_signal.as_ref()) {
                    (Some(receiver), Some(signal)) => {
                        tokio::select! {
                            _ = signal.cancelled() => {
                                anyhow::bail!(AUTHENTICATION_CANCELLED_MESSAGE);
                            }
                            accepted = listener.accept() => {
                                let (stream, _) = accepted?;
                                if let Some(callback) = handle_oauth_callback_stream(stream, expected_state).await? {
                                    return Ok(callback);
                                }
                            }
                            maybe_url = receiver.recv() => {
                                let Some(callback_url) = maybe_url else {
                                    manual_callback_rx = None;
                                    continue;
                                };
                                if let Some(callback) = validate_manual_oauth_callback(&callback_url, expected_state)? {
                                    return Ok(callback);
                                }
                            }
                        }
                    }
                    (Some(receiver), None) => {
                        tokio::select! {
                            accepted = listener.accept() => {
                                let (stream, _) = accepted?;
                                if let Some(callback) = handle_oauth_callback_stream(stream, expected_state).await? {
                                    return Ok(callback);
                                }
                            }
                            maybe_url = receiver.recv() => {
                                let Some(callback_url) = maybe_url else {
                                    manual_callback_rx = None;
                                    continue;
                                };
                                if let Some(callback) = validate_manual_oauth_callback(&callback_url, expected_state)? {
                                    return Ok(callback);
                                }
                            }
                        }
                    }
                    (None, Some(signal)) => {
                        tokio::select! {
                            _ = signal.cancelled() => {
                                anyhow::bail!(AUTHENTICATION_CANCELLED_MESSAGE);
                            }
                            accepted = listener.accept() => {
                                let (stream, _) = accepted?;
                                if let Some(callback) = handle_oauth_callback_stream(stream, expected_state).await? {
                                    return Ok(callback);
                                }
                            }
                        }
                    }
                    (None, None) => {
                        let (stream, _) = listener.accept().await?;
                        if let Some(callback) =
                            handle_oauth_callback_stream(stream, expected_state).await?
                        {
                            return Ok(callback);
                        }
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(AUTH_CALLBACK_TIMEOUT_SECS), callback)
            .await
            .map_err(|_| anyhow::anyhow!("Authentication timeout"))?
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub(super) struct CachedReauthState {
        pub(super) step_up_scope: Option<String>,
        pub(super) resource_metadata_url: Option<String>,
    }

    pub(super) fn cached_reauth_state_from_entry(entry: Option<&Value>) -> CachedReauthState {
        // Maps to: CC `performMCPOAuthFlow` cached `stepUpScope` and
        // `discoveryState.resourceMetadataUrl` read before clearing tokens.
        let Some(entry) = entry else {
            return CachedReauthState::default();
        };
        CachedReauthState {
            step_up_scope: trimmed_entry_string(entry, "stepUpScope"),
            resource_metadata_url: discovery_state_string(entry, "resourceMetadataUrl"),
        }
    }

    fn clear_server_tokens_from_local_storage(server_key: &str) -> anyhow::Result<()> {
        // Maps to: CC `services/mcp/auth.ts#clearServerTokensFromLocalStorage`.
        let mut credentials = read_credentials_json_for_update();
        let Some(mcp_oauth) = credentials
            .get_mut("mcpOAuth")
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };
        mcp_oauth.remove(server_key);
        write_credentials_json(&credentials)
    }

    #[derive(Clone, Debug, Default, serde::Deserialize)]
    struct ProtectedResourceMetadataSnapshot {
        resource: Option<String>,
        authorization_server: Option<String>,
        authorization_servers: Option<Vec<String>>,
        scopes_supported: Option<Vec<String>>,
    }

    fn is_root_resource_identifier(value: &str) -> bool {
        reqwest::Url::parse(value)
            .is_ok_and(|url| url.path() == "/" && url.query().is_none() && url.fragment().is_none())
    }

    pub(super) fn resource_identifiers_match(expected: &str, actual: &str) -> bool {
        expected == actual
            || (is_root_resource_identifier(expected) && actual == expected.trim_end_matches('/'))
            || (is_root_resource_identifier(actual) && expected == actual.trim_end_matches('/'))
    }

    pub(super) fn preconfigured_oauth_client_config(
        client_id: &str,
        redirect_uri: &str,
        scopes: &[String],
        server_key: &str,
    ) -> OAuthClientConfig {
        // Maps to: CC `ClaudeAuthProvider.clientInformation()` preconfigured
        // `oauth.clientId` path: read `mcpOAuthClientConfig[serverKey]` and
        // pass the client_secret to the SDK token exchange without requiring
        // dynamic client registration.
        let mut client_config =
            OAuthClientConfig::new(client_id.to_string(), redirect_uri.to_string())
                .with_scopes(scopes.to_vec());
        if let Some(client_secret) = mcp_oauth_client_secret_by_key(server_key) {
            client_config = client_config.with_client_secret(client_secret);
        }
        client_config
    }

    async fn fetch_cached_resource_metadata(
        server_url: &str,
        cached_resource_metadata_url: Option<&str>,
    ) -> anyhow::Result<Option<(AuthorizationMetadata, Vec<String>, String)>> {
        // Maps to: CC `fetchAuthServerMetadata(..., resourceMetadataUrl)`
        // when `performMCPOAuthFlow` consumes cached step-up discovery state.
        let Some(resource_metadata_url) = cached_resource_metadata_url
            .and_then(|url| same_origin_resource_metadata_url(server_url, url))
        else {
            return Ok(None);
        };
        // The SDK's default `fetch`: the global dispatcher.
        let client = crate::utils::proxy::get_proxy_fetch_options(false)?
            .timeout(Duration::from_millis(AUTH_REQUEST_TIMEOUT_MS))
            .build()?;
        let request = client
            .get(&resource_metadata_url)
            .header(reqwest::header::ACCEPT, "application/json");
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let resource_metadata = response.json::<ProtectedResourceMetadataSnapshot>().await?;
        if let Some(resource) = resource_metadata
            .resource
            .as_deref()
            .map(str::trim)
            .filter(|resource| !resource.is_empty())
        {
            if !resource_identifiers_match(server_url, resource) {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
        let auth_server = resource_metadata
            .authorization_server
            .or_else(|| {
                resource_metadata
                    .authorization_servers
                    .and_then(|servers| servers.into_iter().next())
            })
            .and_then(|url| allowed_authorization_server_metadata_url(url.trim()));
        let Some(auth_server) = auth_server else {
            return Ok(None);
        };
        let metadata = fetch_rmcp_auth_server_metadata(&auth_server, None)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("cached resource metadata did not resolve OAuth metadata")
            })?;
        Ok(Some((
            metadata,
            resource_metadata.scopes_supported.unwrap_or_default(),
            resource_metadata_url,
        )))
    }

    /// Maps to: CC `services/mcp/auth.ts#performMCPOAuthFlow`.
    ///
    /// This uses rmcp's official OAuth state machine for discovery, DCR/client
    /// setup, authorization URL construction, PKCE state storage, and token
    /// exchange. The local loopback callback server and credentials-json write
    /// mirror Claude Code's service boundary.
    pub async fn perform_mcp_oauth_flow(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        options: McpOAuthFlowOptions,
    ) -> anyhow::Result<McpOAuthFlowResult> {
        let transport_type = oauth_transport_type(config)?;
        let server_url = config
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP OAuth server is missing URL"))?;
        let server_key = get_server_key(server_name, transport_type, server_url, &config.headers);
        if config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.get("xaa"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if !crate::services::mcp::xaa_idp_login::is_xaa_enabled() {
                anyhow::bail!(
                    "XAA is not enabled (set CLAUDE_CODE_ENABLE_XAA=1). Remove 'oauth.xaa' from server '{server_name}' to use the standard consent flow."
                );
            }
            if crate::services::mcp::xaa_idp_login::get_xaa_idp_settings().is_none() {
                anyhow::bail!(
                    "XAA: no IdP connection configured. Run 'claude mcp xaa setup --issuer <url> --client-id <id> --client-secret' to configure."
                );
            }
            if oauth_configured_client_id(config).is_none() {
                anyhow::bail!(
                    "XAA: server '{server_name}' needs an AS client_id. Re-add with --client-id."
                );
            }
            if mcp_oauth_client_secret_by_key(&server_key).is_none() {
                anyhow::bail!(
                    "XAA: AS client secret not found for '{server_name}'. Re-add with --client-secret."
                );
            }
            let authorization_url =
                perform_mcp_xaa_auth(server_name, config, &server_key, server_url, &options)
                    .await?
                    .unwrap_or_default();
            return Ok(McpOAuthFlowResult {
                authorization_url,
                redirect_uri: String::new(),
                port: 0,
                state: String::new(),
                server_key,
                access_token_saved: true,
            });
        }
        let port = match oauth_configured_callback_port(config) {
            Some(port) => port,
            None => find_available_port().await?,
        };
        let redirect_uri = build_redirect_uri(Some(port));

        let cached_entry = mcp_oauth_entry_by_key(&server_key);
        let cached_reauth_state = cached_reauth_state_from_entry(cached_entry.as_ref());
        clear_server_tokens_from_local_storage(&server_key)?;

        let cached_resource_metadata = fetch_cached_resource_metadata(
            server_url,
            cached_reauth_state.resource_metadata_url.as_deref(),
        )
        .await
        .ok()
        .flatten();
        let (metadata, protected_resource_scopes, resource_metadata_url) =
            if let Some((metadata, scopes, resource_metadata_url)) = cached_resource_metadata {
                (metadata, scopes, Some(resource_metadata_url))
            } else {
                (
                    fetch_rmcp_auth_server_metadata(
                        server_url,
                        oauth_configured_metadata_url(config),
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("MCP server did not advertise OAuth metadata")
                    })?,
                    Vec::new(),
                    None,
                )
            };
        let mut manager =
            AuthorizationManager::new_with_oauth_http_client(server_url, oauth_http_client()?)
                .await?;
        manager.set_metadata(metadata.clone());
        let scopes = if let Some(step_up_scope) = cached_reauth_state.step_up_scope.as_deref() {
            step_up_scope
                .split(' ')
                .filter(|scope| !scope.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        } else if !protected_resource_scopes.is_empty() {
            protected_resource_scopes
        } else {
            manager.select_scopes(None, &[])
        };
        let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
        let (client_id, client_secret) = if let Some(client_id) = oauth_configured_client_id(config)
        {
            let client_config =
                preconfigured_oauth_client_config(client_id, &redirect_uri, &scopes, &server_key);
            manager.configure_client(client_config)?;
            (client_id.to_string(), None)
        } else if metadata
            .additional_fields
            .get("client_id_metadata_document_supported")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let client_metadata_url = crate::utils::process_env::var("MCP_OAUTH_CLIENT_METADATA_URL")
                .truthy()
                .unwrap_or_else(|| crate::constants::oauth::MCP_CLIENT_METADATA_URL.to_string());
            let client_config =
                OAuthClientConfig::new(client_metadata_url.clone(), redirect_uri.clone())
                    .with_scopes(scopes.clone());
            manager.configure_client(client_config)?;
            (client_metadata_url, None)
        } else {
            let client_name = format!("Claude Code ({server_name})");
            // Deviation (L2, user-authorized OAuth safety gate): dynamic
            // registration can mint a client secret. Metadata, scopes, and the
            // complete registration inputs remain prepared before the SDK send.
            if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
                return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
            }
            let client_config = manager
                .register_client(&client_name, &redirect_uri, &scope_refs)
                .await?;
            (client_config.client_id, client_config.client_secret)
        };
        // Deviation (L2, user-authorized OAuth safety gate): rmcp couples
        // authorization-request construction to mutation of its PKCE/state
        // store in `get_authorization_url`. Guard the SDK method boundary so a
        // blocked flow leaves that credential state cache unchanged.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let authorization_url = manager.get_authorization_url(&scope_refs).await?;
        // Deviation (L2, same gate): independently guard the callback listener
        // bind so this outlet cannot bypass the product switch if SDK behavior
        // or the switch changes later.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let listener = TokioTcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AddrInUse {
                    anyhow::anyhow!("OAuth callback port {port} is already in use")
                } else {
                    anyhow::anyhow!("OAuth callback server failed: {error}")
                }
            })?;
        if let Some(on_authorization_url) = &options.on_authorization_url {
            on_authorization_url(authorization_url.clone());
        }
        let state = authorization_state_from_url(&authorization_url)?;
        let manual_callback_rx = options.on_waiting_for_callback.as_ref().map(|on_waiting| {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            on_waiting(McpManualCallbackSubmit { sender });
            receiver
        });
        if !options.skip_browser_open {
            let _ = crate::utils::browser::open_browser(&authorization_url).await?;
        }

        if options
            .abort_signal
            .as_ref()
            .is_some_and(McpOAuthAbortSignal::is_aborted)
        {
            anyhow::bail!(AUTHENTICATION_CANCELLED_MESSAGE);
        }

        let callback = wait_for_oauth_callback(
            listener,
            &state,
            manual_callback_rx,
            options.abort_signal.clone(),
        )
        .await?;
        // Deviation (L2, user-authorized OAuth safety gate): CC
        // `services/mcp/auth.ts:1219-1224` performs the final authorization-code
        // token exchange only after callback/state validation. The SDK request
        // is fully prepared before this default-closed outlet.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let token_response = manager
            .exchange_code_for_token_with_issuer(
                &callback.code,
                &callback.state,
                callback.issuer.as_deref(),
            )
            .await?;
        let access_token_saved = persist_mcp_oauth_tokens(
            server_name,
            &server_key,
            server_url,
            &client_id,
            client_secret.as_deref(),
            &metadata,
            &token_response,
            resource_metadata_url.as_deref(),
        )?;
        Ok(McpOAuthFlowResult {
            authorization_url,
            redirect_uri,
            port,
            state,
            server_key,
            access_token_saved,
        })
    }
}

#[cfg(not(feature = "mcp_runtime"))]
mod runtime {
    use super::*;

    /// Maps to: CC `services/mcp/auth.ts#fetchAuthServerMetadata`.
    /// Current behavior: no runtime OAuth network discovery without the
    /// `mcp_runtime` feature. The full implementation is compiled with rmcp's
    /// official `auth` feature.
    pub async fn fetch_auth_server_metadata(
        _server_url: &str,
        _configured_metadata_url: Option<&str>,
    ) -> anyhow::Result<Option<McpAuthorizationServerMetadata>> {
        anyhow::bail!("mcp_runtime feature is disabled; MCP OAuth discovery is not compiled")
    }

    pub async fn refresh_mcp_oauth_access_token_after_auth_failure(
        _server_name: &str,
        _config: &super::super::types::ScopedMcpServerConfig,
        _transport_type: &str,
        _server_url: &str,
    ) -> anyhow::Result<Option<String>> {
        anyhow::bail!("mcp_runtime feature is disabled; MCP OAuth token refresh is not compiled")
    }

    pub fn record_mcp_www_authenticate_challenge(
        _server_name: &str,
        _config: &super::super::types::ScopedMcpServerConfig,
        _transport_type: &str,
        _server_url: &str,
        _www_authenticate_header: &str,
        _required_scope: Option<&str>,
    ) -> anyhow::Result<bool> {
        anyhow::bail!(
            "mcp_runtime feature is disabled; MCP OAuth WWW-Authenticate handling is not compiled"
        )
    }

    pub async fn revoke_server_tokens(
        _server_name: &str,
        _config: &super::super::types::ScopedMcpServerConfig,
        _preserve_step_up_state: bool,
    ) -> anyhow::Result<()> {
        anyhow::bail!("mcp_runtime feature is disabled; MCP OAuth revocation is not compiled")
    }

    pub fn save_mcp_client_secret(
        _server_name: &str,
        _config: &super::super::types::ScopedMcpServerConfig,
        _client_secret: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!(
            "mcp_runtime feature is disabled; MCP OAuth client-secret storage is not compiled"
        )
    }

    pub fn get_mcp_client_config(
        _server_name: &str,
        _config: &super::super::types::ScopedMcpServerConfig,
    ) -> anyhow::Result<Option<String>> {
        anyhow::bail!(
            "mcp_runtime feature is disabled; MCP OAuth client-secret storage is not compiled"
        )
    }

    pub fn clear_mcp_client_config(
        _server_name: &str,
        _config: &super::super::types::ScopedMcpServerConfig,
    ) -> anyhow::Result<()> {
        anyhow::bail!(
            "mcp_runtime feature is disabled; MCP OAuth client-secret storage is not compiled"
        )
    }

    pub async fn perform_mcp_oauth_flow(
        _server_name: &str,
        _config: &super::super::types::ScopedMcpServerConfig,
        _options: McpOAuthFlowOptions,
    ) -> anyhow::Result<McpOAuthFlowResult> {
        anyhow::bail!("mcp_runtime feature is disabled; MCP OAuth flow is not compiled")
    }
}

pub use runtime::{
    clear_mcp_client_config, fetch_auth_server_metadata, get_mcp_client_config,
    perform_mcp_oauth_flow, record_mcp_www_authenticate_challenge,
    refresh_mcp_oauth_access_token_after_auth_failure, revoke_server_tokens,
    save_mcp_client_secret,
};
#[cfg(feature = "mcp_runtime")]
pub(crate) use runtime::oauth_http_client;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn get_server_key_matches_official_hash_input_shape() {
        let headers = BTreeMap::from([("X-Test".to_string(), "value".to_string())]);
        assert_eq!(
            get_server_key("docs", "http", "https://example.com/mcp", &headers),
            "docs|9a364a44ea6c0ebc"
        );
    }

    /// CC's MCP auth requests run on the global `fetch`: rmcp's OAuth
    /// discovery goes through the environment's proxy. `HTTPS_PROXY` carries
    /// an `http://` server too, CC's one proxy for every scheme, where rmcp's
    /// built-in client would connect direct. Discovery asks for no redirect
    /// and follows same-origin ones itself, so a cross-origin 302 is never
    /// followed.
    #[cfg(feature = "mcp_runtime")]
    #[tokio::test]
    async fn oauth_discovery_goes_through_the_fetch_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::tls_provider::install_crypto_provider();
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let _env: Vec<_> = ["https_proxy", "http_proxy", "HTTP_PROXY", "no_proxy", "NO_PROXY"]
            .into_iter()
            .map(EnvVarGuard::unset)
            .chain([EnvVarGuard::set("HTTPS_PROXY", format!("http://{proxy_address}"))])
            .collect();
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = proxy.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..count]).into_owned();
                let _ = seen_tx.send(head.lines().next().unwrap_or_default().to_owned());
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nlocation: http://other.invalid/metadata\r\n\
                          content-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            }
        });

        let manager = rmcp::transport::auth::AuthorizationManager::new_with_oauth_http_client(
            "http://mcp.invalid/mcp",
            oauth_http_client().unwrap(),
        )
        .await
        .unwrap();
        let _ = manager.discover_metadata().await;
        server.abort();
        let first = seen_rx.recv().await.expect("a request reached the proxy");
        assert!(first.starts_with("GET http://mcp.invalid/"), "{first}");
        while let Ok(line) = seen_rx.try_recv() {
            assert!(!line.contains("other.invalid"), "followed a cross-origin redirect: {line}");
        }
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn mcp_token_presence_and_discovery_only_state_use_the_auth_owner() {
        with_temp_config_home(|dir| {
            let config = crate::services::mcp::types::ScopedMcpServerConfig {
                name: None,
                scope: crate::services::mcp::types::ConfigScope::User,
                transport: crate::services::mcp::types::Transport::Http,
                command: None,
                args: Vec::new(),
                env: BTreeMap::new(),
                url: Some("https://example.test/mcp".to_string()),
                headers: BTreeMap::new(),
                headers_helper: None,
                oauth: None,
                ide_running_in_windows: None,
                ide_name: None,
                auth_token: None,
                id: None,
                plugin_source: None,
            };
            let key = get_server_key("docs", "http", "https://example.test/mcp", &BTreeMap::new());
            let credentials_path = dir.join(".credentials.json");
            let write_oauth_entry = |entry: serde_json::Value| {
                let mut entries = serde_json::Map::new();
                entries.insert(key.clone(), entry);
                std::fs::write(
                    &credentials_path,
                    serde_json::json!({ "mcpOAuth": serde_json::Value::Object(entries) })
                        .to_string(),
                )
                .unwrap();
            };

            write_oauth_entry(serde_json::json!({
                "accessToken": "access",
                "expiresAt": now_ms() + 60_000,
            }));
            assert!(
                ClaudeAuthProvider::new("docs", &config)
                    .tokens()
                    .unwrap()
                    .is_some()
            );
            assert!(!has_mcp_discovery_but_no_token("docs", &config));

            write_oauth_entry(serde_json::json!({
                "accessToken": "expired",
                "expiresAt": now_ms().saturating_sub(1_000),
            }));
            assert!(
                ClaudeAuthProvider::new("docs", &config)
                    .tokens()
                    .unwrap()
                    .is_none()
            );

            write_oauth_entry(serde_json::json!({
                "accessToken": "expired",
                "refreshToken": "refresh",
                "expiresAt": 0,
            }));
            let error = ClaudeAuthProvider::new("docs", &config)
                .tokens()
                .expect_err("proactive refresh must hit the closed credential outlet");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );

            write_oauth_entry(serde_json::json!({
                "discoveryState": {"authorizationServerMetadata": {}}
            }));
            assert!(has_mcp_discovery_but_no_token("docs", &config));
        });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn manual_callback_url_submit_parses_and_validates_like_official() {
        let callback = super::runtime::validate_manual_oauth_callback(
            "http://127.0.0.1:3118/callback?code=abc&state=state-1&iss=https%3A%2F%2Fissuer.example",
            "state-1",
        )
        .unwrap()
        .expect("callback");
        assert_eq!(callback.code, "abc");
        assert_eq!(callback.state, "state-1");
        assert_eq!(callback.issuer.as_deref(), Some("https://issuer.example"));

        assert!(
            super::runtime::validate_manual_oauth_callback("not a url", "state-1")
                .unwrap()
                .is_none()
        );
        assert!(
            super::runtime::validate_manual_oauth_callback(
                "http://127.0.0.1:3118/callback?state=state-1",
                "state-1",
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::runtime::validate_manual_oauth_callback(
                "http://127.0.0.1:3118/callback?code=abc&state=wrong",
                "state-1",
            )
            .unwrap_err()
            .to_string()
            .contains("OAuth state mismatch")
        );
        assert!(
            super::runtime::validate_manual_oauth_callback(
                "http://127.0.0.1:3118/callback?error=access_denied&error_description=nope",
                "state-1",
            )
            .unwrap_err()
            .to_string()
            .contains("OAuth error: access_denied - nope")
        );
    }

    #[test]
    fn manual_callback_submit_forwards_url_like_official_submit_function() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let submit = McpManualCallbackSubmit { sender };
        submit.submit("http://localhost:3118/callback?code=abc&state=s");
        assert_eq!(
            receiver.try_recv().unwrap(),
            "http://localhost:3118/callback?code=abc&state=s"
        );
    }

    #[tokio::test]
    async fn oauth_abort_handle_notifies_signal_like_abort_controller() {
        let handle = McpOAuthAbortHandle::new();
        let signal = handle.signal();
        assert!(!signal.is_aborted());
        let waiter = tokio::spawn({
            let signal = signal.clone();
            async move { signal.cancelled().await }
        });
        handle.abort();
        waiter.await.unwrap();
        assert!(signal.is_aborted());
        assert!(is_authentication_cancelled_error(&anyhow::anyhow!(
            AUTHENTICATION_CANCELLED_MESSAGE
        )));
    }

    #[test]
    fn get_scope_from_metadata_matches_official_precedence() {
        let metadata = McpAuthorizationServerMetadata {
            scope: Some("direct:scope".to_string()),
            default_scope: Some("default:scope".to_string()),
            scopes_supported: Some(vec!["read".to_string(), "write".to_string()]),
            ..McpAuthorizationServerMetadata::default()
        };
        assert_eq!(
            get_scope_from_metadata(&metadata).as_deref(),
            Some("direct:scope")
        );

        let metadata = McpAuthorizationServerMetadata {
            default_scope: Some("default:scope".to_string()),
            scopes_supported: Some(vec!["read".to_string(), "write".to_string()]),
            ..McpAuthorizationServerMetadata::default()
        };
        assert_eq!(
            get_scope_from_metadata(&metadata).as_deref(),
            Some("default:scope")
        );

        let metadata = McpAuthorizationServerMetadata {
            scopes_supported: Some(vec![
                "read".to_string(),
                " write ".to_string(),
                "".to_string(),
            ]),
            ..McpAuthorizationServerMetadata::default()
        };
        assert_eq!(
            get_scope_from_metadata(&metadata).as_deref(),
            Some("read write")
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[tokio::test]
    async fn configured_metadata_url_requires_https_like_official() {
        let error = fetch_auth_server_metadata(
            "https://example.com/mcp",
            Some("http://auth.example.com/.well-known/oauth-authorization-server"),
        )
        .await
        .expect_err("non-https configured metadata URL must fail");
        assert!(
            error
                .to_string()
                .contains("authServerMetadataUrl must use https://"),
            "error={error}"
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[tokio::test]
    async fn mcp_revocation_send_uses_canonical_default_closed_gate() {
        crate::utils::tls_provider::install_crypto_provider();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/revoke", listener.local_addr().unwrap());
        let error = runtime::revoke_token(
            "docs",
            &endpoint,
            "refresh-token",
            "refresh_token",
            Some("client"),
            Some("secret"),
            Some("access-token"),
            "client_secret_post",
        )
        .await
        .expect_err("OAuth revocation send must be default-closed");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[cfg(feature = "mcp_runtime")]
    fn test_http_config() -> crate::services::mcp::types::ScopedMcpServerConfig {
        crate::services::mcp::types::ScopedMcpServerConfig {
            name: None,
            scope: crate::services::mcp::types::ConfigScope::User,
            transport: crate::services::mcp::types::Transport::Http,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("https://example.com/mcp".to_string()),
            headers: BTreeMap::new(),
            headers_helper: None,
            oauth: Some(serde_json::json!({"clientId":"configured-client"})),
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        }
    }

    #[cfg(feature = "mcp_runtime")]
    fn test_http_xaa_config() -> crate::services::mcp::types::ScopedMcpServerConfig {
        let mut config = test_http_config();
        config.oauth = Some(serde_json::json!({"clientId":"configured-client", "xaa": true}));
        config
    }

    #[cfg(feature = "mcp_runtime")]
    fn with_temp_config_home<T>(body: impl FnOnce(std::path::PathBuf) -> T) -> T {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-mcp-oauth-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &temp_dir);
        let result = body(temp_dir.clone());
        let _ = std::fs::remove_dir_all(temp_dir);
        result
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn mcp_client_secret_helpers_are_default_closed_without_changing_bytes() {
        with_temp_config_home(|home| {
            let credentials_path = home.join(".credentials.json");
            let original = br#"{"mcpOAuthClientConfig":{},"other":"preserved"}"#;
            std::fs::write(&credentials_path, original).unwrap();
            let error = save_mcp_client_secret("docs", &test_http_config(), "secret-value")
                .expect_err("MCP OAuth client-secret write must be default-closed");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );
            assert_eq!(std::fs::read(&credentials_path).unwrap(), original);

            let error = clear_mcp_client_config("docs", &test_http_config())
                .expect_err("MCP OAuth client-secret delete must be default-closed");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );
            assert_eq!(std::fs::read(&credentials_path).unwrap(), original);
        });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn preconfigured_oauth_client_config_uses_readonly_saved_secret_for_token_exchange() {
        with_temp_config_home(|home| {
            let server_key =
                get_server_key("docs", "http", "https://example.com/mcp", &BTreeMap::new());
            std::fs::write(
                home.join(".credentials.json"),
                serde_json::json!({
                    "mcpOAuthClientConfig": {
                        server_key.clone(): {"clientSecret": "secret-value"}
                    }
                })
                .to_string(),
            )
            .unwrap();
            let client_config = runtime::preconfigured_oauth_client_config(
                "configured-client",
                "http://localhost:3118/callback",
                &["read".to_string(), "write".to_string()],
                &server_key,
            );
            assert_eq!(client_config.client_id, "configured-client");
            assert_eq!(client_config.client_secret.as_deref(), Some("secret-value"));
            assert_eq!(client_config.scopes, vec!["read", "write"]);
        });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn perform_mcp_oauth_flow_xaa_disabled_fails_without_standard_oauth_fallback() {
        with_temp_config_home(|_| {
            let _xaa = EnvVarGuard::unset("CLAUDE_CODE_ENABLE_XAA");
            let error = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(perform_mcp_oauth_flow(
                    "docs",
                    &test_http_xaa_config(),
                    McpOAuthFlowOptions::default(),
                ))
                .expect_err("XAA config must not fall back to normal OAuth when gate is off");
            assert!(
                error.to_string().contains("XAA is not enabled"),
                "error={error}"
            );
        });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn claude_auth_provider_tokens_uses_active_cached_token_without_refresh_probe() {
        let token = with_temp_config_home(|home| {
            let server_key =
                get_server_key("docs", "http", "https://example.com/mcp", &BTreeMap::new());
            std::fs::write(
                home.join(".credentials.json"),
                serde_json::json!({
                    "mcpOAuth": {
                        server_key: {
                            "serverName": "docs",
                            "serverUrl": "https://example.com/mcp",
                            "accessToken": "active-token",
                            "expiresAt": now_ms() + 10 * 60 * 1000,
                            "clientId": "stored-client"
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();
            ClaudeAuthProvider::new("docs", &test_http_config())
                .tokens()
                .map(|tokens| tokens.and_then(|tokens| tokens.access_token))
        })
        .unwrap();

        assert_eq!(token.as_deref(), Some("active-token"));
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn claude_auth_provider_tokens_returns_none_for_expired_token_without_refresh() {
        let token = with_temp_config_home(|home| {
            let server_key =
                get_server_key("docs", "http", "https://example.com/mcp", &BTreeMap::new());
            std::fs::write(
                home.join(".credentials.json"),
                serde_json::json!({
                    "mcpOAuth": {
                        server_key: {
                            "serverName": "docs",
                            "serverUrl": "https://example.com/mcp",
                            "accessToken": "expired-token",
                            "expiresAt": now_ms().saturating_sub(1000),
                            "clientId": "stored-client"
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();
            ClaudeAuthProvider::new("docs", &test_http_config())
                .tokens()
                .map(|tokens| tokens.and_then(|tokens| tokens.access_token))
        })
        .unwrap();

        assert_eq!(token, None);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn blocked_www_authenticate_cache_write_returns_unavailable_and_preserves_bytes() {
        with_temp_config_home(|home| {
            let credentials_path = home.join(".credentials.json");
            let original = br#"{"other":"preserved"}"#;
            std::fs::write(&credentials_path, original).unwrap();
            let error = record_mcp_www_authenticate_challenge(
                "docs",
                &test_http_config(),
                "http",
                "https://example.com/mcp",
                r#"Bearer error="insufficient_scope", scope="admin write", resource_metadata="/.well-known/oauth-protected-resource/mcp""#,
                None,
            )
            .expect_err("credential-related challenge cache write must be closed");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );
            assert_eq!(std::fs::read(&credentials_path).unwrap(), original);
        });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn blocked_mcp_token_delete_preserves_step_up_entry_bytes() {
        with_temp_config_home(|home| {
            let server_key =
                get_server_key("docs", "http", "https://example.com/mcp", &BTreeMap::new());
            let credentials_path = home.join(".credentials.json");
            let original = serde_json::json!({
                "mcpOAuth": {
                    server_key.clone(): {
                        "serverName": "docs",
                        "serverUrl": "https://example.com/mcp",
                        "accessToken": "",
                        "expiresAt": 0,
                        "stepUpScope": "admin write",
                        "discoveryState": {
                            "authorizationServerUrl": "https://auth.example.com",
                            "resourceMetadataUrl": "https://example.com/.well-known/oauth-protected-resource/mcp",
                            "authorizationServerMetadata": {"large": "legacy blob stripped"}
                        }
                    }
                }
            })
            .to_string();
            std::fs::write(&credentials_path, &original).unwrap();
            let error = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(revoke_server_tokens("docs", &test_http_config(), true))
                .expect_err("MCP OAuth token deletion must be default-closed");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );
            assert_eq!(
                std::fs::read_to_string(&credentials_path).unwrap(),
                original
            );
        });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn cached_resource_metadata_resource_validation_matches_rmcp_mixup_guard() {
        assert!(runtime::resource_identifiers_match(
            "https://example.com/",
            "https://example.com"
        ));
        assert!(!runtime::resource_identifiers_match(
            "https://example.com/mcp",
            "https://evil.example/mcp"
        ));
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn cached_resource_metadata_auth_server_url_rejects_private_hosts_like_rmcp() {
        assert_eq!(
            runtime::allowed_authorization_server_metadata_url("https://auth.example.com"),
            Some("https://auth.example.com/".to_string())
        );
        assert_eq!(
            runtime::allowed_authorization_server_metadata_url("http://127.0.0.1:8080"),
            None
        );
        assert_eq!(
            runtime::allowed_authorization_server_metadata_url("https://metadata.google.internal"),
            None
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn cached_reauth_state_reads_step_up_scope_and_resource_metadata_like_official() {
        let entry = serde_json::json!({
            "stepUpScope": "admin write",
            "discoveryState": {
                "resourceMetadataUrl": "https://example.com/.well-known/oauth-protected-resource/mcp"
            }
        });
        let cached = runtime::cached_reauth_state_from_entry(Some(&entry));
        assert_eq!(cached.step_up_scope.as_deref(), Some("admin write"));
        assert_eq!(
            cached.resource_metadata_url.as_deref(),
            Some("https://example.com/.well-known/oauth-protected-resource/mcp")
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn oauth_token_response_from_snapshot_preserves_refresh_token_for_rmcp_refresh() {
        let snapshot = runtime::McpOAuthTokenSnapshot {
            access_token: Some(String::new()),
            refresh_token: Some("refresh-token".to_string()),
            expires_in_secs: 0,
            scope: Some("read write".to_string()),
            client_id: Some("client".to_string()),
            client_secret: None,
        };
        let response = runtime::oauth_token_response_from_snapshot(&snapshot)
            .expect("refresh-only snapshot should deserialize for rmcp");
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(
            value.get("refresh_token").and_then(Value::as_str),
            Some("refresh-token")
        );
        assert_eq!(
            value.get("scope").and_then(Value::as_str),
            Some("read write")
        );
    }
}
