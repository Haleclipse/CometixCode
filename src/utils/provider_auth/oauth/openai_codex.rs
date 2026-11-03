//! OpenAI Codex (ChatGPT sign-in): PKCE authorization-code flow.
//!
//! Protocol facts (client id, endpoints, scope, redirect) follow Pi Agent's
//! `openai-codex` module; the code is nyan's own. The login steps are split so
//! the caller owns the browser and the wait:
//! [`authorization_request`] -> user signs in -> [`parse_authorization_input`]
//! -> [`exchange_code`] -> [`save_login`]. [`refresh`] renews a stored account.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use sha2::{Digest as _, Sha256};

use super::{Reply, credential_from, parse_token_response, post_form, refresh_failure};
use crate::utils::provider_auth::refresh::TokenError;
use crate::utils::provider_auth::store::{
    AuthStore, AuthStoreError, OAuthCredential, ProviderAuth,
};

pub const PROVIDER_ID: &str = "openai-codex";

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_AUTH_BASE: &str = "https://auth.openai.com";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const ACCOUNT_CLAIM: &str = "https://api.openai.com/auth";

/// Where the token endpoint lives. Production uses [`Endpoints::default`];
/// tests point it at a local mock.
#[derive(Debug, Clone)]
pub struct Endpoints {
    auth_base: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth_base: DEFAULT_AUTH_BASE.to_string(),
        }
    }
}

impl Endpoints {
    #[cfg(test)]
    pub(crate) fn at(auth_base: &str) -> Self {
        Self {
            auth_base: auth_base.trim_end_matches('/').to_string(),
        }
    }

    fn authorize(&self) -> String {
        format!("{}/oauth/authorize", self.auth_base)
    }

    fn token(&self) -> String {
        format!("{}/oauth/token", self.auth_base)
    }
}

/// The URL to open plus the two secrets the exchange needs afterwards. Keep
/// `verifier` and `state` in memory only.
pub struct AuthorizationRequest {
    pub url: String,
    pub state: String,
    pub verifier: String,
}

impl std::fmt::Debug for AuthorizationRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationRequest")
            .field("url", &self.url)
            .field("state", &"<redacted>")
            .field("verifier", &"<redacted>")
            .finish()
    }
}

fn random_url_safe(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|error| format!("OS randomness failed: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn authorization_request(
    endpoints: &Endpoints,
    originator: &str,
) -> Result<AuthorizationRequest, String> {
    let verifier = random_url_safe(32)?;
    let state = random_url_safe(16)?;
    let mut url = url::Url::parse(&endpoints.authorize()).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &challenge_for(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", originator);
    Ok(AuthorizationRequest {
        url: url.to_string(),
        state,
        verifier,
    })
}

fn pick(pairs: url::form_urlencoded::Parse<'_>) -> (Option<String>, Option<String>) {
    let mut code = None;
    let mut state = None;
    for (key, value) in pairs {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    (code, state)
}

/// Reads what the user pastes back: the full redirect URL, `code#state`, a
/// query string, or a bare code. Returns `(code, state)`.
pub fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    if let Ok(url) = url::Url::parse(value) {
        return pick(url.query_pairs());
    }
    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_string()), Some(state.to_string()));
    }
    if value.contains("code=") {
        return pick(url::form_urlencoded::parse(value.as_bytes()));
    }
    (Some(value.to_string()), None)
}

/// Checks the pasted state against the one this login generated.
pub fn check_state(expected: &str, received: Option<&str>) -> Result<(), String> {
    match received {
        Some(received) if received == expected => Ok(()),
        Some(_) => Err("state mismatch; start the sign-in again".to_string()),
        None => Ok(()),
    }
}

/// The ChatGPT account id is a claim inside the access token. The token is
/// only decoded, never verified: the server already issued it to us over TLS,
/// and the id is used as a request header, not as proof of identity.
pub fn account_id_from_token(access: &str) -> Option<String> {
    let payload = access.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| STANDARD_NO_PAD.decode(payload.trim_end_matches('=')))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let id = json
        .get(ACCOUNT_CLAIM)?
        .get("chatgpt_account_id")?
        .as_str()?;
    (!id.is_empty()).then(|| id.to_string())
}

fn build_credential(
    reply: &Reply,
    previous_refresh: Option<&str>,
    now_ms: i64,
) -> Result<OAuthCredential, String> {
    let token = parse_token_response(reply)?;
    let account_id = account_id_from_token(&token.access_token)
        .ok_or_else(|| "access token has no ChatGPT account id".to_string())?;
    credential_from(
        token,
        previous_refresh,
        now_ms,
        Some(account_id),
        BTreeMap::new(),
    )
}

