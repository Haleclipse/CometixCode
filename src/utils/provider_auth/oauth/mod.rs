//! Provider-owned OAuth modules.
//!
//! Each submodule owns one provider's endpoints, client id, token schema and
//! refresh call. Nothing here is a shared broker: the only code the two
//! providers have in common is how a token endpoint is called and how its
//! answer is read without ever quoting it.
//!
//! No function here opens a browser, binds a port, or prints. A caller that
//! runs a login drives the steps itself (build URL, wait for the user, exchange
//! the code); the model-request phase calls `refresh`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

use super::refresh::TokenError;
use super::store::OAuthCredential;

pub mod openai_codex;
pub mod opencode;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP client for token endpoints: the proxy/TLS settings every other nyan
/// request uses, a request timeout, and no redirect following, so a refresh
/// token in a request body is never replayed to another host.
pub fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(build_client(
        crate::utils::proxy::get_proxy_fetch_options()?
    )?)
}

/// Applies the token-endpoint rules to any builder; tests pass one without a
/// proxy.
pub(crate) fn build_client(builder: reqwest::ClientBuilder) -> reqwest::Result<reqwest::Client> {
    builder
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What a token endpoint answered, reduced to what may be reported.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
}

/// POSTs `application/x-www-form-urlencoded` to `url`. The body can hold a
/// refresh token, so it never reaches an error.
pub(crate) async fn post_form(
    client: &reqwest::Client,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<Reply, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    send(
        client
            .post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(body),
    )
    .await
}

pub(crate) async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<Reply, String> {
    send(
        client
            .post(url)
            .header("accept", "application/json")
            .json(body),
    )
    .await
}

async fn send(request: reqwest::RequestBuilder) -> Result<Reply, String> {
    let response = request.send().await.map_err(describe_transport_error)?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|_| "response body could not be read".to_string())?;
    Ok(Reply { status, body })
}

/// reqwest's own message can embed the request URL; the URL here never holds a
/// secret, but the category is enough to act on.
fn describe_transport_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "request timed out".to_string()
    } else if error.is_connect() {
        "could not connect".to_string()
    } else {
        "request failed".to_string()
    }
}

/// An OAuth `error` code is a short token. Anything else (HTML, a message that
/// echoes input) is dropped rather than reported.
pub(crate) fn error_code(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Body {
        error: Option<serde_json::Value>,
    }
    let value = serde_json::from_str::<Body>(body).ok()?.error?;
    let code = match &value {
        serde_json::Value::String(code) => code.as_str(),
        serde_json::Value::Object(object) => object.get("code")?.as_str()?,
        _ => return None,
    };
    let clean = !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    clean.then(|| code.to_string())
}

/// Maps a failed refresh to what the user can do about it. `invalid_grant`
/// means the refresh token is dead; everything else may pass.
pub(crate) fn refresh_failure(provider: &str, reply: &Reply) -> TokenError {
    let code = error_code(&reply.body);
    if code.as_deref() == Some("invalid_grant") {
        return TokenError::SignedOut {
            provider: provider.to_string(),
        };
    }
    TokenError::Refresh {
        provider: provider.to_string(),
        reason: match code {
            Some(code) => format!("HTTP {} ({code})", reply.status),
            None => format!("HTTP {}", reply.status),
        },
    }
}

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    pub org_id: Option<String>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_in", &self.expires_in)
            .field("org_id", &self.org_id)
            .finish()
    }
}

/// Reads a successful token response. A body that does not parse is reported by
/// shape only; it holds the tokens.
pub(crate) fn parse_token_response(reply: &Reply) -> Result<TokenResponse, String> {
    let parsed: TokenResponse = serde_json::from_str(&reply.body)
        .map_err(|_| "token response is missing access_token or expires_in".to_string())?;
    if parsed.access_token.is_empty() {
        return Err("token response has an empty access_token".to_string());
    }
    if parsed.expires_in <= 0 {
        return Err("token response has a non-positive expires_in".to_string());
    }
    Ok(parsed)
}

pub(crate) fn credential_from(
    token: TokenResponse,
    previous_refresh: Option<&str>,
    now_ms: i64,
    account_id: Option<String>,
    metadata: BTreeMap<String, String>,
) -> Result<OAuthCredential, String> {
    let refresh = token
        .refresh_token
        .filter(|refresh| !refresh.is_empty())
        .or_else(|| previous_refresh.map(str::to_string))
        .ok_or_else(|| "token response has no refresh_token".to_string())?;
    Ok(OAuthCredential {
        access: token.access_token,
        refresh,
        expires_at_ms: now_ms.saturating_add(token.expires_in.saturating_mul(1000)),
        account_id,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status,
            body: body.to_string(),
        }
    }

    #[test]
    fn error_codes_are_short_tokens_only() {
        assert_eq!(
            error_code(r#"{"error":"invalid_grant"}"#).as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(
            error_code(r#"{"error":{"code":"deviceauth_authorization_pending"}}"#).as_deref(),
            Some("deviceauth_authorization_pending")
        );
        assert_eq!(error_code(r#"{"error":"has spaces and sk-secret"}"#), None);
        assert_eq!(error_code("<html>sk-secret</html>"), None);
        assert_eq!(
            error_code(&format!(r#"{{"error":"{}"}}"#, "a".repeat(65))),
            None
        );
    }

    #[test]
    fn invalid_grant_means_signed_out_and_other_failures_name_status_only() {
        let signed_out = refresh_failure("opencode", &reply(400, r#"{"error":"invalid_grant"}"#));
        assert!(matches!(signed_out, TokenError::SignedOut { .. }));
        let other = refresh_failure(
            "opencode",
            &reply(
                500,
                r#"{"error":"server_error","error_description":"refresh-secret-1"}"#,
            ),
        );
        let text = other.to_string();
        assert!(text.contains("HTTP 500") && text.contains("server_error"));
        assert!(!text.contains("refresh-secret-1"));
        let opaque = refresh_failure("opencode", &reply(502, "refresh-secret-2"));
        assert!(!opaque.to_string().contains("refresh-secret-2"));
    }

    #[test]
    fn token_response_must_have_access_and_positive_expiry() {
        let ok = parse_token_response(&reply(
            200,
            r#"{"access_token":"a","refresh_token":"r","expires_in":3600}"#,
        ))
        .unwrap();
        assert_eq!(ok.expires_in, 3600);
        for body in [
            r#"{"refresh_token":"r","expires_in":3600}"#,
            r#"{"access_token":"a"}"#,
            r#"{"access_token":"","expires_in":10}"#,
            r#"{"access_token":"a","expires_in":0}"#,
            "not json with secret-token",
        ] {
            let error = parse_token_response(&reply(200, body)).unwrap_err();
            assert!(!error.contains("secret-token"), "{error}");
        }
    }

    #[test]
    fn refresh_token_rotates_or_falls_back_to_the_previous_one() {
        let token = |refresh: Option<&str>| TokenResponse {
            access_token: "a".into(),
            refresh_token: refresh.map(str::to_string),
            expires_in: 60,
            org_id: None,
        };
        let rotated = credential_from(
            token(Some("new")),
            Some("old"),
            1_000,
            None,
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(rotated.refresh, "new");
        assert_eq!(rotated.expires_at_ms, 61_000);
        let kept = credential_from(token(None), Some("old"), 1_000, None, BTreeMap::new()).unwrap();
        assert_eq!(kept.refresh, "old");
        assert!(credential_from(token(None), None, 1_000, None, BTreeMap::new()).is_err());
    }
}
