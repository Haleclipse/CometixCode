//! OAuth constants and environment-selected endpoints.
//! Maps to: CC `constants/oauth.ts`.

/// Maps to: CC `OauthConfig`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OauthConfig {
    pub base_api_url: String,
    pub console_authorize_url: String,
    pub claude_ai_authorize_url: String,
    pub claude_ai_origin: String,
    pub token_url: String,
    pub api_key_url: String,
    pub roles_url: String,
    pub console_success_url: String,
    pub claude_ai_success_url: String,
    pub manual_redirect_url: String,
    pub client_id: String,
    pub oauth_file_suffix: String,
    pub mcp_proxy_url: String,
    pub mcp_proxy_path: String,
}

/// Maps to: CC `constants/oauth.ts:33` `CLAUDE_AI_INFERENCE_SCOPE`.
pub const CLAUDE_AI_INFERENCE_SCOPE: &str = "user:inference";
/// Maps to: CC `constants/oauth.ts:34` `CLAUDE_AI_PROFILE_SCOPE`.
pub const CLAUDE_AI_PROFILE_SCOPE: &str = "user:profile";
/// Maps to: CC `constants/oauth.ts:45-52` `CLAUDE_AI_OAUTH_SCOPES`.
pub const CLAUDE_AI_OAUTH_SCOPES: [&str; 5] = [
    CLAUDE_AI_PROFILE_SCOPE,
    CLAUDE_AI_INFERENCE_SCOPE,
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
];
/// Maps to: CC `constants/oauth.ts:36` `OAUTH_BETA_HEADER`.
pub const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
/// Maps to: CC `constants/oauth.ts:113-114` `MCP_CLIENT_METADATA_URL`.
pub const MCP_CLIENT_METADATA_URL: &str = "https://claude.ai/oauth/claude-code-client-metadata";

/// No CC counterpart. This is the sole user-authorized L2 product switch for
/// OAuth network, process/listener, credential-storage, revocation, and
/// credential-cache side effects. It is intentionally compile-time only.
pub const OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED: bool = false;

/// Stable message for the user-authorized default-closed OAuth outlet gate.
pub const OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE: &str =
    "OAuth credential side effects are unavailable in this product build";

/// Minimal typed L2 failure shared by source-owned OAuth outlet functions.
///
/// This type carries no policy; each canonical source function checks the one
/// constant immediately before its own final side effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OAuthCredentialSideEffectsUnavailable;

impl std::fmt::Display for OAuthCredentialSideEffectsUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE)
    }
}

impl std::error::Error for OAuthCredentialSideEffectsUnavailable {}

/// Maps to: CC `constants/oauth.ts:84-108` `PROD_OAUTH_CONFIG`.
static PROD_OAUTH_CONFIG: std::sync::LazyLock<OauthConfig> = std::sync::LazyLock::new(|| {
    OauthConfig {
        base_api_url: "https://api.anthropic.com".to_string(),
        console_authorize_url: "https://platform.claude.com/oauth/authorize".to_string(),
        claude_ai_authorize_url: "https://claude.com/cai/oauth/authorize".to_string(),
        claude_ai_origin: "https://claude.ai".to_string(),
        token_url: "https://platform.claude.com/v1/oauth/token".to_string(),
        api_key_url: "https://api.anthropic.com/api/oauth/claude_cli/create_api_key".to_string(),
        roles_url: "https://api.anthropic.com/api/oauth/claude_cli/roles".to_string(),
        console_success_url: "https://platform.claude.com/buy_credits?returnUrl=/oauth/code/success%3Fapp%3Dclaude-code".to_string(),
        claude_ai_success_url: "https://platform.claude.com/oauth/code/success?app=claude-code".to_string(),
        manual_redirect_url: "https://platform.claude.com/oauth/code/callback".to_string(),
        client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string(),
        oauth_file_suffix: String::new(),
        mcp_proxy_url: "https://mcp-proxy.anthropic.com".to_string(),
        mcp_proxy_path: "/v1/mcp/{server_id}".to_string(),
}
});

