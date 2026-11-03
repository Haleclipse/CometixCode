//! OpenCode Console sign-in: OAuth device flow.
//!
//! Protocol facts (client id, endpoint paths, `slow_down` handling) follow
//! OpenCode's `opencode` provider plugin; the code is nyan's own. The caller
//! owns the browser and the wait: [`start_device_login`] -> show
//! `verification_url` and `user_code` -> [`poll_device_login`] ->
//! [`save_login`]. [`refresh`] renews a stored account against the Console
//! server that issued it.

use std::future::Future;
use std::time::Duration;

use serde::Deserialize;

use super::{Reply, credential_from, error_code, parse_token_response, post_json, refresh_failure};
use crate::utils::provider_auth::refresh::TokenError;
use crate::utils::provider_auth::store::{
    AuthStore, AuthStoreError, OAuthCredential, ProviderAuth,
};

pub const PROVIDER_ID: &str = "opencode";

const CLIENT_ID: &str = "opencode-cli";
pub const DEFAULT_SERVER: &str = "https://opencode.ai/console";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
/// Metadata key holding the Console server a credential was issued by.
pub const SERVER_KEY: &str = "server";
pub const ORG_KEY: &str = "orgId";

/// Accepts `https` servers and plain `http` only for loopback, so a refresh
/// token is never sent over cleartext to a remote host. Returns the server with
/// any trailing slash removed.
pub fn normalize_server(input: &str) -> Result<String, String> {
    let url =
        url::Url::parse(input).map_err(|_| "OpenCode server is not a valid URL".to_string())?;
    let loopback = matches!(
        url.host(),
        Some(url::Host::Ipv4(ip)) if ip.is_loopback()
    ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
        || url.host_str() == Some("localhost");
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err("OpenCode server must use https".to_string()),
    }
    if url.username() != "" || url.password().is_some() || url.query().is_some() {
        return Err("OpenCode server URL must not carry credentials or a query".to_string());
    }
    Ok(format!(
        "{}{}",
        url.origin().ascii_serialization(),
        url.path().trim_end_matches('/')
    ))
}

/// What the user has to do, plus what polling needs.
#[derive(Debug, Clone)]
pub struct DeviceLogin {
    pub server: String,
    pub user_code: String,
    pub verification_url: String,
    device_code: String,
    pub interval: Duration,
    pub expires_in: Duration,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

pub async fn start_device_login(
    client: &reqwest::Client,
    server: &str,
) -> Result<DeviceLogin, String> {
    let server = normalize_server(server)?;
    let reply = post_json(
        client,
        &format!("{server}/auth/device/code"),
        &serde_json::json!({ "client_id": CLIENT_ID, "supports_org_scope": true }),
    )
    .await
    .map_err(|reason| format!("OpenCode device login failed: {reason}"))?;
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "OpenCode device login failed: HTTP {}",
            reply.status
        ));
    }
    let device: DeviceResponse = serde_json::from_str(&reply.body)
        .map_err(|_| "OpenCode device login response is malformed".to_string())?;
    let base = url::Url::parse(&format!("{server}/")).map_err(|error| error.to_string())?;
    let verification = base
        .join(&device.verification_uri_complete)
        .map_err(|_| "OpenCode verification URL is malformed".to_string())?;
    if !matches!(verification.scheme(), "http" | "https") {
        return Err("OpenCode verification URL must be http(s)".to_string());
    }
    Ok(DeviceLogin {
        server,
        user_code: device.user_code,
        verification_url: verification.to_string(),
        device_code: device.device_code,
        interval: Duration::from_secs(device.interval.max(1)),
        expires_in: Duration::from_secs(device.expires_in),
    })
}

fn build_credential(
    reply: &Reply,
    server: &str,
    previous: Option<&OAuthCredential>,
    now_ms: i64,
) -> Result<OAuthCredential, String> {
    let token = parse_token_response(reply)?;
    let mut metadata = previous.map(|c| c.metadata.clone()).unwrap_or_default();
    metadata.insert(SERVER_KEY.to_string(), server.to_string());
    if let Some(org) = token.org_id.clone() {
        metadata.insert(ORG_KEY.to_string(), org);
    }
    let previous_refresh = previous.map(|c| c.refresh.as_str());
    credential_from(
        token,
        previous_refresh,
        now_ms,
        previous.and_then(|c| c.account_id.clone()),
        metadata,
    )
}

