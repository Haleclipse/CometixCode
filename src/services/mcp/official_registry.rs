//! Official MCP registry cache.
//! Maps to: CC `services/mcp/officialRegistry.ts`.

#[cfg(feature = "mcp_runtime")]
mod runtime {
    use serde::Deserialize;
    use std::collections::BTreeSet;
    use std::sync::{LazyLock, Mutex};
    use std::time::Duration;

    use crate::utils::process_env::JsTruthy;

    const REGISTRY_URL: &str =
        "https://api.anthropic.com/mcp-registry/v0/servers?version=latest&visibility=commercial";
    const FETCH_TIMEOUT_MS: u64 = 5_000;

    #[derive(Debug, Deserialize)]
    struct RegistryRemote {
        url: String,
    }

    #[derive(Debug, Deserialize)]
    struct RegistryServerBody {
        remotes: Option<Vec<RegistryRemote>>,
    }

    #[derive(Debug, Deserialize)]
    struct RegistryServer {
        server: RegistryServerBody,
    }

    #[derive(Debug, Deserialize)]
    struct RegistryResponse {
        servers: Vec<RegistryServer>,
    }

    static OFFICIAL_URLS: LazyLock<Mutex<Option<BTreeSet<String>>>> =
        LazyLock::new(|| Mutex::new(None));

    /// Maps to: CC `services/mcp/officialRegistry.ts#normalizeUrl`.
    fn normalize_url(url: &str) -> Option<String> {
        let mut parsed = reqwest::Url::parse(url).ok()?;
        parsed.set_query(None);
        Some(parsed.to_string().trim_end_matches('/').to_string())
    }

    fn debug_log(message: impl AsRef<str>) {
        crate::utils::debug::log_for_debugging(message.as_ref());
    }

    /// Maps to: CC `services/mcp/officialRegistry.ts#prefetchOfficialMcpUrls`.
    pub async fn prefetch_official_mcp_urls() {
        if crate::utils::process_env::var_os("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
            .as_deref()
            .truthy()
            .is_some()
        {
            return;
        }

        let result = async {
            // CC `axios.get` (`officialRegistry.ts:39-42`), through the global
            // interceptor.
            let response = crate::utils::proxy::create_axios_instance()?
                .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
                .build()?
                .get(REGISTRY_URL)
                .send()
                .await?
                .error_for_status()?
                .json::<RegistryResponse>()
                .await?;

            let mut urls = BTreeSet::new();
            for entry in response.servers {
                for remote in entry.server.remotes.unwrap_or_default() {
                    if let Some(normalized) = normalize_url(&remote.url) {
                        urls.insert(normalized);
                    }
                }
            }
            anyhow::Ok(urls)
        }
        .await;

        match result {
            Ok(urls) => {
                debug_log(format!(
                    "[mcp-registry] Loaded {} official MCP URLs",
                    urls.len()
                ));
                *OFFICIAL_URLS.lock().expect("official MCP URL cache") = Some(urls);
            }
            Err(error) => {
                debug_log(format!("Failed to fetch MCP registry: {error}"));
            }
        }
    }

    /// Maps to: CC `services/mcp/officialRegistry.ts#isOfficialMcpUrl`.
    pub fn is_official_mcp_url(normalized_url: &str) -> bool {
        OFFICIAL_URLS
            .lock()
            .expect("official MCP URL cache")
            .as_ref()
            .is_some_and(|urls| urls.contains(normalized_url))
    }

    /// Maps to: CC `services/mcp/officialRegistry.ts#resetOfficialMcpUrlsForTesting`.
    pub fn reset_official_mcp_urls_for_testing() {
        *OFFICIAL_URLS.lock().expect("official MCP URL cache") = None;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn normalize_url_strips_query_and_trailing_slash_like_official_registry() {
            assert_eq!(
                normalize_url("https://example.com/mcp/?token=secret"),
                Some("https://example.com/mcp".to_string())
            );
            assert_eq!(normalize_url("not a url"), None);
        }

        #[test]
        fn official_mcp_url_lookup_fails_closed_until_registry_is_loaded() {
            reset_official_mcp_urls_for_testing();
            assert!(!is_official_mcp_url("https://example.com/mcp"));
            *OFFICIAL_URLS.lock().expect("official MCP URL cache") =
                Some(BTreeSet::from(["https://example.com/mcp".to_string()]));
            assert!(is_official_mcp_url("https://example.com/mcp"));
            reset_official_mcp_urls_for_testing();
            assert!(!is_official_mcp_url("https://example.com/mcp"));
        }
    }
}

#[cfg(feature = "mcp_runtime")]
pub use runtime::{
    is_official_mcp_url, prefetch_official_mcp_urls, reset_official_mcp_urls_for_testing,
};

#[cfg(not(feature = "mcp_runtime"))]
pub async fn prefetch_official_mcp_urls() {}

#[cfg(not(feature = "mcp_runtime"))]
pub fn is_official_mcp_url(_normalized_url: &str) -> bool {
    false
}

#[cfg(not(feature = "mcp_runtime"))]
pub fn reset_official_mcp_urls_for_testing() {}
