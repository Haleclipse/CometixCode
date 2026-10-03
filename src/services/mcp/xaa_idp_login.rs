//! XAA IdP login helpers.
//! Maps to: CC `services/mcp/xaaIdpLogin.ts`.
//!
//! This module owns the enterprise IdP OIDC login/cache boundary. The XAA
//! token-exchange chain itself lives in `xaa.rs`, matching CC's split between
//! `xaaIdpLogin.ts` and `xaa.ts`.

use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

const CLAUDE_CODE_ENABLE_XAA: &str = "CLAUDE_CODE_ENABLE_XAA";
const ID_TOKEN_EXPIRY_BUFFER_S: i64 = 60;
#[cfg(feature = "mcp_runtime")]
const IDP_LOGIN_TIMEOUT_SECS: u64 = 5 * 60;
#[cfg(feature = "mcp_runtime")]
const IDP_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Maps to: CC `services/mcp/xaaIdpLogin.ts#XaaIdpSettings`.
pub type XaaIdpSettings = crate::utils::settings::types::XaaIdpSettings;

/// Maps to: CC `services/mcp/xaaIdpLogin.ts#IdpLoginOptions`.
pub type XaaAuthorizationUrlCallback = Arc<dyn Fn(String) + Send + Sync>;

/// Maps to: CC `services/mcp/xaaIdpLogin.ts#IdpLoginOptions`.
#[derive(Clone, Default)]
pub struct IdpLoginOptions {
    pub idp_issuer: String,
    pub idp_client_id: String,
    pub idp_client_secret: Option<String>,
    pub callback_port: Option<u16>,
    pub on_authorization_url: Option<XaaAuthorizationUrlCallback>,
    pub skip_browser_open: bool,
}

