//! HTTP hook execution.
//! Maps to: CC `utils/hooks/execHttpHook.ts`.
//!
//! This module owns HTTP hook policy checks, header interpolation, SSRF guard
//! preflight, timeout handling, and response shaping. It deliberately stays out
//! of hook matching/orchestration; callers decide when an HTTP hook should run.

use super::ssrf_guard::ssrf_guarded_lookup;
use crate::utils::settings::SettingsJson;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;

/// Maps to: CC `DEFAULT_HTTP_HOOK_TIMEOUT_MS`.
pub const DEFAULT_HTTP_HOOK_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

/// Maps to: CC `HttpHook` from `schemas/hooks.ts`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpHook {
    pub url: String,
    pub timeout: Option<u64>,
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub allowed_env_vars: Option<Vec<String>>,
    /// Maps to schemas/hooks.ts `statusMessage`; execution ignores it but UI
    /// display helpers can show it.
    pub status_message: Option<String>,
    #[serde(rename = "if")]
    pub condition: Option<String>,
    pub once: Option<bool>,
}

/// Maps to: CC `getHttpHookPolicy()` return shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpHookPolicy {
    pub allowed_urls: Option<Vec<String>>,
    pub allowed_env_vars: Option<Vec<String>>,
}

/// Maps to: CC `execHttpHook(...)` return value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecHttpHookResult {
    pub ok: bool,
    pub status_code: Option<u16>,
    pub body: String,
    pub error: Option<String>,
    pub aborted: bool,
}

/// Maps to: CC `getHttpHookPolicy()`.
pub fn get_http_hook_policy_from_settings(settings: &SettingsJson) -> HttpHookPolicy {
    HttpHookPolicy {
        allowed_urls: settings.allowed_http_hook_urls.clone(),
        allowed_env_vars: settings.http_hook_allowed_env_vars.clone(),
    }
}

/// Maps to: CC `urlMatchesPattern(url, pattern)`.
pub fn url_matches_pattern(url: &str, pattern: &str) -> bool {
    let regex = format!("^{}$", regex::escape(pattern).replace("\\*", ".*"));
    regex::Regex::new(&regex)
        .map(|regex| regex.is_match(url))
        .unwrap_or(false)
}

/// Maps to: CC `sanitizeHeaderValue(value)`.
pub fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n' | '\0'))
        .collect()
}

fn is_env_var_start(ch: u8) -> bool {
    ch == b'_' || ch.is_ascii_uppercase()
}

fn is_env_var_continue(ch: u8) -> bool {
    ch == b'_' || ch.is_ascii_uppercase() || ch.is_ascii_digit()
}

/// Testable form of CC `interpolateEnvVars(...)`.
pub fn interpolate_env_vars_with(
    value: &str,
    allowed_env_vars: &HashSet<String>,
    get_env: impl Fn(&str) -> Option<String>,
) -> String {
    let bytes = value.as_bytes();
    let mut output = String::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'$' {
            output.push(bytes[index] as char);
            index += 1;
            continue;
        }

        if index + 1 < bytes.len() && bytes[index + 1] == b'{' {
            let name_start = index + 2;
            if name_start < bytes.len() && is_env_var_start(bytes[name_start]) {
                let mut name_end = name_start + 1;
                while name_end < bytes.len() && is_env_var_continue(bytes[name_end]) {
                    name_end += 1;
                }
                if name_end < bytes.len() && bytes[name_end] == b'}' {
                    let name = &value[name_start..name_end];
                    if allowed_env_vars.contains(name) {
                        output.push_str(&get_env(name).unwrap_or_default());
                    }
                    index = name_end + 1;
                    continue;
                }
            }
        } else if index + 1 < bytes.len() && is_env_var_start(bytes[index + 1]) {
            let name_start = index + 1;
            let mut name_end = name_start + 1;
            while name_end < bytes.len() && is_env_var_continue(bytes[name_end]) {
                name_end += 1;
            }
            let name = &value[name_start..name_end];
            if allowed_env_vars.contains(name) {
                output.push_str(&get_env(name).unwrap_or_default());
            }
            index = name_end;
            continue;
        }

        output.push('$');
        index += 1;
    }

    sanitize_header_value(&output)
}

