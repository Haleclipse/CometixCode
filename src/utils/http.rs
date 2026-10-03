//! HTTP utility constants and helpers.
//! Maps to: CC `utils/http.ts`.
//!
//! Auth header helpers and OAuth retry remain in their existing official
//! service/auth boundaries. This module owns the shared user-agent helpers that
//! were previously duplicated in API/MCP/WebFetch code.

use crate::utils::process_env::JsTruthy;

fn env_non_empty(key: &str) -> Option<String> {
    crate::utils::process_env::var(key).truthy()
}

/// Maps to: CC `utils/http.ts#getUserAgent`.
pub fn get_user_agent() -> String {
    let agent_sdk_version = env_non_empty("CLAUDE_AGENT_SDK_VERSION")
        .map(|value| format!(", agent-sdk/{value}"))
        .unwrap_or_default();
    let client_app = env_non_empty("CLAUDE_AGENT_SDK_CLIENT_APP")
        .map(|value| format!(", client-app/{value}"))
        .unwrap_or_default();
    let workload = crate::utils::workload_context::get_workload()
        .map(|value| format!(", workload/{value}"))
        .unwrap_or_default();
    let user_type = crate::utils::build_profile::build_audience().as_str();
    let entrypoint = crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").unwrap_or_else(|| "cli".to_string());
    format!(
        "claude-cli/{} ({user_type}, {entrypoint}{agent_sdk_version}{client_app}{workload})",
        crate::constants::product::USER_AGENT_VERSION
    )
}

/// Maps to: CC `utils/http.ts#getMCPUserAgent`.
pub fn get_mcp_user_agent() -> String {
    let mut parts = Vec::new();
    if let Some(entrypoint) = env_non_empty("CLAUDE_CODE_ENTRYPOINT") {
        parts.push(entrypoint);
    }
    if let Some(version) = env_non_empty("CLAUDE_AGENT_SDK_VERSION") {
        parts.push(format!("agent-sdk/{version}"));
    }
    if let Some(client_app) = env_non_empty("CLAUDE_AGENT_SDK_CLIENT_APP") {
        parts.push(format!("client-app/{client_app}"));
    }
    let suffix = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };
    format!(
        "claude-code/{}{}",
        crate::constants::product::VERSION,
        suffix
    )
}

/// Maps to: CC `utils/http.ts#getWebFetchUserAgent`.
pub fn get_web_fetch_user_agent() -> String {
    format!(
        "Claude-User ({}; +https://support.anthropic.com/)",
        crate::utils::user_agent::get_claude_code_user_agent()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn user_agent_matches_official_http_shape() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = [
            EnvVarGuard::set("CLAUDE_CODE_ENTRYPOINT", "cli"),
            EnvVarGuard::set("CLAUDE_AGENT_SDK_VERSION", "1.2.3"),
            EnvVarGuard::set("CLAUDE_AGENT_SDK_CLIENT_APP", "my-app/1.0"),
        ];
        assert_eq!(
            get_user_agent(),
            format!(
                "claude-cli/{} ({}, cli, agent-sdk/1.2.3, client-app/my-app/1.0)",
                crate::constants::product::USER_AGENT_VERSION,
                crate::utils::build_profile::build_audience().as_str()
            )
        );
    }

    #[test]
    fn mcp_user_agent_matches_official_optional_suffixes() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = [
            EnvVarGuard::set("CLAUDE_CODE_ENTRYPOINT", "sdk"),
            EnvVarGuard::set("CLAUDE_AGENT_SDK_VERSION", "1.2.3"),
            EnvVarGuard::unset("CLAUDE_AGENT_SDK_CLIENT_APP"),
        ];
        assert_eq!(
            get_mcp_user_agent(),
            format!(
                "claude-code/{} (sdk, agent-sdk/1.2.3)",
                crate::constants::product::VERSION
            )
        );
    }

    #[test]
    fn web_fetch_user_agent_uses_public_claude_user_identity() {
        assert_eq!(
            get_web_fetch_user_agent(),
            format!(
                "Claude-User (claude-code/{}; +https://support.anthropic.com/)",
                crate::constants::product::VERSION
            )
        );
    }
}
