//! XAA token exchange helpers.
//! Maps to: CC `services/mcp/xaa.ts`.
//!
//! XAA (Cross-App Access / SEP-990) is intentionally service-owned: UI and
//! `/mcp` only dispatch auth actions, while this module owns protected-resource
//! discovery, token exchange, and AS JWT-bearer grants. IdP login/cache lives in
//! `xaa_idp_login.rs`, matching CC `services/mcp/xaaIdpLogin.ts`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
const TOKEN_EXCHANGE_GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const ID_JAG_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:id-jag";
const ID_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:id_token";

/// Maps to: CC `services/mcp/xaa.ts#XaaTokenExchangeError`.
#[derive(Debug)]
pub struct XaaTokenExchangeError {
    message: String,
    pub should_clear_id_token: bool,
}

impl XaaTokenExchangeError {
    pub fn new(message: impl Into<String>, should_clear_id_token: bool) -> Self {
        Self {
            message: message.into(),
            should_clear_id_token,
        }
    }
}

impl std::fmt::Display for XaaTokenExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for XaaTokenExchangeError {}

/// Maps to: CC `services/mcp/xaa.ts#ProtectedResourceMetadata`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProtectedResourceMetadata {
    pub resource: String,
    pub authorization_servers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawProtectedResourceMetadata {
    resource: Option<String>,
    authorization_server: Option<String>,
    authorization_servers: Option<Vec<String>>,
}

/// Maps to: CC `services/mcp/xaa.ts#AuthorizationServerMetadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XaaAuthorizationServerMetadata {
    pub issuer: String,
    pub token_endpoint: String,
    pub grant_types_supported: Option<Vec<String>>,
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// Maps to: CC `services/mcp/xaa.ts#JwtAuthGrantResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtAuthGrantResult {
    pub jwt_auth_grant: String,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
}

/// Maps to: CC `services/mcp/xaa.ts#XaaTokenResult` / `XaaResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XaaResult {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
    pub refresh_token: Option<String>,
    pub authorization_server_url: String,
}

/// Maps to: CC `services/mcp/xaa.ts#XaaConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XaaConfig {
    pub client_id: String,
    pub client_secret: String,
    pub idp_client_id: String,
    pub idp_client_secret: Option<String>,
    pub idp_id_token: String,
    pub idp_token_endpoint: String,
}

#[cfg(feature = "mcp_runtime")]
mod runtime {
    use super::*;
    use base64::Engine as _;
    use rmcp::transport::auth::AuthorizationManager;

    const XAA_REQUEST_TIMEOUT_SECS: u64 = 30;

    /// CC `makeXaaFetch` (`xaa.ts:42-54`) wraps the global `fetch`, whose
    /// dispatcher carries the proxy and TLS options.
    fn http_client() -> anyhow::Result<reqwest::Client> {
        Ok(crate::utils::proxy::get_proxy_fetch_options(false)?
            .timeout(std::time::Duration::from_secs(XAA_REQUEST_TIMEOUT_SECS))
            .build()?)
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

    fn basic_auth_value(client_id: &str, client_secret: &str) -> String {
        // CC encodes each side with encodeURIComponent before base64.
        let source = format!(
            "{}:{}",
            form_percent_encode(client_id).replace('+', "%20"),
            form_percent_encode(client_secret).replace('+', "%20")
        );
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(source)
        )
    }

    /// Maps to: CC `services/mcp/xaa.ts:61-67` `normalizeUrl`.
    fn normalize_url(value: &str) -> String {
        reqwest::Url::parse(value)
            .map(|mut url| {
                let path = url.path().trim_end_matches('/').to_string();
                url.set_path(&path);
                url.to_string().trim_end_matches('/').to_string()
            })
            .unwrap_or_else(|_| value.trim_end_matches('/').to_string())
    }

