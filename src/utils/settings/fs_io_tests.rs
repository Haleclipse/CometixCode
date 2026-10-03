//! Filesystem boundary regressions using independently scripted activeFs reads.
use super::*;
use crate::utils::fs_operations::*;
use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
use futures::future::BoxFuture;
use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct RecordingFs {
    calls: Mutex<Vec<&'static str>>,
    reads: Mutex<VecDeque<String>>,
    fail_mkdir: bool,
    mkdir_error: Option<&'static str>,
    read_error: Option<(&'static str, &'static str)>,
    rm_error: Option<&'static str>,
}
impl RecordingFs {
    fn record(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
    fn with_reads(reads: &[&str]) -> Self {
        Self {
            reads: Mutex::new(reads.iter().map(|s| s.to_string()).collect()),
            ..Self::default()
        }
    }
}
impl FsOperations for RecordingFs {
    fn cwd(&self) -> io::Result<PathBuf> {
        self.record("cwd");
        NodeFsOperations.cwd()
    }
    fn exists_sync(&self, path: &Path) -> bool {
        self.record("exists_sync");
        NodeFsOperations.exists_sync(path)
    }
    fn stat(&self, path: &Path) -> BoxFuture<'static, io::Result<FsStats>> {
        self.record("stat");
        NodeFsOperations.stat(path)
    }
    fn readdir(&self, path: &Path) -> BoxFuture<'static, io::Result<Vec<FsDirent>>> {
        self.record("readdir");
        NodeFsOperations.readdir(path)
    }
    fn unlink(&self, path: &Path) -> BoxFuture<'static, io::Result<()>> {
        self.record("unlink");
        NodeFsOperations.unlink(path)
    }
    fn rmdir(&self, path: &Path) -> BoxFuture<'static, io::Result<()>> {
        self.record("rmdir");
        NodeFsOperations.rmdir(path)
    }
    fn rm(&self, path: &Path, options: RmOptions) -> BoxFuture<'static, io::Result<()>> {
        self.record("rm");
        if let Some(message) = self.rm_error {
            return Box::pin(async move {
                Err(io::Error::other(FsError {
                    code: "EACCES",
                    syscall: None,
                    path: None,
                    dest: None,
                    message: message.to_owned(),
                    native: None,
                }))
            });
        }
        NodeFsOperations.rm(path, options)
    }
    fn mkdir(&self, path: &Path, mode: Option<u32>) -> BoxFuture<'static, io::Result<()>> {
        self.record("mkdir");
        NodeFsOperations.mkdir(path, mode)
    }
    fn read_file(
        &self,
        path: &Path,
        encoding: BufferEncoding,
    ) -> BoxFuture<'static, io::Result<FsText>> {
        self.record("read_file");
        assert_eq!(encoding, BufferEncoding::Utf8);
        if let Some((code, message)) = self.read_error {
            return Box::pin(async move {
                Err(io::Error::other(FsError {
                    code,
                    syscall: None,
                    path: None,
                    dest: None,
                    message: message.to_owned(),
                    native: None,
                }))
            });
        }
        if let Some(text) = self.reads.lock().unwrap().pop_front() {
            return Box::pin(async move { Ok(FsText::from(text)) });
        }
        NodeFsOperations.read_file(path, encoding)
    }
    fn rename(&self, source: &Path, target: &Path) -> BoxFuture<'static, io::Result<()>> {
        self.record("rename");
        NodeFsOperations.rename(source, target)
    }
    fn stat_sync(&self, path: &Path) -> io::Result<FsStats> {
        self.record("stat_sync");
        NodeFsOperations.stat_sync(path)
    }
    fn lstat_sync(&self, path: &Path) -> io::Result<FsStats> {
        self.record("lstat_sync");
        NodeFsOperations.lstat_sync(path)
    }
    fn read_file_sync(&self, path: &Path, encoding: BufferEncoding) -> io::Result<FsText> {
        self.record("read_file_sync");
        if let Some(text) = self.reads.lock().unwrap().pop_front() {
            return Ok(FsText::from(text));
        }
        NodeFsOperations.read_file_sync(path, encoding)
    }
    fn read_file_bytes_sync(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.record("read_file_bytes_sync");
        NodeFsOperations.read_file_bytes_sync(path)
    }
    fn read_sync(&self, path: &Path, length: usize) -> io::Result<ReadSyncResult> {
        self.record("read_sync");
        NodeFsOperations.read_sync(path, length)
    }
    fn append_file_sync(&self, path: &Path, data: &FsText, mode: Option<u32>) -> io::Result<()> {
        self.record("append_file_sync");
        NodeFsOperations.append_file_sync(path, data, mode)
    }
    fn copy_file_sync(&self, source: &Path, target: &Path) -> io::Result<()> {
        self.record("copy_file_sync");
        NodeFsOperations.copy_file_sync(source, target)
    }
    fn unlink_sync(&self, path: &Path) -> io::Result<()> {
        self.record("unlink_sync");
        NodeFsOperations.unlink_sync(path)
    }
    fn rename_sync(&self, source: &Path, target: &Path) -> io::Result<()> {
        self.record("rename_sync");
        NodeFsOperations.rename_sync(source, target)
    }
    fn link_sync(&self, target: &Path, path: &Path) -> io::Result<()> {
        self.record("link_sync");
        NodeFsOperations.link_sync(target, path)
    }
    fn symlink_sync(
        &self,
        target: &Path,
        path: &Path,
        kind: Option<SymlinkType>,
    ) -> io::Result<()> {
        self.record("symlink_sync");
        NodeFsOperations.symlink_sync(target, path, kind)
    }
    fn readlink_sync(&self, path: &Path) -> io::Result<PathBuf> {
        self.record("readlink_sync");
        NodeFsOperations.readlink_sync(path)
    }
    fn realpath_sync(&self, path: &Path) -> io::Result<PathBuf> {
        self.record("realpath_sync");
        NodeFsOperations.realpath_sync(path)
    }
    fn mkdir_sync(&self, path: &Path, mode: Option<u32>) -> io::Result<()> {
        self.record("mkdir_sync");
        if let Some(code) = self.mkdir_error {
            return Err(io::Error::other(FsError {
                code,
                syscall: None,
                path: None,
                dest: None,
                message: "injected directory failure".into(),
                native: None,
            }));
        }
        if self.fail_mkdir {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        NodeFsOperations.mkdir_sync(path, mode)
    }
    fn readdir_sync(&self, path: &Path) -> io::Result<Vec<FsDirent>> {
        self.record("readdir_sync");
        NodeFsOperations.readdir_sync(path)
    }
    fn readdir_string_sync(&self, path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
        self.record("readdir_string_sync");
        NodeFsOperations.readdir_string_sync(path)
    }
    fn is_dir_empty_sync(&self, path: &Path) -> io::Result<bool> {
        self.record("is_dir_empty_sync");
        NodeFsOperations.is_dir_empty_sync(path)
    }
    fn rmdir_sync(&self, path: &Path) -> io::Result<()> {
        self.record("rmdir_sync");
        NodeFsOperations.rmdir_sync(path)
    }
    fn rm_sync(&self, path: &Path, options: RmOptions) -> io::Result<()> {
        self.record("rm_sync");
        NodeFsOperations.rm_sync(path, options)
    }
    fn create_write_stream(&self, path: &Path) -> FsWriteStream {
        self.record("create_write_stream");
        NodeFsOperations.create_write_stream(path)
    }
    fn read_file_bytes(
        &self,
        path: &Path,
        max_bytes: Option<f64>,
    ) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        self.record("read_file_bytes");
        NodeFsOperations.read_file_bytes(path, max_bytes)
    }
}
struct ResetFs(Arc<dyn FsOperations>);
impl ResetFs {
    fn install(fs: Arc<dyn FsOperations>) -> Self {
        let reset = Self(get_fs_implementation());
        set_fs_implementation(fs);
        settings_cache::reset_settings_cache();
        reset
    }
}
impl Drop for ResetFs {
    fn drop(&mut self) {
        set_fs_implementation(self.0.clone());
        settings_cache::reset_settings_cache();
        internal_writes::clear_internal_writes();
    }
}
struct TestRoot(PathBuf);
impl TestRoot {
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn environment() -> (TestRoot, EnvVarGuard) {
    let root = TestRoot(std::env::temp_dir().join(format!("fs-consumer-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(root.path()).unwrap();
    let guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.path());
    (root, guard)
}

#[test]
fn settings_mkdir_failure_matches_official_no_read() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (_root, _env) = environment();
    let fs = Arc::new(RecordingFs {
        fail_mkdir: true,
        ..RecordingFs::default()
    });
    let _reset = ResetFs::install(fs.clone());
    assert!(update_settings_for_source(SettingSource::User, &serde_json::Map::new()).is_err());
    assert_eq!(fs.calls(), vec!["mkdir_sync"]);
}

#[test]
fn settings_validated_read_matches_official_cached_json_and_write_mark() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (root, _env) = environment();
    let path = root.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"model":"sonnet","unknownField":{"retained":true}}"#,
    )
    .unwrap();
    let fs = Arc::new(RecordingFs::default());
    let _reset = ResetFs::install(fs.clone());
    assert!(parse_settings_file(&path).0.is_some());
    let before = fs.calls();
    assert_eq!(
        before.iter().filter(|name| **name == "lstat_sync").count(),
        2
    );
    std::fs::write(&path, "{}").unwrap();
    update_settings_for_source(
        SettingSource::User,
        &serde_json::Map::from_iter([("language".into(), serde_json::json!("zh"))]),
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"model":"sonnet","unknownField":{"retained":true},"language":"zh"})
    );
    assert_eq!(
        fs.calls()
            .iter()
            .filter(|name| **name == "read_file_sync")
            .count(),
        1
    );
    assert!(internal_writes::consume_internal_write(&path, 5_000));
    assert!(!internal_writes::consume_internal_write(&path, 5_000));
}

