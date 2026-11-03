//! Provider-owned auth files: `~/.nyan/auth/<provider>.json`.
//!
//! One file per OAuth provider, one entry per account inside it, so providers
//! and accounts never share token state. The directory is `0700` and each file
//! `0600` on Unix; on Windows every file carries a verified owner-only ACL and
//! a file that cannot be verified is refused (decision 17).
//!
//! Reads are lock-free because files are replaced atomically. Every write goes
//! through an [`AuthLock`], which holds an exclusive cross-process lock on
//! `<provider>.lock`, so read-modify-write cycles (token refresh) serialize.
//! Errors name paths and reasons, never file contents.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::reference::Secret;
use crate::utils::secure_storage::plain_text_storage::{
    verify_owner_only_file, write_owner_only_file,
};

const SCHEMA_VERSION: u32 = 1;
const MAX_PROVIDER_ID_LEN: usize = 64;

#[derive(Debug)]
pub enum AuthStoreError {
    /// The provider id is not a safe file stem.
    InvalidProvider(String),
    /// The file exists but is not owner-only, or its ACL could not be verified.
    Insecure {
        path: PathBuf,
        reason: String,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
    /// The file is not a valid auth file. Never carries file contents.
    Format {
        path: PathBuf,
        reason: String,
    },
}

impl fmt::Display for AuthStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProvider(id) => write!(
                f,
                "invalid provider id {id:?}; use 1-{MAX_PROVIDER_ID_LEN} characters of a-z, 0-9 and '-'"
            ),
            Self::Insecure { path, reason } => write!(
                f,
                "auth file {} was refused: {reason}; delete it and sign in again",
                path.display()
            ),
            Self::Io { path, source } => write!(f, "auth store {}: {source}", path.display()),
            Self::Format { path, reason } => {
                write!(f, "auth file {} is invalid: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for AuthStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Provider ids become file names, so only a conservative alphabet is allowed.
pub fn validate_provider_id(id: &str) -> Result<(), AuthStoreError> {
    let valid = !id.is_empty()
        && id.len() <= MAX_PROVIDER_ID_LEN
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(AuthStoreError::InvalidProvider(id.to_string()))
    }
}

/// OAuth state for one account. The token schema is shared by the first two
/// providers; anything provider-specific goes in `metadata`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCredential {
    pub access: String,
    pub refresh: String,
    /// Unix time in milliseconds at which `access` stops being valid.
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl OAuthCredential {
    pub fn is_expired(&self, now_ms: i64, margin_ms: i64) -> bool {
        now_ms.saturating_add(margin_ms) >= self.expires_at_ms
    }

    pub fn access_secret(&self) -> Secret {
        Secret::new(self.access.clone())
    }
}

impl fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("access", &"<redacted>")
            .field("refresh", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("account_id", &self.account_id)
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// Everything stored for one provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuth {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_account: Option<String>,
    #[serde(default)]
    pub accounts: BTreeMap<String, OAuthCredential>,
}

impl Default for ProviderAuth {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            default_account: None,
            accounts: BTreeMap::new(),
        }
    }
}

impl ProviderAuth {
    /// An explicit name must exist. Without one, the default account is used,
    /// or the only account; several accounts and no default is ambiguous and
    /// yields `None` rather than a guess.
    pub fn account(&self, name: Option<&str>) -> Option<(&str, &OAuthCredential)> {
        let key = match name {
            Some(name) => name,
            None => match self.default_account.as_deref() {
                Some(default) => default,
                None if self.accounts.len() == 1 => self.accounts.keys().next()?.as_str(),
                None => return None,
            },
        };
        self.accounts
            .get_key_value(key)
            .map(|(key, value)| (key.as_str(), value))
    }

    pub fn set_account(&mut self, name: &str, credential: OAuthCredential) {
        if self.default_account.is_none() {
            self.default_account = Some(name.to_string());
        }
        self.accounts.insert(name.to_string(), credential);
    }

    pub fn remove_account(&mut self, name: &str) -> bool {
        let removed = self.accounts.remove(name).is_some();
        if self.default_account.as_deref() == Some(name) {
            self.default_account = self.accounts.keys().next().cloned();
        }
        removed
    }
}

