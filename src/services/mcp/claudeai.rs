//! Claude.ai managed MCP connector discovery.
//! Maps to: CC `services/mcp/claudeai.ts`.
//!
//! Analytics logging in CC is intentionally not implemented here per project
//! policy; this module preserves the eligibility states as debug strings only.

use crate::services::mcp::normalization::normalize_name_for_mcp;
use crate::services::mcp::types::{ConfigScope, ScopedMcpServerConfig, Transport};
use std::collections::{BTreeMap, BTreeSet};

const MCP_SERVERS_BETA_HEADER: &str = "mcp-servers-2025-12-04";

fn debug_log(message: impl AsRef<str>) {
    crate::utils::debug::log_for_debugging(message.as_ref());
}

#[cfg(feature = "mcp_runtime")]
mod runtime {
    use super::*;
    use serde::Deserialize;
    use std::sync::{LazyLock, Mutex};
    use std::time::Duration;

    const FETCH_TIMEOUT_MS: u64 = 5_000;

    #[derive(Debug, Deserialize)]
    struct ClaudeAiMcpServer {
        id: String,
        display_name: String,
        url: String,
    }

    #[derive(Debug, Deserialize)]
    struct ClaudeAiMcpServersResponse {
        data: Vec<ClaudeAiMcpServer>,
    }

    // lodash memoize caches the Promise immediately, not its eventual value.
    // Clearing only drops this reference: an older request may still resolve
    // for its callers but must never write itself back into the new cache.
    static CLAUDE_AI_MCP_CONFIGS_CACHE: LazyLock<
        Mutex<Option<super::super::config::McpConfigPromise>>,
    > = LazyLock::new(|| Mutex::new(None));

    fn memoized_config_promise(
        cache: &Mutex<Option<super::super::config::McpConfigPromise>>,
        create: impl FnOnce() -> super::super::config::McpConfigPromise,
    ) -> super::super::config::McpConfigPromise {
        let mut cached = cache.lock().expect("claude.ai MCP config cache");
        cached.get_or_insert_with(create).clone()
    }

