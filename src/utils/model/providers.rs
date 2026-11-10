//! API provider helpers.
//! Maps to CC `utils/model/providers.ts`.

/// The only supported API provider. Provider configuration is supplied by the
/// selected settings.nyan profile, never by provider-selection environment
/// variables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiProvider {
    FirstParty,
}

pub fn get_api_provider() -> ApiProvider {
    ApiProvider::FirstParty
}

/// Wire name of the API provider, as reported in the SDK `account` payload.
pub fn get_api_provider_name() -> &'static str {
    "firstParty"
}

/// Pure value form of CC `utils/model/providers.ts#isFirstPartyAnthropicBaseUrl`.
pub fn is_first_party_anthropic_base_url_for(base_url: Option<&str>) -> bool {
    // CC `if (!baseUrl) return true`.
    let Some(base_url) = base_url.filter(|value| !value.is_empty()) else {
        return true;
    };
    parse_url_host(base_url).as_deref() == Some("api.anthropic.com")
}

/// Maps to CC `utils/model/providers.ts#isFirstPartyAnthropicBaseUrl`.
///
/// The endpoint comes from the selected nyan profile. No profile (or no
/// configured endpoint) is the SDK's first-party default.
pub fn is_first_party_anthropic_base_url() -> bool {
    let base_url = crate::utils::model::profile::resolve_main_profile(
        &crate::utils::settings::get_initial_settings(),
    )
    .ok()
    .and_then(|profile| profile.base_url);
    is_first_party_anthropic_base_url_for(base_url.as_deref())
}

/// CC `new URL(baseUrl).host` (`providers.ts:31`): the WHATWG host, with a
/// non-default port and without a default one (`https://h:443` is `h`).
/// `None` where `new URL` throws.
fn parse_url_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn first_party_base_url_matches_official_host_allowlist() {
        // No base URL is the SDK's first-party default.
        assert!(is_first_party_anthropic_base_url_for(None));
        // `!baseUrl`: an empty value is unset.
        assert!(is_first_party_anthropic_base_url_for(Some("")));

        assert!(is_first_party_anthropic_base_url_for(Some(
            "https://api.anthropic.com/v1"
        )));
        // `new URL(...).host` drops a default port and keeps any other.
        assert!(is_first_party_anthropic_base_url_for(Some(
            "https://api.anthropic.com:443"
        )));
        assert!(!is_first_party_anthropic_base_url_for(Some(
            "https://api.anthropic.com:8443"
        )));
        assert!(is_first_party_anthropic_base_url_for(Some(
            "HTTPS://API.Anthropic.COM/v1"
        )));

        assert!(!is_first_party_anthropic_base_url_for(Some(
            "https://api-staging.anthropic.com"
        )));

        assert!(!is_first_party_anthropic_base_url_for(Some(
            "api.anthropic.com"
        )));
        assert!(!is_first_party_anthropic_base_url_for(Some("not a url")));
    }

    #[test]
    fn first_party_base_url_comes_from_the_main_profile_not_the_environment() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "nyan-first-party-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        let _ambient = EnvVarGuard::set("ANTHROPIC_BASE_URL", "https://ambient.example");
        crate::utils::settings::settings_cache::reset_settings_cache();

        // No profile: the first-party default, regardless of the environment.
        assert!(is_first_party_anthropic_base_url());

        std::fs::write(
            root.join("settings.json"),
            r#"{
                "nyan": {
                    "providers": {"anthropic": {"kind": "anthropic", "protocol": "anthropicMessages", "baseUrl": "https://proxy.example"}},
                    "models": {"main": {"provider": "anthropic", "model": "claude-sonnet-4-6", "capabilities": {"contextWindow": 200000, "maxOutputTokens": 128000}}},
                    "selection": {"main": "main"}
                }
            }"#,
        )
        .unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
        assert!(!is_first_party_anthropic_base_url());

        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }
}
