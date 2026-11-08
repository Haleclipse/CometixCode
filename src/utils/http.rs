//! HTTP utility constants and helpers.
//! Maps to: CC `utils/http.ts`.
//!
//! Auth header helpers and OAuth retry remain in their existing official
//! service/auth boundaries. This module owns the shared user-agent helpers that
//! were previously duplicated in API/MCP/WebFetch code.

/// One outbound User-Agent for model, WebFetch, MCP, and every other HTTP client.
///
/// `nyan/<version> (<os>; <arch>)`. Provider compatibility strings are not sent;
/// model traffic goes through the CPA gateway.
pub fn user_agent() -> String {
    format!(
        "nyan/{} ({}; {})",
        crate::constants::product::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

pub fn get_user_agent() -> String {
    user_agent()
}

pub fn get_mcp_user_agent() -> String {
    user_agent()
}

pub fn get_web_fetch_user_agent() -> String {
    user_agent()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn every_http_surface_uses_the_same_nyan_user_agent() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = [
            EnvVarGuard::set("NYAN_ENTRYPOINT", "sdk"),
            EnvVarGuard::set("CLAUDE_AGENT_SDK_VERSION", "1.2.3"),
            EnvVarGuard::set("CLAUDE_AGENT_SDK_CLIENT_APP", "my-app/1.0"),
        ];
        let expected = format!(
            "nyan/{} ({}; {})",
            crate::constants::product::VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        assert_eq!(user_agent(), expected);
        assert_eq!(get_user_agent(), expected);
        assert_eq!(get_mcp_user_agent(), expected);
        assert_eq!(get_web_fetch_user_agent(), expected);
    }
}