    fn protected_resource_well_known_urls(server_url: &str) -> anyhow::Result<Vec<reqwest::Url>> {
        let url = reqwest::Url::parse(server_url)?;
        let mut urls = Vec::new();
        let mut root = url.clone();
        root.set_path("/.well-known/oauth-protected-resource");
        root.set_query(None);
        urls.push(root);

        let trimmed = url.path().trim_matches('/').to_string();
        if !trimmed.is_empty() {
            let mut path_url = url;
            path_url.set_path(&format!("/{trimmed}/.well-known/oauth-protected-resource"));
            path_url.set_query(None);
            if !urls.iter().any(|candidate| candidate == &path_url) {
                urls.push(path_url);
            }
        }
        Ok(urls)
    }

    fn parse_www_authenticate_resource_metadata(header: &str) -> Option<String> {
        let lower = header.to_ascii_lowercase();
        let key = "resource_metadata=";
        let pos = lower.find(key)? + key.len();
        let rest = &header[pos..];
        if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"')?;
            Some(stripped[..end].to_string())
        } else {
            let end = rest
                .find(|ch: char| ch == ',' || ch.is_whitespace())
                .unwrap_or(rest.len());
            Some(rest[..end].trim().to_string()).filter(|value| !value.is_empty())
        }
    }

    async fn fetch_resource_metadata_url_from_challenge(
        client: &reqwest::Client,
        server_url: &str,
    ) -> anyhow::Result<Option<reqwest::Url>> {
        let request = client.get(server_url);
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        let Some(header) = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(None);
        };
        let Some(value) = parse_www_authenticate_resource_metadata(header) else {
            return Ok(None);
        };
        let base = reqwest::Url::parse(server_url)?;
        Ok(reqwest::Url::parse(&value)
            .or_else(|_| base.join(&value))
            .ok())
    }

    async fn fetch_prm_from_url(
        client: &reqwest::Client,
        url: &reqwest::Url,
    ) -> anyhow::Result<Option<RawProtectedResourceMetadata>> {
        let request = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "application/json");
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        Ok(Some(response.json::<RawProtectedResourceMetadata>().await?))
    }

    fn normalize_prm(
        server_url: &str,
        prm: RawProtectedResourceMetadata,
    ) -> anyhow::Result<ProtectedResourceMetadata> {
        let resource = prm
            .resource
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "XAA: PRM discovery failed: PRM missing resource or authorization_servers"
                )
            })?;
        let mut authorization_servers = prm.authorization_servers.unwrap_or_default();
        if let Some(single) = prm.authorization_server {
            if !authorization_servers.iter().any(|value| value == &single) {
                authorization_servers.insert(0, single);
            }
        }
        authorization_servers.retain(|value| !value.trim().is_empty());
        if authorization_servers.is_empty() {
            anyhow::bail!(
                "XAA: PRM discovery failed: PRM missing resource or authorization_servers"
            );
        }
        if normalize_url(&resource) != normalize_url(server_url) {
            anyhow::bail!(
                "XAA: PRM discovery failed: PRM resource mismatch: expected {server_url}, got {resource}"
            );
        }
        Ok(ProtectedResourceMetadata {
            resource,
            authorization_servers,
        })
    }

    /// Maps to: CC `services/mcp/xaa.ts#discoverProtectedResource`.
    pub async fn discover_protected_resource(
        server_url: &str,
    ) -> anyhow::Result<ProtectedResourceMetadata> {
        let client = http_client()?;
        if let Some(resource_metadata_url) =
            fetch_resource_metadata_url_from_challenge(&client, server_url).await?
        {
            if let Some(prm) = fetch_prm_from_url(&client, &resource_metadata_url).await? {
                return normalize_prm(server_url, prm);
            }
        }
        for url in protected_resource_well_known_urls(server_url)? {
            if let Some(prm) = fetch_prm_from_url(&client, &url).await? {
                return normalize_prm(server_url, prm);
            }
        }
        anyhow::bail!("XAA: PRM discovery failed: no protected resource metadata found")
    }

    fn metadata_string_array(
        metadata: &rmcp::transport::auth::AuthorizationMetadata,
        key: &str,
    ) -> Option<Vec<String>> {
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
            .filter(|values| !values.is_empty())
    }

    /// Maps to: CC `services/mcp/xaa.ts#discoverAuthorizationServer`.
    pub async fn discover_authorization_server(
        as_url: &str,
    ) -> anyhow::Result<XaaAuthorizationServerMetadata> {
        // CC passes `makeXaaFetch` as `fetchFn` (`xaa.ts:178-210`, `:183`):
        // the global `fetch`.
        let manager = AuthorizationManager::new_with_oauth_http_client(
            as_url.to_string(),
            crate::services::mcp::auth::oauth_http_client()?,
        )
        .await?;
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let metadata = manager.discover_metadata().await?;
        let issuer = metadata
            .issuer
            .clone()
            .filter(|issuer| !issuer.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("XAA: AS metadata discovery failed: no valid metadata at {as_url}")
            })?;
        if metadata.token_endpoint.trim().is_empty() {
            anyhow::bail!("XAA: AS metadata discovery failed: no valid metadata at {as_url}");
        }
        if normalize_url(&issuer) != normalize_url(as_url) {
            anyhow::bail!(
                "XAA: AS metadata discovery failed: issuer mismatch: expected {as_url}, got {issuer}"
            );
        }
        let token_url = reqwest::Url::parse(&metadata.token_endpoint)?;
        if token_url.scheme() != "https" {
            anyhow::bail!(
                "XAA: refusing non-HTTPS token endpoint: {}",
                metadata.token_endpoint
            );
        }
        let grant_types_supported = metadata_string_array(&metadata, "grant_types_supported");
        let token_endpoint_auth_methods_supported =
            metadata_string_array(&metadata, "token_endpoint_auth_methods_supported");
        Ok(XaaAuthorizationServerMetadata {
            issuer,
            token_endpoint: metadata.token_endpoint,
            grant_types_supported,
            token_endpoint_auth_methods_supported,
        })
    }

    /// Maps to: CC `services/mcp/xaa.ts:94-97` `redactTokens`.
    fn redact_tokens(raw: &str) -> String {
        let sensitive = [
            "access_token",
            "refresh_token",
            "id_token",
            "assertion",
            "subject_token",
            "client_secret",
        ];
        let mut value =
            serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
        redact_tokens_value(&mut value, &sensitive);
        match value {
            Value::String(s) => s,
            other => {
                serde_json::to_string(&other).unwrap_or_else(|_| "[unserializable]".to_string())
            }
        }
    }

    fn redact_tokens_value(value: &mut Value, sensitive: &[&str]) {
        match value {
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    if sensitive.iter().any(|candidate| candidate == key) {
                        *value = Value::String("[REDACTED]".to_string());
                    } else {
                        redact_tokens_value(value, sensitive);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    redact_tokens_value(value, sensitive);
                }
            }
            _ => {}
        }
    }

    /// Maps to: CC `services/mcp/xaa.ts#requestJwtAuthorizationGrant`.
    pub async fn request_jwt_authorization_grant(
        token_endpoint: &str,
        audience: &str,
        resource: &str,
        id_token: &str,
        client_id: &str,
        client_secret: Option<&str>,
        scope: Option<&str>,
    ) -> anyhow::Result<JwtAuthGrantResult> {
        let mut params = vec![
            ("grant_type".to_string(), TOKEN_EXCHANGE_GRANT.to_string()),
            (
                "requested_token_type".to_string(),
                ID_JAG_TOKEN_TYPE.to_string(),
            ),
            ("audience".to_string(), audience.to_string()),
            ("resource".to_string(), resource.to_string()),
            ("subject_token".to_string(), id_token.to_string()),
            ("subject_token_type".to_string(), ID_TOKEN_TYPE.to_string()),
            ("client_id".to_string(), client_id.to_string()),
        ];
        if let Some(secret) = client_secret {
            params.push(("client_secret".to_string(), secret.to_string()));
        }
        if let Some(scope) = scope {
            params.push(("scope".to_string(), scope.to_string()));
        }

        let request = http_client()?
            .post(token_endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form_urlencoded_body(&params));
        // Deviation (L2, user-authorized OAuth safety gate): CC
        // `services/mcp/xaa.ts:233-318` sends the fully prepared ID-token
        // exchange. Cometix rejects at the final credential HTTP outlet.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let should_clear = response.status().as_u16() < 500;
            let status = response.status();
            let body = redact_tokens(&response.text().await.unwrap_or_default())
                .chars()
                .take(200)
                .collect::<String>();
            return Err(XaaTokenExchangeError::new(
                format!("XAA: token exchange failed: HTTP {status}: {body}"),
                should_clear,
            )
            .into());
        }
        let raw = response.json::<Value>().await.map_err(|_| {
            XaaTokenExchangeError::new(
                format!(
                    "XAA: token exchange returned non-JSON (captive portal?) at {token_endpoint}"
                ),
                false,
            )
        })?;
        let access_token = raw
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                XaaTokenExchangeError::new(
                    format!(
                        "XAA: token exchange response missing access_token: {}",
                        redact_tokens(&raw.to_string())
                    ),
                    true,
                )
            })?;
        if raw.get("issued_token_type").and_then(Value::as_str) != Some(ID_JAG_TOKEN_TYPE) {
            return Err(XaaTokenExchangeError::new(
                format!(
                    "XAA: token exchange returned unexpected issued_token_type: {}",
                    raw.get("issued_token_type")
                        .and_then(Value::as_str)
                        .unwrap_or("undefined")
                ),
                true,
            )
            .into());
        }
        Ok(JwtAuthGrantResult {
            jwt_auth_grant: access_token.to_string(),
            expires_in: raw.get("expires_in").and_then(Value::as_u64),
            scope: raw
                .get("scope")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        })
    }

    /// Maps to: CC `services/mcp/xaa.ts#exchangeJwtAuthGrant`.
    pub async fn exchange_jwt_auth_grant(
        token_endpoint: &str,
        assertion: &str,
        client_id: &str,
        client_secret: &str,
        auth_method: &str,
        scope: Option<&str>,
    ) -> anyhow::Result<XaaResult> {
        let mut params = vec![
            ("grant_type".to_string(), JWT_BEARER_GRANT.to_string()),
            ("assertion".to_string(), assertion.to_string()),
        ];
        if let Some(scope) = scope {
            params.push(("scope".to_string(), scope.to_string()));
        }
        let mut request = http_client()?.post(token_endpoint).header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        );
        if auth_method == "client_secret_post" {
            params.push(("client_id".to_string(), client_id.to_string()));
            params.push(("client_secret".to_string(), client_secret.to_string()));
        } else {
            request = request.header(
                reqwest::header::AUTHORIZATION,
                basic_auth_value(client_id, client_secret),
            );
        }
        let request = request.body(form_urlencoded_body(&params));
        // Deviation (L2, user-authorized OAuth safety gate): CC
        // `services/mcp/xaa.ts:337-405` sends the prepared ID-JAG bearer
        // grant. Cometix rejects at the final credential HTTP outlet.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = redact_tokens(&response.text().await.unwrap_or_default())
                .chars()
                .take(200)
                .collect::<String>();
            anyhow::bail!("XAA: jwt-bearer grant failed: HTTP {status}: {body}");
        }
        let raw = response.json::<Value>().await.map_err(|_| {
            anyhow::anyhow!(
                "XAA: jwt-bearer grant returned non-JSON (captive portal?) at {token_endpoint}"
            )
        })?;
        let access_token = raw
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "XAA: jwt-bearer response did not match expected shape: {}",
                    redact_tokens(&raw.to_string())
                )
            })?;
        Ok(XaaResult {
            access_token: access_token.to_string(),
            token_type: raw
                .get("token_type")
                .and_then(Value::as_str)
                .unwrap_or("Bearer")
                .to_string(),
            expires_in: raw.get("expires_in").and_then(Value::as_u64),
            scope: raw
                .get("scope")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            refresh_token: raw
                .get("refresh_token")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            authorization_server_url: String::new(),
        })
    }

    /// Maps to: CC `services/mcp/xaa.ts#performCrossAppAccess`.
    pub async fn perform_cross_app_access(
        server_url: &str,
        config: XaaConfig,
        server_name: &str,
    ) -> anyhow::Result<XaaResult> {
        tracing::debug!(
            server = server_name,
            url = server_url,
            "XAA: discovering PRM"
        );
        let prm = discover_protected_resource(server_url).await?;
        let mut as_errors = Vec::new();
        let mut selected = None;
        for as_url in &prm.authorization_servers {
            match discover_authorization_server(as_url).await {
                Ok(candidate) => {
                    if candidate
                        .grant_types_supported
                        .as_ref()
                        .is_some_and(|grants| !grants.iter().any(|grant| grant == JWT_BEARER_GRANT))
                    {
                        as_errors.push(format!(
                            "{as_url}: does not advertise jwt-bearer grant (supported: {})",
                            candidate
                                .grant_types_supported
                                .unwrap_or_default()
                                .join(", ")
                        ));
                        continue;
                    }
                    selected = Some(candidate);
                    break;
                }
                Err(error) => as_errors.push(format!("{as_url}: {error}")),
            }
        }
        let as_meta = selected.ok_or_else(|| {
            anyhow::anyhow!(
                "XAA: no authorization server supports jwt-bearer. Tried: {}",
                as_errors.join("; ")
            )
        })?;
        let auth_method = if as_meta
            .token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|methods| {
                !methods.iter().any(|method| method == "client_secret_basic")
                    && methods.iter().any(|method| method == "client_secret_post")
            }) {
            "client_secret_post"
        } else {
            "client_secret_basic"
        };
        let jag = request_jwt_authorization_grant(
            &config.idp_token_endpoint,
            &as_meta.issuer,
            &prm.resource,
            &config.idp_id_token,
            &config.idp_client_id,
            config.idp_client_secret.as_deref(),
            None,
        )
        .await?;
        let mut tokens = exchange_jwt_auth_grant(
            &as_meta.token_endpoint,
            &jag.jwt_auth_grant,
            &config.client_id,
            &config.client_secret,
            auth_method,
            None,
        )
        .await?;
        tokens.authorization_server_url = as_meta.issuer;
        Ok(tokens)
    }
}

