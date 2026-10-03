//! Maps to: CC `utils/plugins/marketplaceHelpers.ts`.
//! Inputs retain the existing schema-validated JSON carrier. The full
//! `loadMarketplacesWithGracefulDegradation` retains per-marketplace failures.

use crate::utils::settings::{SettingSource, get_settings_for_source};
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

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:159-165#getStrictKnownMarketplaces`.
pub fn get_strict_known_marketplaces() -> Option<Vec<Value>> {
    get_settings_for_source(SettingSource::Policy)
        .and_then(|settings| settings.strict_known_marketplaces)
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:171-177#getBlockedMarketplaces`.
pub fn get_blocked_marketplaces() -> Option<Vec<Value>> {
    get_settings_for_source(SettingSource::Policy)
        .and_then(|settings| settings.blocked_marketplaces)
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:183-185#getPluginTrustMessage`.
pub fn get_plugin_trust_message() -> Option<String> {
    get_settings_for_source(SettingSource::Policy)
        .and_then(|settings| settings.plugin_trust_message)
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:191-223#areSourcesEqual`.
fn are_sources_equal(a: &Value, b: &Value) -> bool {
    if a.get("source") != b.get("source") {
        return false;
    }
    match a["source"].as_str() {
        Some("url") => a.get("url") == b.get("url"),
        Some("github") => {
            a.get("repo") == b.get("repo")
                && a["ref"].as_str().filter(|v| !v.is_empty())
                    == b["ref"].as_str().filter(|v| !v.is_empty())
                && a["path"].as_str().filter(|v| !v.is_empty())
                    == b["path"].as_str().filter(|v| !v.is_empty())
        }
        Some("git") => {
            a.get("url") == b.get("url")
                && a["ref"].as_str().filter(|v| !v.is_empty())
                    == b["ref"].as_str().filter(|v| !v.is_empty())
                && a["path"].as_str().filter(|v| !v.is_empty())
                    == b["path"].as_str().filter(|v| !v.is_empty())
        }
        Some("npm") => a.get("package") == b.get("package"),
        Some("file") | Some("directory") => a.get("path") == b.get("path"),
        Some("settings") => {
            // The schema's narrow settings plugin entries contain only JSON
            // strings, booleans, arrays and objects, so value equality is the
            // source lodash isEqual here (including object key-order freedom).
            a.get("name") == b.get("name") && a.get("plugins") == b.get("plugins")
        }
        _ => false,
    }
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:235-268#extractHostFromSource`.
pub fn extract_host_from_source(source: &Value) -> Option<String> {
    match source["source"].as_str() {
        Some("github") => Some("github.com".into()),
        Some("git") => {
            let source_url = source["url"].as_str().expect("git source URL is a string");
            let ssh =
                regress::Regex::new(r"^[^@]+@([^:]+):").expect("source SSH host regex compiles");
            if let Some(host) = ssh
                .find(source_url)
                .and_then(|matched| matched.group(1))
                .map(|range| &source_url[range])
                .filter(|host| !host.is_empty())
            {
                return Some(host.into());
            }
            url::Url::parse(source_url)
                .ok()
                .map(|url| url.host_str().unwrap_or("").to_owned())
        }
        Some("url") => url::Url::parse(source["url"].as_str().expect("URL source URL is a string"))
            .ok()
            .map(|url| url.host_str().unwrap_or("").to_owned()),
        _ => None,
    }
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:278-295#doesSourceMatchHostPattern`.
fn does_source_match_host_pattern(source: &Value, pattern: &Value) -> bool {
    let Some(host) = extract_host_from_source(source).filter(|host| !host.is_empty()) else {
        return false;
    };
    let pattern = pattern["hostPattern"]
        .as_str()
        .expect("hostPattern is a string");
    let regex = regress::Regex::from_unicode(
        pattern.encode_utf16().map(u32::from),
        regress::Flags::default(),
    );
    match regex {
        Ok(regex) => regex
            .find_from_ucs2(&host.encode_utf16().collect::<Vec<_>>(), 0)
            .next()
            .is_some(),
        Err(_) => {
            crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
                "Invalid hostPattern regex: {pattern}"
            )));
            false
        }
    }
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:305-321#doesSourceMatchPathPattern`.
fn does_source_match_path_pattern(source: &Value, pattern: &Value) -> bool {
    if !matches!(source["source"].as_str(), Some("file" | "directory")) {
        return false;
    }
    let pattern = pattern["pathPattern"]
        .as_str()
        .expect("pathPattern is a string");
    let regex = regress::Regex::from_unicode(
        pattern.encode_utf16().map(u32::from),
        regress::Flags::default(),
    );
    match regex {
        Ok(regex) => regex
            .find_from_ucs2(
                &source["path"]
                    .as_str()
                    .expect("local source path is a string")
                    .encode_utf16()
                    .collect::<Vec<_>>(),
                0,
            )
            .next()
            .is_some(),
        Err(_) => {
            crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
                "Invalid pathPattern regex: {pattern}"
            )));
            false
        }
    }
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:327-337#getHostPatternsFromAllowlist`.
pub fn get_host_patterns_from_allowlist() -> Vec<String> {
    let Some(allowlist) = get_strict_known_marketplaces() else {
        return Vec::new();
    };
    allowlist
        .iter()
        .filter(|entry| entry["source"] == "hostPattern")
        .map(|entry| {
            entry["hostPattern"]
                .as_str()
                .expect("hostPattern is a string")
                .to_owned()
        })
        .collect()
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:348-364#extractGitHubRepoFromGitUrl`.
fn extract_git_hub_repo_from_git_url(url: &str) -> Option<String> {
    let ssh = regress::Regex::new(r"^git@github\.com:([^/]+\/[^/]+?)(?:\.git)?$")
        .expect("source GitHub SSH regex compiles");
    if let Some(repo) = ssh.find(url).and_then(|matched| matched.group(1)) {
        return Some(url[repo].to_owned());
    }
    let https = regress::Regex::new(r"^https?:\/\/github\.com\/([^/]+\/[^/]+?)(?:\.git)?$")
        .expect("source GitHub HTTPS regex compiles");
    if let Some(repo) = https.find(url).and_then(|matched| matched.group(1)) {
        return Some(url[repo].to_owned());
    }
    None
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:371-381#blockedConstraintMatches`.
fn blocked_constraint_matches(blocked_value: Option<&str>, source_value: Option<&str>) -> bool {
    let Some(blocked_value) = blocked_value.filter(|value| !value.is_empty()) else {
        return true;
    };
    Some(blocked_value) == source_value.filter(|value| !value.is_empty())
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:391-452#areSourcesEquivalentForBlocklist`.
fn are_sources_equivalent_for_blocklist(source: &Value, blocked: &Value) -> bool {
    if source.get("source") == blocked.get("source") {
        return match source["source"].as_str() {
            Some("github") => {
                if source.get("repo") != blocked.get("repo") {
                    return false;
                }
                blocked_constraint_matches(blocked["ref"].as_str(), source["ref"].as_str())
                    && blocked_constraint_matches(blocked["path"].as_str(), source["path"].as_str())
            }
            Some("git") => {
                if source.get("url") != blocked.get("url") {
                    return false;
                }
                blocked_constraint_matches(blocked["ref"].as_str(), source["ref"].as_str())
                    && blocked_constraint_matches(blocked["path"].as_str(), source["path"].as_str())
            }
            Some("url") => source.get("url") == blocked.get("url"),
            Some("npm") => source.get("package") == blocked.get("package"),
            Some("file") | Some("directory") => source.get("path") == blocked.get("path"),
            Some("settings") => source.get("name") == blocked.get("name"),
            _ => false,
        };
    }
    if source["source"] == "git" && blocked["source"] == "github" {
        let repo =
            extract_git_hub_repo_from_git_url(source["url"].as_str().expect("git source URL"));
        if repo.as_deref() == blocked["repo"].as_str() {
            return blocked_constraint_matches(blocked["ref"].as_str(), source["ref"].as_str())
                && blocked_constraint_matches(blocked["path"].as_str(), source["path"].as_str());
        }
    }
    if source["source"] == "github" && blocked["source"] == "git" {
        let repo =
            extract_git_hub_repo_from_git_url(blocked["url"].as_str().expect("git source URL"));
        if repo.as_deref() == source["repo"].as_str() {
            return blocked_constraint_matches(blocked["ref"].as_str(), source["ref"].as_str())
                && blocked_constraint_matches(blocked["path"].as_str(), source["path"].as_str());
        }
    }
    false
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:461-469#isSourceInBlocklist`.
pub fn is_source_in_blocklist(source: &Value) -> bool {
    let Some(blocklist) = get_blocked_marketplaces() else {
        return false;
    };
    blocklist
        .iter()
        .any(|blocked| are_sources_equivalent_for_blocklist(source, blocked))
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:480-505#isSourceAllowedByPolicy`.
pub fn is_source_allowed_by_policy(source: &Value) -> bool {
    if is_source_in_blocklist(source) {
        return false;
    }
    let Some(allowlist) = get_strict_known_marketplaces() else {
        return true;
    };
    allowlist.iter().any(|allowed| {
        if allowed["source"] == "hostPattern" {
            return does_source_match_host_pattern(source, allowed);
        }
        if allowed["source"] == "pathPattern" {
            return does_source_match_path_pattern(source, allowed);
        }
        are_sources_equal(source, allowed)
    })
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:510-533#formatSourceForDisplay`.
pub fn format_source_for_display(source: &Value) -> String {
    match source["source"].as_str() {
        Some("github") => format!(
            "github:{}{}",
            source["repo"].as_str().expect("GitHub repo is a string"),
            source["ref"]
                .as_str()
                .filter(|r| !r.is_empty())
                .map(|r| format!("@{r}"))
                .unwrap_or_default()
        ),
        Some("url") => source["url"].as_str().expect("URL is a string").to_owned(),
        Some("git") => format!(
            "git:{}{}",
            source["url"].as_str().expect("git URL is a string"),
            source["ref"]
                .as_str()
                .filter(|r| !r.is_empty())
                .map(|r| format!("@{r}"))
                .unwrap_or_default()
        ),
        Some("npm") => format!(
            "npm:{}",
            source["package"].as_str().expect("npm package is a string")
        ),
        Some("file") => format!(
            "file:{}",
            source["path"].as_str().expect("file path is a string")
        ),
        Some("directory") => format!(
            "dir:{}",
            source["path"].as_str().expect("directory path is a string")
        ),
        Some("hostPattern") => format!(
            "hostPattern:{}",
            source["hostPattern"]
                .as_str()
                .expect("hostPattern is a string")
        ),
        Some("pathPattern") => format!(
            "pathPattern:{}",
            source["pathPattern"]
                .as_str()
                .expect("pathPattern is a string")
        ),
        Some("settings") => {
            let count = source["plugins"]
                .as_array()
                .expect("settings plugins is an array")
                .len();
            format!(
                "settings:{} ({count} {})",
                source["name"].as_str().expect("settings name is a string"),
                crate::utils::string_utils::plural(count, "plugin", None)
            )
        }
        _ => "unknown source".into(),
    }
}

/// Maps to: CC `utils/plugins/marketplaceHelpers.ts:538-544#EmptyMarketplaceReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmptyMarketplaceReason {
    GitNotInstalled,
    AllBlockedByPolicy,
    PolicyRestrictsSources,
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
    if let Some(allowlist) = get_strict_known_marketplaces() {
        if allowlist.is_empty() {
            return EmptyMarketplaceReason::AllBlockedByPolicy;
        }
        if configured_marketplace_count == 0 {
            return EmptyMarketplaceReason::PolicyRestrictsSources;
        }
    }
    if configured_marketplace_count == 0 {
        return EmptyMarketplaceReason::NoMarketplacesConfigured;
    }
    if failed_marketplace_count > 0 && failed_marketplace_count == configured_marketplace_count {
        return EmptyMarketplaceReason::AllMarketplacesFailed;
    }
    EmptyMarketplaceReason::AllPluginsInstalled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::{SettingsJson, settings_cache};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;

    #[test]
    fn source_hosts_matches_official_ssh_before_url_and_empty_hostname() {
        // CC marketplaceHelpers.ts:235-268; Bun oracle 2026-09-14 proof cases 0–8.
        for (source, expected) in [
            (json!({"source":"github","repo":"O/R"}), Some("github.com")),
            (
                json!({"source":"git","url":"git@GitHub.COM:o/r"}),
                Some("GitHub.COM"),
            ),
            (
                json!({"source":"git","url":"https://user@GitHub.COM:8443/o/r"}),
                Some("GitHub.COM"),
            ),
            (
                json!({"source":"url","url":"https://user@GitHub.COM:8443/o/r"}),
                Some("github.com"),
            ),
            (
                json!({"source":"url","url":"https://[2001:db8::1]:8080/a"}),
                Some("[2001:db8::1]"),
            ),
            (json!({"source":"url","url":"mailto:x@y.com"}), Some("")),
            (json!({"source":"url","url":"file:///tmp/a"}), Some("")),
            (json!({"source":"git","url":"not a URL"}), None),
            (json!({"source":"file","path":"/tmp/x"}), None),
        ] {
            assert_eq!(
                extract_host_from_source(&source).as_deref(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn github_repo_extraction_matches_official_exact_regex() {
        // CC marketplaceHelpers.ts:348-364; Bun oracle proof cases 9–16.
        for (url, expected) in [
            ("git@github.com:o/r.git", Some("o/r")),
            ("https://github.com/o/r.git", Some("o/r")),
            ("http://github.com/o/r", Some("o/r")),
            ("https://GitHub.com/o/r", None),
            ("ssh://git@github.com/o/r.git", None),
            ("https://github.com/o/r/", None),
            ("https://github.com/o/r.git\\n", Some("o/r.git\\n")),
            ("git@github.com:o/r.git.git", Some("o/r.git")),
        ] {
            assert_eq!(
                extract_git_hub_repo_from_git_url(url).as_deref(),
                expected,
                "{url}"
            );
        }
    }

    #[test]
    fn source_equality_matches_official_fields_and_settings_deep_value() {
        // CC marketplaceHelpers.ts:191-223; Bun oracle proof cases 17–21.
        assert!(are_sources_equal(
            &json!({"source":"url","url":"https://x/a","headers":{"x":"a"}}),
            &json!({"source":"url","url":"https://x/a","headers":{"x":"b"}}),
        ));
        assert!(are_sources_equal(
            &json!({"source":"github","repo":"o/r","ref":"","path":""}),
            &json!({"source":"github","repo":"o/r"}),
        ));
        assert!(!are_sources_equal(
            &json!({"source":"github","repo":"o/r","ref":"main"}),
            &json!({"source":"github","repo":"o/r"}),
        ));
        assert!(are_sources_equal(
            &json!({"source":"settings","name":"m","plugins":[{"name":"p","source":{"source":"npm","package":"p"}}],"owner":{"name":"A"}}),
            &json!({"source":"settings","name":"m","plugins":[{"source":{"package":"p","source":"npm"},"name":"p"}],"owner":{"name":"B"}}),
        ));
        assert!(!are_sources_equal(
            &json!({"source":"settings","name":"m","plugins":[{"name":"p","source":{"source":"npm","package":"p"}}]}),
            &json!({"source":"settings","name":"m","plugins":[{"name":"q","source":{"source":"npm","package":"p"}}]}),
        ));
    }

    #[test]
    fn source_blocklist_matches_official_asymmetric_constraints_and_equivalence() {
        // CC marketplaceHelpers.ts:371-452; Bun oracle proof cases 22–26.
        let source =
            json!({"source":"git","url":"https://github.com/o/r.git","ref":"main","path":"x"});
        for (blocked, expected) in [
            (json!({"source":"github","repo":"o/r"}), true),
            (json!({"source":"github","repo":"o/r","ref":"dev"}), false),
            (
                json!({"source":"github","repo":"o/r","ref":"main","path":"x"}),
                true,
            ),
            (json!({"source":"hostPattern","hostPattern":".*"}), false),
        ] {
            assert_eq!(
                are_sources_equivalent_for_blocklist(&source, &blocked),
                expected
            );
        }
        assert!(are_sources_equivalent_for_blocklist(
            &json!({"source":"settings","name":"m","plugins":[{"name":"p"}]}),
            &json!({"source":"settings","name":"m","plugins":[]}),
        ));
        // CC :439-449 covers the opposite source-type direction independently.
        assert!(are_sources_equivalent_for_blocklist(
            &json!({"source":"github","repo":"o/r","ref":"main","path":"x"}),
            &json!({"source":"git","url":"git@github.com:o/r.git"}),
        ));
        assert!(!are_sources_equivalent_for_blocklist(
            &json!({"source":"github","repo":"o/r"}),
            &json!({"source":"git","url":"git@github.com:o/r.git","ref":"main"}),
        ));
    }

    #[test]
    fn policy_regex_matches_official_non_unicode_ecmascript_and_applicability() {
        // CC marketplaceHelpers.ts:278-321; Bun oracle proof cases 27–35.
        for (path, pattern, expected) in [
            ("😀", "^.$", false),
            ("😀", "^..$", true),
            ("/😀", "(?<=/)😀$", true),
            ("aa", r"^(.)\1$", true),
            ("q", r"^\q$", true),
        ] {
            assert_eq!(
                does_source_match_path_pattern(
                    &json!({"source":"file","path":path}),
                    &json!({"source":"pathPattern","pathPattern":pattern}),
                ),
                expected,
                "{pattern}"
            );
        }
        assert!(!does_source_match_path_pattern(
            &json!({"source":"git","url":"https://x/a","path":"x"}),
            &json!({"source":"pathPattern","pathPattern":"["}),
        ));
        assert!(!does_source_match_host_pattern(
            &json!({"source":"directory","path":"/a"}),
            &json!({"source":"hostPattern","hostPattern":"["}),
        ));
    }

    #[test]
    fn managed_policy_matches_official_absence_empty_precedence_and_order() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let source = json!({"source":"github","repo":"o/r","ref":"main"});
        // CC marketplaceHelpers.ts:159-177,327-337,461-505; Bun proof 36–41.
        for (settings, allowed, blocked) in [
            (None, true, false),
            (Some(json!({})), true, false),
            (Some(json!({"strictKnownMarketplaces":[]})), false, false),
            (Some(json!({"blockedMarketplaces":[]})), true, false),
            (
                Some(
                    json!({"strictKnownMarketplaces":[{"source":"hostPattern","hostPattern":"github"}]}),
                ),
                true,
                false,
            ),
            (
                Some(
                    json!({"strictKnownMarketplaces":[{"source":"hostPattern","hostPattern":"["}],"blockedMarketplaces":[{"source":"github","repo":"o/r"}]}),
                ),
                false,
                true,
            ),
        ] {
            let expected_strict = settings
                .as_ref()
                .and_then(|s| s.get("strictKnownMarketplaces"))
                .and_then(Value::as_array)
                .cloned();
            let expected_blocked = settings
                .as_ref()
                .and_then(|s| s.get("blockedMarketplaces"))
                .and_then(Value::as_array)
                .cloned();
            settings_cache::set_cached_settings_for_source(
                SettingSource::Policy,
                settings.map(|settings| serde_json::from_value::<SettingsJson>(settings).unwrap()),
            );
            assert_eq!(get_strict_known_marketplaces(), expected_strict);
            assert_eq!(get_blocked_marketplaces(), expected_blocked);
            assert_eq!(is_source_allowed_by_policy(&source), allowed);
            assert_eq!(is_source_in_blocklist(&source), blocked);
        }
        settings_cache::set_cached_settings_for_source(
            SettingSource::Policy,
            Some(SettingsJson {
                strict_known_marketplaces: Some(vec![
                    json!({"source":"hostPattern","hostPattern":"b"}),
                    json!({"source":"pathPattern","pathPattern":"x"}),
                    json!({"source":"hostPattern","hostPattern":"a"}),
                    json!({"source":"hostPattern","hostPattern":"b"}),
                ]),
                ..Default::default()
            }),
        );
        // CC :329-336 filters only host entries and preserves repeats/order.
        assert_eq!(get_host_patterns_from_allowlist(), ["b", "a", "b"]);
        settings_cache::reset_settings_cache();
    }

    #[test]
    fn invalid_policy_regex_matches_official_log_error_messages_and_short_circuit() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env: Vec<_> = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_ERROR_REPORTING",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ]
        .into_iter()
        .map(EnvVarGuard::unset)
        .collect();
        crate::utils::log::_reset_error_log_for_testing();
        // CC marketplaceHelpers.ts:278-321; original Bun errors exactly match.
        assert!(!does_source_match_host_pattern(
            &json!({"source":"github","repo":"o/r"}),
            &json!({"source":"hostPattern","hostPattern":"["}),
        ));
        assert!(!does_source_match_path_pattern(
            &json!({"source":"file","path":"x"}),
            &json!({"source":"pathPattern","pathPattern":"["}),
        ));
        let errors = crate::utils::log::get_in_memory_errors();
        assert_eq!(errors.len(), 2);
        assert!(errors[0].error.contains("Invalid hostPattern regex: ["));
        assert!(errors[1].error.contains("Invalid pathPattern regex: ["));
        settings_cache::set_cached_settings_for_source(
            SettingSource::Policy,
            Some(SettingsJson {
                strict_known_marketplaces: Some(vec![
                    json!({"source":"hostPattern","hostPattern":"github"}),
                    json!({"source":"hostPattern","hostPattern":"["}),
                ]),
                ..Default::default()
            }),
        );
        // CC :494-504 Array.some stops before the invalid second pattern.
        assert!(is_source_allowed_by_policy(
            &json!({"source":"github","repo":"o/r"})
        ));
        assert_eq!(crate::utils::log::get_in_memory_errors().len(), 2);
        settings_cache::reset_settings_cache();
        crate::utils::log::_reset_error_log_for_testing();
    }

    #[test]
    fn source_display_matches_official_prefixes_ref_and_plural() {
        // CC marketplaceHelpers.ts:510-533; original Bun proof cases 42–52.
        for (source, expected) in [
            (
                json!({"source":"github","repo":"o/r","ref":"","path":"x"}),
                "github:o/r",
            ),
            (
                json!({"source":"git","url":"https://x/r","ref":"main"}),
                "git:https://x/r@main",
            ),
            (json!({"source":"url","url":"https://x/a"}), "https://x/a"),
            (json!({"source":"npm","package":"p"}), "npm:p"),
            (json!({"source":"file","path":"x"}), "file:x"),
            (json!({"source":"directory","path":"x"}), "dir:x"),
            (
                json!({"source":"hostPattern","hostPattern":"x"}),
                "hostPattern:x",
            ),
            (
                json!({"source":"pathPattern","pathPattern":"x"}),
                "pathPattern:x",
            ),
            (
                json!({"source":"settings","name":"m","plugins":[]}),
                "settings:m (0 plugins)",
            ),
            (
                json!({"source":"settings","name":"m","plugins":[{}]}),
                "settings:m (1 plugin)",
            ),
            (
                json!({"source":"settings","name":"m","plugins":[{},{}]}),
                "settings:m (2 plugins)",
            ),
        ] {
            assert_eq!(format_source_for_display(&source), expected);
        }
    }
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
  "trust": [
    {"input":null,"result":null,"trace":[["settings","policySettings"]]},
    {"input":{},"result":null,"trace":[["settings","policySettings"]]},
    {"input":{"pluginTrustMessage":""},"result":"","trace":[["settings","policySettings"]]},
    {"input":{"pluginTrustMessage":" custom\n "},"result":" custom\n ","trace":[["settings","policySettings"]]}
  ],
  "empty": [
    {"git":false,"policy":{"strictKnownMarketplaces":[]},"configured":1,"failed":1,"result":"git-not-installed","trace":[["git"]]},
    {"git":true,"policy":{"strictKnownMarketplaces":[]},"configured":1,"failed":1,"result":"all-blocked-by-policy","trace":[["git"],["settings","policySettings"]]},
    {"git":true,"policy":{"strictKnownMarketplaces":[{"source":"github","repo":"o/r"}]},"configured":0,"failed":0,"result":"policy-restricts-sources","trace":[["git"],["settings","policySettings"]]},
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
        for row in oracle["trust"].as_array().unwrap() {
            settings_cache::set_cached_settings_for_source(
                SettingSource::Policy,
                (!row["input"].is_null())
                    .then(|| serde_json::from_value::<SettingsJson>(row["input"].clone()).unwrap()),
            );
            assert_eq!(
                get_plugin_trust_message().as_deref(),
                row["result"].as_str()
            );
        }
        for row in oracle["empty"].as_array().unwrap() {
            settings_cache::set_cached_settings_for_source(
                SettingSource::Policy,
                (!row["policy"].is_null()).then(|| {
                    serde_json::from_value::<SettingsJson>(row["policy"].clone()).unwrap()
                }),
            );
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
        settings_cache::reset_settings_cache();
    }
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
        if !is_source_allowed_by_policy(&entry["source"]) {
            continue;
        }
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