/// Maps to: CC `constants/oauth.ts:118-150` `STAGING_OAUTH_CONFIG`.
static STAGING_OAUTH_CONFIG: std::sync::LazyLock<OauthConfig> = std::sync::LazyLock::new(|| {
    OauthConfig {
        base_api_url: "https://api-staging.anthropic.com".to_string(),
        console_authorize_url: "https://platform.staging.ant.dev/oauth/authorize".to_string(),
        claude_ai_authorize_url: "https://claude-ai.staging.ant.dev/oauth/authorize".to_string(),
        claude_ai_origin: "https://claude-ai.staging.ant.dev".to_string(),
        token_url: "https://platform.staging.ant.dev/v1/oauth/token".to_string(),
        api_key_url: "https://api-staging.anthropic.com/api/oauth/claude_cli/create_api_key".to_string(),
        roles_url: "https://api-staging.anthropic.com/api/oauth/claude_cli/roles".to_string(),
        console_success_url: "https://platform.staging.ant.dev/buy_credits?returnUrl=/oauth/code/success%3Fapp%3Dclaude-code".to_string(),
        claude_ai_success_url: "https://platform.staging.ant.dev/oauth/code/success?app=claude-code".to_string(),
        manual_redirect_url: "https://platform.staging.ant.dev/oauth/code/callback".to_string(),
        client_id: "22422756-60c9-4084-8eb7-27705fd5cf9a".to_string(),
        oauth_file_suffix: "-staging-oauth".to_string(),
        mcp_proxy_url: "https://mcp-proxy-staging.anthropic.com".to_string(),
        mcp_proxy_path: "/v1/mcp/{server_id}".to_string(),
}
});

/// Maps to: CC `constants/oauth.ts:156-177` `getLocalOauthConfig`.
fn get_local_oauth_config(get_env: &impl Fn(&str) -> Option<String>) -> OauthConfig {
    // `?.replace(/\/$/, '')`: one trailing slash.
    let strip = |value: String| value.strip_suffix('/').unwrap_or(&value).to_string();
    let api = get_env("CLAUDE_LOCAL_OAUTH_API_BASE")
        .map(strip)
        .unwrap_or_else(|| "http://localhost:8000".to_string());
    let apps = get_env("CLAUDE_LOCAL_OAUTH_APPS_BASE")
        .map(strip)
        .unwrap_or_else(|| "http://localhost:4000".to_string());
    let console_base = get_env("CLAUDE_LOCAL_OAUTH_CONSOLE_BASE")
        .map(strip)
        .unwrap_or_else(|| "http://localhost:3000".to_string());

    OauthConfig {
        base_api_url: api.clone(),
        console_authorize_url: format!("{console_base}/oauth/authorize"),
        claude_ai_authorize_url: format!("{apps}/oauth/authorize"),
        claude_ai_origin: apps,
        token_url: format!("{api}/v1/oauth/token"),
        api_key_url: format!("{api}/api/oauth/claude_cli/create_api_key"),
        roles_url: format!("{api}/api/oauth/claude_cli/roles"),
        console_success_url: format!(
            "{console_base}/buy_credits?returnUrl=/oauth/code/success%3Fapp%3Dclaude-code"
        ),
        claude_ai_success_url: format!("{console_base}/oauth/code/success?app=claude-code"),
        manual_redirect_url: format!("{console_base}/oauth/code/callback"),
        client_id: "22422756-60c9-4084-8eb7-27705fd5cf9a".to_string(),
        oauth_file_suffix: "-local-oauth".to_string(),
        mcp_proxy_url: "http://localhost:8205".to_string(),
        mcp_proxy_path: "/v1/toolbox/shttp/mcp/{server_id}".to_string(),
    }
}

/// Maps to: CC `constants/oauth.ts:179-184` `ALLOWED_OAUTH_BASE_URLS`.
const ALLOWED_OAUTH_BASE_URLS: [&str; 3] = [
    "https://beacon.claude-ai.staging.ant.dev",
    "https://claude.fedstart.com",
    "https://claude-staging.fedstart.com",
];