#[cfg(feature = "mcp_runtime")]
pub use runtime::{
    discover_authorization_server, discover_protected_resource, exchange_jwt_auth_grant,
    perform_cross_app_access, request_jwt_authorization_grant,
};

#[cfg(not(feature = "mcp_runtime"))]
pub async fn perform_cross_app_access(
    _server_url: &str,
    _config: XaaConfig,
    _server_name: &str,
) -> anyhow::Result<XaaResult> {
    anyhow::bail!("mcp_runtime feature is disabled; MCP XAA is not compiled")
}

#[cfg(all(test, feature = "mcp_runtime"))]
mod tests {
    use super::*;

    fn unaccepted_endpoint() -> (std::net::TcpListener, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        (listener, endpoint)
    }

    fn assert_no_connection(listener: &std::net::TcpListener) {
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[tokio::test]
    async fn xaa_token_exchange_outlets_use_canonical_default_closed_gate() {
        crate::utils::tls_provider::install_crypto_provider();
        let (listener, endpoint) = unaccepted_endpoint();
        let error = request_jwt_authorization_grant(
            &endpoint,
            "audience",
            "resource",
            "id-token",
            "idp-client",
            Some("idp-secret"),
            Some("openid"),
        )
        .await
        .expect_err("ID-token exchange must be default-closed");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        assert_no_connection(&listener);

        let (listener, endpoint) = unaccepted_endpoint();
        let error = exchange_jwt_auth_grant(
            &endpoint,
            "id-jag",
            "as-client",
            "as-secret",
            "client_secret_post",
            Some("read write"),
        )
        .await
        .expect_err("JWT bearer exchange must be default-closed");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        assert_no_connection(&listener);
    }
}
