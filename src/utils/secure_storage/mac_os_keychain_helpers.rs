//! Maps to: CC `utils/secureStorage/macOsKeychainHelpers.ts`.

use crate::utils::process_env::JsTruthy;
use sha2::{Digest, Sha256};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Maps to: CC `utils/secureStorage/macOsKeychainHelpers.ts:25`
/// `CREDENTIALS_SERVICE_SUFFIX`.
pub(crate) const CREDENTIALS_SERVICE_SUFFIX: &str = "-credentials";

/// Maps to: CC `utils/secureStorage/macOsKeychainHelpers.ts:29-41`
/// `getMacOsKeychainStorageServiceName`.
pub(crate) fn get_mac_os_keychain_storage_service_name(
    service_suffix: &str,
) -> anyhow::Result<String> {
    let config_dir = crate::utils::env_utils::get_claude_config_home_dir();
    let dir_hash = if crate::utils::process_env::var_os("CLAUDE_CONFIG_DIR")
        .as_deref()
        .truthy()
        .is_none()
    {
        String::new()
    } else {
        let digest = Sha256::digest(config_dir.to_string_lossy().as_bytes());
        let hash = digest
            .iter()
            .take(4)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("-{hash}")
    };
    let oauth_suffix = crate::constants::oauth::get_oauth_config()?.oauth_file_suffix;
    Ok(format!(
        "Claude Code{oauth_suffix}{service_suffix}{dir_hash}"
    ))
}

/// Maps to: CC `utils/secureStorage/macOsKeychainHelpers.ts:43-49`
/// `getUsername`.
pub(crate) fn get_username() -> String {
    if let Some(username) = crate::utils::process_env::var("USER").truthy() {
        return username;
    }

    #[cfg(target_os = "macos")]
    {
        let mut password = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0 as libc::c_char; 16 * 1024];
        // SAFETY: `password` and `buffer` remain alive for the call and the
        // returned pointer is read only when libc points it at `password`.
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                password.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        if status == 0 && !result.is_null() {
            // SAFETY: successful `getpwuid_r` returns a NUL-terminated name
            // whose storage is backed by the live buffer above.
            let username = unsafe { std::ffi::CStr::from_ptr((*result).pw_name) }
                .to_string_lossy()
                .into_owned();
            if !username.is_empty() {
                return username;
            }
        }
    }

    #[cfg(windows)]
    if let Some(username) = crate::utils::process_env::var("USERNAME").truthy() {
        return username;
    }

    "claude-code-user".to_string()
}

/// Maps to: CC `utils/secureStorage/macOsKeychainHelpers.ts:69`
/// `KEYCHAIN_CACHE_TTL_MS`.
pub(crate) const KEYCHAIN_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default)]
pub(crate) struct KeychainCacheRecord {
    pub(crate) data: Option<super::SecureStorageData>,
    pub(crate) cached_at: Option<Instant>,
}

/// Rust synchronization projection of CC
/// `utils/secureStorage/macOsKeychainHelpers.ts:71-85` `keychainCacheState`.
#[derive(Debug, Default)]
pub(crate) struct KeychainCacheState {
    pub(crate) cache: KeychainCacheRecord,
    pub(crate) generation: u64,
}

pub(crate) static KEYCHAIN_CACHE_STATE: LazyLock<Mutex<KeychainCacheState>> =
    LazyLock::new(|| Mutex::new(KeychainCacheState::default()));

/// Maps to: CC `utils/secureStorage/macOsKeychainHelpers.ts:87-91`
/// `clearKeychainCache`.
pub(crate) fn clear_keychain_cache() {
    let mut state = KEYCHAIN_CACHE_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.cache = KeychainCacheRecord::default();
    state.generation = state.generation.wrapping_add(1);
}

/// Maps to: CC `utils/secureStorage/macOsKeychainHelpers.ts:98-111`
/// `primeKeychainCacheFromPrefetch`.
pub(crate) fn prime_keychain_cache_from_prefetch(stdout: Option<String>) {
    let mut state = KEYCHAIN_CACHE_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.cache.cached_at.is_some() {
        return;
    }
    let data = match stdout {
        Some(stdout) => match serde_json::from_str(&stdout) {
            Ok(data) => Some(data),
            Err(_) => return,
        },
        None => None,
    };
    state.cache = KeychainCacheRecord {
        data,
        cached_at: Some(Instant::now()),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[cfg(target_os = "macos")]
    #[test]
    fn username_falls_back_to_os_account_not_logname() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _user = EnvVarGuard::unset("USER");
        let _logname = EnvVarGuard::set("LOGNAME", "wrong-logname");
        assert_ne!(get_username(), "wrong-logname");
        assert_ne!(get_username(), "claude-code-user");
    }

    #[test]
    fn service_name_and_username_match_official_default_and_custom_dir_shapes() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _config = EnvVarGuard::unset("CLAUDE_CONFIG_DIR");
        let _custom_oauth = EnvVarGuard::unset("CLAUDE_CODE_CUSTOM_OAUTH_URL");
        let _local = EnvVarGuard::unset("USE_LOCAL_OAUTH");
        let _staging = EnvVarGuard::unset("USE_STAGING_OAUTH");
        assert_eq!(
            get_mac_os_keychain_storage_service_name("").unwrap(),
            "Claude Code"
        );
        assert_eq!(
            get_mac_os_keychain_storage_service_name(CREDENTIALS_SERVICE_SUFFIX).unwrap(),
            "Claude Code-credentials"
        );
        drop(_config);
        let custom_dir = std::path::Path::new("/tmp/cometix-claude-config");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", custom_dir);
        assert_eq!(
            get_mac_os_keychain_storage_service_name(CREDENTIALS_SERVICE_SUFFIX).unwrap(),
            "Claude Code-credentials-4f825471"
        );

        let _user = EnvVarGuard::set("USER", "alice");
        assert_eq!(get_username(), "alice");
    }
}