/// Maps to: CC `interpolateEnvVars(...)`.
pub fn interpolate_env_vars(value: &str, allowed_env_vars: &HashSet<String>) -> String {
    interpolate_env_vars_with(value, allowed_env_vars, |name| crate::utils::process_env::var(name))
}

fn effective_allowed_env_vars(hook: &HttpHook, policy: &HttpHookPolicy) -> HashSet<String> {
    let hook_vars = hook.allowed_env_vars.clone().unwrap_or_default();
    match &policy.allowed_env_vars {
        Some(policy_vars) => {
            let policy_set = policy_vars.iter().cloned().collect::<HashSet<_>>();
            hook_vars
                .into_iter()
                .filter(|name| policy_set.contains(name))
                .collect()
        }
        None => hook_vars.into_iter().collect(),
    }
}

fn build_headers_with(
    hook: &HttpHook,
    policy: &HttpHookPolicy,
    get_env: impl Fn(&str) -> Option<String>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let Some(hook_headers) = &hook.headers else {
        return Ok(headers);
    };

    let allowed_env_vars = effective_allowed_env_vars(hook, policy);
    for (name, value) in hook_headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("Invalid HTTP hook header name {name:?}: {error}"))?;
        let value = interpolate_env_vars_with(value, &allowed_env_vars, &get_env);
        let header_value = HeaderValue::from_str(&value)
            .map_err(|error| format!("Invalid HTTP hook header value for {name:?}: {error}"))?;
        headers.insert(header_name, header_value);
    }

    Ok(headers)
}

fn validate_url_policy(hook: &HttpHook, policy: &HttpHookPolicy) -> Option<String> {
    let allowed_urls = policy.allowed_urls.as_ref()?;
    if allowed_urls
        .iter()
        .any(|pattern| url_matches_pattern(&hook.url, pattern))
    {
        return None;
    }
    Some(format!(
        "HTTP hook blocked: {} does not match any pattern in allowedHttpHookUrls",
        hook.url
    ))
}

fn validate_ssrf_preflight(url: &reqwest::Url) -> Result<(), String> {
    let Some(host) = url.host_str() else {
        return Err(format!("HTTP hook URL has no host: {url}"));
    };
    ssrf_guarded_lookup(host)
        .map(|_| ())
        .map_err(|error| error.message)
}

/// Execute an HTTP hook with an explicit policy snapshot.
/// Maps to: CC `execHttpHook(...)` with `getHttpHookPolicy()` injected for
/// deterministic tests and service callers that already loaded settings.
pub async fn exec_http_hook_with_policy(
    hook: &HttpHook,
    json_input: &str,
    policy: &HttpHookPolicy,
) -> ExecHttpHookResult {
    if let Some(error) = validate_url_policy(hook, policy) {
        return ExecHttpHookResult {
            ok: false,
            body: String::new(),
            error: Some(error),
            ..Default::default()
        };
    }

    let url = match reqwest::Url::parse(&hook.url) {
        Ok(url) => url,
        Err(error) => {
            return ExecHttpHookResult {
                ok: false,
                body: String::new(),
                error: Some(error.to_string()),
                ..Default::default()
            };
        }
    };

    // Maps to: CC `execHttpHook.ts:173-199`. The env-var proxy (HTTP_PROXY /
    // HTTPS_PROXY, respecting NO_PROXY) resolves the target itself.
    // DEVIATION(SCOPE): the sandbox network proxy (`getSandboxProxyConfig`,
    // `:21-41`) is not wired here yet.
    // Cometix-specific deviation: CC tests `getProxyUrl() !== undefined`, so a
    // proxy variable set to "" drops the SSRF guard while `configureGlobalAgents`
    // (`if (proxyUrl)`) installs no proxy: an unguarded direct request. Here an
    // empty value is no proxy, and the guard stays.
    let env = crate::utils::process_env::snapshot();
    let env_proxy_active = crate::utils::proxy::get_proxy_url(&env).is_some()
        && !crate::utils::proxy::should_bypass_proxy(
            &hook.url,
            crate::utils::proxy::get_no_proxy(&env).as_deref(),
        );
    if env_proxy_active {
        crate::utils::debug::log_for_debugging(&format!(
            "Hooks: HTTP hook POST to {} (via env-var proxy)",
            hook.url
        ));
    } else {
        crate::utils::debug::log_for_debugging(&format!("Hooks: HTTP hook POST to {}", hook.url));
    }

    // SSRF guard (`:210-215`): skipped when a proxy is in use. The proxy
    // resolves the target, and a corporate proxy on a private IP must not be
    // blocked. Cometix checks the target before connecting, where CC guards
    // the connection's DNS lookup.
    if !env_proxy_active {
        if let Err(error) = validate_ssrf_preflight(&url) {
            return ExecHttpHookResult {
                ok: false,
                body: String::new(),
                error: Some(error),
                ..Default::default()
            };
        }
    }

    let headers = match build_headers_with(hook, policy, |name| crate::utils::process_env::var(name)) {
        Ok(headers) => headers,
        Err(error) => {
            return ExecHttpHookResult {
                ok: false,
                body: String::new(),
                error: Some(error),
                ..Default::default()
            };
        }
    };

    let timeout_ms = hook
        .timeout
        .map(|seconds| seconds * 1_000)
        .unwrap_or(DEFAULT_HTTP_HOOK_TIMEOUT_MS);
    // CC posts through the global axios instance, whose interceptor
    // `configureGlobalAgents` installs; `create_axios_instance` resolves the
    // same proxy, NO_PROXY, mTLS and CA options. `maxRedirects: 0`.
    let client = match crate::utils::proxy::create_axios_instance().and_then(|builder| {
        builder
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(anyhow::Error::from)
    }) {
        Ok(client) => client,
        Err(error) => {
            return ExecHttpHookResult {
                ok: false,
                body: String::new(),
                error: Some(error.to_string()),
                ..Default::default()
            };
        }
    };

    match client
        .post(url)
        .headers(headers)
        .body(json_input.to_string())
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            ExecHttpHookResult {
                ok: status.is_success(),
                status_code: Some(status.as_u16()),
                body,
                error: None,
                aborted: false,
            }
        }
        Err(error) => ExecHttpHookResult {
            ok: false,
            status_code: error.status().map(|status| status.as_u16()),
            body: String::new(),
            error: if error.is_timeout() {
                None
            } else {
                Some(error.to_string())
            },
            aborted: error.is_timeout(),
        },
    }
}