/// Maps to: CC `constants/oauth.ts:186-238` `getOauthConfig`.
/// L1 (`Compile-time distribution capability projection`): the explicit
/// `BuildAudience` is a test-only projection of CC distribution identity;
/// production enters through [`get_oauth_config`].
pub fn get_oauth_config_for_audience(
    get_env: impl Fn(&str) -> Option<String>,
    audience: crate::utils::build_profile::BuildAudience,
) -> anyhow::Result<OauthConfig> {
    let mut config = if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Authentication,
    ) {
        if crate::utils::env_utils::is_env_truthy(get_env("USE_LOCAL_OAUTH").as_deref()) {
            get_local_oauth_config(&get_env)
        } else if crate::utils::env_utils::is_env_truthy(get_env("USE_STAGING_OAUTH").as_deref()) {
            STAGING_OAUTH_CONFIG.clone()
        } else {
            PROD_OAUTH_CONFIG.clone()
        }
    } else {
        PROD_OAUTH_CONFIG.clone()
    };

    use crate::utils::process_env::JsTruthy as _;
    if let Some(oauth_base_url) = get_env("CLAUDE_CODE_CUSTOM_OAUTH_URL").truthy() {
        // `oauthBaseUrl.replace(/\/$/, '')`: one trailing slash.
        let base = oauth_base_url
            .strip_suffix('/')
            .unwrap_or(&oauth_base_url)
            .to_string();
        if !ALLOWED_OAUTH_BASE_URLS.contains(&base.as_str()) {
            anyhow::bail!("CLAUDE_CODE_CUSTOM_OAUTH_URL is not an approved endpoint.");
        }
        config.base_api_url = base.clone();
        config.console_authorize_url = format!("{base}/oauth/authorize");
        config.claude_ai_authorize_url = format!("{base}/oauth/authorize");
        config.claude_ai_origin = base.clone();
        config.token_url = format!("{base}/v1/oauth/token");
        config.api_key_url = format!("{base}/api/oauth/claude_cli/create_api_key");
        config.roles_url = format!("{base}/api/oauth/claude_cli/roles");
        config.console_success_url = format!("{base}/oauth/code/success?app=claude-code");
        config.claude_ai_success_url = format!("{base}/oauth/code/success?app=claude-code");
        config.manual_redirect_url = format!("{base}/oauth/code/callback");
        config.oauth_file_suffix = "-custom-oauth".to_string();
    }

    if let Some(client_id) = get_env("CLAUDE_CODE_OAUTH_CLIENT_ID") {
        if !client_id.is_empty() {
            config.client_id = client_id;
        }
    }

    Ok(config)
}

/// Maps to: CC `constants/oauth.ts:186-238` `getOauthConfig`.
pub fn get_oauth_config() -> anyhow::Result<OauthConfig> {
    get_oauth_config_for_audience(
        |key| crate::utils::process_env::var(key),
        crate::utils::build_profile::build_audience(),
    )
}

/// Maps to: CC `constants/oauth.ts:18-29` `fileSuffixForOauthConfig`.
/// L1 (`Compile-time distribution capability projection`): the explicit
/// `BuildAudience` is a test-only projection; production enters through
/// [`file_suffix_for_oauth_config`].
pub fn file_suffix_for_oauth_config_for_audience(
    get_env: impl Fn(&str) -> Option<String>,
    audience: crate::utils::build_profile::BuildAudience,
) -> String {
    use crate::utils::process_env::JsTruthy as _;
    if get_env("CLAUDE_CODE_CUSTOM_OAUTH_URL").truthy().is_some() {
        return "-custom-oauth".to_string();
    }
    if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Authentication,
    ) {
        if crate::utils::env_utils::is_env_truthy(get_env("USE_LOCAL_OAUTH").as_deref()) {
            return "-local-oauth".to_string();
        }
        if crate::utils::env_utils::is_env_truthy(get_env("USE_STAGING_OAUTH").as_deref()) {
            return "-staging-oauth".to_string();
        }
    }
    String::new()
}

