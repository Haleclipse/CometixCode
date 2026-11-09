//! Session ingress authentication helpers.
//! Maps to: CC `utils/sessionIngressAuth.ts`.
//!
//! This read-only slice preserves the official lookup order used by MCP auth
//! state and transports: environment variable, legacy file descriptor, then the
//! CCR well-known fallback file. Unlike CC `maybePersistTokenForSubprocesses`,
//! Nyan does not write the token back to disk in this helper.
//!
//! @nyan: the token is always sent as a bearer. CC's claude.ai session-key
//! form (`sk-ant-sid…` as a `sessionKey` cookie plus `X-Organization-Uuid`)
//! is not ported.

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
    if let Ok(cache) = SESSION_INGRESS_TOKEN_CACHE.lock()
        && let Some(cached) = cache.clone()
    {
        return cached;
    }

    let fallback_path = env_value(get_env, "NYAN_SESSION_INGRESS_TOKEN_FILE")
        .unwrap_or_else(|| CCR_SESSION_INGRESS_TOKEN_PATH.to_string());
    let token = match env_value(get_env, "NYAN_WEBSOCKET_AUTH_FILE_DESCRIPTOR") {
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
    env_value(get_env, "NYAN_SESSION_ACCESS_TOKEN")
        .or_else(|| get_token_from_file_descriptor_with_env(get_env))
}

/// Maps to: CC `sessionIngressAuth.ts#getSessionIngressAuthToken`.
pub fn get_session_ingress_auth_token() -> Option<String> {
    get_session_ingress_auth_token_with_env(&|key| crate::utils::process_env::var(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ingress_auth_prefers_trimmed_env_token_like_official() {
        let token = get_session_ingress_auth_token_with_env(&|key| match key {
            "NYAN_SESSION_ACCESS_TOKEN" => Some("  jwt-token\n".to_string()),
            _ => None,
        });
        assert_eq!(token.as_deref(), Some("jwt-token"));
    }
}