impl std::fmt::Debug for IdpLoginOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdpLoginOptions")
            .field("idp_issuer", &self.idp_issuer)
            .field("idp_client_id", &self.idp_client_id)
            .field(
                "idp_client_secret",
                &self.idp_client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("callback_port", &self.callback_port)
            .field(
                "on_authorization_url",
                &self.on_authorization_url.as_ref().map(|_| "[callback]"),
            )
            .field("skip_browser_open", &self.skip_browser_open)
            .finish()
    }
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts#discoverOidc()` return shape.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OidcMetadata {
    pub issuer: Option<String>,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: String,
    pub response_types_supported: Option<Vec<String>>,
    pub code_challenge_methods_supported: Option<Vec<String>>,
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts#isXaaEnabled`.
pub fn is_xaa_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var(CLAUDE_CODE_ENABLE_XAA).as_deref())
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts#getXaaIdpSettings`.
pub fn get_xaa_idp_settings() -> Option<XaaIdpSettings> {
    crate::utils::settings::get_initial_settings().xaa_idp
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts#issuerKey`.
pub fn issuer_key(issuer: &str) -> String {
    #[cfg(feature = "mcp_runtime")]
    {
        if let Ok(mut url) = reqwest::Url::parse(issuer) {
            let path = url.path().trim_end_matches('/').to_string();
            url.set_path(&path);
            if let Some(host) = url.host_str().map(|host| host.to_ascii_lowercase()) {
                let _ = url.set_host(Some(&host));
            }
            return url.to_string();
        }
    }
    issuer.trim_end_matches('/').to_string()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:99-107` `getCachedIdpIdToken`.
pub fn get_cached_idp_id_token(idp_issuer: &str) -> Option<String> {
    let data = crate::utils::secure_storage::get_secure_storage()
        .read()
        .unwrap_or_else(|| serde_json::json!({}));
    let key = issuer_key(idp_issuer);
    let entry = data.get("mcpXaaIdp")?.get(&key)?;
    let expires_at = entry.get("expiresAt")?.as_i64()?;
    if expires_at - now_ms() <= ID_TOKEN_EXPIRY_BUFFER_S * 1000 {
        return None;
    }
    entry
        .get("idToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:109-131` `saveIdpIdToken`.
fn save_idp_id_token(idp_issuer: &str, id_token: &str, expires_at: i64) -> anyhow::Result<()> {
    let storage = crate::utils::secure_storage::get_secure_storage();
    let mut data = storage.read().unwrap_or_else(|| serde_json::json!({}));
    if !data.is_object() {
        data = serde_json::json!({});
    }
    let root = data.as_object_mut().expect("object checked above");
    let slot = root
        .entry("mcpXaaIdp".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !slot.is_object() {
        *slot = serde_json::json!({});
    }
    slot.as_object_mut().expect("mcpXaaIdp object").insert(
        issuer_key(idp_issuer),
        serde_json::json!({ "idToken": id_token, "expiresAt": expires_at }),
    );
    let status = storage.update(&data)?;
    if status.success {
        Ok(())
    } else {
        anyhow::bail!("Failed to save XAA IdP token")
    }
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:133-141` `saveIdpIdTokenFromJwt`.
pub fn save_idp_id_token_from_jwt(idp_issuer: &str, id_token: &str) -> anyhow::Result<i64> {
    let expires_at = jwt_exp(id_token)
        .map(|exp| exp * 1000)
        .unwrap_or_else(|| now_ms() + 3600 * 1000);
    save_idp_id_token(idp_issuer, id_token, expires_at)?;
    Ok(expires_at)
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:143-157` `clearIdpIdToken`.
pub fn clear_idp_id_token(idp_issuer: &str) -> anyhow::Result<()> {
    let storage = crate::utils::secure_storage::get_secure_storage();
    let mut data = storage.read().unwrap_or_else(|| serde_json::json!({}));
    let Some(slot) = data.get_mut("mcpXaaIdp").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    slot.remove(&issuer_key(idp_issuer));
    let status = storage.update(&data)?;
    if status.success {
        Ok(())
    } else {
        anyhow::bail!("Failed to clear XAA IdP token")
    }
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:159-175` `saveIdpClientSecret`.
pub fn save_idp_client_secret(idp_issuer: &str, client_secret: &str) -> anyhow::Result<()> {
    let storage = crate::utils::secure_storage::get_secure_storage();
    let mut data = storage.read().unwrap_or_else(|| serde_json::json!({}));
    if !data.is_object() {
        data = serde_json::json!({});
    }
    let root = data.as_object_mut().expect("object checked above");
    let slot = root
        .entry("mcpXaaIdpConfig".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !slot.is_object() {
        *slot = serde_json::json!({});
    }
    slot.as_object_mut()
        .expect("mcpXaaIdpConfig object")
        .insert(
            issuer_key(idp_issuer),
            serde_json::json!({ "clientSecret": client_secret }),
        );
    let status = storage.update(&data)?;
    if status.success {
        Ok(())
    } else {
        anyhow::bail!("Failed to save XAA IdP client secret")
    }
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:177-185` `getIdpClientSecret`.
pub fn get_idp_client_secret(idp_issuer: &str) -> Option<String> {
    crate::utils::secure_storage::get_secure_storage()
        .read()?
        .get("mcpXaaIdpConfig")?
        .get(issuer_key(idp_issuer))?
        .get("clientSecret")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(ToOwned::to_owned)
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:187-200` `clearIdpClientSecret`.
pub fn clear_idp_client_secret(idp_issuer: &str) -> anyhow::Result<()> {
    let storage = crate::utils::secure_storage::get_secure_storage();
    let mut data = storage.read().unwrap_or_else(|| serde_json::json!({}));
    let Some(slot) = data
        .get_mut("mcpXaaIdpConfig")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    slot.remove(&issuer_key(idp_issuer));
    let status = storage.update(&data)?;
    if status.success {
        Ok(())
    } else {
        anyhow::bail!("Failed to clear XAA IdP client secret")
    }
}

/// Maps to: CC `services/mcp/xaaIdpLogin.ts:252-270` `jwtExp`.
fn jwt_exp(jwt: &str) -> Option<i64> {
    let mut parts = jwt.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value = serde_json::from_slice::<Value>(&decoded).ok()?;
    value.get("exp")?.as_i64()
}

#[cfg(feature = "mcp_runtime")]
mod runtime {
    use super::*;
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener as TokioTcpListener;

    /// CC's IdP requests (`xaaIdpLogin.ts:208-211,453-465`) use the global
    /// `fetch`, whose dispatcher carries the proxy and TLS options.
    fn http_client() -> anyhow::Result<reqwest::Client> {
        Ok(crate::utils::proxy::get_proxy_fetch_options(false)?
            .timeout(std::time::Duration::from_secs(IDP_REQUEST_TIMEOUT_SECS))
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct IdpAuthorizationStart {
        authorization_url: String,
        code_verifier: String,
        state: String,
    }

    /// Maps to: CC `services/mcp/xaaIdpLogin.ts#startAuthorization(...)` call.
    ///
    /// This intentionally does not use `AuthorizationManager::get_authorization_url`:
    /// rmcp's MCP OAuth helper adds a `resource` parameter for protected MCP
    /// resources, while the XAA IdP OIDC login in CC requests only `openid`.
    fn start_idp_authorization(
        idp_issuer: &str,
        metadata: &OidcMetadata,
        client_id: &str,
        redirect_uri: &str,
        state: Option<String>,
    ) -> anyhow::Result<IdpAuthorizationStart> {
        let authorization_endpoint = metadata
            .authorization_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("XAA IdP: invalid OIDC metadata: missing authorization_endpoint")
            })?;
        if metadata
            .response_types_supported
            .as_ref()
            .is_some_and(|values| !values.iter().any(|value| value == "code"))
        {
            anyhow::bail!("Incompatible auth server: does not support response type code");
        }
        if metadata
            .code_challenge_methods_supported
            .as_ref()
            .is_some_and(|values| !values.iter().any(|value| value == "S256"))
        {
            anyhow::bail!("Incompatible auth server: does not support code challenge method S256");
        }
        let mut url = reqwest::Url::parse(authorization_endpoint)
            .or_else(|_| reqwest::Url::parse(idp_issuer)?.join(authorization_endpoint))?;
        let mut code_verifier_bytes = [0_u8; 32];
        getrandom::fill(&mut code_verifier_bytes)
            .map_err(|error| anyhow::anyhow!("XAA IdP PKCE randomness failed: {error}"))?;
        let code_verifier =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(code_verifier_bytes);
        let state = match state {
            Some(state) => state,
            None => {
                let mut state_bytes = [0_u8; 32];
                getrandom::fill(&mut state_bytes)
                    .map_err(|error| anyhow::anyhow!("XAA IdP state randomness failed: {error}"))?;
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes)
            }
        };
        let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(code_verifier.as_bytes()));
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", client_id);
            query.append_pair("code_challenge", &code_challenge);
            query.append_pair("code_challenge_method", "S256");
            query.append_pair("redirect_uri", redirect_uri);
            query.append_pair("state", &state);
            query.append_pair("scope", "openid");
        }
        Ok(IdpAuthorizationStart {
            authorization_url: url.to_string(),
            code_verifier,
            state,
        })
    }

    fn idp_client_auth_method(
        metadata: &OidcMetadata,
        client_secret: Option<&str>,
    ) -> &'static str {
        let supported = metadata
            .token_endpoint_auth_methods_supported
            .as_deref()
            .unwrap_or_default();
        let has_secret = client_secret.is_some();
        if supported.is_empty() {
            return if has_secret {
                "client_secret_basic"
            } else {
                "none"
            };
        }
        if has_secret
            && supported
                .iter()
                .any(|method| method == "client_secret_basic")
        {
            return "client_secret_basic";
        }
        if has_secret
            && supported
                .iter()
                .any(|method| method == "client_secret_post")
        {
            return "client_secret_post";
        }
        if supported.iter().any(|method| method == "none") {
            return "none";
        }
        if has_secret {
            "client_secret_post"
        } else {
            "none"
        }
    }

    fn idp_basic_auth_value(client_id: &str, client_secret: &str) -> String {
        // Maps to SDK `applyBasicAuth`: btoa(`${clientId}:${clientSecret}`).
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{client_id}:{client_secret}"))
        )
    }

    #[derive(Debug, Deserialize)]
    struct IdpTokenResponse {
        id_token: Option<String>,
        expires_in: Option<u64>,
    }

    /// Maps to: CC `services/mcp/xaaIdpLogin.ts#exchangeAuthorization(...)` call.
    async fn exchange_idp_authorization_code(
        _idp_issuer: &str,
        metadata: &OidcMetadata,
        client_id: &str,
        client_secret: Option<&str>,
        authorization_code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<IdpTokenResponse> {
        let auth_method = idp_client_auth_method(metadata, client_secret);
        let mut params = vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), authorization_code.to_string()),
            ("code_verifier".to_string(), code_verifier.to_string()),
            ("redirect_uri".to_string(), redirect_uri.to_string()),
        ];
        let mut request = http_client()?
            .post(&metadata.token_endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json");
        match auth_method {
            "client_secret_basic" => {
                let secret = client_secret.ok_or_else(|| {
                    anyhow::anyhow!("client_secret_basic authentication requires a client_secret")
                })?;
                request = request.header(
                    reqwest::header::AUTHORIZATION,
                    idp_basic_auth_value(client_id, secret),
                );
            }
            "client_secret_post" => {
                params.push(("client_id".to_string(), client_id.to_string()));
                if let Some(secret) = client_secret {
                    params.push(("client_secret".to_string(), secret.to_string()));
                }
            }
            _ => {
                params.push(("client_id".to_string(), client_id.to_string()));
            }
        }
        let request = request.body(form_urlencoded_body(&params));
        // Deviation (L2, user-authorized OAuth safety gate): CC
        // `services/mcp/xaaIdpLogin.ts:443-454` sends the prepared OIDC code
        // exchange. Cometix rejects at the final credential HTTP outlet.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>();
            anyhow::bail!("XAA IdP: token exchange failed: HTTP {status}: {body}");
        }
        response.json::<IdpTokenResponse>().await.map_err(|_| {
            anyhow::anyhow!(
                "XAA IdP: token exchange returned non-JSON (captive portal?) at {}",
                metadata.token_endpoint
            )
        })
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct IdpCallback {
        code: String,
        state: String,
    }

    fn html_escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
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

    fn parse_idp_callback_target(target: &str) -> anyhow::Result<IdpCallback> {
        let parsed = reqwest::Url::parse(&format!("http://localhost{target}"))?;
        if parsed.path() != "/callback" {
            anyhow::bail!("ignoring non-callback XAA IdP request");
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
            anyhow::bail!(
                "XAA IdP: {error}{}",
                if description.is_empty() {
                    String::new()
                } else {
                    format!(" — {description}")
                }
            );
        }
        let code = parsed
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("XAA IdP: callback missing code"))?;
        let state = parsed
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("XAA IdP: callback missing state"))?;
        Ok(IdpCallback { code, state })
    }

    /// Maps to: CC `services/mcp/xaaIdpLogin.ts#waitForCallback`.
    async fn wait_for_callback<F, Fut>(
        port: u16,
        expected_state: &str,
        on_listening: F,
    ) -> anyhow::Result<String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        // Deviation (L2, user-authorized OAuth safety gate): CC
        // `services/mcp/xaaIdpLogin.ts:272-397` binds inside
        // `waitForCallback`; keep the guard at that source-owned final socket
        // outlet, after the caller has prepared metadata, redirect URI, state,
        // PKCE, and the authorization URL.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let listener = TokioTcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AddrInUse {
                    let find_cmd = if cfg!(target_os = "windows") {
                        format!("netstat -ano | findstr :{port}")
                    } else {
                        format!("lsof -ti:{port} -sTCP:LISTEN")
                    };
                    anyhow::anyhow!(
                        "XAA IdP: callback port {port} is already in use. Run `{find_cmd}` to find the holder."
                    )
                } else {
                    anyhow::anyhow!("XAA IdP: callback server failed: {error}")
                }
            })?;
        on_listening().await?;
        let callback = async {
            loop {
                let (mut stream, _) = listener.accept().await?;
                let mut buffer = vec![0_u8; 8192];
                let read = stream.read(&mut buffer).await?;
                let request = String::from_utf8_lossy(&buffer[..read]);
                let Some(request_line) = request.lines().next() else {
                    continue;
                };
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default();
                let target = parts.next().unwrap_or_default();
                if method != "GET" {
                    write_html_response(
                        &mut stream,
                        "405 Method Not Allowed",
                        "<html><body><h3>Unsupported method</h3></body></html>",
                    )
                    .await?;
                    continue;
                }
                match parse_idp_callback_target(target) {
                    Ok(callback) if callback.state == expected_state => {
                        write_html_response(
                            &mut stream,
                            "200 OK",
                            "<html><body><h3>IdP login complete — you can close this window.</h3></body></html>",
                        )
                        .await?;
                        return Ok(callback.code);
                    }
                    Ok(_) => {
                        write_html_response(
                            &mut stream,
                            "400 Bad Request",
                            "<html><body><h3>State mismatch</h3></body></html>",
                        )
                        .await?;
                        anyhow::bail!("XAA IdP: state mismatch (possible CSRF)");
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let body = if message.starts_with("XAA IdP:") {
                            format!(
                                "<html><body><h3>IdP login failed</h3><p>{}</p></body></html>",
                                html_escape(&message)
                            )
                        } else {
                            format!(
                                "<html><body><h3>{}</h3></body></html>",
                                html_escape(&message)
                            )
                        };
                        write_html_response(&mut stream, "400 Bad Request", &body).await?;
                        if message.starts_with("ignoring non-callback") {
                            continue;
                        }
                        return Err(error);
                    }
                }
            }
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(IDP_LOGIN_TIMEOUT_SECS),
            callback,
        )
        .await
        .map_err(|_| anyhow::anyhow!("XAA IdP: login timed out"))?
    }

    /// Maps to: CC `services/mcp/xaaIdpLogin.ts#acquireIdpIdToken`.
    pub async fn acquire_idp_id_token(opts: IdpLoginOptions) -> anyhow::Result<String> {
        if let Some(cached) = get_cached_idp_id_token(&opts.idp_issuer) {
            return Ok(cached);
        }
        let metadata = discover_oidc(&opts.idp_issuer).await?;
        let port = match opts.callback_port {
            Some(port) => port,
            None => crate::services::mcp::oauth_port::find_available_port().await?,
        };
        let redirect_uri = crate::services::mcp::oauth_port::build_redirect_uri(Some(port));
        let auth = start_idp_authorization(
            &opts.idp_issuer,
            &metadata,
            &opts.idp_client_id,
            &redirect_uri,
            None,
        )?;
        let authorization_code = wait_for_callback(port, &auth.state, || async {
            if let Some(on_authorization_url) = &opts.on_authorization_url {
                on_authorization_url(auth.authorization_url.clone());
            }
            if !opts.skip_browser_open {
                let _ = crate::utils::browser::open_browser(&auth.authorization_url).await?;
            }
            Ok(())
        })
        .await?;
        let tokens = exchange_idp_authorization_code(
            &opts.idp_issuer,
            &metadata,
            &opts.idp_client_id,
            opts.idp_client_secret.as_deref(),
            &authorization_code,
            &auth.code_verifier,
            &redirect_uri,
        )
        .await?;
        let id_token = tokens
            .id_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("XAA IdP: token response missing id_token (check scope=openid)")
            })?;
        let expires_at = jwt_exp(id_token)
            .map(|exp| exp * 1000)
            .unwrap_or_else(|| now_ms() + tokens.expires_in.unwrap_or(3600) as i64 * 1000);
        save_idp_id_token(&opts.idp_issuer, id_token, expires_at)?;
        Ok(id_token.to_string())
    }

    /// Maps to: CC `services/mcp/xaaIdpLogin.ts#discoverOidc`.
    pub async fn discover_oidc(idp_issuer: &str) -> anyhow::Result<OidcMetadata> {
        let base = if idp_issuer.ends_with('/') {
            idp_issuer.to_string()
        } else {
            format!("{idp_issuer}/")
        };
        let url = reqwest::Url::parse(&base)?.join(".well-known/openid-configuration")?;
        let request = http_client()?
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "application/json");
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "XAA IdP: OIDC discovery failed: HTTP {} at {url}",
                response.status()
            );
        }
        let metadata = response.json::<OidcMetadata>().await.map_err(|_| {
            anyhow::anyhow!(
                "XAA IdP: OIDC discovery returned non-JSON at {url} (captive portal or proxy?)"
            )
        })?;
        let token_url = reqwest::Url::parse(&metadata.token_endpoint)?;
        if token_url.scheme() != "https" {
            anyhow::bail!(
                "XAA IdP: refusing non-HTTPS token endpoint: {}",
                metadata.token_endpoint
            );
        }
        Ok(metadata)
    }

    #[cfg(test)]
    mod runtime_tests {
        use super::*;

        fn oidc_metadata() -> OidcMetadata {
            OidcMetadata {
                issuer: Some("https://idp.example.com".to_string()),
                authorization_endpoint: Some("https://idp.example.com/authorize".to_string()),
                token_endpoint: "https://idp.example.com/token".to_string(),
                response_types_supported: Some(vec!["code".to_string()]),
                code_challenge_methods_supported: Some(vec!["S256".to_string()]),
                token_endpoint_auth_methods_supported: None,
            }
        }

        #[test]
        fn idp_authorization_url_matches_official_oidc_pkce_shape_without_resource() {
            let auth = start_idp_authorization(
                "https://idp.example.com",
                &oidc_metadata(),
                "client-123",
                "http://localhost:3118/callback",
                Some("state-abc".to_string()),
            )
            .unwrap();
            let url = reqwest::Url::parse(&auth.authorization_url).unwrap();
            let pairs = url
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<std::collections::BTreeMap<_, _>>();

            assert_eq!(
                url.as_str().split('?').next().unwrap(),
                "https://idp.example.com/authorize"
            );
            assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
            assert_eq!(
                pairs.get("client_id").map(String::as_str),
                Some("client-123")
            );
            assert_eq!(
                pairs.get("code_challenge_method").map(String::as_str),
                Some("S256")
            );
            assert_eq!(
                pairs.get("redirect_uri").map(String::as_str),
                Some("http://localhost:3118/callback")
            );
            assert_eq!(pairs.get("state").map(String::as_str), Some("state-abc"));
            assert_eq!(pairs.get("scope").map(String::as_str), Some("openid"));
            assert!(
                !pairs.contains_key("resource"),
                "XAA IdP login must not use MCP resource param"
            );
            assert!(!auth.code_verifier.is_empty());
        }

        #[test]
        fn idp_client_auth_selection_matches_typescript_sdk_priority() {
            let mut metadata = oidc_metadata();
            metadata.token_endpoint_auth_methods_supported = None;
            assert_eq!(
                idp_client_auth_method(&metadata, Some("secret")),
                "client_secret_basic"
            );
            assert_eq!(idp_client_auth_method(&metadata, None), "none");

            metadata.token_endpoint_auth_methods_supported =
                Some(vec!["client_secret_post".to_string()]);
            assert_eq!(
                idp_client_auth_method(&metadata, Some("secret")),
                "client_secret_post"
            );

            metadata.token_endpoint_auth_methods_supported = Some(vec!["none".to_string()]);
            assert_eq!(idp_client_auth_method(&metadata, Some("secret")), "none");
        }

        #[tokio::test]
        async fn xaa_idp_exchange_and_listener_use_canonical_default_closed_gate() {
            crate::utils::tls_provider::install_crypto_provider();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let mut metadata = oidc_metadata();
            metadata.token_endpoint = format!("http://{address}/token");
            let error = exchange_idp_authorization_code(
                "https://idp.example.com",
                &metadata,
                "client",
                Some("secret"),
                "code",
                "verifier",
                "http://localhost:3118/callback",
            )
            .await
            .expect_err("OIDC code exchange must be default-closed");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );
            assert!(matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ));

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let error = wait_for_callback(port, "expected-state", || async { Ok(()) })
                .await
                .expect_err("OAuth callback listener must be default-closed");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );
            let rebound = std::net::TcpListener::bind(("127.0.0.1", port));
            assert!(
                rebound.is_ok(),
                "blocked outlet must leave the port unbound"
            );
        }
    }
}

