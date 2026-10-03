//! Claude OAuth client transport.
//! Maps to: CC `services/oauth/client.ts`.
//!
//! The live slice refreshes an existing Claude.ai token for the MCP proxy.
//! Authorization-code login, profile enrichment, account/config updates, roles,
//! and API-key creation remain explicit partial seams.

use crate::constants::oauth::{
    CLAUDE_AI_INFERENCE_SCOPE, CLAUDE_AI_OAUTH_SCOPES, get_oauth_config,
};
use crate::utils::auth::ClaudeAiOAuthTokensSnapshot;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maps to: CC `services/oauth/client.ts:38-40` `shouldUseClaudeAIAuth`.
pub fn should_use_claude_ai_auth(scopes: Option<&[String]>) -> bool {
    scopes.is_some_and(|scopes| {
        scopes
            .iter()
            .any(|scope| scope == CLAUDE_AI_INFERENCE_SCOPE)
    })
}

/// Maps to: CC `services/oauth/client.ts:42-44` `parseScopes`.
pub fn parse_scopes(scope_string: Option<&str>) -> Vec<String> {
    scope_string
        .map(|scope_string| {
            scope_string
                .split(' ')
                .filter(|scope| !scope.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Maps to: CC `services/oauth/client.ts:344-352` `isOAuthTokenExpired`.
pub fn is_oauth_token_expired(expires_at: Option<u64>) -> bool {
    let Some(expires_at) = expires_at else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    now.saturating_add(5 * 60 * 1000) >= expires_at
}

#[cfg(test)]
static LAST_PREPARED_REFRESH_REQUEST: std::sync::Mutex<Option<(String, Value)>> =
    std::sync::Mutex::new(None);

/// Maps to: CC `services/oauth/client.ts:146-274` `refreshOAuthToken`.
///
/// The existing Rust token snapshot is the typed transport used by the live MCP
/// proxy caller. Requested scopes model the source `options.scopes`; an absent
/// or empty list delegates to canonical `CLAUDE_AI_OAUTH_SCOPES`.
///
/// Partial seam: the source profile request, global-account update, raw profile,
/// and token-account projection are not available in the current Rust auth
/// lifecycle. Existing subscription/rate-limit values are therefore carried
/// through unchanged. The current transport also preserves its prior scope and
/// one-hour expiry fallbacks for incomplete refresh responses.
pub async fn refresh_oauth_token(
    tokens: &ClaudeAiOAuthTokensSnapshot,
    requested_scopes: Option<&[String]>,
) -> anyhow::Result<ClaudeAiOAuthTokensSnapshot> {
    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .filter(|refresh_token| !refresh_token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("OAuth refresh token is unavailable"))?;
    let config = get_oauth_config()?;
    let requested_scopes = requested_scopes
        .filter(|scopes| !scopes.is_empty())
        .map(|scopes| scopes.to_vec())
        .unwrap_or_else(|| {
            CLAUDE_AI_OAUTH_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect()
        });
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": config.client_id,
        "scope": requested_scopes.join(" "),
    });
    #[cfg(test)]
    {
        *LAST_PREPARED_REFRESH_REQUEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((config.token_url.clone(), body.clone()));
    }

    // CC `axios.post` (`client.ts:166-169`), through the global interceptor;
    // `create_axios_instance` resolves the same proxy, NO_PROXY, mTLS and CA.
    let client = crate::utils::proxy::create_axios_instance()?
        .timeout(Duration::from_millis(15_000))
        .build()?;
    let request = client
        .post(&config.token_url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body);
    // Deviation (L2, user-authorized OAuth safety gate): CC
    // `services/oauth/client.ts:169-174` sends the prepared refresh request;
    // Cometix fails closed at the final HTTP outlet with no runtime bypass.
    if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
        return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
    }
    let response = request.send().await?;
    if response.status() != reqwest::StatusCode::OK {
        anyhow::bail!("Token refresh failed: {}", response.status());
    }

    let data = response.json::<Value>().await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();

    // CC: success/failure logEvent calls join with the intentionally omitted
    // analytics subsystem; this source owner keeps the live transport result.
    let access_token = data
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Token refresh did not return access_token"))?
        .to_string();
    let refresh_token = data
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| tokens.refresh_token.clone());
    let expires_in_secs = data
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    let parsed_scopes = parse_scopes(data.get("scope").and_then(Value::as_str));
    let scopes = if parsed_scopes.is_empty() {
        tokens.scopes.clone()
    } else {
        parsed_scopes
    };
    Ok(ClaudeAiOAuthTokensSnapshot {
        access_token,
        refresh_token,
        expires_at: Some(now.saturating_add(expires_in_secs.saturating_mul(1000))),
        scopes,
        subscription_type: tokens.subscription_type.clone(),
        rate_limit_tier: tokens.rate_limit_tier.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn tokens() -> ClaudeAiOAuthTokensSnapshot {
        ClaudeAiOAuthTokensSnapshot {
            access_token: "old-access".to_string(),
            refresh_token: Some("old-refresh".to_string()),
            expires_at: Some(1),
            scopes: vec![CLAUDE_AI_INFERENCE_SCOPE.to_string()],
            subscription_type: Some("pro".to_string()),
            rate_limit_tier: Some("default_claude_pro".to_string()),
        }
    }

    #[test]
    fn should_use_claude_ai_auth_matches_official_scope_check() {
        assert!(!should_use_claude_ai_auth(None));
        assert!(!should_use_claude_ai_auth(Some(&[
            "user:profile".to_string()
        ])));
        assert!(should_use_claude_ai_auth(Some(&[
            "user:profile".to_string(),
            "user:inference".to_string(),
        ])));
    }

    #[test]
    fn parse_scopes_matches_official_space_split() {
        assert_eq!(
            parse_scopes(Some("user:profile  user:inference")),
            vec!["user:profile", "user:inference"]
        );
        assert!(parse_scopes(None).is_empty());
    }

    #[test]
    fn is_oauth_token_expired_matches_official_five_minute_buffer() {
        assert!(!is_oauth_token_expired(None));
        assert!(is_oauth_token_expired(Some(0)));
        assert!(!is_oauth_token_expired(Some(u64::MAX)));
    }

    #[test]
    fn refresh_oauth_token_preserves_canonical_custom_endpoint_validation() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _custom = EnvVarGuard::set("CLAUDE_CODE_CUSTOM_OAUTH_URL", "https://evil.example");
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(refresh_oauth_token(&tokens(), None))
            .expect_err("unapproved endpoint must fail before transport");
        assert_eq!(
            error.to_string(),
            "CLAUDE_CODE_CUSTOM_OAUTH_URL is not an approved endpoint."
        );
    }

    #[test]
    fn refresh_oauth_preparation_and_gate_closed_behavior_match_contract() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _local = EnvVarGuard::unset("USE_LOCAL_OAUTH");
        let _api_base = EnvVarGuard::unset("CLAUDE_LOCAL_OAUTH_API_BASE");
        let _staging = EnvVarGuard::unset("USE_STAGING_OAUTH");
        let _custom = EnvVarGuard::unset("CLAUDE_CODE_CUSTOM_OAUTH_URL");
        let _client_override = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_CLIENT_ID");

        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(refresh_oauth_token(&tokens(), None))
            .expect_err("credential HTTP send must be default-closed");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        let (token_url, body) = LAST_PREPARED_REFRESH_REQUEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("refresh preparation must complete before the product gate");
        assert_eq!(token_url, "https://platform.claude.com/v1/oauth/token");
        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["refresh_token"], "old-refresh");
        assert_eq!(body["client_id"], "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(body["scope"], CLAUDE_AI_OAUTH_SCOPES.join(" "));
    }
}
