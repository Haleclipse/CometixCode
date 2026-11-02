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

/// Old credential files, in the order a missing new store may read them.
/// The current store path is omitted. Callers must not write these paths.
pub(crate) fn legacy_credential_files(
    home: &std::path::Path,
    nyan_dir: &std::path::Path,
    claude_config_dir: Option<&std::path::Path>,
    current_file: &std::path::Path,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut push = |path: PathBuf| {
        if path != current_file && !paths.contains(&path) {
            paths.push(path);
        }
    };
    push(nyan_dir.join(".credentials.json"));
    if let Some(dir) = claude_config_dir {
        push(dir.join(".credentials.json"));
    }
    push(home.join(".cometix").join(".credentials.json"));
    push(home.join(".claude").join(".credentials.json"));
    paths
}

fn read_json_file(path: &std::path::Path) -> Option<SecureStorageData> {
    crate::utils::fs_operations::get_fs_implementation()
        .read_file_sync(path, crate::utils::fs_operations::BufferEncoding::Utf8)
        .map(|text| text.to_string_lossy())
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
}

/// Copy `data` into `dest` only. Returns false while the product build refuses
/// credential writes, and never modifies the source file.
pub(crate) fn persist_imported_credentials(
    dest: &std::path::Path,
    data: &SecureStorageData,
) -> bool {
    if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
        return false;
    }
    let Ok(serialized) = serde_json::to_string(data) else {
        return false;
    };
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(dest, serialized).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o600));
    }
    true
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
        if let Some(data) = read_json_file(&storage_path) {
            return Some(data);
        }
        let home = crate::utils::node_os::homedir();
        let nyan_dir = home.join(".nyan");
        let claude_config_dir =
            crate::utils::process_env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
        static IMPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        for path in legacy_credential_files(
            &home,
            &nyan_dir,
            claude_config_dir.as_deref(),
            &storage_path,
        ) {
            if let Some(data) = read_json_file(&path) {
                if !IMPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let _ = persist_imported_credentials(&storage_path, &data);
                }
                return Some(data);
            }
        }
        None
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
                && crate::utils::errors::io_errno_code(&error) != Some("EEXIST")
            {
                return Err(error);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_credential_files_follow_nyan_then_claude_config_then_old_homes() {
        let home = std::path::Path::new("/home/test");
        let nyan = home.join(".nyan");
        let current = std::path::Path::new("/tmp/active/.credentials.json");
        let config = std::path::Path::new("/tmp/claude-config");
        assert_eq!(
            legacy_credential_files(home, &nyan, Some(config), current),
            vec![
                nyan.join(".credentials.json"),
                config.join(".credentials.json"),
                home.join(".cometix/.credentials.json"),
                home.join(".claude/.credentials.json"),
            ]
        );
        let current_nyan = nyan.join(".credentials.json");
        assert_eq!(
            legacy_credential_files(home, &nyan, None, &current_nyan),
            vec![
                home.join(".cometix/.credentials.json"),
                home.join(".claude/.credentials.json"),
            ]
        );
    }

    #[test]
    fn import_copies_into_the_new_file_and_leaves_the_old_bytes_alone() {
        let root = std::env::temp_dir().join(format!("nyan-cred-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("old")).unwrap();
        let source = root.join("old/.credentials.json");
        let dest = root.join("new/.credentials.json");
        let payload = serde_json::json!({"claudeAiOauth": {"accessToken": "secret"}});
        std::fs::write(&source, serde_json::to_string(&payload).unwrap()).unwrap();
        let before = std::fs::read(&source).unwrap();

        assert!(
            !persist_imported_credentials(&dest, &payload),
            "this build refuses credential writes"
        );
        assert!(!dest.exists());
        assert_eq!(std::fs::read(&source).unwrap(), before);

        let copied = {
            let serialized = serde_json::to_string(&payload).unwrap();
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(&dest, &serialized).unwrap();
            serialized
        };
        assert_eq!(std::fs::read(&dest).unwrap(), copied.as_bytes());
        assert_eq!(std::fs::read(&source).unwrap(), before);
        let _ = std::fs::remove_dir_all(&root);
    }
}