/// Locates auth files. Tests build one over a scratch directory with
/// [`AuthStore::at`]; production code uses [`AuthStore::user`].
#[derive(Debug, Clone)]
pub struct AuthStore {
    dir: PathBuf,
}

impl AuthStore {
    /// `~/.nyan/auth`. Deliberately not overridable by an environment
    /// variable: settings can set variables, and a repository must not be able
    /// to point token storage somewhere else.
    pub fn user() -> Self {
        Self::at(crate::utils::node_os::homedir().join(".nyan").join("auth"))
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn data_path(&self, provider: &str) -> Result<PathBuf, AuthStoreError> {
        validate_provider_id(provider)?;
        Ok(self.dir.join(format!("{provider}.json")))
    }

    fn io_error(path: &Path, source: io::Error) -> AuthStoreError {
        AuthStoreError::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    fn ensure_dir(&self) -> Result<(), AuthStoreError> {
        let result = (|| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&self.dir)?;
                std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))
            }
            #[cfg(not(unix))]
            {
                std::fs::create_dir_all(&self.dir)
            }
        })();
        result.map_err(|source| Self::io_error(&self.dir, source))
    }

    /// Reads a provider's auth file. `Ok(None)` means there is none. A file
    /// that is not owner-only (or whose ACL cannot be verified) is an error,
    /// and so is any unknown schema version.
    pub fn load(&self, provider: &str) -> Result<Option<ProviderAuth>, AuthStoreError> {
        let path = self.data_path(provider)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Self::io_error(&path, source)),
        }
        verify_owner_only_file(&path).map_err(|error| AuthStoreError::Insecure {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        let text =
            std::fs::read_to_string(&path).map_err(|source| Self::io_error(&path, source))?;
        let parsed: ProviderAuth =
            serde_json::from_str(&text).map_err(|error| AuthStoreError::Format {
                path: path.clone(),
                // serde_json's message can quote the offending value, so only
                // its category and position are reported.
                reason: format!(
                    "{:?} error at line {} column {}",
                    error.classify(),
                    error.line(),
                    error.column()
                ),
            })?;
        if parsed.version != SCHEMA_VERSION {
            return Err(AuthStoreError::Format {
                path,
                reason: format!("unsupported schema version {}", parsed.version),
            });
        }
        Ok(Some(parsed))
    }

    /// Takes the provider's exclusive lock, waiting for other holders. Blocks
    /// the calling thread; async callers use [`AuthStore::lock_async`].
    pub fn lock(&self, provider: &str) -> Result<AuthLock, AuthStoreError> {
        validate_provider_id(provider)?;
        self.ensure_dir()?;
        let path = self.dir.join(format!("{provider}.lock"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|source| Self::io_error(&path, source))?;
        file.lock()
            .map_err(|source| Self::io_error(&path, source))?;
        Ok(AuthLock {
            _file: file,
            store: self.clone(),
            provider: provider.to_string(),
        })
    }

    pub async fn lock_async(&self, provider: &str) -> Result<AuthLock, AuthStoreError> {
        let store = self.clone();
        let provider = provider.to_string();
        tokio::task::spawn_blocking(move || store.lock(&provider))
            .await
            .map_err(|error| AuthStoreError::Io {
                path: self.dir.clone(),
                source: io::Error::other(error),
            })?
    }
}

/// Proof that the provider's lock is held; the only way to write. Dropping it
/// closes the lock file, which releases the lock.
pub struct AuthLock {
    _file: File,
    store: AuthStore,
    provider: String,
}

impl AuthLock {
    pub fn load(&self) -> Result<Option<ProviderAuth>, AuthStoreError> {
        self.store.load(&self.provider)
    }

    /// Replaces the provider's file atomically with owner-only permissions.
    pub fn save(&self, auth: &ProviderAuth) -> Result<(), AuthStoreError> {
        let path = self.store.data_path(&self.provider)?;
        let text = serde_json::to_string_pretty(auth).map_err(|error| AuthStoreError::Format {
            path: path.clone(),
            reason: format!("{:?} error", error.classify()),
        })?;
        write_owner_only_file(&path, &text).map_err(|source| AuthStore::io_error(&path, source))
    }

    /// Removes the provider's file. `false` if there was none.
    pub fn delete(&self) -> Result<bool, AuthStoreError> {
        let path = self.store.data_path(&self.provider)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(AuthStore::io_error(&path, source)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> (Self, AuthStore) {
            let root = crate::utils::node_os::tmpdir().join(format!(
                "nyan-pr3-store-{name}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            let store = AuthStore::at(root.join("auth"));
            (Self(root), store)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn credential(tag: &str) -> OAuthCredential {
        OAuthCredential {
            access: format!("access-{tag}"),
            refresh: format!("refresh-{tag}"),
            expires_at_ms: 4_000_000_000_000,
            account_id: Some(format!("acct-{tag}")),
            metadata: BTreeMap::from([("email".to_string(), format!("{tag}@example.com"))]),
        }
    }

    #[test]
    fn provider_ids_are_safe_file_stems() {
        for good in ["openai-codex", "opencode", "a", "x1-2"] {
            assert!(validate_provider_id(good).is_ok(), "{good}");
        }
        let long = "a".repeat(65);
        for bad in [
            "", "../x", "a/b", "a\\b", "A", "a.b", "-a", "a b", "a\0b", ".", "..", &long,
        ] {
            assert!(validate_provider_id(bad).is_err(), "{bad:?}");
        }
        let (_scratch, store) = Scratch::new("ids");
        assert!(store.load("../escape").is_err());
        assert!(store.lock("../escape").is_err());
    }

    #[test]
    fn accounts_round_trip_and_stay_isolated_per_provider() {
        let (_scratch, store) = Scratch::new("isolation");
        assert_eq!(store.load("opencode").unwrap(), None);

        let mut codex = ProviderAuth::default();
        codex.set_account("work", credential("work"));
        codex.set_account("home", credential("home"));
        store.lock("openai-codex").unwrap().save(&codex).unwrap();
        let mut opencode = ProviderAuth::default();
        opencode.set_account("console", credential("console"));
        store.lock("opencode").unwrap().save(&opencode).unwrap();

        let loaded = store.load("openai-codex").unwrap().unwrap();
        assert_eq!(loaded, codex);
        assert_eq!(loaded.account(None).unwrap().0, "work");
        assert_eq!(
            loaded.account(Some("home")).unwrap().1.access,
            "access-home"
        );
        assert!(loaded.account(Some("console")).is_none());
        let other = store.load("opencode").unwrap().unwrap();
        assert_eq!(other.accounts.len(), 1);
        assert!(!other.accounts.contains_key("work"));
    }

    #[test]
    fn account_choice_is_never_a_guess() {
        let mut auth = ProviderAuth::default();
        assert!(auth.account(None).is_none());
        auth.accounts.insert("a".into(), credential("a"));
        assert_eq!(auth.account(None).unwrap().0, "a");
        auth.accounts.insert("b".into(), credential("b"));
        assert!(auth.account(None).is_none(), "two accounts, no default");
        auth.default_account = Some("b".into());
        assert_eq!(auth.account(None).unwrap().0, "b");
        assert!(auth.remove_account("b"));
        assert_eq!(auth.default_account.as_deref(), Some("a"));
        assert!(!auth.remove_account("missing"));
    }

    #[cfg(unix)]
    #[test]
    fn directory_is_0700_and_files_are_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_scratch, store) = Scratch::new("modes");
        let lock = store.lock("opencode").unwrap();
        lock.save(&ProviderAuth::default()).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(store.dir()), 0o700);
        assert_eq!(mode(&store.dir().join("opencode.json")), 0o600);
        assert_eq!(mode(&store.dir().join("opencode.lock")), 0o600);
        assert!(store.load("opencode").unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_file_is_refused_without_leaking_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_scratch, store) = Scratch::new("loose");
        let mut auth = ProviderAuth::default();
        auth.set_account("a", credential("loose"));
        store.lock("opencode").unwrap().save(&auth).unwrap();
        let path = store.dir().join("opencode.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = store.load("opencode").unwrap_err();
        assert!(matches!(error, AuthStoreError::Insecure { .. }));
        assert!(!error.to_string().contains("access-loose"));
        assert!(!error.to_string().contains("refresh-loose"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_auth_file_is_refused() {
        let (_scratch, store) = Scratch::new("symlink");
        let mut auth = ProviderAuth::default();
        auth.set_account("a", credential("link"));
        let lock = store.lock("opencode").unwrap();
        lock.save(&auth).unwrap();
        let real = store.dir().join("opencode.json");
        let moved = store.dir().join("real-copy");
        std::fs::rename(&real, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &real).unwrap();
        assert!(matches!(
            store.load("opencode"),
            Err(AuthStoreError::Insecure { .. })
        ));
    }

    #[test]
    fn malformed_and_unknown_version_files_do_not_quote_contents() {
        let (_scratch, store) = Scratch::new("format");
        let lock = store.lock("opencode").unwrap();
        let path = store.dir().join("opencode.json");

        write_owner_only_file(
            &path,
            r#"{"version":1,"accounts":{"a":{"access":"TOPSECRET"}}}"#,
        )
        .unwrap();
        let error = lock.load().unwrap_err();
        assert!(matches!(error, AuthStoreError::Format { .. }));
        assert!(!error.to_string().contains("TOPSECRET"));

        write_owner_only_file(&path, r#"{"version":1,"accounts":{"a":"TOPSECRET"}}"#).unwrap();
        assert!(!lock.load().unwrap_err().to_string().contains("TOPSECRET"));

        write_owner_only_file(&path, r#"{"version":9,"accounts":{}}"#).unwrap();
        assert!(
            lock.load()
                .unwrap_err()
                .to_string()
                .contains("unsupported schema version 9")
        );
    }

    #[test]
    fn delete_removes_the_file_and_reports_absence() {
        let (_scratch, store) = Scratch::new("delete");
        let lock = store.lock("opencode").unwrap();
        assert!(!lock.delete().unwrap());
        lock.save(&ProviderAuth::default()).unwrap();
        assert!(lock.delete().unwrap());
        assert_eq!(store.load("opencode").unwrap(), None);
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let (_scratch, store) = Scratch::new("lock");
        let first = store.lock("opencode").unwrap();
        let (sent, received) = mpsc::channel();
        let contender = store.clone();
        let handle = std::thread::spawn(move || {
            let _second = contender.lock("opencode").unwrap();
            sent.send(()).unwrap();
        });
        assert!(
            received.recv_timeout(Duration::from_millis(300)).is_err(),
            "second lock must wait while the first is held"
        );
        drop(first);
        received
            .recv_timeout(Duration::from_secs(10))
            .expect("second lock proceeds after release");
        handle.join().unwrap();
    }

    /// Runs on the Windows CI job: round trip through the real ACL writer and
    /// verifier, refusal of a file whose ACL was never restricted, and a
    /// working exclusive lock.
    #[cfg(windows)]
    #[test]
    fn windows_smoke_auth_store_round_trip_acl_refusal_and_lock() {
        let (_scratch, store) = Scratch::new("windows");
        let mut auth = ProviderAuth::default();
        auth.set_account("a", credential("win"));
        let lock = store.lock("opencode").unwrap();
        lock.save(&auth).unwrap();
        assert_eq!(store.load("opencode").unwrap(), Some(auth.clone()));
        drop(lock);

        let path = store.dir().join("opencode.json");
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, serde_json::to_string(&auth).unwrap()).unwrap();
        let error = store.load("opencode").unwrap_err();
        assert!(
            matches!(error, AuthStoreError::Insecure { .. }),
            "an inherited ACL must be refused, got {error}"
        );
        assert!(!error.to_string().contains("access-win"));

        let first = store.lock("opencode").unwrap();
        let (sent, received) = mpsc::channel();
        let contender = store.clone();
        let handle = std::thread::spawn(move || {
            let _second = contender.lock("opencode").unwrap();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(300)).is_err());
        drop(first);
        received.recv_timeout(Duration::from_secs(10)).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn locks_for_different_providers_do_not_block_each_other() {
        let (_scratch, store) = Scratch::new("lock-iso");
        let _codex = store.lock("openai-codex").unwrap();
        let _opencode = store.lock("opencode").unwrap();
    }

    #[test]
    fn debug_output_never_contains_tokens() {
        let mut auth = ProviderAuth::default();
        auth.set_account("a", credential("dbg"));
        let text = format!("{auth:?} {:?}", credential("dbg"));
        assert!(!text.contains("access-dbg"));
        assert!(!text.contains("refresh-dbg"));
        assert!(text.contains("<redacted>"));
    }
}
