//! Session ingress authentication helpers.
//! Maps to: CC `utils/sessionIngressAuth.ts`.
//!
//! This read-only slice preserves the official lookup order used by MCP auth
//! state and transports: environment variable, legacy file descriptor, then the
//! CCR well-known fallback file. Unlike CC `maybePersistTokenForSubprocesses`,
//! Cometix does not write the token back to disk in this helper.

use crate::utils::auth_file_descriptor::{
    CCR_SESSION_INGRESS_TOKEN_PATH, read_token_from_well_known_file,
};
use std::sync::{LazyLock, Mutex};

static SESSION_INGRESS_TOKEN_CACHE: LazyLock<Mutex<Option<Option<String>>>> =
    LazyLock::new(|| Mutex::new(None));

fn env_value(get_env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    get_env(key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn fd_path(fd: i32) -> String {
    if cfg!(any(target_os = "macos", target_os = "freebsd")) {
        format!("/dev/fd/{fd}")
    } else {
        format!("/proc/self/fd/{fd}")
    }
}

/// Maps to: CC `sessionIngressAuth.ts#getTokenFromFileDescriptor`.
pub fn get_token_from_file_descriptor_with_env(
    get_env: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Ok(cache) = SESSION_INGRESS_TOKEN_CACHE.lock() {
        if let Some(cached) = cache.clone() {
            return cached;
        }
    }

    let fallback_path = env_value(get_env, "CLAUDE_SESSION_INGRESS_TOKEN_FILE")
        .unwrap_or_else(|| CCR_SESSION_INGRESS_TOKEN_PATH.to_string());
    let token = match env_value(get_env, "CLAUDE_CODE_WEBSOCKET_AUTH_FILE_DESCRIPTOR") {
        Some(fd_env) => fd_env
            .parse::<i32>()
            .ok()
            .and_then(|fd| read_token_from_well_known_file(&fd_path(fd), "session ingress token"))
            .or_else(|| read_token_from_well_known_file(&fallback_path, "session ingress token")),
        None => read_token_from_well_known_file(&fallback_path, "session ingress token"),
    };

    if let Ok(mut cache) = SESSION_INGRESS_TOKEN_CACHE.lock() {
        *cache = Some(token.clone());
    }
    token
}

/// Maps to: CC `sessionIngressAuth.ts#getSessionIngressAuthToken`.
pub fn get_session_ingress_auth_token_with_env(
    get_env: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    env_value(get_env, "CLAUDE_CODE_SESSION_ACCESS_TOKEN")
        .or_else(|| get_token_from_file_descriptor_with_env(get_env))
}

/// Maps to: CC `sessionIngressAuth.ts#getSessionIngressAuthToken`.
pub fn get_session_ingress_auth_token() -> Option<String> {
    get_session_ingress_auth_token_with_env(&|key| crate::utils::process_env::var(key))
}

/// Maps to: CC `sessionIngressAuth.ts#getSessionIngressAuthHeaders`.
pub fn get_session_ingress_auth_headers_with_env(
    get_env: &impl Fn(&str) -> Option<String>,
) -> std::collections::BTreeMap<String, String> {
    let Some(token) = get_session_ingress_auth_token_with_env(get_env) else {
        return std::collections::BTreeMap::new();
    };
    if token.starts_with("sk-ant-sid") {
        let mut headers = std::collections::BTreeMap::from([(
            "Cookie".to_string(),
            format!("sessionKey={token}"),
        )]);
        if let Some(org_uuid) = env_value(get_env, "CLAUDE_CODE_ORGANIZATION_UUID") {
            headers.insert("X-Organization-Uuid".to_string(), org_uuid);
        }
        headers
    } else {
        std::collections::BTreeMap::from([("Authorization".to_string(), format!("Bearer {token}"))])
    }
}

/// Maps to: CC `sessionIngressAuth.ts#getSessionIngressAuthHeaders`.
pub fn get_session_ingress_auth_headers() -> std::collections::BTreeMap<String, String> {
    get_session_ingress_auth_headers_with_env(&|key| crate::utils::process_env::var(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ingress_auth_prefers_env_token_like_official() {
        let headers = get_session_ingress_auth_headers_with_env(&|key| match key {
            "CLAUDE_CODE_SESSION_ACCESS_TOKEN" => Some("jwt-token".to_string()),
            _ => None,
        });
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer jwt-token")
        );
    }

    #[test]
    fn session_ingress_auth_builds_cookie_headers_for_session_keys() {
        let headers = get_session_ingress_auth_headers_with_env(&|key| match key {
            "CLAUDE_CODE_SESSION_ACCESS_TOKEN" => Some("sk-ant-sid-test".to_string()),
            "CLAUDE_CODE_ORGANIZATION_UUID" => Some("org-123".to_string()),
            _ => None,
        });
        assert_eq!(
            headers.get("Cookie").map(String::as_str),
            Some("sessionKey=sk-ant-sid-test")
        );
        assert_eq!(
            headers.get("X-Organization-Uuid").map(String::as_str),
            Some("org-123")
        );
    }
}
