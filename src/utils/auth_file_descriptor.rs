//! CCR file-descriptor credential discovery.
//!
//! Maps to: CC `utils/authFileDescriptor.ts`.

use crate::utils::process_env::JsTruthy;

/// Maps to: CC `utils/authFileDescriptor.ts:23` `CCR_OAUTH_TOKEN_PATH`.
pub const CCR_OAUTH_TOKEN_PATH: &str = "/home/claude/.claude/remote/.oauth_token";
/// Maps to: CC `utils/authFileDescriptor.ts:24` `CCR_API_KEY_PATH`.
pub const CCR_API_KEY_PATH: &str = "/home/claude/.claude/remote/.api_key";
/// Maps to: CC `utils/authFileDescriptor.ts:25` `CCR_SESSION_INGRESS_TOKEN_PATH`.
pub const CCR_SESSION_INGRESS_TOKEN_PATH: &str =
    "/home/claude/.claude/remote/.session_ingress_token";

/// Maps to: CC `utils/authFileDescriptor.ts:67-91` `readTokenFromWellKnownFile`.
pub fn read_token_from_well_known_file(path: &str, _token_name: &str) -> Option<String> {
    crate::utils::auth::record_auth_io(crate::utils::auth::AuthIoOperation::TokenFileRead);
    std::fs::read_to_string(path)
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Maps to: CC `utils/authFileDescriptor.ts:107-163` `getCredentialFromFd`.
fn get_credential_from_fd(
    env_var: &str,
    well_known_path: &str,
    label: &str,
    get_cached: fn() -> Option<Option<String>>,
    set_cached: fn(Option<String>),
) -> Option<String> {
    if let Some(cached) = get_cached() {
        return cached;
    }

    let Some(fd_env) = crate::utils::process_env::var(env_var).truthy() else {
        let token = read_token_from_well_known_file(well_known_path, label);
        set_cached(token.clone());
        return token;
    };

    let bytes = fd_env.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let negative = if bytes.get(index) == Some(&b'-') {
        index += 1;
        true
    } else {
        if bytes.get(index) == Some(&b'+') {
            index += 1;
        }
        false
    };
    let start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if start == index {
        set_cached(None);
        return None;
    }
    let Some(fd) = fd_env[start..index]
        .parse::<i32>()
        .ok()
        .map(|fd| if negative { -fd } else { fd })
    else {
        set_cached(None);
        return None;
    };
    let fd_path = if cfg!(any(target_os = "macos", target_os = "freebsd")) {
        format!("/dev/fd/{fd}")
    } else {
        format!("/proc/self/fd/{fd}")
    };

    crate::utils::auth::record_auth_io(crate::utils::auth::AuthIoOperation::TokenFileRead);
    match std::fs::read_to_string(fd_path) {
        Ok(value) => {
            let token = value.trim().to_string();
            if token.is_empty() {
                set_cached(None);
                return None;
            }
            set_cached(Some(token.clone()));
            // CC: `maybePersistTokenForSubprocesses(wellKnownPath, token, label)`
            // — the source write is omitted at this call site because the
            // user-authorized OAuth credential-side-effect capability is closed.
            Some(token)
        }
        Err(_) => {
            let token = read_token_from_well_known_file(well_known_path, label);
            set_cached(token.clone());
            token
        }
    }
}

/// Maps to: CC `utils/authFileDescriptor.ts:173-180`
/// `getOAuthTokenFromFileDescriptor`.
pub fn get_oauth_token_from_file_descriptor() -> Option<String> {
    get_credential_from_fd(
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        CCR_OAUTH_TOKEN_PATH,
        "OAuth token",
        crate::bootstrap::state::get_oauth_token_from_fd,
        crate::bootstrap::state::set_oauth_token_from_fd,
    )
}

/// Maps to: CC `utils/authFileDescriptor.ts:188-195`
/// `getApiKeyFromFileDescriptor`.
pub fn get_api_key_from_file_descriptor() -> Option<String> {
    get_credential_from_fd(
        "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
        CCR_API_KEY_PATH,
        "API key",
        crate::bootstrap::state::get_api_key_from_fd,
        crate::bootstrap::state::set_api_key_from_fd,
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::os::fd::AsRawFd;

    #[test]
    fn oauth_token_prefers_and_caches_the_file_descriptor_like_official() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _fd = EnvVarGuard::preserve("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR");
        let path = std::env::temp_dir().join(format!(
            "cometix-auth-fd-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, " oauth-token\n").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        crate::utils::process_env::set(
            "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
            file.as_raw_fd().to_string(),
        );
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();

        assert_eq!(
            get_oauth_token_from_file_descriptor().as_deref(),
            Some("oauth-token")
        );
        drop(file);
        assert_eq!(
            get_oauth_token_from_file_descriptor().as_deref(),
            Some("oauth-token")
        );

        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn empty_fd_is_cached_null_without_falling_back_like_official() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let env_key = "COMETIX_TEST_EMPTY_AUTH_FD";
        let _fd = EnvVarGuard::preserve(env_key);
        let root =
            std::env::temp_dir().join(format!("cometix-empty-auth-fd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let empty_path = root.join("empty");
        let fallback_path = root.join("fallback");
        std::fs::write(&empty_path, "").unwrap();
        std::fs::write(&fallback_path, "fallback-token").unwrap();
        let file = std::fs::File::open(&empty_path).unwrap();
        crate::utils::process_env::set(env_key, file.as_raw_fd().to_string());
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();

        assert_eq!(
            get_credential_from_fd(
                env_key,
                fallback_path.to_str().unwrap(),
                "test token",
                crate::bootstrap::state::get_api_key_from_fd,
                crate::bootstrap::state::set_api_key_from_fd,
            ),
            None
        );
        assert_eq!(crate::bootstrap::state::get_api_key_from_fd(), Some(None));

        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        drop(file);
        let _ = std::fs::remove_dir_all(root);
    }
}