    /// Maps to: CC services/mcp/claudeai.ts#fetchClaudeAIMcpConfigsIfEligible,
    /// including lodash memoize's cached in-flight Promise identity.
    pub fn fetch_claude_ai_mcp_configs_if_eligible() -> super::super::config::McpConfigPromise {
        memoized_config_promise(&CLAUDE_AI_MCP_CONFIGS_CACHE, || {
            super::super::config::start_mcp_config_promise(async {
                let get_env = |key: &str| crate::utils::process_env::var(key);
                if crate::utils::env_utils::is_env_defined_falsy(
                    get_env("ENABLE_CLAUDEAI_MCP_SERVERS").as_deref(),
                ) {
                    debug_log("[claudeai-mcp] Disabled via env var");
                    return indexmap::IndexMap::new();
                }

                let Some(tokens) = crate::utils::auth::get_claude_ai_oauth_tokens() else {
                    debug_log("[claudeai-mcp] No access token");
                    return indexmap::IndexMap::new();
                };
                if !tokens
                    .scopes
                    .iter()
                    .any(|scope| scope == "user:mcp_servers")
                {
                    debug_log(format!(
                        "[claudeai-mcp] Missing user:mcp_servers scope (scopes={})",
                        if tokens.scopes.is_empty() {
                            "none".to_string()
                        } else {
                            tokens.scopes.join(",")
                        }
                    ));
                    return indexmap::IndexMap::new();
                }

                let base_url = crate::constants::oauth::get_oauth_config()
                    .map(|config| config.base_api_url)
                    .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
                let url = format!("{base_url}/v1/mcp_servers?limit=1000");
                debug_log(format!("[claudeai-mcp] Fetching from {url}"));

                let result = async {
                    // CC `axios.get` (`claudeai.ts:82-90`), through the
                    // global interceptor.
                    let response = crate::utils::proxy::create_axios_instance()?
                        .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
                        .build()?
                        .get(&url)
                        .header(
                            reqwest::header::AUTHORIZATION,
                            format!("Bearer {}", tokens.access_token),
                        )
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .header("anthropic-beta", MCP_SERVERS_BETA_HEADER)
                        .header("anthropic-version", "2023-06-01")
                        .send()
                        .await?
                        .error_for_status()?
                        .json::<ClaudeAiMcpServersResponse>()
                        .await?;
                    let mut configs = indexmap::IndexMap::new();
                    let mut used_normalized_names = BTreeSet::new();
                    for server in response.data {
                        let base_name = format!("claude.ai {}", server.display_name);
                        let mut final_name = base_name.clone();
                        let mut final_normalized = normalize_name_for_mcp(&final_name);
                        let mut count = 1;
                        while used_normalized_names.contains(&final_normalized) {
                            count += 1;
                            final_name = format!("{base_name} ({count})");
                            final_normalized = normalize_name_for_mcp(&final_name);
                        }
                        used_normalized_names.insert(final_normalized);
                        configs.insert(
                            final_name,
                            ScopedMcpServerConfig {
                                name: None,
                                scope: ConfigScope::ClaudeAi,
                                transport: Transport::ClaudeAiProxy,
                                command: None,
                                args: Vec::new(),
                                env: BTreeMap::new(),
                                url: Some(server.url),
                                headers: BTreeMap::new(),
                                headers_helper: None,
                                oauth: None,
                                ide_running_in_windows: None,
                                ide_name: None,
                                auth_token: None,
                                id: Some(server.id),
                                plugin_source: None,
                            },
                        );
                    }
                    anyhow::Ok(configs)
                }
                .await;

                match result {
                    Ok(configs) => {
                        debug_log(format!("[claudeai-mcp] Fetched {} servers", configs.len()));
                        configs
                    }
                    Err(_) => {
                        debug_log("[claudeai-mcp] Fetch failed");
                        indexmap::IndexMap::new()
                    }
                }
            })
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn memoized_claudeai_promise_matches_source_inflight_and_clear_oracle() {
            crate::utils::process_runtime::initialize_test_process_runtime();
            let oracle: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/oracles/mcp-discovery-0915/claudeai-cache-oracle.json")).unwrap();
            let cache = Mutex::new(None);
            let (old_tx, old_rx) = futures::channel::oneshot::channel();
            let old = memoized_config_promise(&cache, || {
                super::super::super::config::start_mcp_config_promise(async move {
                    old_rx.await.unwrap()
                })
            });
            let same = memoized_config_promise(&cache, || panic!("must reuse pending Promise"));
            *cache.lock().unwrap() = None;
            let (new_tx, new_rx) = futures::channel::oneshot::channel();
            let fresh = memoized_config_promise(&cache, || {
                super::super::super::config::start_mcp_config_promise(async move {
                    new_rx.await.unwrap()
                })
            });
            let configs = |name: &str| {
                indexmap::IndexMap::from([(
                    name.to_owned(),
                    ScopedMcpServerConfig::from_config(
                        ConfigScope::ClaudeAi,
                        &crate::utils::config::McpServerConfig::default(),
                    ),
                )])
            };
            new_tx.send(configs("claude.ai new")).unwrap();
            let fresh_result = futures::executor::block_on(fresh);
            old_tx.send(configs("claude.ai old")).unwrap();
            let old_result = futures::executor::block_on(old);
            assert_eq!(old_result, futures::executor::block_on(same));
            let after = futures::executor::block_on(memoized_config_promise(&cache, || {
                panic!("old completion must not evict fresh Promise")
            }));
            assert_eq!(after, fresh_result);
            assert_eq!(
                serde_json::json!(after.keys().collect::<Vec<_>>()),
                oracle["afterOldCompletionKeys"]
            );
            assert_eq!(
                serde_json::json!(old_result.keys().collect::<Vec<_>>()),
                oracle["oldKeys"]
            );
        }
    }

    /// Maps to: CC `services/mcp/claudeai.ts#clearClaudeAIMcpConfigsCache`.
    pub fn clear_claude_ai_mcp_configs_cache() {
        *CLAUDE_AI_MCP_CONFIGS_CACHE
            .lock()
            .expect("claude.ai MCP config cache") = None;
        crate::services::mcp::client::clear_mcp_auth_cache();
    }
}

#[cfg(feature = "mcp_runtime")]
pub use runtime::{clear_claude_ai_mcp_configs_cache, fetch_claude_ai_mcp_configs_if_eligible};

#[cfg(not(feature = "mcp_runtime"))]
pub async fn fetch_claude_ai_mcp_configs_if_eligible()
-> indexmap::IndexMap<String, ScopedMcpServerConfig> {
    indexmap::IndexMap::new()
}

#[cfg(not(feature = "mcp_runtime"))]
pub fn clear_claude_ai_mcp_configs_cache() {
    crate::services::mcp::client::clear_mcp_auth_cache();
}

/// Maps to: CC `services/mcp/claudeai.ts#markClaudeAiMcpConnected`.
pub fn mark_claude_ai_mcp_connected(name: &str) -> anyhow::Result<()> {
    crate::utils::config::save_global_config(|config| {
        let seen = config
            .claude_ai_mcp_ever_connected
            .get_or_insert_with(Vec::new);
        if !seen.iter().any(|existing| existing == name) {
            seen.push(name.to_string());
        }
    })
}

/// Maps to: CC `services/mcp/claudeai.ts#hasClaudeAiMcpEverConnected`.
pub fn has_claude_ai_mcp_ever_connected(name: &str) -> bool {
    crate::utils::config::load_global_config()
        .claude_ai_mcp_ever_connected
        .unwrap_or_default()
        .iter()
        .any(|existing| existing == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_defined_falsy_matches_official_claudeai_gate() {
        assert!(crate::utils::env_utils::is_env_defined_falsy(Some("false")));
        assert!(crate::utils::env_utils::is_env_defined_falsy(Some("0")));
        assert!(!crate::utils::env_utils::is_env_defined_falsy(Some("")));
        assert!(!crate::utils::env_utils::is_env_defined_falsy(None));
        assert!(!crate::utils::env_utils::is_env_defined_falsy(Some("true")));
    }
}
