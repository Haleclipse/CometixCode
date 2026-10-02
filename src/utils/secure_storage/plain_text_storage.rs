//! Maps to: CC `utils/secureStorage/plainTextStorage.ts`.

use super::{SecureStorageBackend, SecureStorageData, SecureStorageUpdate};
use std::path::PathBuf;

pub(crate) struct PlainTextStorage;

/// Maps to: CC `utils/secureStorage/plainTextStorage.ts:10-14` `getStoragePath`.
fn get_storage_path() -> (PathBuf, PathBuf) {
    let storage_dir = crate::utils::env_utils::get_claude_config_home_dir();
    let storage_path = storage_dir.join(".credentials.json");
    (storage_dir, storage_path)
}

impl SecureStorageBackend for PlainTextStorage {
    /// Maps to: CC `utils/secureStorage/plainTextStorage.ts:20` `name`.
    fn name(&self) -> &str {
        "plaintext"
    }

    /// Maps to: CC `utils/secureStorage/plainTextStorage.ts:21-32` `read`.
    fn read(&self) -> Option<SecureStorageData> {
        let (_, storage_path) = get_storage_path();
        crate::utils::auth::record_auth_io(crate::utils::auth::AuthIoOperation::TokenFileRead);
        crate::utils::fs_operations::get_fs_implementation()
            .read_file_sync(
                &storage_path,
                crate::utils::fs_operations::BufferEncoding::Utf8,
            )
            .map(|text| text.to_string_lossy())
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
    }

    /// Maps to: CC `utils/secureStorage/plainTextStorage.ts:44-68` `update`.
    fn update(&self, data: &SecureStorageData) -> anyhow::Result<SecureStorageUpdate> {
        let (storage_dir, storage_path) = get_storage_path();
        let serialized = match serde_json::to_string(data) {
            Ok(serialized) => serialized,
            Err(_) => return Ok(SecureStorageUpdate::default()),
        };
        // Deviation (L2, user-authorized OAuth safety gate): CC writes the
        // prepared plaintext credential object here. Directory creation,
        // bytes, and permissions remain untouched while the sole switch is off.
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        let updated = (|| -> std::io::Result<()> {
            if let Err(error) =
                crate::utils::fs_operations::get_fs_implementation().mkdir_sync(&storage_dir, None)
            {
                if crate::utils::errors::io_errno_code(&error) != Some("EEXIST") {
                    return Err(error);
                }
            }
            std::fs::write(&storage_path, serialized)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&storage_path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        })()
        .is_ok();
        Ok(SecureStorageUpdate {
            success: updated,
            warning: updated.then(|| "Warning: Storing credentials in plaintext.".to_string()),
        })
    }

    /// Maps to: CC `utils/secureStorage/plainTextStorage.ts:70-84` `delete`.
    fn delete(&self) -> anyhow::Result<bool> {
        let (_, storage_path) = get_storage_path();
        if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
            return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
        }
        match crate::utils::fs_operations::get_fs_implementation().unlink_sync(&storage_path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(_) => Ok(false),
        }
    }
}
