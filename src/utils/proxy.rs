//! Maps to: CC `utils/proxy.ts`: the proxy and TLS transport of outbound
//! HTTP clients.
//!
//! Node reaches every client through two globals that `configureGlobalAgents`
//! installs: an axios request interceptor and undici's global dispatcher.
//! Rust has neither, so a client takes its transport from here when it is
//! built. Callers CC makes through `fetch` (the Anthropic API client, MCP
//! transports and OAuth, XAA) take [`get_proxy_fetch_options`]; callers CC
//! makes through axios take [`create_axios_instance`].
//!
//! The two paths keep CC's two NO_PROXY rule sets: [`should_bypass_proxy`] for
//! axios, and undici's `EnvHttpProxyAgent` for `fetch`
//! ([`crate::utils::undici::env_http_proxy_agent`]).
//!
//! No client is shared. CC memoizes `getProxyAgent` and routes the rest
//! through one global dispatcher, which is safe on Node's single event loop.
//! A reqwest connection is driven by a task on the tokio runtime that opened
//! it, and Cometix runs each query, subagent and forked agent on its own
//! runtime. A shared pool would lend one runtime's connection to another:
//! the request hangs while the owner is not polled, and a stream breaks when
//! the owner shuts down. So every call builds a client. What is cached is the
//! TLS material ([`crate::utils::mtls::get_mtls_agent`]), as in CC's
//! `getMTLSConfig`/`getCACertificates` memos.
//!
//! Cometix-specific deviations:
//! - With no proxy variable set, clients keep reqwest's system-proxy detection
//!   where CC connects directly (environment redesign §9.3).
//! - The proxy URL is read as reqwest reads it. A missing scheme is `http://`
//!   and `socks5h://` is accepted, where CC's `new URL` or undici throws.
//!   `socks5://` and `socks://` resolve the target at the proxy (`socks5h`),
//!   as undici does; the axios path accepts them too, where CC's
//!   `HttpsProxyAgent` would speak HTTP to them. Any other scheme is an error,
//!   not a silent direct connection. A bad URL fails each request; CC's
//!   `configureGlobalAgents` throws during `init` instead.
//! - An `http://` target goes through the proxy as an absolute-form request;
//!   CC's agents open a CONNECT tunnel for it too.
//! - On the fetch path the client certificate and CA certificates also apply
//!   to the TLS connection to an HTTPS proxy. undici keeps that one separate
//!   (`proxyTls`, which CC leaves unset); reqwest has one TLS configuration.
//! - Keep-alive and the unix socket follow Bun, where `keepalive: false` stops
//!   pooling; the proxy and TLS branches follow Node.
//! - `CLAUDE_CODE_PROXY_RESOLVES_HOSTS` (`:151-158`) needs no code: reqwest
//!   sends the target host name to the proxy and resolves only the proxy's.
//! - `getAddressFamily` (`:41-55`) exists only for that Node `lookup` hook.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::utils::debug::log_for_debugging;
use crate::utils::process_env::{EnvSnapshot, JsTruthy};
use crate::utils::undici::env_http_proxy_agent::EnvHttpProxyAgent;

/// Maps to: CC `utils/proxy.ts:27` `keepAliveDisabled`: sticky for the
/// process once a pooled connection turned out dead.
static KEEP_ALIVE_DISABLED: AtomicBool = AtomicBool::new(false);

/// Maps to: CC `utils/proxy.ts:29-31` `disableKeepAlive`. Clients built from
/// [`get_proxy_fetch_options`] after this keep no idle connection. CC's flag
/// rides only the calls that spread `getProxyFetchOptions()`; here every
/// fetch-path client takes it, plain `fetch` callers included.
pub fn disable_keep_alive() {
    KEEP_ALIVE_DISABLED.store(true, Ordering::Relaxed);
}