/// Exchanges the authorization code for tokens.
pub async fn exchange_code(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    code: &str,
    verifier: &str,
    now_ms: i64,
) -> Result<OAuthCredential, String> {
    let reply = post_form(
        client,
        &endpoints.token(),
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
        ],
    )
    .await
    .map_err(|reason| format!("OpenAI Codex token exchange failed: {reason}"))?;
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "OpenAI Codex token exchange failed: HTTP {}",
            reply.status
        ));
    }
    build_credential(&reply, None, now_ms)
}

/// Renews a stored account. Pass this (wrapped) to `fresh_credential`.
pub async fn refresh(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    stored: OAuthCredential,
    now_ms: i64,
) -> Result<OAuthCredential, TokenError> {
    let reply = post_form(
        client,
        &endpoints.token(),
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &stored.refresh),
            ("client_id", CLIENT_ID),
        ],
    )
    .await
    .map_err(|reason| TokenError::Refresh {
        provider: PROVIDER_ID.to_string(),
        reason,
    })?;
    if !(200..300).contains(&reply.status) {
        return Err(refresh_failure(PROVIDER_ID, &reply));
    }
    build_credential(&reply, Some(&stored.refresh), now_ms).map_err(|reason| TokenError::Refresh {
        provider: PROVIDER_ID.to_string(),
        reason,
    })
}

