//! HTTP utility constants and helpers.
//! Maps to: CC `utils/http.ts`.
//!
//! Auth header helpers and OAuth retry remain in their existing official
//! service/auth boundaries. This module owns the shared user-agent helpers that
//! were previously duplicated in API/MCP/WebFetch code.

fn env_non_empty(key: &str) -> Option<String> {
    crate::utils::process_env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
}

/// The one constructor for HTTP clients.
///
/// CC `utils/proxy.ts#getProxyFetchOptions` joins through reqwest's proxy
/// support, whose own discovery reads the frozen real environment; the same
/// rules are applied to the effective `process.env` instead ([`env_proxies`]).
#[allow(clippy::disallowed_methods)] // The sanctioned `reqwest::Client::builder`.
pub fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder().no_proxy();
    let env = crate::utils::process_env::snapshot();
    let Some(proxies) = env_proxies(&env) else {
        return builder;
    };
    let no_proxy = reqwest::NoProxy::from_string(proxies.no_proxy);
    let http = proxies
        .http
        .into_iter()
        .find_map(|url| reqwest::Proxy::http(url).ok());
    let https = proxies
        .https
        .into_iter()
        .find_map(|url| reqwest::Proxy::https(url).ok());
    [http, https]
        .into_iter()
        .flatten()
        .fold(builder, |builder, proxy| {
            builder.proxy(proxy.no_proxy(no_proxy.clone()))
        })
}

/// Proxy candidates per scheme, in the order tried, and the `NO_PROXY` list.
#[derive(Debug, PartialEq)]
struct EnvProxies<'a> {
    http: Vec<&'a str>,
    https: Vec<&'a str>,
    no_proxy: &'a str,
}

/// hyper-util's `Matcher::from_env` (curl) rules: `HTTP_PROXY`/`HTTPS_PROXY`
/// by scheme, each falling back to `ALL_PROXY`, excluding `NO_PROXY`, and no
/// proxy at all under CGI (`REQUEST_METHOD`). Of each pair the uppercase name
/// wins if present at all, even empty.
fn env_proxies(env: &crate::utils::process_env::EnvSnapshot) -> Option<EnvProxies<'_>> {
    if env.contains("REQUEST_METHOD") {
        return None;
    }
    let first = |names: [&str; 2]| {
        names
            .into_iter()
            .find_map(|name| env.var(name))
            .unwrap_or("")
    };
    let candidates = |names| {
        [first(names), first(["ALL_PROXY", "all_proxy"])]
            .into_iter()
            .filter(|url| !url.is_empty())
            .collect()
    };
    Some(EnvProxies {
        http: candidates(["HTTP_PROXY", "http_proxy"]),
        https: candidates(["HTTPS_PROXY", "https_proxy"]),
        no_proxy: first(["NO_PROXY", "no_proxy"]),
    })
}

/// Maps to: CC `utils/http.ts#getUserAgent`.
pub fn get_user_agent() -> String {
    let agent_sdk_version = env_non_empty("CLAUDE_AGENT_SDK_VERSION")
        .map(|value| format!(", agent-sdk/{value}"))
        .unwrap_or_default();
    let client_app = env_non_empty("CLAUDE_AGENT_SDK_CLIENT_APP")
        .map(|value| format!(", client-app/{value}"))
        .unwrap_or_default();
    let workload = crate::utils::workload_context::get_workload()
        .map(|value| format!(", workload/{value}"))
        .unwrap_or_default();
    let user_type = crate::utils::build_profile::build_audience().as_str();
    let entrypoint = crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT")
        .unwrap_or_else(|_| "cli".to_string());
    format!(
        "claude-cli/{} ({user_type}, {entrypoint}{agent_sdk_version}{client_app}{workload})",
        crate::constants::product::USER_AGENT_VERSION
    )
}

/// Maps to: CC `utils/http.ts#getMCPUserAgent`.
pub fn get_mcp_user_agent() -> String {
    let mut parts = Vec::new();
    if let Some(entrypoint) = env_non_empty("CLAUDE_CODE_ENTRYPOINT") {
        parts.push(entrypoint);
    }
    if let Some(version) = env_non_empty("CLAUDE_AGENT_SDK_VERSION") {
        parts.push(format!("agent-sdk/{version}"));
    }
    if let Some(client_app) = env_non_empty("CLAUDE_AGENT_SDK_CLIENT_APP") {
        parts.push(format!("client-app/{client_app}"));
    }
    let suffix = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };
    format!(
        "claude-code/{}{}",
        crate::constants::product::VERSION,
        suffix
    )
}

