//! Maps to: undici 7.24.6 `lib/dispatcher/env-http-proxy-agent.js`, the
//! dispatcher CC's `getProxyAgent` builds (`utils/proxy.ts:198-237`).
//!
//! undici's agent sends each request to one of three agents, chosen by the
//! request origin and `noProxy`: a direct one, the HTTP proxy, or the HTTPS
//! proxy. Here only that choice is ported. reqwest's `Proxy::custom` asks
//! [`EnvHttpProxyAgent::get_proxy_for_url`] for each connection, and the
//! agents are the reqwest client itself.

use crate::utils::process_env::JsTruthy;

/// `DEFAULT_PORTS` (`:8-11`); anything else is `0`.
fn default_port(scheme: &str) -> u64 {
    match scheme {
        "http" => 80,
        "https" => 443,
        _ => 0,
    }
}

/// Maps to: undici `EnvHttpProxyAgent`.
#[derive(Clone, Debug)]
pub struct EnvHttpProxyAgent {
    http_proxy: Option<String>,
    https_proxy: Option<String>,
    /// `opts.noProxy`. `None` reads the environment on every request, as
    /// undici's `#noProxyChanged` re-parse does.
    no_proxy: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct NoProxyEntry {
    hostname: String,
    /// `0` is "any port".
    port: u64,
}

impl EnvHttpProxyAgent {
    /// Maps to: the constructor (`:18-41`). Each proxy is
    /// `opts ?? lowercase env ?? uppercase env`; an empty one means the
    /// direct agent, and an unset HTTPS proxy falls back to the HTTP one.
    pub fn new(
        http_proxy: Option<String>,
        https_proxy: Option<String>,
        no_proxy: Option<String>,
    ) -> Self {
        let env = crate::utils::process_env::snapshot();
        let from_env = |lower: &str, upper: &str| {
            env.var(lower).or_else(|| env.var(upper)).map(str::to_owned)
        };
        let http_proxy = http_proxy
            .or_else(|| from_env("http_proxy", "HTTP_PROXY"))
            .truthy();
        let https_proxy = https_proxy
            .or_else(|| from_env("https_proxy", "HTTPS_PROXY"))
            .truthy()
            .or_else(|| http_proxy.clone());
        Self {
            http_proxy,
            https_proxy,
            no_proxy,
        }
    }

    /// Maps to: `#getProxyAgentForUrl` (`:65-79`): the proxy for `url`'s
    /// origin, `None` for the direct agent. The host keeps IPv6 brackets;
    /// `parseInt(port) || DEFAULT_PORTS[protocol] || 0` makes port 0 the
    /// scheme's default too.
    pub fn get_proxy_for_url(&self, url: &reqwest::Url) -> Option<&str> {
        let hostname = url.host_str().unwrap_or_default().to_lowercase();
        let port = url
            .port()
            .map(u64::from)
            .filter(|port| *port != 0)
            .unwrap_or_else(|| default_port(url.scheme()));
        if !self.should_proxy(&hostname, port) {
            return None;
        }
        if url.scheme() == "https" {
            self.https_proxy.as_deref()
        } else {
            self.http_proxy.as_deref()
        }
    }

