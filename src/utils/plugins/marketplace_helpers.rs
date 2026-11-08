//! Maps to: CC `utils/plugins/marketplaceHelpers.ts`.
//! Inputs retain the existing schema-validated JSON carrier. The full
//! `loadMarketplacesWithGracefulDegradation` retains per-marketplace failures.

use serde_json::Value;

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:16-33#formatFailureDetails`.
/// Rust carrier for the source's anonymous failure entry parameter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginFailureDetail {
    pub name: String,
    pub reason: Option<String>,
    pub error: Option<String>,
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:16-33#formatFailureDetails`.
pub fn format_failure_details(failures: &[PluginFailureDetail], include_reasons: bool) -> String {
    let max_show = 2;
    let details = failures
        .iter()
        .take(max_show)
        .map(|failure| {
            let reason = failure
                .reason
                .as_deref()
                .filter(|value| !value.is_empty())
                .or_else(|| failure.error.as_deref().filter(|value| !value.is_empty()))
                .unwrap_or("unknown error");
            if include_reasons {
                format!("{} ({reason})", failure.name)
            } else {
                failure.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(if include_reasons { "; " } else { ", " });
    if failures.len() > max_show {
        format!("{details} and {} more", failures.len() - max_show)
    } else {
        details
    }
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:38-55#getMarketplaceSourceDisplay`.
pub fn get_marketplace_source_display(source: &Value) -> String {
    match source["source"].as_str() {
        Some("github") => source["repo"]
            .as_str()
            .expect("GitHub repo is a string")
            .into(),
        Some("url" | "git") => source["url"]
            .as_str()
            .expect("source URL is a string")
            .into(),
        Some("directory" | "file") => source["path"]
            .as_str()
            .expect("source path is a string")
            .into(),
        Some("settings") => format!(
            "settings:{}",
            source["name"].as_str().expect("settings name is a string")
        ),
        _ => "Unknown source".into(),
    }
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:60-65#createPluginId`.
pub fn create_plugin_id(plugin_name: &str, marketplace_name: &str) -> String {
    format!("{plugin_name}@{marketplace_name}")
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:119-141#formatMarketplaceLoadingErrors`.
/// Rust carrier for the source's anonymous failure entry parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketplaceLoadingFailure {
    pub name: String,
    pub error: String,
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:119-141#formatMarketplaceLoadingErrors`.
/// Rust carrier for the source's literal `type` union in the return object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketplaceLoadingErrorType {
    Warning,
    Error,
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:119-141#formatMarketplaceLoadingErrors`.
/// Rust carrier for the source's anonymous return object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketplaceLoadingError {
    pub r#type: MarketplaceLoadingErrorType,
    pub message: String,
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:119-141#formatMarketplaceLoadingErrors`.
pub fn format_marketplace_loading_errors(
    failures: &[MarketplaceLoadingFailure],
    success_count: usize,
) -> Option<MarketplaceLoadingError> {
    if failures.is_empty() {
        return None;
    }
    if success_count > 0 {
        let message = if failures.len() == 1 {
            format!(
                "Warning: Failed to load marketplace '{}': {}",
                failures[0].name, failures[0].error
            )
        } else {
            format!(
                "Warning: Failed to load {} marketplaces: {}",
                failures.len(),
                format_failure_names(failures)
            )
        };
        return Some(MarketplaceLoadingError {
            r#type: MarketplaceLoadingErrorType::Warning,
            message,
        });
    }
    Some(MarketplaceLoadingError {
        r#type: MarketplaceLoadingErrorType::Error,
        message: format!(
            "Failed to load all marketplaces. Errors: {}",
            format_failure_errors(failures)
        ),
    })
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:143-147#formatFailureNames`.
fn format_failure_names(failures: &[MarketplaceLoadingFailure]) -> String {
    failures
        .iter()
        .map(|failure| failure.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:149-153#formatFailureErrors`.
fn format_failure_errors(failures: &[MarketplaceLoadingFailure]) -> String {
    failures
        .iter()
        .map(|failure| format!("{}: {}", failure.name, failure.error))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:538-544#EmptyMarketplaceReason`.
/// @nyan: no managed policy layer, so no `all-blocked-by-policy` /
/// `policy-restricts-sources` reasons (nor CC's `strictKnownMarketplaces`,
/// `blockedMarketplaces`, or `pluginTrustMessage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmptyMarketplaceReason {
    GitNotInstalled,
    AllMarketplacesFailed,
    NoMarketplacesConfigured,
    AllPluginsInstalled,
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:550-592#detectEmptyMarketplaceReason`.
pub async fn detect_empty_marketplace_reason(
    configured_marketplace_count: usize,
    failed_marketplace_count: usize,
) -> EmptyMarketplaceReason {
    if !super::git_availability::check_git_available().await {
        return EmptyMarketplaceReason::GitNotInstalled;
    }
    if configured_marketplace_count == 0 {
        return EmptyMarketplaceReason::NoMarketplacesConfigured;
    }
    if failed_marketplace_count > 0 && failed_marketplace_count == configured_marketplace_count {
        return EmptyMarketplaceReason::AllMarketplacesFailed;
    }
    EmptyMarketplaceReason::AllPluginsInstalled
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:73-89` load result entries.
pub struct LoadedMarketplace {
    pub name: String,
    pub config: Value,
    pub data: Option<std::sync::Arc<Value>>,
}
/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:73-89` load result.
pub struct LoadedMarketplacesWithGracefulDegradation {
    pub marketplaces: Vec<LoadedMarketplace>,
    pub failures: Vec<MarketplaceLoadingFailure>,
}
/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:71-114#loadMarketplacesWithGracefulDegradation`.
pub async fn load_marketplaces_with_graceful_degradation(
    config: &super::marketplace_manager::KnownMarketplacesConfig,
) -> LoadedMarketplacesWithGracefulDegradation {
    let mut marketplaces = Vec::new();
    let mut failures = Vec::new();
    for (name, entry) in crate::utils::process_env::ecmascript_object_entries(config.iter()) {
        let data = match super::marketplace_manager::get_marketplace(name).await {
            Ok(data) => Some(data),
            Err(error) => {
                failures.push(MarketplaceLoadingFailure {
                    name: name.to_owned(),
                    error: error.to_string(),
                });
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                None
            }
        };
        marketplaces.push(LoadedMarketplace {
            name: name.to_owned(),
            config: entry.clone(),
            data,
        });
    }
    LoadedMarketplacesWithGracefulDegradation {
        marketplaces,
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use serde_json::json;

    #[test]
    fn discover_helpers_matches_official_bun_failure_source_and_id_oracle() {
        // CC :16-65,119-153,183-185,550-592. Original Bun 1.3.14,
        // research/proof/plugin-discover-empty-0914/oracle.json (41 vectors).
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let oracle: Value = serde_json::from_str(r###"{
  "details": [
    {"input":[],"includeReasons":false,"result":""},
    {"input":[],"includeReasons":true,"result":""},
    {"input":[{"name":"a"}],"includeReasons":false,"result":"a"},
    {"input":[{"name":"a"}],"includeReasons":true,"result":"a (unknown error)"},
    {"input":[{"name":"a","reason":"","error":""},{"name":"b","reason":" ","error":"ignored"}],"includeReasons":false,"result":"a, b"},
    {"input":[{"name":"a","reason":"","error":""},{"name":"b","reason":" ","error":"ignored"}],"includeReasons":true,"result":"a (unknown error); b ( )"},
    {"input":[{"name":"a","reason":"primary","error":"secondary"},{"name":"b","error":"oops"},{"name":"c"},{"name":"d"}],"includeReasons":false,"result":"a, b and 2 more"},
    {"input":[{"name":"a","reason":"primary","error":"secondary"},{"name":"b","error":"oops"},{"name":"c"},{"name":"d"}],"includeReasons":true,"result":"a (primary); b (oops) and 2 more"}
  ],
  "sources": [
    {"input":{"source":"github","repo":"o/r","ref":"main"},"result":"o/r"},
    {"input":{"source":"url","url":"https://x"},"result":"https://x"},
    {"input":{"source":"git","url":"git@x:y","ref":"main"},"result":"git@x:y"},
    {"input":{"source":"directory","path":"./local"},"result":"./local"},
    {"input":{"source":"file","path":"f.json"},"result":"f.json"},
    {"input":{"source":"settings","name":"local","plugins":[]},"result":"settings:local"},
    {"input":{"source":"npm","package":"x"},"result":"Unknown source"},
    {"input":{"source":"hostPattern","hostPattern":"x"},"result":"Unknown source"},
    {"input":{"source":"pathPattern","pathPattern":"x"},"result":"Unknown source"}
  ],
  "ids": [
    {"input":["p","m"],"result":"p@m"},
    {"input":["",""],"result":"@"},
    {"input":["a@b","c@d"],"result":"a@b@c@d"},
    {"input":["汉 字","m\n"],"result":"汉 字@m\n"}
  ],
  "loadErrors": [
    {"input":[],"successCount":0,"result":null},
    {"input":[],"successCount":1,"result":null},
    {"input":[{"name":"one","error":"bad"}],"successCount":0,"result":{"type":"error","message":"Failed to load all marketplaces. Errors: one: bad"}},
    {"input":[{"name":"one","error":"bad"}],"successCount":1,"result":{"type":"warning","message":"Warning: Failed to load marketplace 'one': bad"}},
    {"input":[{"name":"one","error":"bad"},{"name":"two","error":"worse"},{"name":"three","error":""}],"successCount":0,"result":{"type":"error","message":"Failed to load all marketplaces. Errors: one: bad; two: worse; three: "}},
    {"input":[{"name":"one","error":"bad"},{"name":"two","error":"worse"},{"name":"three","error":""}],"successCount":1,"result":{"type":"warning","message":"Warning: Failed to load 3 marketplaces: one, two, three"}}
  ],
  "empty": [
    {"git":false,"policy":{"strictKnownMarketplaces":[]},"configured":1,"failed":1,"result":"git-not-installed","trace":[["git"]]},
    {"git":true,"policy":null,"configured":0,"failed":0,"result":"no-marketplaces-configured","trace":[["git"],["settings","policySettings"]]},
    {"git":true,"policy":null,"configured":2,"failed":2,"result":"all-marketplaces-failed","trace":[["git"],["settings","policySettings"]]},
    {"git":true,"policy":null,"configured":2,"failed":1,"result":"all-plugins-installed","trace":[["git"],["settings","policySettings"]]},
    {"git":true,"policy":{"blockedMarketplaces":[{"source":"github","repo":"o/r"}]},"configured":1,"failed":0,"result":"all-plugins-installed","trace":[["git"],["settings","policySettings"]]},
    {"git":true,"policy":{},"configured":0,"failed":2,"result":"no-marketplaces-configured","trace":[["git"],["settings","policySettings"]]},
    {"git":true,"policy":{},"configured":2,"failed":3,"result":"all-plugins-installed","trace":[["git"],["settings","policySettings"]]},
    {"git":true,"policy":{},"configured":1,"failed":0,"result":"all-plugins-installed","trace":[["git"],["settings","policySettings"]]}
  ]
}"###).unwrap();
        for row in oracle["details"].as_array().unwrap() {
            let failures = row["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| PluginFailureDetail {
                    name: value["name"].as_str().unwrap().into(),
                    reason: value["reason"].as_str().map(str::to_owned),
                    error: value["error"].as_str().map(str::to_owned),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                format_failure_details(&failures, row["includeReasons"].as_bool().unwrap()),
                row["result"].as_str().unwrap()
            );
        }
        for row in oracle["sources"].as_array().unwrap() {
            assert_eq!(
                get_marketplace_source_display(&row["input"]),
                row["result"].as_str().unwrap()
            );
        }
        for row in oracle["ids"].as_array().unwrap() {
            assert_eq!(
                create_plugin_id(
                    row["input"][0].as_str().unwrap(),
                    row["input"][1].as_str().unwrap()
                ),
                row["result"].as_str().unwrap()
            );
        }
        for row in oracle["loadErrors"].as_array().unwrap() {
            let failures = row["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| MarketplaceLoadingFailure {
                    name: value["name"].as_str().unwrap().into(),
                    error: value["error"].as_str().unwrap().into(),
                })
                .collect::<Vec<_>>();
            let output = format_marketplace_loading_errors(
                &failures,
                row["successCount"].as_u64().unwrap() as usize,
            );
            if row["result"].is_null() {
                assert!(output.is_none());
            } else {
                let output = output.unwrap();
                assert_eq!(output.message, row["result"]["message"].as_str().unwrap());
                assert_eq!(
                    output.r#type,
                    if row["result"]["type"] == "warning" {
                        MarketplaceLoadingErrorType::Warning
                    } else {
                        MarketplaceLoadingErrorType::Error
                    }
                );
            }
        }
        // @nyan: the oracle's `policy` column is inert (no managed settings);
        // the two policy-only reasons were removed from it.
        for row in oracle["empty"].as_array().unwrap() {
            super::super::git_availability::tests::set_cached_git_availability(
                row["git"].as_bool().unwrap(),
            );
            let reason = futures::executor::block_on(detect_empty_marketplace_reason(
                row["configured"].as_u64().unwrap() as usize,
                row["failed"].as_u64().unwrap() as usize,
            ));
            assert_eq!(serde_json::to_value(reason).unwrap(), row["result"]);
        }
        super::super::git_availability::clear_git_availability_cache();
    }
}