/// Polls until the user approves, the code expires, or the Console refuses.
/// `sleep` is injected so tests do not wait in real time. Waiting is counted,
/// not clocked, so the deadline is the sum of the waits.
pub async fn poll_device_login<S, Fut>(
    client: &reqwest::Client,
    login: &DeviceLogin,
    now_ms: impl Fn() -> i64,
    sleep: S,
) -> Result<OAuthCredential, String>
where
    S: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut wait = login.interval;
    let mut waited = Duration::ZERO;
    loop {
        sleep(wait).await;
        waited += wait;
        let reply = post_json(
            client,
            &format!("{}/auth/device/token", login.server),
            &serde_json::json!({
                "grant_type": DEVICE_GRANT,
                "device_code": login.device_code,
                "client_id": CLIENT_ID,
            }),
        )
        .await
        .map_err(|reason| format!("OpenCode device login failed: {reason}"))?;
        if (200..300).contains(&reply.status) {
            return build_credential(&reply, &login.server, None, now_ms());
        }
        match error_code(&reply.body).as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => wait += SLOW_DOWN_STEP,
            Some("expired_token") => {
                return Err("OpenCode device code expired; start the sign-in again".to_string());
            }
            Some("access_denied") => {
                return Err("OpenCode sign-in was denied".to_string());
            }
            _ => {
                return Err(format!(
                    "OpenCode device login failed: HTTP {}",
                    reply.status
                ));
            }
        }
        if waited >= login.expires_in {
            return Err("OpenCode device code expired; start the sign-in again".to_string());
        }
    }
}