/// Stores a finished login as `account` and makes it the default if there is
/// none. Replaces an existing credential of the same name.
pub async fn save_login(
    store: &AuthStore,
    account: &str,
    credential: OAuthCredential,
) -> Result<(), AuthStoreError> {
    let lock = store.lock_async(PROVIDER_ID).await?;
    let mut auth = lock.load()?.unwrap_or_else(ProviderAuth::default);
    auth.set_account(account, credential);
    lock.save(&auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::web::test_support::{Route, serve};

    fn jwt(account: Option<&str>) -> String {
        let claims = match account {
            Some(id) => serde_json::json!({ ACCOUNT_CLAIM: { "chatgpt_account_id": id } }),
            None => serde_json::json!({ "sub": "x" }),
        };
        let part = |value: &serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string());
        format!(
            "{}.{}.sig",
            part(&serde_json::json!({"alg":"none"})),
            part(&claims)
        )
    }

    fn token_body(access: &str, refresh: &str) -> String {
        serde_json::json!({"access_token": access, "refresh_token": refresh, "expires_in": 3600})
            .to_string()
    }

    fn client() -> reqwest::Client {
        let builder = crate::utils::proxy::client_builder().no_proxy();
        super::super::build_client(builder).unwrap()
    }

    #[test]
    fn authorization_url_carries_pkce_and_state() {
        let request = authorization_request(&Endpoints::default(), "nyan").unwrap();
        let url = url::Url::parse(&request.url).unwrap();
        assert_eq!(url.host_str(), Some("auth.openai.com"));
        assert_eq!(url.path(), "/oauth/authorize");
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(query["redirect_uri"], REDIRECT_URI);
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["code_challenge"], challenge_for(&request.verifier));
        assert_eq!(query["state"], request.state);
        assert_eq!(query["originator"], "nyan");
        assert!(query["scope"].contains("offline_access"));
        assert!(request.verifier.len() >= 43, "RFC 7636 minimum length");
        let second = authorization_request(&Endpoints::default(), "nyan").unwrap();
        assert_ne!(request.verifier, second.verifier);
        assert_ne!(request.state, second.state);
        assert!(!format!("{request:?}").contains(&request.verifier));
    }

    #[test]
    fn challenge_matches_the_rfc7636_example() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pasted_input_forms_are_understood() {
        let both = (Some("abc".to_string()), Some("st".to_string()));
        assert_eq!(
            parse_authorization_input("http://localhost:1455/auth/callback?code=abc&state=st"),
            both
        );
        assert_eq!(parse_authorization_input("abc#st"), both);
        assert_eq!(parse_authorization_input("code=abc&state=st"), both);
        assert_eq!(
            parse_authorization_input("  abc \n"),
            (Some("abc".into()), None)
        );
        assert_eq!(parse_authorization_input("  "), (None, None));
        assert!(check_state("st", Some("st")).is_ok());
        assert!(check_state("st", None).is_ok());
        assert!(check_state("st", Some("other")).is_err());
    }

    #[test]
    fn account_id_comes_from_the_token_claim() {
        assert_eq!(
            account_id_from_token(&jwt(Some("acct_1"))).as_deref(),
            Some("acct_1")
        );
        assert_eq!(account_id_from_token(&jwt(None)), None);
        assert_eq!(account_id_from_token("not-a-jwt"), None);
    }

    #[tokio::test]
    async fn code_exchange_sends_pkce_form_and_builds_a_credential() {
        let access = jwt(Some("acct_7"));
        let server = serve(vec![(
            "/oauth/token",
            Route::new(200, "application/json", token_body(&access, "r-1")),
        )])
        .await;
        let credential = exchange_code(
            &client(),
            &Endpoints::at(&server.origin),
            "the-code",
            "the-verifier",
            1_000,
        )
        .await
        .unwrap();
        assert_eq!(credential.refresh, "r-1");
        assert_eq!(credential.account_id.as_deref(), Some("acct_7"));
        assert_eq!(credential.expires_at_ms, 1_000 + 3_600_000);
        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        let form: BTreeMap<_, _> = url::form_urlencoded::parse(seen[0].body.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "the-code");
        assert_eq!(form["code_verifier"], "the-verifier");
        assert_eq!(form["redirect_uri"], REDIRECT_URI);
        assert_eq!(form["client_id"], CLIENT_ID);
    }

    #[tokio::test]
    async fn exchange_failure_never_quotes_the_response() {
        let server = serve(vec![(
            "/oauth/token",
            Route::new(
                400,
                "application/json",
                r#"{"error":"bad","detail":"leak-me-123"}"#,
            ),
        )])
        .await;
        let error = exchange_code(&client(), &Endpoints::at(&server.origin), "c", "v", 0)
            .await
            .unwrap_err();
        assert!(error.contains("HTTP 400"));
        assert!(!error.contains("leak-me-123"));
    }

    #[tokio::test]
    async fn exchange_without_a_chatgpt_account_is_rejected() {
        let server = serve(vec![(
            "/oauth/token",
            Route::new(200, "application/json", token_body(&jwt(None), "r")),
        )])
        .await;
        let error = exchange_code(&client(), &Endpoints::at(&server.origin), "c", "v", 0)
            .await
            .unwrap_err();
        assert!(error.contains("account id"));
    }

    fn stored(refresh: &str) -> OAuthCredential {
        OAuthCredential {
            access: "old-access".into(),
            refresh: refresh.into(),
            expires_at_ms: 0,
            account_id: Some("acct_old".into()),
            metadata: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn refresh_rotates_the_refresh_token() {
        let server = serve(vec![(
            "/oauth/token",
            Route::new(
                200,
                "application/json",
                token_body(&jwt(Some("acct_9")), "r-2"),
            ),
        )])
        .await;
        let renewed = refresh(
            &client(),
            &Endpoints::at(&server.origin),
            stored("r-1"),
            5_000,
        )
        .await
        .unwrap();
        assert_eq!(renewed.refresh, "r-2");
        assert_eq!(renewed.account_id.as_deref(), Some("acct_9"));
        let form: BTreeMap<_, _> = url::form_urlencoded::parse(server.seen()[0].body.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["refresh_token"], "r-1");
    }

    #[tokio::test]
    async fn refresh_maps_invalid_grant_to_signed_out_and_hides_the_token() {
        let server = serve(vec![(
            "/oauth/token",
            Route::new(400, "application/json", r#"{"error":"invalid_grant"}"#),
        )])
        .await;
        let error = refresh(
            &client(),
            &Endpoints::at(&server.origin),
            stored("r-secret"),
            0,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TokenError::SignedOut { .. }));
        assert!(!error.to_string().contains("r-secret"));

        let server = serve(vec![(
            "/oauth/token",
            Route::new(503, "text/plain", "r-secret upstream down"),
        )])
        .await;
        let error = refresh(
            &client(),
            &Endpoints::at(&server.origin),
            stored("r-secret"),
            0,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TokenError::Refresh { .. }));
        assert!(!error.to_string().contains("r-secret"));
    }

    #[tokio::test]
    async fn refresh_does_not_follow_redirects_with_the_token() {
        let target = serve(vec![("/steal", Route::new(200, "application/json", "{}"))]).await;
        let server = serve(vec![(
            "/oauth/token",
            Route::redirect(307, &target.url("/steal")),
        )])
        .await;
        let error = refresh(
            &client(),
            &Endpoints::at(&server.origin),
            stored("r-secret"),
            0,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TokenError::Refresh { .. }));
        assert!(
            target.seen().is_empty(),
            "the refresh token must not follow a redirect"
        );
    }
}
