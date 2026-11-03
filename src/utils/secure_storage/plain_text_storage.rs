//! Maps to: CC `utils/secureStorage/plainTextStorage.ts`.

use super::{SecureStorageBackend, SecureStorageData, SecureStorageUpdate};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

pub(crate) struct PlainTextStorage;

#[derive(Debug)]
enum CredentialPersistenceError {
    Write(io::Error),
    Acl(io::Error),
}

fn apply_windows_credential_acl(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_acl::set_owner_only_acl(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(windows)]
fn replace_credential_file(temporary: &Path, dest: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let dest = dest
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            dest.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_credential_file(temporary: &Path, dest: &Path) -> io::Result<()> {
    std::fs::rename(temporary, dest)
}

fn persist_credential_file(
    dest: &Path,
    serialized: &str,
    apply_acl: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), CredentialPersistenceError> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(
        ".nyan-credentials-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(CredentialPersistenceError::Write)?;

    let result = (|| {
        apply_acl(&temporary).map_err(CredentialPersistenceError::Acl)?;
        file.write_all(serialized.as_bytes())
            .map_err(CredentialPersistenceError::Write)?;
        file.flush().map_err(CredentialPersistenceError::Write)?;
        file.sync_all().map_err(CredentialPersistenceError::Write)
    })();
    drop(file);

    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = replace_credential_file(&temporary, dest) {
        let _ = std::fs::remove_file(&temporary);
        return Err(CredentialPersistenceError::Write(error));
    }
    Ok(())
}

#[cfg(windows)]
mod windows_acl {
    use super::*;
    use std::{mem::size_of, os::windows::ffi::OsStrExt, ptr::null_mut, slice};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE},
        Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW},
        Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, GetLengthSid,
            GetTokenInformation, InitializeAcl, PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{DELETE, FILE_GENERIC_READ, FILE_GENERIC_WRITE},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    fn last_error() -> io::Error {
        io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
    }

    fn current_user_sid() -> io::Result<Vec<u8>> {
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(last_error());
        }
        let _token = OwnedHandle(token);

        let mut token_user_size = 0u32;
        unsafe {
            let _ = GetTokenInformation(token, TokenUser, null_mut(), 0, &mut token_user_size);
        }
        if token_user_size == 0 {
            return Err(last_error());
        }
        let word_count = (token_user_size as usize).div_ceil(size_of::<u64>());
        let mut token_user_buffer = vec![0u64; word_count];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_user_buffer.as_mut_ptr().cast(),
                token_user_size,
                &mut token_user_size,
            )
        } == 0
        {
            return Err(last_error());
        }

        let token_user = unsafe { &*token_user_buffer.as_ptr().cast::<TOKEN_USER>() };
        if token_user.User.Sid.is_null() {
            return Err(io::Error::other("Windows token did not contain a user SID"));
        }
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
        if sid_length == 0 {
            return Err(last_error());
        }
        Ok(
            unsafe { slice::from_raw_parts(token_user.User.Sid.cast::<u8>(), sid_length as usize) }
                .to_vec(),
        )
    }

    pub(super) fn set_owner_only_acl(path: &Path) -> io::Result<()> {
        let sid = current_user_sid()?;
        let acl_size =
            size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() + sid.len() - size_of::<u32>();
        if acl_size > u16::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows user SID is too large for a file ACL",
            ));
        }
        let mut acl_buffer = vec![0u64; acl_size.div_ceil(size_of::<u64>())];
        let acl = acl_buffer.as_mut_ptr().cast::<ACL>();
        if unsafe { InitializeAcl(acl, acl_size as u32, ACL_REVISION) } == 0 {
            return Err(last_error());
        }
        let access_mask = DELETE | FILE_GENERIC_READ | FILE_GENERIC_WRITE;
        if unsafe {
            windows_sys::Win32::Security::AddAccessAllowedAce(
                acl,
                ACL_REVISION,
                access_mask,
                sid.as_ptr().cast_mut().cast(),
            )
        } == 0
        {
            return Err(last_error());
        }

        let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
        wide_path.push(0);
        let result = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null_mut(),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        Ok(())
    }
}

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
    persist_credential_file(dest, &serialized, apply_windows_credential_acl).is_ok()
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
        if let Err(error) =
            crate::utils::fs_operations::get_fs_implementation().mkdir_sync(&storage_dir, None)
            && crate::utils::errors::io_errno_code(&error) != Some("EEXIST")
        {
            return Ok(SecureStorageUpdate::default());
        }
        let updated = match persist_credential_file(
            &storage_path,
            &serialized,
            apply_windows_credential_acl,
        ) {
            Ok(()) => true,
            Err(CredentialPersistenceError::Write(_)) => false,
            Err(CredentialPersistenceError::Acl(error)) => {
                return Err(anyhow::anyhow!(
                    "Could not secure Windows credential file {}: {error}. Credentials were not saved; set CLAUDE_CODE_OAUTH_TOKEN instead.",
                    storage_path.display()
                ));
            }
        };
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

    #[test]
    fn credential_file_rejects_mocked_acl_failure_and_preserves_old_file() {
        let path = std::env::temp_dir().join(format!("nyan-cred-acl-mock-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "old secret").unwrap();
        let result = persist_credential_file(&path, "new secret", |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "ACL denied",
            ))
        });
        assert!(matches!(result, Err(CredentialPersistenceError::Acl(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old secret");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(windows)]
    #[test]
    fn windows_smoke_plaintext_credentials_replace_existing_file_with_owner_only_acl() {
        let path =
            std::env::temp_dir().join(format!("nyan-cred-acl-windows-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "old secret").unwrap();
        persist_credential_file(&path, "new secret", apply_windows_credential_acl)
            .expect("Windows credential ACL should replace the existing file");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new secret");
        let _ = std::fs::remove_file(&path);
    }
}