/// Maps to: CC `utils/http.ts#getWebFetchUserAgent`.
pub fn get_web_fetch_user_agent() -> String {
    format!(
        "Claude-User ({}; +https://support.anthropic.com/)",
        crate::utils::user_agent::get_claude_code_user_agent()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        _values: Vec<crate::utils::env_utils::EnvVarGuard>,
    }

    impl EnvGuard {
        fn set(updates: &[(&'static str, Option<&str>)]) -> Self {
            let _values = updates
                .iter()
                .map(|(key, value)| match value {
                    Some(value) => crate::utils::env_utils::EnvVarGuard::set(*key, value),
                    None => crate::utils::env_utils::EnvVarGuard::unset(*key),
                })
                .collect();
            Self { _values }
        }
    }

    #[test]
    fn user_agent_matches_official_http_shape() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("CLAUDE_CODE_ENTRYPOINT", Some("cli")),
            ("CLAUDE_AGENT_SDK_VERSION", Some("1.2.3")),
            ("CLAUDE_AGENT_SDK_CLIENT_APP", Some("my-app/1.0")),
        ]);
        assert_eq!(
            get_user_agent(),
            format!(
                "claude-cli/{} ({}, cli, agent-sdk/1.2.3, client-app/my-app/1.0)",
                crate::constants::product::USER_AGENT_VERSION,
                crate::utils::build_profile::build_audience().as_str()
            )
        );
    }

    #[test]
    fn mcp_user_agent_matches_official_optional_suffixes() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("CLAUDE_CODE_ENTRYPOINT", Some("sdk")),
            ("CLAUDE_AGENT_SDK_VERSION", Some("1.2.3")),
            ("CLAUDE_AGENT_SDK_CLIENT_APP", None),
        ]);
        assert_eq!(
            get_mcp_user_agent(),
            format!(
                "claude-code/{} (sdk, agent-sdk/1.2.3)",
                crate::constants::product::VERSION
            )
        );
    }

    #[test]
    fn web_fetch_user_agent_uses_public_claude_user_identity() {
        assert_eq!(
            get_web_fetch_user_agent(),
            format!(
                "Claude-User (claude-code/{}; +https://support.anthropic.com/)",
                crate::constants::product::VERSION
            )
        );
    }

    /// hyper-util `Matcher::from_env` rules, read from the effective
    /// environment rather than the frozen real one.
    #[test]
    fn env_proxies_match_curl_rules_over_the_effective_environment() {
        use crate::utils::env_utils::EnvVarGuard;
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        // Separate bindings drop in reverse, and each lowercase name is cleared
        // before its uppercase one is set: on Windows each pair is one key.
        let _cgi = EnvVarGuard::unset("REQUEST_METHOD");
        let _all_lower = EnvVarGuard::unset("all_proxy");
        let _all = EnvVarGuard::unset("ALL_PROXY");
        let _http_lower = EnvVarGuard::unset("http_proxy");
        let _http = EnvVarGuard::set("HTTP_PROXY", "");
        let _https_lower = EnvVarGuard::unset("https_proxy");
        let _https = EnvVarGuard::set("HTTPS_PROXY", "http://secure.proxy:3128");
        let _no_lower = EnvVarGuard::unset("no_proxy");
        let _no = EnvVarGuard::set("NO_PROXY", "localhost,.internal");

        let env = crate::utils::process_env::snapshot();
        assert_eq!(
            env_proxies(&env),
            Some(EnvProxies {
                http: vec![],
                https: vec!["http://secure.proxy:3128"],
                no_proxy: "localhost,.internal",
            })
        );

        // An empty scheme variable falls through to ALL_PROXY.
        crate::utils::process_env::set("ALL_PROXY", "http://any.proxy:8080");
        let env = crate::utils::process_env::snapshot();
        let proxies = env_proxies(&env).unwrap();
        assert_eq!(proxies.http, ["http://any.proxy:8080"]);
        assert_eq!(
            proxies.https,
            ["http://secure.proxy:3128", "http://any.proxy:8080"]
        );

        crate::utils::process_env::set("REQUEST_METHOD", "GET");
        assert_eq!(env_proxies(&crate::utils::process_env::snapshot()), None);
    }

    /// A proxy named only in the effective environment carries the request.
    #[tokio::test]
    async fn client_builder_routes_through_the_effective_environment_proxy() {
        use crate::utils::env_utils::EnvVarGuard;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let _cgi = EnvVarGuard::unset("REQUEST_METHOD");
        let _no_lower = EnvVarGuard::unset("no_proxy");
        let _no = EnvVarGuard::unset("NO_PROXY");
        let _http_lower = EnvVarGuard::unset("http_proxy");
        let _http = EnvVarGuard::set("HTTP_PROXY", &proxy);

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        crate::utils::tls_provider::install_crypto_provider();
        let response = client_builder()
            .build()
            .unwrap()
            .get("http://cometix-proxy-probe.invalid/path")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        let request = server.await.unwrap();
        assert!(
            request.starts_with("GET http://cometix-proxy-probe.invalid/path HTTP/1.1"),
            "{request}"
        );
    }
}