    /// Maps to: `#shouldProxy` (`:81-110`).
    fn should_proxy(&self, hostname: &str, port: u64) -> bool {
        let no_proxy_value = match &self.no_proxy {
            Some(value) => value.clone(),
            None => no_proxy_env(),
        };
        let entries = parse_no_proxy(&no_proxy_value);
        if entries.is_empty() {
            return true;
        }
        if no_proxy_value == "*" {
            return false;
        }
        !entries.iter().any(|entry| {
            (entry.port == 0 || entry.port == port)
                && (hostname == entry.hostname
                    || hostname.ends_with(&format!(".{}", entry.hostname)))
        })
    }
}

/// Maps to: `#noProxyEnv` (`:141-143`): `no_proxy ?? NO_PROXY ?? ''`.
fn no_proxy_env() -> String {
    let env = crate::utils::process_env::snapshot();
    env.var("no_proxy")
        .or_else(|| env.var("NO_PROXY"))
        .unwrap_or_default()
        .to_owned()
}

/// Maps to: `#parseNoProxy` (`:112-132`). Entries split on `/[,\s]/`; a
/// trailing `:digits` is the port (`/^(.+):(\d+)$/`), and a leading `.` or
/// `*.` is dropped.
fn parse_no_proxy(value: &str) -> Vec<NoProxyEntry> {
    value
        .split(|c: char| c == ',' || (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (host, port) = match entry.rsplit_once(':') {
                Some((host, digits))
                    if !host.is_empty()
                        && !digits.is_empty()
                        && digits.bytes().all(|byte| byte.is_ascii_digit()) =>
                {
                    // `Number.parseInt` of a digit run; one too long for u64
                    // is no real port.
                    (host, digits.parse().unwrap_or(u64::MAX))
                }
                _ => (entry, 0),
            };
            let host = host
                .strip_prefix("*.")
                .or_else(|| host.strip_prefix('.'))
                .unwrap_or(host);
            NoProxyEntry {
                hostname: host.to_lowercase(),
                port,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn agent(no_proxy: &str) -> EnvHttpProxyAgent {
        EnvHttpProxyAgent::new(
            Some("http://proxy:3128".into()),
            Some("http://proxy:3128".into()),
            Some(no_proxy.into()),
        )
    }

    fn proxied(agent: &EnvHttpProxyAgent, url: &str) -> bool {
        agent.get_proxy_for_url(&url.parse().unwrap()).is_some()
    }

    #[test]
    fn no_proxy_entries_follow_undici() {
        // Empty: always proxy. Whole value `*`: never.
        assert!(proxied(&agent(""), "https://api.anthropic.com"));
        assert!(!proxied(&agent("*"), "https://api.anthropic.com"));
        assert!(!proxied(&agent("*"), "http://127.0.0.1:8080"));
        // `*` inside a list is only a host name.
        assert!(proxied(&agent("a.test,*"), "https://api.anthropic.com"));

        // A bare name matches itself and its subdomains, unlike CC's own
        // `shouldBypassProxy`; `.x` and `*.x` are the same.
        for entry in ["anthropic.com", ".anthropic.com", "*.anthropic.com"] {
            assert!(!proxied(&agent(entry), "https://api.anthropic.com"), "{entry}");
            assert!(!proxied(&agent(entry), "https://anthropic.com"), "{entry}");
            assert!(proxied(&agent(entry), "https://notanthropic.com"), "{entry}");
        }

        // Ports filter first; the default port comes from the scheme.
        assert!(!proxied(&agent("example.com:443"), "https://example.com"));
        assert!(proxied(&agent("example.com:443"), "http://example.com"));
        assert!(!proxied(&agent("example.com:8080"), "http://sub.example.com:8080"));
        // Port 0 is `parseInt('0') || 80`.
        assert!(!proxied(&agent("example.com:80"), "http://example.com:0"));

        // Separators are commas or single whitespace; case is ignored.
        assert!(!proxied(&agent("a.test  EXAMPLE.com"), "https://example.com"));

        // IPv6 keeps its brackets.
        assert!(!proxied(&agent("[::1]"), "http://[::1]:8080"));
        assert!(!proxied(&agent("[::1]:8080"), "http://[::1]:8080"));
        assert!(proxied(&agent("::1"), "http://[::1]:8080"));
    }

    #[test]
    fn unset_no_proxy_is_read_from_the_environment_on_each_request() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env: Vec<_> = ["no_proxy", "NO_PROXY", "https_proxy", "HTTPS_PROXY"]
            .into_iter()
            .map(EnvVarGuard::unset)
            .collect();
        let agent = EnvHttpProxyAgent::new(Some("http://proxy:3128".into()), None, None);
        let url: reqwest::Url = "https://example.com".parse().unwrap();
        // The HTTPS proxy falls back to the HTTP one.
        assert_eq!(agent.get_proxy_for_url(&url), Some("http://proxy:3128"));

        crate::utils::process_env::set("NO_PROXY", "example.com");
        assert_eq!(agent.get_proxy_for_url(&url), None);
        // `no_proxy ?? NO_PROXY`: a set lowercase value wins, even when empty.
        crate::utils::process_env::set("no_proxy", "");
        assert_eq!(agent.get_proxy_for_url(&url), Some("http://proxy:3128"));
    }
}