/// Maps to: CC `constants/oauth.ts:18-29` `fileSuffixForOauthConfig`.
pub fn file_suffix_for_oauth_config() -> String {
    file_suffix_for_oauth_config_for_audience(
        |key| crate::utils::process_env::var(key),
        crate::utils::build_profile::build_audience(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| map.get(key).map(|value| value.to_string())
    }

    #[test]
    fn oauth_side_effect_switch_is_single_typed_and_default_closed() {
        assert!(!OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED);
        let error = anyhow::Error::new(OAuthCredentialSideEffectsUnavailable);
        assert_eq!(
            error.root_cause().to_string(),
            OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE
        );
        assert!(
            error
                .downcast_ref::<OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
    }

    #[test]
    fn oauth_config_defaults_to_prod_like_official() {
        let config = get_oauth_config_for_audience(
            env(HashMap::new()),
            crate::utils::build_profile::build_audience(),
        )
        .unwrap();
        assert_eq!(config.claude_ai_origin, "https://claude.ai");
        assert_eq!(config.oauth_file_suffix, "");
        assert_eq!(config.mcp_proxy_path, "/v1/mcp/{server_id}");
    }

    #[test]
    fn ant_staging_and_local_oauth_config_match_official_urls() {
        let config = get_oauth_config_for_audience(
            env(HashMap::from([("USE_STAGING_OAUTH", "1")])),
            crate::utils::build_profile::BuildAudience::AnthropicInternal,
        )
        .unwrap();
        assert_eq!(config.claude_ai_origin, "https://claude-ai.staging.ant.dev");
        assert_eq!(config.oauth_file_suffix, "-staging-oauth");

        let config = get_oauth_config_for_audience(
            env(HashMap::from([
                ("USE_LOCAL_OAUTH", "true"),
                ("CLAUDE_LOCAL_OAUTH_APPS_BASE", "http://apps.local/"),
                ("CLAUDE_LOCAL_OAUTH_API_BASE", "http://api.local/"),
                ("CLAUDE_LOCAL_OAUTH_CONSOLE_BASE", "http://console.local/"),
            ])),
            crate::utils::build_profile::BuildAudience::AnthropicInternal,
        )
        .unwrap();
        assert_eq!(config.claude_ai_origin, "http://apps.local");
        assert_eq!(config.token_url, "http://api.local/v1/oauth/token");
        assert_eq!(config.oauth_file_suffix, "-local-oauth");
    }

    #[test]
    fn custom_oauth_url_is_allowlisted_like_official() {
        assert!(
            get_oauth_config_for_audience(
                env(HashMap::from([(
                    "CLAUDE_CODE_CUSTOM_OAUTH_URL",
                    "https://evil.example"
                )])),
                crate::utils::build_profile::build_audience(),
            )
            .is_err()
        );
        let config = get_oauth_config_for_audience(
            env(HashMap::from([
                (
                    "CLAUDE_CODE_CUSTOM_OAUTH_URL",
                    "https://claude.fedstart.com/",
                ),
                ("CLAUDE_CODE_OAUTH_CLIENT_ID", "client-override"),
            ])),
            crate::utils::build_profile::build_audience(),
        )
        .unwrap();
        assert_eq!(config.claude_ai_origin, "https://claude.fedstart.com");
        assert_eq!(config.client_id, "client-override");
        assert_eq!(config.oauth_file_suffix, "-custom-oauth");
        // `if (oauthBaseUrl)`: an empty value is no override.
        let config = get_oauth_config_for_audience(
            env(HashMap::from([("CLAUDE_CODE_CUSTOM_OAUTH_URL", "")])),
            crate::utils::build_profile::build_audience(),
        )
        .unwrap();
        assert_eq!(config.claude_ai_origin, PROD_OAUTH_CONFIG.claude_ai_origin);
        // `replace(/\/$/, '')` drops one slash, which leaves this unlisted.
        assert!(
            get_oauth_config_for_audience(
                env(HashMap::from([(
                    "CLAUDE_CODE_CUSTOM_OAUTH_URL",
                    "https://claude.fedstart.com//"
                )])),
                crate::utils::build_profile::build_audience(),
            )
            .is_err()
        );
    }

    #[test]
    fn file_suffix_matches_official_selection_order() {
        assert_eq!(
            file_suffix_for_oauth_config_for_audience(
                env(HashMap::new()),
                crate::utils::build_profile::build_audience(),
            ),
            ""
        );
        assert_eq!(
            file_suffix_for_oauth_config_for_audience(
                env(HashMap::from([("USE_LOCAL_OAUTH", "1")])),
                crate::utils::build_profile::BuildAudience::AnthropicInternal,
            ),
            "-local-oauth"
        );
        assert_eq!(
            file_suffix_for_oauth_config_for_audience(
                env(HashMap::from([(
                    "CLAUDE_CODE_CUSTOM_OAUTH_URL",
                    "https://claude.fedstart.com"
                )])),
                crate::utils::build_profile::build_audience(),
            ),
            "-custom-oauth"
        );
        assert_eq!(
            file_suffix_for_oauth_config_for_audience(
                env(HashMap::from([("CLAUDE_CODE_CUSTOM_OAUTH_URL", "")])),
                crate::utils::build_profile::build_audience(),
            ),
            ""
        );
    }
}
