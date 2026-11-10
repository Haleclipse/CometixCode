//! File-descriptor credential discovery.
//!
//! Maps to: CC `utils/authFileDescriptor.ts`.

use crate::utils::process_env::JsTruthy;

/// Maps to: CC `utils/authFileDescriptor.ts:107-163` `getCredentialFromFd`.
fn get_credential_from_fd(
    env_var: &str,
    get_cached: fn() -> Option<Option<String>>,
    set_cached: fn(Option<String>),
) -> Option<String> {
    if let Some(cached) = get_cached() {
        return cached;
    }

    let Some(fd_env) = crate::utils::process_env::var(env_var).truthy() else {
        set_cached(None);
        return None;
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
    let token = std::fs::read_to_string(fd_path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    set_cached(token.clone());
    token
}

/// Maps to: CC `utils/authFileDescriptor.ts:188-195`
/// `getApiKeyFromFileDescriptor`.
pub fn get_api_key_from_file_descriptor() -> Option<String> {
    get_credential_from_fd(
        "NYAN_API_KEY_FILE_DESCRIPTOR",
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
    fn descriptor_credentials_are_trimmed_and_cached_including_empty_values() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let env_key = "NYAN_TEST_AUTH_FD";
        let _fd = EnvVarGuard::preserve(env_key);
        let root = std::env::temp_dir().join(format!("nyan-auth-fd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        for (value, expected) in [("", None), ("  test-token\n", Some("test-token"))] {
            std::fs::write(&path, value).unwrap();
            let file = std::fs::File::open(&path).unwrap();
            crate::utils::process_env::set(env_key, file.as_raw_fd().to_string());
            crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
            assert_eq!(
                get_credential_from_fd(
                    env_key,
                    crate::bootstrap::state::get_api_key_from_fd,
                    crate::bootstrap::state::set_api_key_from_fd,
                )
                .as_deref(),
                expected
            );
            assert_eq!(
                crate::bootstrap::state::get_api_key_from_fd(),
                Some(expected.map(str::to_string))
            );
        }
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn absent_or_unreadable_descriptor_has_no_container_file_fallback() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _fd = EnvVarGuard::preserve("NYAN_API_KEY_FILE_DESCRIPTOR");
        for value in [None, Some("invalid"), Some("-1")] {
            match value {
                Some(value) => {
                    crate::utils::process_env::set("NYAN_API_KEY_FILE_DESCRIPTOR", value)
                }
                None => crate::utils::process_env::remove("NYAN_API_KEY_FILE_DESCRIPTOR"),
            }
            crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
            assert_eq!(get_api_key_from_file_descriptor(), None);
            assert_eq!(crate::bootstrap::state::get_api_key_from_fd(), Some(None));
        }
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
    }
}