/// Maps to: CC `utils/proxy.ts:33-35` `_resetKeepAliveForTesting`.
#[cfg(test)]
pub fn reset_keep_alive_for_testing() {
    KEEP_ALIVE_DISABLED.store(false, Ordering::Relaxed);
}

#[cfg(test)]
pub fn is_keep_alive_disabled_for_testing() -> bool {
    KEEP_ALIVE_DISABLED.load(Ordering::Relaxed)
}

/// Maps to: CC `utils/proxy.ts:64-66` `getProxyUrl`:
/// `https_proxy || HTTPS_PROXY || http_proxy || HTTP_PROXY`, one proxy for
/// every scheme.
pub fn get_proxy_url(env: &EnvSnapshot) -> Option<String> {
    ["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"]
        .into_iter()
        .find_map(|key| env.var(key).filter(|value| !value.is_empty()))
        .map(str::to_owned)
}

/// Maps to: CC `utils/proxy.ts:73-75` `getNoProxy`: `no_proxy || NO_PROXY`.
pub fn get_no_proxy(env: &EnvSnapshot) -> Option<String> {
    env.var("no_proxy")
        .truthy()
        .or_else(|| env.var("NO_PROXY"))
        .map(str::to_owned)
}

/// Maps to: CC `utils/proxy.ts:88-129` `shouldBypassProxy`, the NO_PROXY
/// rules of CC's own axios and WebSocket paths:
/// - the whole value `*` bypasses everything;
/// - entries split on commas and whitespace, compared lowercased;
/// - `host:port` must equal the target's host and port, where the default
///   port is 443 for `https:` and 80 for every other scheme (`wss:` too, as
///   in CC);
/// - `.example.com` matches the domain and its subdomains;
/// - anything else, IP addresses included, must equal the host name.
///
/// A URL that does not parse is not bypassed.
pub fn should_bypass_proxy(url_string: &str, no_proxy: Option<&str>) -> bool {
    let Some(no_proxy) = no_proxy.filter(|value| !value.is_empty()) else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let Ok(url) = reqwest::Url::parse(url_string) else {
        return false;
    };
    let hostname = url.host_str().unwrap_or_default().to_lowercase();
    let port = url.port().map_or_else(
        || if url.scheme() == "https" { "443" } else { "80" }.to_owned(),
        |port| port.to_string(),
    );
    let host_with_port = format!("{hostname}:{port}");

    no_proxy
        .split(|c: char| c == ',' || (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| {
            let pattern = pattern.to_lowercase();
            if pattern.contains(':') {
                host_with_port == pattern
            } else if let Some(domain) = pattern.strip_prefix('.') {
                hostname == domain || hostname.ends_with(&pattern)
            } else {
                hostname == pattern
            }
        })
}

/// The proxy URL as the agents take it: a missing scheme is `http://`, as
/// reqwest reads one, and a SOCKS proxy resolves the target itself. Errors
/// leave the URL out: it may carry credentials, and CC's `new URL` reports
/// only "Invalid URL".
fn parse_proxy_url(proxy_url: &str) -> anyhow::Result<reqwest::Url> {
    let proxy_url = if proxy_url.contains("://") {
        proxy_url.to_owned()
    } else {
        format!("http://{proxy_url}")
    };
    let invalid = |error: url::ParseError| anyhow::anyhow!("Invalid proxy URL: {error}");
    let mut url = reqwest::Url::parse(&proxy_url).map_err(invalid)?;
    match url.scheme() {
        "http" | "https" | "socks5h" => {}
        "socks5" | "socks" => {
            let rest = &proxy_url[proxy_url.find("://").map_or(0, |index| index + 3)..];
            url = reqwest::Url::parse(&format!("socks5h://{rest}")).map_err(invalid)?;
        }
        scheme => anyhow::bail!(
            "Unsupported proxy URL scheme {scheme:?}: use http, https or socks5"
        ),
    }
    Ok(url)
}

/// A client builder with what every client shares: the crypto provider and
/// Node's `NODE_TLS_REJECT_UNAUTHORIZED`.
fn client_builder() -> reqwest::ClientBuilder {
    crate::utils::tls_provider::install_crypto_provider();
    let builder = reqwest::Client::builder();
    if crate::utils::tls_provider::get_allow_unauthorized() {
        builder.tls_danger_accept_invalid_certs(true)
    } else {
        builder
    }
}

/// Maps to: CC `utils/proxy.ts:168-192` `createAxiosInstance`: the global
/// interceptor's proxy, NO_PROXY, mTLS and CA resolution, for one instance.
/// CC's axios callers use the global instance that `configureGlobalAgents`
/// sets up the same way; Rust has no global instance, so each takes this.
///
/// Callers add their own request options (redirects, timeout) and build.
/// NO_PROXY is the one in the environment when this is called.
pub fn create_axios_instance() -> anyhow::Result<reqwest::ClientBuilder> {
    let env = crate::utils::process_env::snapshot();
    let proxy_url = get_proxy_url(&env);
    let mut builder = client_builder();
    if let Some(mtls_agent) = crate::utils::mtls::get_mtls_agent() {
        builder = mtls_agent.apply(builder);
    }
    let Some(proxy_url) = proxy_url else {
        return Ok(builder);
    };

    // `createHttpsProxyAgent(proxyUrl)` and the interceptor's
    // `shouldBypassProxy(config.url)`.
    let proxy = parse_proxy_url(&proxy_url)?;
    let no_proxy = get_no_proxy(&env);
    Ok(builder.proxy(reqwest::Proxy::custom(move |url| {
        (!should_bypass_proxy(url.as_str(), no_proxy.as_deref())).then(|| proxy.clone())
    })))
}

/// CC passes `keepalive: false` per request; reqwest pools per client, so the
/// flag is read when the fetch builder is made.
fn fetch_builder(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    if KEEP_ALIVE_DISABLED.load(Ordering::Relaxed) {
        builder.pool_max_idle_per_host(0)
    } else {
        builder
    }
}

/// Maps to: CC `utils/proxy.ts:198-237` `getProxyAgent`: an
/// `EnvHttpProxyAgent` with `uri` as both proxies and
/// `NO_PROXY || no_proxy`, with the mTLS and CA options on the tunnelled and
/// the direct connections alike. A new builder on every call, not memoized
/// (see the module docs).
pub fn get_proxy_agent(uri: &str) -> anyhow::Result<reqwest::ClientBuilder> {
    let proxy = parse_proxy_url(uri)?;
    let env = crate::utils::process_env::snapshot();
    let no_proxy = env
        .var("NO_PROXY")
        .truthy()
        .or_else(|| env.var("no_proxy"))
        .map(str::to_owned);
    let agent = EnvHttpProxyAgent::new(Some(proxy.to_string()), Some(proxy.to_string()), no_proxy);
    let mut builder = client_builder();
    if let Some(mtls_agent) = crate::utils::mtls::get_mtls_agent() {
        builder = mtls_agent.apply(builder);
    }
    Ok(fetch_builder(builder.proxy(reqwest::Proxy::custom(move |url| {
        agent.get_proxy_for_url(url).map(str::to_owned)
    }))))
}

/// Maps to: CC `utils/proxy.ts:288-319` `getProxyFetchOptions`: the transport
/// for `fetch`, here a client builder that carries it. Callers add their own
/// request options (timeout, redirects) and build, as CC's spread the options
/// into their own `fetch` init.
/// - `for_anthropic_api` with `ANTHROPIC_UNIX_SOCKET`: that socket, without
///   proxy or TLS options. CC does this only under Bun, which Cometix, a
///   native binary, follows. Unix only; elsewhere the variable is ignored.
/// - A proxy: [`get_proxy_agent`].
/// - Otherwise the mTLS and CA options if any (`getTLSFetchOptions`,
///   `mtls.ts:117-152`), else the defaults.
///
/// CC's plain `fetch` calls take undici's global dispatcher, which
/// `configureGlobalAgents` sets to the same agent (`:370-386`); they map here
/// with `for_anthropic_api: false`.
pub fn get_proxy_fetch_options(for_anthropic_api: bool) -> anyhow::Result<reqwest::ClientBuilder> {
    let env = crate::utils::process_env::snapshot();

    if for_anthropic_api {
        if let Some(unix_socket) = env.var("ANTHROPIC_UNIX_SOCKET").truthy() {
            // `.no_proxy()`: reqwest ignores proxies on a socket but would
            // still attach a system proxy's credentials to the request.
            #[cfg(unix)]
            return Ok(fetch_builder(client_builder().no_proxy().unix_socket(unix_socket)));
            #[cfg(not(unix))]
            log_for_debugging(&format!(
                "ANTHROPIC_UNIX_SOCKET={unix_socket} is not supported on this platform; ignored"
            ));
        }
    }

    if let Some(proxy_url) = get_proxy_url(&env) {
        return get_proxy_agent(&proxy_url);
    }

    let builder = client_builder();
    Ok(fetch_builder(match crate::utils::mtls::get_mtls_agent() {
        Some(mtls_agent) => {
            log_for_debugging("TLS: Created undici agent with custom certificates");
            mtls_agent.apply(builder)
        }
        None => builder,
    }))
}

/// Maps to: CC `utils/proxy.ts:327-388` `configureGlobalAgents`. There are
/// no globals to install and no agent to keep (see the module docs). What is
/// left is CC's eager work: resolving the mTLS and CA agent, and reading the
/// proxy URL, so certificate files are read and a bad proxy URL is logged at
/// startup and after the settings environment applies, not first at a
/// request.
pub fn configure_global_agents() {
    let proxy_url = get_proxy_url(&crate::utils::process_env::snapshot());
    let _mtls_agent = crate::utils::mtls::get_mtls_agent();
    if let Some(proxy_url) = proxy_url {
        if let Err(error) = parse_proxy_url(&proxy_url) {
            crate::utils::debug::log_for_debugging_with_level(
                &format!("Proxy: {error:#}"),
                crate::utils::debug::DebugLogLevel::Error,
            );
        }
    }
}

/// Maps to: CC `utils/proxy.ts:423-426` `clearProxyCache`. No agent is
/// memoized here (see the module docs); the TLS material has its own caches,
/// which `managed_env` clears alongside.
pub fn clear_proxy_cache() {
    log_for_debugging("Cleared proxy agent cache");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const PROXY_VARS: [&str; 6] = [
        "https_proxy",
        "HTTPS_PROXY",
        "http_proxy",
        "HTTP_PROXY",
        "no_proxy",
        "NO_PROXY",
    ];

    fn clear_proxy_env() -> Vec<EnvVarGuard> {
        PROXY_VARS.into_iter().map(EnvVarGuard::unset).collect()
    }

    // Windows environment names ignore case, as Node's `process.env` does
    // there, so lowercase and uppercase are one variable.
    #[cfg(not(windows))]
    #[test]
    fn proxy_url_prefers_lowercase_and_skips_empty_values() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = clear_proxy_env();
        let snapshot = crate::utils::process_env::snapshot;
        assert_eq!(get_proxy_url(&snapshot()), None);

        crate::utils::process_env::set("HTTP_PROXY", "http://upper-http:1");
        crate::utils::process_env::set("http_proxy", "http://lower-http:1");
        assert_eq!(get_proxy_url(&snapshot()).as_deref(), Some("http://lower-http:1"));
        crate::utils::process_env::set("HTTPS_PROXY", "http://upper-https:1");
        assert_eq!(get_proxy_url(&snapshot()).as_deref(), Some("http://upper-https:1"));
        crate::utils::process_env::set("https_proxy", "");
        assert_eq!(get_proxy_url(&snapshot()).as_deref(), Some("http://upper-https:1"));

        crate::utils::process_env::set("NO_PROXY", "upper");
        crate::utils::process_env::set("no_proxy", "");
        assert_eq!(get_no_proxy(&snapshot()).as_deref(), Some("upper"));
        crate::utils::process_env::set("no_proxy", "lower");
        assert_eq!(get_no_proxy(&snapshot()).as_deref(), Some("lower"));
    }

    #[test]
    fn should_bypass_proxy_follows_cc_rules() {
        let bypass = |url: &str, no_proxy: &str| should_bypass_proxy(url, Some(no_proxy));
        assert!(!should_bypass_proxy("https://example.com", None));
        assert!(!bypass("https://example.com", ""));
        assert!(bypass("https://example.com", "*"));
        // `*` in a list is only a name.
        assert!(!bypass("https://example.com", "a.test,*"));

        // A bare name is exact, unlike undici's.
        assert!(bypass("https://example.com", "example.com"));
        assert!(!bypass("https://api.example.com", "example.com"));
        assert!(bypass("https://api.example.com", ".example.com"));
        assert!(bypass("https://example.com", ".example.com"));
        assert!(!bypass("https://notexample.com", ".example.com"));
        assert!(!bypass("https://api.example.com", "*.example.com"));

        // `host:port` against the default port of the scheme.
        assert!(bypass("https://example.com", "example.com:443"));
        assert!(bypass("http://example.com", "example.com:80"));
        assert!(bypass("http://localhost:3000/x", "localhost:3000"));
        assert!(!bypass("http://localhost:3001", "localhost:3000"));
        // CC gives `wss:` port 80.
        assert!(bypass("wss://example.com", "example.com:80"));
        assert!(!bypass("wss://example.com", "example.com:443"));

        // Commas and whitespace separate; case is ignored; IPs are names.
        assert!(bypass("http://127.0.0.1:8080", "a.test,\t 127.0.0.1"));
        assert!(bypass("https://EXAMPLE.com", "Example.COM"));
        assert!(!bypass("not a url", "*.x"));
    }

    #[test]
    fn proxy_urls_take_a_default_scheme_and_socks_resolves_remotely() {
        assert_eq!(parse_proxy_url("127.0.0.1:7890").unwrap().as_str(), "http://127.0.0.1:7890/");
        assert_eq!(
            parse_proxy_url("socks5://user:p%40ss@proxy:1080").unwrap().as_str(),
            "socks5h://user:p%40ss@proxy:1080"
        );
        assert_eq!(parse_proxy_url("socks://proxy:1080").unwrap().scheme(), "socks5h");
        assert_eq!(parse_proxy_url("https://proxy:443").unwrap().scheme(), "https");
        assert!(parse_proxy_url("ftp://proxy:21").is_err());
    }

    #[test]
    fn proxy_url_errors_leave_credentials_out() {
        for bad in ["ftp://user:secret@proxy:21", "http://user:secret@[bad"] {
            let error = parse_proxy_url(bad).unwrap_err().to_string();
            assert!(!error.contains("secret"), "{error}");
        }
    }

    /// One HTTP request accepted on `listener`; its head is returned.
    async fn accept_one(listener: tokio::net::TcpListener) -> String {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
        }
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8_lossy(&request).into_owned()
    }

    /// The fetch client goes through the snapshot's proxy, `HTTPS_PROXY` for
    /// an `http://` origin too, and undici's NO_PROXY sends a matching origin
    /// direct.
    #[tokio::test]
    async fn fetch_client_routes_through_the_proxy_unless_no_proxy_matches() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = clear_proxy_env();
        let _socket = EnvVarGuard::unset("ANTHROPIC_UNIX_SOCKET");

        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        crate::utils::process_env::set("HTTPS_PROXY", format!("http://{proxy_address}"));
        let seen = tokio::spawn(accept_one(proxy));
        let client = get_proxy_fetch_options(true).unwrap().build().unwrap();
        let response = client.get("http://origin.invalid/v1/models").send().await.unwrap();
        assert_eq!(response.status(), 204);
        let request = seen.await.unwrap();
        assert!(
            request.starts_with("GET http://origin.invalid/v1/models HTTP/1.1"),
            "{request}"
        );

        // `NO_PROXY || no_proxy`, read when the agent is built.
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_address = origin.local_addr().unwrap();
        crate::utils::process_env::set("NO_PROXY", "127.0.0.1");
        let seen = tokio::spawn(accept_one(origin));
        let client = get_proxy_fetch_options(true).unwrap().build().unwrap();
        let response = client
            .get(format!("http://{origin_address}/direct"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 204);
        assert!(seen.await.unwrap().starts_with("GET /direct HTTP/1.1"));

        crate::utils::process_env::set("HTTPS_PROXY", "ftp://proxy:21");
        assert!(get_proxy_fetch_options(true).is_err());
    }

    /// Connections a server accepts while answering `requests` sequential
    /// requests from one fresh fetch client, keeping each connection open.
    async fn connections_for(requests: usize) -> usize {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = accepted.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buffer = [0; 4096];
                    while let Ok(count) = stream.read(&mut buffer).await {
                        if count == 0 {
                            break;
                        }
                        let _ = stream
                            .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n")
                            .await;
                    }
                });
            }
        });
        let client = get_proxy_fetch_options(true).unwrap().build().unwrap();
        for _ in 0..requests {
            let response = client.get(format!("http://{address}/")).send().await.unwrap();
            assert_eq!(response.status(), 204);
        }
        server.abort();
        accepted.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// CC `disableKeepAlive`: once set, each request opens its own connection.
    #[tokio::test]
    async fn disabled_keep_alive_stops_pooling() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = clear_proxy_env();
        let _socket = EnvVarGuard::unset("ANTHROPIC_UNIX_SOCKET");
        // reqwest's system proxy matcher reads the OS `NO_PROXY`, which
        // `just test` pins to loopback; this carrier value no longer reaches it.
        let _no_proxy = EnvVarGuard::set("NO_PROXY", "127.0.0.1");
        reset_keep_alive_for_testing();
        assert_eq!(connections_for(2).await, 1);
        disable_keep_alive();
        assert_eq!(connections_for(2).await, 2);
        reset_keep_alive_for_testing();
    }

    /// CC `getProxyFetchOptions({ forAnthropicAPI: true })` under Bun: the
    /// socket alone. A proxy in the environment adds no credentials to it.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_carries_only_the_anthropic_api_client() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = clear_proxy_env();
        // Socket paths are capped near 104 bytes on macOS.
        let path = std::env::temp_dir()
            .join(format!("cx-{}.sock", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let _socket = EnvVarGuard::set("ANTHROPIC_UNIX_SOCKET", &path);
        let _proxy = EnvVarGuard::set("HTTP_PROXY", "http://user:secret@127.0.0.1:9");
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
                .write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n")
                .await
                .unwrap();
            String::from_utf8_lossy(&request).to_ascii_lowercase()
        });
        let client = get_proxy_fetch_options(true).unwrap().build().unwrap();
        let response = client.get("http://localhost/v1/models").send().await.unwrap();
        assert_eq!(response.status(), 204);
        let request = server.await.unwrap();
        assert!(request.starts_with("get /v1/models http/1.1"), "{request}");
        assert!(!request.contains("proxy-authorization"), "{request}");

        // Other fetch clients take the proxy instead.
        crate::utils::process_env::set("HTTP_PROXY", "ftp://proxy:21");
        assert!(get_proxy_fetch_options(false).is_err());
        let _ = std::fs::remove_file(path);
    }
}