#[test]
fn settings_validation_fallback_matches_official_raw_read_and_syntax_guard() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (root, _env) = environment();
    let path = root.path().join("settings.json");
    std::fs::write(&path, "{}").unwrap();
    let fs = Arc::new(RecordingFs::with_reads(&[
        r#"{"model":17}"#,
        r#"{"unknown":"second"}"#,
    ]));
    let _reset = ResetFs::install(fs.clone());
    update_settings_for_source(SettingSource::User, &serde_json::Map::new()).unwrap();
    assert_eq!(
        fs.calls()
            .iter()
            .filter(|name| **name == "read_file_sync")
            .count(),
        2
    );
    assert_eq!(fs.calls().first(), Some(&"mkdir_sync"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\n  \"unknown\": \"second\"\n}\n"
    );
    settings_cache::reset_settings_cache();
    fs.reads
        .lock()
        .unwrap()
        .extend(["{".to_owned(), "{".to_owned()]);
    let original = std::fs::read(&path).unwrap();
    assert!(
        update_settings_for_source(SettingSource::User, &serde_json::Map::new())
            .unwrap_err()
            .to_string()
            .contains("Invalid JSON syntax")
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn marketplace_initial_read_matches_official_capture_before_poll() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (_root, _env) = environment();
    let first = Arc::new(RecordingFs::with_reads(&["{}"]));
    let _reset = ResetFs::install(first.clone());
    let pending = crate::utils::plugins::marketplace_manager::load_known_marketplaces_config();
    assert_eq!(first.calls(), vec!["read_file"]);
    let second = Arc::new(RecordingFs::with_reads(&["invalid"]));
    set_fs_implementation(second.clone());
    assert!(pending.await.unwrap().is_empty());
    assert!(second.calls().is_empty());
}

#[tokio::test]
async fn marketplace_finalization_matches_official_captured_rm_and_rename() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (_root, _env) = environment();
    let first = Arc::new(RecordingFs::default());
    let _reset = ResetFs::install(first.clone());
    let source = serde_json::json!({"source":"settings","name":"temporary","plugins":[]});
    let pending =
        crate::utils::plugins::marketplace_manager::load_and_cache_marketplace(&source, None);
    assert_eq!(first.calls(), vec!["mkdir"]);
    let second = Arc::new(RecordingFs::with_reads(&[
        r#"{"name":"final","owner":{"name":"test"},"plugins":[]}"#,
    ]));
    set_fs_implementation(second.clone());
    let loaded = pending.await.unwrap();
    assert_eq!(loaded.marketplace["name"], "final");
    assert_eq!(first.calls(), vec!["mkdir", "mkdir", "rm", "rename"]);
    assert_eq!(second.calls(), vec!["read_file"]);
}

