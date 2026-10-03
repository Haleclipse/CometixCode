//! Maps to: CC `utils/secureStorage/macOsKeychainStorage.ts`.

use super::mac_os_keychain_helpers::{
    CREDENTIALS_SERVICE_SUFFIX, get_mac_os_keychain_storage_service_name, get_username,
};
use super::{SecureStorageBackend, SecureStorageData, SecureStorageUpdate};

pub(crate) struct MacOsKeychainStorage;

impl SecureStorageBackend for MacOsKeychainStorage {
    /// Maps to: CC `utils/secureStorage/macOsKeychainStorage.ts:27` `name`.
    fn name(&self) -> &str {
        "keychain"
    }

    /// Maps to: CC `utils/secureStorage/macOsKeychainStorage.ts:28-67` `read`.
    fn read(&self) -> Option<SecureStorageData> {
        let previous = super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cache
            .clone();
        if previous.cached_at.is_some_and(|cached_at| {
            cached_at.elapsed() < super::mac_os_keychain_helpers::KEYCHAIN_CACHE_TTL
        }) {
            return previous.data;
        }

        #[cfg(all(target_os = "macos", not(test)))]
        let parsed = (|| -> Option<SecureStorageData> {
            let username = get_username();
            let service_name =
                get_mac_os_keychain_storage_service_name(CREDENTIALS_SERVICE_SUFFIX).ok()?;
            crate::utils::auth::record_auth_io(
                crate::utils::auth::AuthIoOperation::KeychainSubprocess,
            );
            let mut security = std::process::Command::new("security");
            // CC `execSyncWithDefaults_DEPRECATED` passes `env: process.env`; the carrier is its counterpart.
            crate::utils::subprocess_env::apply_process_env_std(&mut security);
            let output = security
                .args([
                    "find-generic-password",
                    "-a",
                    &username,
                    "-w",
                    "-s",
                    &service_name,
                ])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let value = String::from_utf8(output.stdout).ok()?;
            serde_json::from_str(value.trim()).ok()
        })();
        #[cfg(any(not(target_os = "macos"), test))]
        let parsed: Option<SecureStorageData> = {
            #[cfg(test)]
            crate::utils::auth::record_auth_io(
                crate::utils::auth::AuthIoOperation::KeychainSubprocess,
            );
            None
        };
        let data = parsed.or(previous.data);
        super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cache = super::mac_os_keychain_helpers::KeychainCacheRecord {
            data: data.clone(),
            cached_at: Some(std::time::Instant::now()),
        };
        data
    }

