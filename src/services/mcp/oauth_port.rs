//! OAuth redirect port helpers.
//! Maps to: CC `services/mcp/oauthPort.ts`.

use std::net::TcpListener;

/// Maps to: CC `services/mcp/oauthPort.ts:10-13` `REDIRECT_PORT_RANGE`.
#[cfg(target_os = "windows")]
const REDIRECT_PORT_RANGE: (u16, u16) = (39_152, 49_151);
/// Maps to: CC `services/mcp/oauthPort.ts:10-13` `REDIRECT_PORT_RANGE`.
#[cfg(not(target_os = "windows"))]
const REDIRECT_PORT_RANGE: (u16, u16) = (49_152, 65_535);

/// Maps to: CC `services/mcp/oauthPort.ts:14` `REDIRECT_PORT_FALLBACK`.
const REDIRECT_PORT_FALLBACK: u16 = 3118;

/// Maps to: CC `services/mcp/oauthPort.ts:21-25` `buildRedirectUri`.
///
/// `None` is Rust's projection of the source default parameter.
pub fn build_redirect_uri(port: Option<u16>) -> String {
    format!(
        "http://localhost:{}/callback",
        port.unwrap_or(REDIRECT_PORT_FALLBACK)
    )
}

/// Maps to: CC `services/mcp/oauthPort.ts:27-33` `getMcpOAuthCallbackPort`.
fn get_mcp_oauth_callback_port() -> Option<u16> {
    let value = crate::utils::process_env::var("MCP_OAUTH_CALLBACK_PORT")?;
    let digits = value
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let port = digits.parse::<u64>().ok()?;
    u16::try_from(port).ok().filter(|port| *port > 0)
}

/// Maps to: CC `services/mcp/oauthPort.ts:36-92` `findAvailablePort`.
pub async fn find_available_port() -> anyhow::Result<u16> {
    if let Some(configured_port) = get_mcp_oauth_callback_port() {
        return Ok(configured_port);
    }

    let (min, max) = REDIRECT_PORT_RANGE;
    let range = u32::from(max - min) + 1;
    let max_attempts = range.min(100);
    for _ in 0..max_attempts {
        let offset = (uuid::Uuid::new_v4().as_u128() % u128::from(range)) as u16;
        let port = min + offset;
        // Deviation (L2, user-authorized OAuth safety gate): CC temporarily
        // binds this prepared random callback port. Reject only at the final
        // OAuth-only socket outlet; do not extract a parallel gate function.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    // The source independently tries its fallback listener, so this alternate
    // final socket outlet uses the same sole product switch.
    if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
        return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
    }
    if TcpListener::bind(("127.0.0.1", REDIRECT_PORT_FALLBACK)).is_ok() {
        return Ok(REDIRECT_PORT_FALLBACK);
    }

    anyhow::bail!("No available ports for OAuth redirect")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn redirect_uri_and_callback_env_match_official_oauth_port_helpers() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(build_redirect_uri(None), "http://localhost:3118/callback");
        assert_eq!(
            build_redirect_uri(Some(4567)),
            "http://localhost:4567/callback"
        );
        let _env = EnvVarGuard::set("MCP_OAUTH_CALLBACK_PORT", " 4567trailing");
        assert_eq!(get_mcp_oauth_callback_port(), Some(4567));
        assert_eq!(
            REDIRECT_PORT_RANGE,
            if cfg!(target_os = "windows") {
                (39_152, 49_151)
            } else {
                (49_152, 65_535)
            }
        );
    }

    #[tokio::test]
    async fn find_available_port_prefers_configured_callback_port_like_official() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = EnvVarGuard::set("MCP_OAUTH_CALLBACK_PORT", "4567");
        assert_eq!(find_available_port().await.unwrap(), 4567);
    }

    #[tokio::test]
    async fn oauth_port_probe_uses_the_single_default_closed_switch() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = EnvVarGuard::unset("MCP_OAUTH_CALLBACK_PORT");
        let error = find_available_port()
            .await
            .expect_err("OAuth-only port probes must not bind by default");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
    }
}