/// Maps to: CC `execHttpHook(...)` loading merged settings internally.
pub async fn exec_http_hook(hook: &HttpHook, json_input: &str) -> ExecHttpHookResult {
    let settings = crate::utils::settings::get_initial_settings();
    let policy = get_http_hook_policy_from_settings(&settings);
    exec_http_hook_with_policy(hook, json_input, &policy).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn http_hook(url: String) -> HttpHook {
        HttpHook {
            url,
            timeout: Some(5),
            headers: None,
            allowed_env_vars: None,
            status_message: None,
            condition: None,
            once: None,
        }
    }

    #[test]
    fn url_patterns_match_official_wildcard_semantics() {
        assert!(url_matches_pattern(
            "https://hooks.example.com/a/b",
            "https://hooks.example.com/*"
        ));
        assert!(url_matches_pattern(
            "https://hooks.example.com/a?x=1",
            "https://hooks.example.com/a?x=*"
        ));
        assert!(!url_matches_pattern(
            "https://evil.example.com/a",
            "https://hooks.example.com/*"
        ));
    }

    #[test]
    fn header_interpolation_allowlists_and_sanitizes_env_values() {
        let allowed = ["TOKEN".to_string()].into_iter().collect::<HashSet<_>>();
        let value =
            interpolate_env_vars_with("Bearer $TOKEN ${MISSING} $DENIED", &allowed, |name| {
                (name == "TOKEN").then(|| "abc\r\nX-Bad: 1".to_string())
            });
        assert_eq!(value, "Bearer abcX-Bad: 1  ");
    }

    #[test]
    fn policy_env_allowlist_intersects_hook_allowlist() {
        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer $TOKEN $OTHER".to_string(),
        );
        let hook = HttpHook {
            headers: Some(headers),
            allowed_env_vars: Some(vec!["TOKEN".to_string(), "OTHER".to_string()]),
            ..http_hook("http://127.0.0.1".to_string())
        };
        let policy = HttpHookPolicy {
            allowed_env_vars: Some(vec!["TOKEN".to_string()]),
            ..Default::default()
        };
        let headers = build_headers_with(&hook, &policy, |name| match name {
            "TOKEN" => Some("secret".to_string()),
            "OTHER" => Some("leak".to_string()),
            _ => None,
        })
        .expect("headers");
        assert_eq!(
            headers.get("Authorization").unwrap().to_str().unwrap(),
            "Bearer secret "
        );
    }

    #[tokio::test]
    async fn blocked_allowlist_returns_official_error_before_io() {
        let hook = http_hook("https://hooks.example.com/a".to_string());
        let result = exec_http_hook_with_policy(
            &hook,
            "{}",
            &HttpHookPolicy {
                allowed_urls: Some(vec!["https://other.example.com/*".to_string()]),
                ..Default::default()
            },
        )
        .await;
        assert!(!result.ok);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("does not match any pattern in allowedHttpHookUrls")
        );
    }

    /// No env-var proxy. The system-proxy fallback reads the OS `NO_PROXY`,
    /// which `just test` pins to loopback; the carrier value set here no
    /// longer reaches it.
    fn without_env_proxy() -> Vec<EnvVarGuard> {
        let mut guards: Vec<_> = ["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"]
            .into_iter()
            .map(EnvVarGuard::unset)
            .collect();
        guards.push(EnvVarGuard::unset("no_proxy"));
        guards.push(EnvVarGuard::set("NO_PROXY", "127.0.0.1"));
        guards
    }

    #[tokio::test]
    async fn posts_json_to_loopback_and_returns_text_response() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = without_env_proxy();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut temp = [0_u8; 1024];
            loop {
                let read = socket.read(&mut temp).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp[..read]);
                if String::from_utf8_lossy(&buffer).contains("\r\n\r\n")
                    && String::from_utf8_lossy(&buffer).contains(r#"{"hello":"world"}"#)
                {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buffer).to_string();
            assert!(request.starts_with("POST /hook HTTP/1.1"), "{request}");
            assert!(
                request.contains("content-type: application/json"),
                "{request}"
            );
            assert!(request.contains(r#"{"hello":"world"}"#), "{request}");
            socket
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Length: 16\r\nContent-Type: text/plain\r\n\r\ncreated-response",
                )
                .await
                .unwrap();
        });

        let hook = http_hook(format!("http://{addr}/hook"));
        let result =
            exec_http_hook_with_policy(&hook, r#"{"hello":"world"}"#, &Default::default()).await;
        server.await.unwrap();

        assert!(result.ok, "{result:?}");
        assert_eq!(result.status_code, Some(201));
        assert_eq!(result.body, "created-response");
    }

    #[tokio::test]
    async fn ssrf_guard_blocks_metadata_ip_before_request() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = without_env_proxy();
        let hook = http_hook("http://169.254.169.254/latest/meta-data".to_string());
        let result = exec_http_hook_with_policy(&hook, "{}", &Default::default()).await;
        assert!(!result.ok);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("private/link-local address")
        );
    }

    /// CC `execHttpHook.ts:184-215`: through an env-var proxy the target is
    /// the proxy's to resolve, so there is no SSRF check; NO_PROXY brings the
    /// check back.
    #[tokio::test]
    async fn env_proxy_carries_the_hook_and_skips_the_ssrf_guard() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = without_env_proxy();
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let _proxy_url = EnvVarGuard::set("HTTP_PROXY", format!("http://{proxy_address}"));
        let _no_proxy = EnvVarGuard::set("NO_PROXY", "169.254.169.254");
        let server = tokio::spawn(async move {
            let (mut socket, _) = proxy.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut temp = [0_u8; 1024];
            while !String::from_utf8_lossy(&buffer).contains("{}") {
                let read = socket.read(&mut temp).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp[..read]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
            String::from_utf8_lossy(&buffer).into_owned()
        });

        // A host only the proxy could resolve.
        let hook = http_hook("http://hooks.invalid/hook".to_string());
        let result = exec_http_hook_with_policy(&hook, "{}", &Default::default()).await;
        assert!(result.ok, "{result:?}");
        let request = server.await.unwrap();
        assert!(request.starts_with("POST http://hooks.invalid/hook HTTP/1.1"), "{request}");

        // Bypassed by NO_PROXY: direct, and the guard applies again.
        let hook = http_hook("http://169.254.169.254/latest/meta-data".to_string());
        let result = exec_http_hook_with_policy(&hook, "{}", &Default::default()).await;
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("private/link-local address"),
            "{result:?}"
        );
    }
}