/// Renews a stored account against the server recorded in its metadata
/// (falling back to the default). Pass this (wrapped) to `fresh_credential`.
pub async fn refresh(
    client: &reqwest::Client,
    stored: OAuthCredential,
    now_ms: i64,
) -> Result<OAuthCredential, TokenError> {
    let failure = |reason: String| TokenError::Refresh {
        provider: PROVIDER_ID.to_string(),
        reason,
    };
    let server = normalize_server(
        stored
            .metadata
            .get(SERVER_KEY)
            .map(String::as_str)
            .unwrap_or(DEFAULT_SERVER),
    )
    .map_err(failure)?;
    let reply = post_json(
        client,
        &format!("{server}/auth/device/token"),
        &serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": stored.refresh,
            "client_id": CLIENT_ID,
        }),
    )
    .await
    .map_err(failure)?;
    if !(200..300).contains(&reply.status) {
        return Err(refresh_failure(PROVIDER_ID, &reply));
    }
    build_credential(&reply, &server, Some(&stored), now_ms).map_err(failure)
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
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn client() -> reqwest::Client {
        let builder = crate::utils::proxy::client_builder().no_proxy();
        super::super::build_client(builder).unwrap()
    }

    fn device_body() -> String {
        serde_json::json!({
            "device_code": "dev-code-1",
            "user_code": "ABCD-1234",
            "verification_uri_complete": "/device?user_code=ABCD-1234",
            "expires_in": 600,
            "interval": 5,
        })
        .to_string()
    }

    fn token_body(access: &str, refresh: &str) -> String {
        serde_json::json!({
            "access_token": access, "refresh_token": refresh,
            "expires_in": 3600, "org_id": "org_1",
        })
        .to_string()
    }

    fn json_of(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn server_must_be_https_or_loopback() {
        assert_eq!(
            normalize_server("https://opencode.ai/console/").unwrap(),
            "https://opencode.ai/console"
        );
        assert_eq!(
            normalize_server("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_server("http://localhost:3000/x").unwrap(),
            "http://localhost:3000/x"
        );
        for bad in [
            "http://evil.example",
            "ftp://opencode.ai",
            "https://user:pw@opencode.ai",
            "https://opencode.ai/?token=1",
            "not a url",
            "",
        ] {
            assert!(normalize_server(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn device_login_start_resolves_a_relative_verification_url() {
        let server = serve(vec![(
            "/auth/device/code",
            Route::new(200, "application/json", device_body()),
        )])
        .await;
        let login = start_device_login(&client(), &server.origin).await.unwrap();
        assert_eq!(login.user_code, "ABCD-1234");
        assert_eq!(
            login.verification_url,
            format!("{}/device?user_code=ABCD-1234", server.origin)
        );
        assert_eq!(login.interval, Duration::from_secs(5));
        let sent = json_of(&server.seen()[0].body);
        assert_eq!(sent["client_id"], CLIENT_ID);
        assert_eq!(sent["supports_org_scope"], true);
    }

    #[tokio::test]
    async fn verification_url_must_be_http() {
        let body = device_body().replace("/device?user_code=ABCD-1234", "javascript:alert(1)");
        let server = serve(vec![(
            "/auth/device/code",
            Route::new(200, "application/json", body),
        )])
        .await;
        assert!(start_device_login(&client(), &server.origin).await.is_err());
    }

    /// Routes are fixed, so the poll tests drive `poll_device_login` against a
    /// server that answers from a queue held behind one route.
    async fn queued_token_server(
        replies: Vec<(u16, String)>,
    ) -> crate::services::web::test_support::Server {
        // The shared mock serves a fixed body per path, so each queued reply
        // gets its own path and the test points `server` at that path.
        let routes: Vec<(&'static str, Route)> = replies
            .into_iter()
            .enumerate()
            .map(|(index, (status, body))| {
                let path: &'static str =
                    Box::leak(format!("/q{index}/auth/device/token").into_boxed_str());
                (path, Route::new(status, "application/json", body))
            })
            .collect();
        serve(routes).await
    }

    fn login_for(server: &str) -> DeviceLogin {
        DeviceLogin {
            server: server.to_string(),
            user_code: "U".into(),
            verification_url: format!("{server}/device"),
            device_code: "dev-code-1".into(),
            interval: Duration::from_secs(5),
            expires_in: Duration::from_secs(600),
        }
    }

    #[tokio::test]
    async fn poll_waits_one_interval_then_returns_the_credential() {
        let server = queued_token_server(vec![(200, token_body("acc-1", "ref-1"))]).await;
        let sleeps = Mutex::new(Vec::new());
        let credential = poll_device_login(
            &client(),
            &login_for(&format!("{}/q0", server.origin)),
            || 1_000,
            |wait| {
                sleeps.lock().unwrap().push(wait);
                async {}
            },
        )
        .await
        .unwrap();
        assert_eq!(credential.access, "acc-1");
        assert_eq!(credential.refresh, "ref-1");
        assert_eq!(credential.expires_at_ms, 1_000 + 3_600_000);
        assert_eq!(credential.metadata[ORG_KEY], "org_1");
        assert_eq!(*sleeps.lock().unwrap(), vec![Duration::from_secs(5)]);
        let sent = json_of(&server.seen()[0].body);
        assert_eq!(sent["grant_type"], DEVICE_GRANT);
        assert_eq!(sent["device_code"], "dev-code-1");
    }

    #[tokio::test]
    async fn poll_backs_off_on_slow_down_and_gives_up_at_expiry() {
        let server = queued_token_server(vec![(400, r#"{"error":"slow_down"}"#.to_string())]).await;
        let sleeps = Mutex::new(Vec::new());
        let mut login = login_for(&format!("{}/q0", server.origin));
        login.expires_in = Duration::from_secs(25);
        let error = poll_device_login(
            &client(),
            &login,
            || 0,
            |wait| {
                sleeps.lock().unwrap().push(wait);
                async {}
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("expired"));
        // 5 -> slow_down -> 10 -> slow_down -> 15; 5 + 10 = 15 < 25, +15 = 30 >= 25.
        assert_eq!(
            *sleeps.lock().unwrap(),
            vec![
                Duration::from_secs(5),
                Duration::from_secs(10),
                Duration::from_secs(15)
            ]
        );
    }

    #[tokio::test]
    async fn poll_stops_on_denial_and_does_not_quote_the_body() {
        let server = queued_token_server(vec![(
            400,
            r#"{"error":"access_denied","error_description":"leak-me"}"#.to_string(),
        )])
        .await;
        let error = poll_device_login(
            &client(),
            &login_for(&format!("{}/q0", server.origin)),
            || 0,
            |_| async {},
        )
        .await
        .unwrap_err();
        assert!(error.contains("denied"));
        assert!(!error.contains("leak-me"));
        let hits = AtomicU32::new(0);
        hits.fetch_add(server.seen().len() as u32, Ordering::SeqCst);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    fn stored(server: &str) -> OAuthCredential {
        OAuthCredential {
            access: "old".into(),
            refresh: "ref-old".into(),
            expires_at_ms: 0,
            account_id: Some("user_1".into()),
            metadata: BTreeMap::from([(SERVER_KEY.to_string(), server.to_string())]),
        }
    }

    #[tokio::test]
    async fn refresh_uses_the_issuing_server_and_keeps_metadata() {
        let server = serve(vec![(
            "/auth/device/token",
            Route::new(200, "application/json", token_body("acc-2", "ref-new")),
        )])
        .await;
        let renewed = refresh(&client(), stored(&server.origin), 7_000)
            .await
            .unwrap();
        assert_eq!(renewed.access, "acc-2");
        assert_eq!(renewed.refresh, "ref-new");
        assert_eq!(renewed.account_id.as_deref(), Some("user_1"));
        assert_eq!(renewed.metadata[SERVER_KEY], server.origin);
        let sent = json_of(&server.seen()[0].body);
        assert_eq!(sent["grant_type"], "refresh_token");
        assert_eq!(sent["refresh_token"], "ref-old");
        assert_eq!(sent["client_id"], CLIENT_ID);
    }

    #[tokio::test]
    async fn refresh_maps_invalid_grant_to_signed_out() {
        let server = serve(vec![(
            "/auth/device/token",
            Route::new(400, "application/json", r#"{"error":"invalid_grant"}"#),
        )])
        .await;
        let error = refresh(&client(), stored(&server.origin), 0)
            .await
            .unwrap_err();
        assert!(matches!(error, TokenError::SignedOut { .. }));
        assert!(!error.to_string().contains("ref-old"));
    }

    #[tokio::test]
    async fn refresh_refuses_a_cleartext_remote_server_before_sending() {
        let error = refresh(&client(), stored("http://evil.example"), 0)
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("https"));
        assert!(!text.contains("ref-old"));
    }
}