#[test]
fn settings_invalid_json_matches_official_schema_null_diagnostic() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (root, _env) = environment();
    let path = root.path().join("settings.json");
    std::fs::write(&path, "{").unwrap();
    let _reset = ResetFs::install(Arc::new(RecordingFs::default()));
    let (settings, errors) = parse_settings_file(&path);
    assert!(settings.is_none());
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].message, "Invalid or malformed JSON");
    assert_eq!(errors[0].path, "");
    assert_eq!(
        errors[0].file.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn marketplace_active_fs_error_matches_official_message_and_errno_identity() {
    use crate::utils::plugins::marketplace_manager::{
        load_known_marketplaces_config, read_cached_marketplace,
    };
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (root, _env) = environment();
    let fs = Arc::new(RecordingFs {
        read_error: Some(("EACCES", "remote catalog unavailable")),
        ..RecordingFs::default()
    });
    let _reset = ResetFs::install(fs);
    let error = load_known_marketplaces_config().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Failed to load marketplace configuration: remote catalog unavailable"
    );
    let error = read_cached_marketplace(root.path()).await.unwrap_err();
    assert_eq!(error.to_string(), "remote catalog unavailable");
    assert_eq!(crate::utils::errors::get_errno_code(&error), Some("EACCES"));
    // An injected Node code must win even if the Rust carrier kind is Other.
    set_fs_implementation(Arc::new(RecordingFs {
        read_error: Some(("ENOENT", "virtual record absent")),
        ..RecordingFs::default()
    }));
    assert!(load_known_marketplaces_config().await.unwrap().is_empty());
}