    /// Maps to: CC `utils/secureStorage/macOsKeychainStorage.ts:97-157` `update`.
    fn update(&self, data: &SecureStorageData) -> anyhow::Result<SecureStorageUpdate> {
        const SECURITY_STDIN_LINE_LIMIT: usize = 4096 - 64;
        let json_string = match serde_json::to_string(data) {
            Ok(value) => value,
            Err(_) => return Ok(SecureStorageUpdate::default()),
        };
        let username = get_username();
        let service_name = get_mac_os_keychain_storage_service_name(CREDENTIALS_SERVICE_SUFFIX)?;
        let hex_value = json_string
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let quoted_username = username.replace('\\', "\\\\").replace('"', "\\\"");
        let quoted_service_name = service_name.replace('\\', "\\\\").replace('"', "\\\"");
        let command = format!(
            "add-generic-password -U -a \"{quoted_username}\" -s \"{quoted_service_name}\" -X \"{hex_value}\"\n"
        );

        // Deviation (L2, user-authorized OAuth safety gate): all source
        // serialization and key selection above remain live; reject immediately
        // before cache invalidation or the `security` process outlet.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        super::mac_os_keychain_helpers::clear_keychain_cache();

        #[cfg(all(target_os = "macos", not(test)))]
        let updated = if command.len() <= SECURITY_STDIN_LINE_LIMIT {
            let mut security = std::process::Command::new("security");
            // CC `execaSync('security', ...)` inherits process.env; the carrier is its counterpart.
            crate::utils::subprocess_env::apply_process_env_std(&mut security);
            let mut child = match security
                .arg("-i")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => return Ok(SecureStorageUpdate::default()),
            };
            if let Some(stdin) = child.stdin.as_mut() {
                use std::io::Write;
                if stdin.write_all(command.as_bytes()).is_err() {
                    return Ok(SecureStorageUpdate::default());
                }
            }
            child.wait().map(|status| status.success()).unwrap_or(false)
        } else {
            let mut security = std::process::Command::new("security");
            // CC `execaSync('security', ...)` inherits process.env; the carrier is its counterpart.
            crate::utils::subprocess_env::apply_process_env_std(&mut security);
            security
                .args([
                    "add-generic-password",
                    "-U",
                    "-a",
                    &username,
                    "-s",
                    &service_name,
                    "-X",
                    &hex_value,
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        #[cfg(any(not(target_os = "macos"), test))]
        let updated = {
            let _ = (SECURITY_STDIN_LINE_LIMIT, command, hex_value);
            false
        };

        if updated {
            super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cache = super::mac_os_keychain_helpers::KeychainCacheRecord {
                data: Some(data.clone()),
                cached_at: Some(std::time::Instant::now()),
            };
        }
        Ok(SecureStorageUpdate {
            success: updated,
            warning: None,
        })
    }

    /// Maps to: CC `utils/secureStorage/macOsKeychainStorage.ts:159-177` `delete`.
    fn delete(&self) -> anyhow::Result<bool> {
        let username = get_username();
        let service_name = get_mac_os_keychain_storage_service_name(CREDENTIALS_SERVICE_SUFFIX)?;
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        super::mac_os_keychain_helpers::clear_keychain_cache();

        #[cfg(all(target_os = "macos", not(test)))]
        let deleted = {
            let mut security = std::process::Command::new("security");
            // CC `execSyncWithDefaults_DEPRECATED` passes `env: process.env`; the carrier is its counterpart.
            crate::utils::subprocess_env::apply_process_env_std(&mut security);
            security
                .args([
                    "delete-generic-password",
                    "-a",
                    &username,
                    "-s",
                    &service_name,
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        #[cfg(any(not(target_os = "macos"), test))]
        let deleted = false;

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn keychain_read_serves_stale_cache_when_refresh_fails_like_official() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sentinel = serde_json::json!({"claudeAiOauth": {"accessToken": "stale"}});
        super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cache = super::super::mac_os_keychain_helpers::KeychainCacheRecord {
            data: Some(sentinel.clone()),
            cached_at: Some(
                std::time::Instant::now()
                    - super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_TTL
                    - std::time::Duration::from_secs(1),
            ),
        };

        assert_eq!(MacOsKeychainStorage.read(), Some(sentinel));
        assert!(
            super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cache
                .cached_at
                .is_some_and(|cached_at| {
                    cached_at.elapsed() < super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_TTL
                })
        );
        super::super::mac_os_keychain_helpers::clear_keychain_cache();
    }

    #[test]
    fn keychain_update_and_delete_are_default_closed_before_cache_mutation() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sentinel = serde_json::json!({"sentinel": true});
        super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cache = super::super::mac_os_keychain_helpers::KeychainCacheRecord {
            data: Some(sentinel.clone()),
            cached_at: Some(std::time::Instant::now()),
        };
        let storage = MacOsKeychainStorage;
        let update_error = storage
            .update(&serde_json::json!({"claudeAiOauth": {"accessToken": "new"}}))
            .expect_err("keychain update must be default-closed");
        assert!(
            update_error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        assert_eq!(
            super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cache
                .data,
            Some(sentinel.clone())
        );
        storage
            .delete()
            .expect_err("keychain delete must be default-closed");
        assert_eq!(
            super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cache
                .data,
            Some(sentinel)
        );
        super::super::mac_os_keychain_helpers::clear_keychain_cache();
    }
}
