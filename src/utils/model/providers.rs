//! API provider helpers.
//! Maps to CC `utils/model/providers.ts`.

/// Maps to CC `utils/model/providers.ts` `APIProvider`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiProvider {
    FirstParty,
    Bedrock,
    Vertex,
    Foundry,
}

/// Maps to CC `utils/model/providers.ts` `getAPIProvider()`.
pub fn get_api_provider() -> ApiProvider {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_BEDROCK")
            .ok()
            .as_deref(),
    ) {
        ApiProvider::Bedrock
    } else if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_VERTEX")
            .ok()
            .as_deref(),
    ) {
        ApiProvider::Vertex
    } else if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_FOUNDRY")
            .ok()
            .as_deref(),
    ) {
        ApiProvider::Foundry
    } else {
        ApiProvider::FirstParty
    }
}

/// Maps to CC `utils/model/providers.ts` `getAPIProviderForStatsig()`.
pub fn get_api_provider_for_statsig() -> &'static str {
    match get_api_provider() {
        ApiProvider::FirstParty => "firstParty",
        ApiProvider::Bedrock => "bedrock",
        ApiProvider::Vertex => "vertex",
        ApiProvider::Foundry => "foundry",
    }
}

/// Pure audience/value form of CC `utils/model/providers.ts#isFirstPartyAnthropicBaseUrl`.
pub fn is_first_party_anthropic_base_url_for_audience(
    base_url: Option<&str>,
    audience: crate::utils::build_profile::BuildAudience,
) -> bool {
    let Some(base_url) = base_url else {
        return true;
    };
    let allowed_hosts: &[&str] = if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Models,
    ) {
        &["api.anthropic.com", "api-staging.anthropic.com"]
    } else {
        &["api.anthropic.com"]
    };

    parse_url_host(base_url)
        .as_deref()
        .is_some_and(|host| allowed_hosts.contains(&host))
}

/// Maps to CC `utils/model/providers.ts#isFirstPartyAnthropicBaseUrl`.
pub fn is_first_party_anthropic_base_url() -> bool {
    let base_url = crate::utils::process_env::var("ANTHROPIC_BASE_URL").ok();
    is_first_party_anthropic_base_url_for_audience(
        base_url.as_deref(),
        crate::utils::build_profile::build_audience(),
    )
}

fn parse_url_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Some((scheme, after_scheme)) = trimmed.split_once("://") else {
        return None;
    };
    if scheme.is_empty() {
        return None;
    }
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host_port.to_string();
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_api_provider_matches_official_env_precedence() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        assert_eq!(get_api_provider(), ApiProvider::FirstParty);

        crate::utils::process_env::set("CLAUDE_CODE_USE_FOUNDRY", "1");
        assert_eq!(get_api_provider(), ApiProvider::Foundry);

        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        assert_eq!(get_api_provider(), ApiProvider::Vertex);

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert_eq!(get_api_provider(), ApiProvider::Bedrock);

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
    }

    #[test]
    fn first_party_base_url_matches_official_host_allowlist() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ANTHROPIC_BASE_URL");
        assert!(is_first_party_anthropic_base_url());

        crate::utils::process_env::set("ANTHROPIC_BASE_URL", "https://api.anthropic.com/v1");
        assert!(is_first_party_anthropic_base_url());
        crate::utils::process_env::set("ANTHROPIC_BASE_URL", "https://api.anthropic.com:443");
        assert!(!is_first_party_anthropic_base_url());

        crate::utils::process_env::set("ANTHROPIC_BASE_URL", "https://api-staging.anthropic.com");
        assert!(is_first_party_anthropic_base_url_for_audience(
            Some("https://api-staging.anthropic.com"),
            crate::utils::build_profile::BuildAudience::AnthropicInternal,
        ));
        assert!(!is_first_party_anthropic_base_url_for_audience(
            Some("https://api-staging.anthropic.com"),
            crate::utils::build_profile::BuildAudience::External,
        ));

        crate::utils::process_env::set("ANTHROPIC_BASE_URL", "api.anthropic.com");
        assert!(!is_first_party_anthropic_base_url());
        crate::utils::process_env::set("ANTHROPIC_BASE_URL", "not a url");
        assert!(!is_first_party_anthropic_base_url());

        crate::utils::process_env::remove("ANTHROPIC_BASE_URL");
    }
}