#[tokio::test]
async fn marketplace_git_cleanup_matches_official_active_fs_error_message() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (root, _env) = environment();
    let cache = root.path().join("absent-cache");
    let fs = Arc::new(RecordingFs {
        rm_error: Some("remote cleanup refused"),
        ..RecordingFs::default()
    });
    let _reset = ResetFs::install(fs.clone());
    // Missing local checkout makes reconciliation fail without contacting a
    // remote. The injected cleanup failure prevents reaching clone at all.
    let error = crate::utils::plugins::marketplace_manager::cache_marketplace_from_git(
        root.path().to_str().unwrap(),
        &cache,
        None,
        None,
        None,
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Failed to clean up existing marketplace directory. Please manually delete the directory at {} and try again.\n\nTechnical details: remote cleanup refused",
            cache.display()
        )
    );
    assert_eq!(fs.calls(), vec!["rm"]);
}

#[test]
fn plaintext_update_keeps_authorized_oauth_gate_before_fs_effects() {
    use crate::utils::secure_storage::{
        SecureStorageBackend, plain_text_storage::PlainTextStorage,
    };
    if crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
        return;
    }
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let (root, _env) = environment();
    let fs = Arc::new(RecordingFs {
        mkdir_error: Some("EEXIST"),
        ..RecordingFs::default()
    });
    let _reset = ResetFs::install(fs.clone());
    let error = PlainTextStorage
        .update(&serde_json::json!({"fixture":true}))
        .unwrap_err();
    assert!(error.is::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>());
    assert!(fs.calls().is_empty());
    assert!(!root.path().join(".credentials.json").exists());
}