#[cfg(feature = "mcp_runtime")]
pub use runtime::{acquire_idp_id_token, discover_oidc};

#[cfg(not(feature = "mcp_runtime"))]
pub async fn discover_oidc(_idp_issuer: &str) -> anyhow::Result<OidcMetadata> {
    anyhow::bail!("mcp_runtime feature is disabled; MCP XAA is not compiled")
}

#[cfg(not(feature = "mcp_runtime"))]
pub async fn acquire_idp_id_token(_opts: IdpLoginOptions) -> anyhow::Result<String> {
    anyhow::bail!("mcp_runtime feature is disabled; MCP XAA is not compiled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn with_temp_config_home(test: impl FnOnce(&std::path::Path)) {
        let dir = std::env::temp_dir().join(format!(
            "cometix-xaa-idp-login-test-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        test(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fake_jwt_with_exp(exp: i64) -> String {
        fn b64url(input: &[u8]) -> String {
            const TABLE: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in input.chunks(3) {
                let b0 = chunk[0];
                let b1 = *chunk.get(1).unwrap_or(&0);
                let b2 = *chunk.get(2).unwrap_or(&0);
                out.push(TABLE[(b0 >> 2) as usize] as char);
                out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
                if chunk.len() > 1 {
                    out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
                }
                if chunk.len() > 2 {
                    out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
                }
            }
            out
        }
        format!(
            "{}.{}.sig",
            b64url(br#"{"alg":"none"}"#),
            b64url(format!(r#"{{"exp":{exp}}}"#).as_bytes())
        )
    }

    #[test]
    fn issuer_key_jwt_parsing_and_default_closed_cache_write_match_contract() {
        with_temp_config_home(|home| {
            let future_exp = chrono::Utc::now().timestamp() + 3600;
            let jwt = fake_jwt_with_exp(future_exp);
            let credentials_path = home.join(".credentials.json");
            let original = br#"{"other":"preserved"}"#;
            std::fs::write(&credentials_path, original).unwrap();
            let error = save_idp_id_token_from_jwt("https://IDP.example.com/tenant/", &jwt)
                .expect_err("IdP token cache write must be default-closed");
            assert!(
                error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
            );
            assert_eq!(std::fs::read(&credentials_path).unwrap(), original);
            assert_eq!(jwt_exp(&jwt), Some(future_exp));
            assert_eq!(
                issuer_key("https://IDP.example.com/tenant/"),
                "https://idp.example.com/tenant".to_string()
            );

            std::fs::write(
                &credentials_path,
                serde_json::json!({
                    "mcpXaaIdp": {
                        issuer_key("https://idp.example.com/tenant"): {
                            "idToken": jwt,
                            "expiresAt": future_exp * 1000
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();
            assert_eq!(
                get_cached_idp_id_token("https://idp.example.com/tenant"),
                Some(jwt)
            );
        });
    }

    #[test]
    fn xaa_gate_uses_official_env_truthy_value_semantics() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _enabled = EnvVarGuard::set(CLAUDE_CODE_ENABLE_XAA, "yes");
        assert!(is_xaa_enabled());
        drop(_enabled);
        let _disabled = EnvVarGuard::set(CLAUDE_CODE_ENABLE_XAA, "0");
        assert!(!is_xaa_enabled());
    }
}
