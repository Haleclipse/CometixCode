//! Maps to: CC `utils/nativeInstaller/pidLock.ts`.
//!
//! Safe read-only subset of PID-based version lock diagnostics. The official
//! module also acquires/releases locks and deletes stale lock files. Cometix's
//! Doctor screen must not mutate installer state, so this module only reads
//! lock metadata and checks PID liveness. Stale cleanup/acquire paths remain
//! deferred to the native-installer runtime slice.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Maps to CC `pidLock.ts#VersionLockContent`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionLockContent {
    pub pid: i32,
    pub version: String,
    pub exec_path: String,
    pub acquired_at: u64,
}

/// Maps to CC `pidLock.ts#LockInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockInfo {
    pub version: String,
    pub pid: i32,
    pub is_process_running: bool,
    pub exec_path: String,
    pub acquired_at: u64,
    pub lock_file_path: PathBuf,
}

/// Maps to CC `pidLock.ts#isPidBasedLockingEnabled`.
///
/// Safety divergence: the official fallback queries GrowthBook gate
/// `tengu_pid_based_version_locking`. Telemetry/GrowthBook is intentionally not
/// run in Cometix; when the env override is unset or unrecognized this returns
/// false, matching the external-user default.
pub fn is_pid_based_locking_enabled() -> bool {
    let value = crate::utils::process_env::var("ENABLE_PID_BASED_VERSION_LOCKING");
    if crate::utils::env_utils::is_env_truthy(value.as_deref()) {
        return true;
    }
    // Recognized-falsy and unset/unrecognized both fall to the external
    // default; the branch stays spelled out to mirror the official
    // truthy/falsy/gate ladder.
    false
}

/// Maps to CC `pidLock.ts#isProcessRunning`.
pub fn is_process_running(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    is_process_running_platform(pid)
}

#[cfg(unix)]
fn is_process_running_platform(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // Signal 0 performs permission/existence checks without delivering a
    // signal, matching Node `process.kill(pid, 0)`.
    unsafe { kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn is_process_running_platform(_pid: i32) -> bool {
    false
}

/// Maps to CC `pidLock.ts#readLockContent`.
pub fn read_lock_content(lock_file_path: impl AsRef<Path>) -> Option<VersionLockContent> {
    let content = fs::read_to_string(lock_file_path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    let parsed: VersionLockContent = serde_json::from_str(&content).ok()?;
    if parsed.pid <= 0 || parsed.version.is_empty() || parsed.exec_path.is_empty() {
        return None;
    }
    Some(parsed)
}

/// Maps to CC `pidLock.ts#isLockActive` primary PID check.
///
/// Transitional behavior: CC additionally validates the command line through
/// `genericProcessUtils.getProcessCommand` to reduce PID reuse risk. That
/// cross-platform process-command helper is not ported yet, so this read-only
/// diagnostic uses the primary PID liveness check only and never deletes locks.
pub fn is_lock_active(lock_file_path: impl AsRef<Path>) -> bool {
    read_lock_content(lock_file_path).is_some_and(|content| is_process_running(content.pid))
}

/// Maps to CC `pidLock.ts#getAllLockInfo`.
pub fn get_all_lock_info(locks_dir: impl AsRef<Path>) -> Vec<LockInfo> {
    let Ok(entries) = fs::read_dir(locks_dir) else {
        return Vec::new();
    };

    let mut lock_infos = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
            continue;
        }
        let Some(content) = read_lock_content(&path) else {
            continue;
        };
        lock_infos.push(LockInfo {
            version: content.version,
            pid: content.pid,
            is_process_running: is_process_running(content.pid),
            exec_path: content.exec_path,
            acquired_at: content.acquired_at,
            lock_file_path: path,
        });
    }
    lock_infos.sort_by(|a, b| a.version.cmp(&b.version));
    lock_infos
}

/// Safe-disabled counterpart for CC `pidLock.ts#cleanupStaleLocks`.
///
/// Current safety behavior: no files or directories are removed. The function
/// returns `0` so Doctor can report a stable snapshot without mutating native
/// installer lock state. The future native-installer runtime slice should port
/// the official deletion logic behind an explicit mutation policy.
pub fn cleanup_stale_locks(_locks_dir: impl AsRef<Path>) -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("cometix-pid-lock-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create tempdir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lock_content_rejects_empty_invalid_and_missing_required_fields() {
        let temp = TestDir::new();
        let empty = temp.path().join("empty.lock");
        fs::write(&empty, "   ").expect("write empty");
        assert_eq!(read_lock_content(&empty), None);

        let invalid = temp.path().join("invalid.lock");
        fs::write(&invalid, r#"{"pid":"bad"}"#).expect("write invalid");
        assert_eq!(read_lock_content(&invalid), None);

        let missing = temp.path().join("missing.lock");
        fs::write(&missing, r#"{"pid":123,"version":"1.0.0"}"#).expect("write missing");
        assert_eq!(read_lock_content(&missing), None);
    }

    #[test]
    fn get_all_lock_info_reads_lock_files_and_ignores_non_locks() {
        let temp = TestDir::new();
        fs::write(temp.path().join("README.txt"), "ignored").expect("write ignored");
        fs::write(
            temp.path().join("2.0.0.lock"),
            serde_json::json!({
                "pid": 999_999,
                "version": "2.0.0",
                "execPath": "/usr/local/bin/claude",
                "acquiredAt": 1234u64,
            })
            .to_string(),
        )
        .expect("write lock");

        let locks = get_all_lock_info(temp.path());
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].version, "2.0.0");
        assert_eq!(locks[0].pid, 999_999);
        assert_eq!(locks[0].exec_path, "/usr/local/bin/claude");
        assert_eq!(locks[0].acquired_at, 1234);
        assert!(!locks[0].is_process_running);
    }

    #[test]
    fn cleanup_stale_locks_is_safe_disabled() {
        let temp = TestDir::new();
        let lock = temp.path().join("stale.lock");
        fs::write(&lock, "not-json").expect("write lock");
        assert_eq!(cleanup_stale_locks(temp.path()), 0);
        assert!(lock.exists());
    }
}
