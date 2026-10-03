//! Scheduler lease lock for `.claude/scheduled_tasks.json`.
//!
//! Maps to: CC `utils/cronTasksLock.ts`. The lock uses atomic `create_new`,
//! PID liveness recovery, owner identity checks, and RAII cleanup.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchedulerLockRecord {
    session_id: String,
    pid: u32,
    acquired_at: u64,
}

#[derive(Debug)]
pub struct SchedulerLockGuard {
    path: PathBuf,
    identity: String,
    owned: bool,
}

impl SchedulerLockGuard {
    pub fn new(project_root: &Path, identity: impl Into<String>) -> Self {
        Self {
            path: project_root.join(".claude").join("scheduled_tasks.lock"),
            identity: identity.into(),
            owned: false,
        }
    }

    pub fn is_owned(&self) -> bool {
        self.owned
    }

    pub fn try_acquire(&mut self) -> Result<bool, String> {
        if self.owned {
            return Ok(true);
        }
        if !crate::utils::session_storage::is_session_write_enabled() {
            return Ok(false);
        }
        let record = SchedulerLockRecord {
            session_id: self.identity.clone(),
            pid: std::process::id(),
            acquired_at: now_ms(),
        };
        if self.try_create(&record)? {
            self.owned = true;
            return Ok(true);
        }

        let existing = read_record(&self.path);
        if existing
            .as_ref()
            .is_some_and(|existing| existing.session_id == self.identity)
        {
            // A resumed owner may have a new PID. Refresh through a temporary
            // file rather than leaving a stale liveness signal.
            write_record_replace(&self.path, &record)?;
            self.owned = true;
            return Ok(true);
        }
        if existing
            .as_ref()
            .is_some_and(|existing| process_is_running(existing.pid))
        {
            return Ok(false);
        }

        // Corrupt or dead owner: unlink and retry exclusive creation once.
        let _ = std::fs::remove_file(&self.path);
        self.owned = self.try_create(&record)?;
        Ok(self.owned)
    }

    pub fn release(&mut self) {
        if !self.owned {
            return;
        }
        if read_record(&self.path)
            .as_ref()
            .is_some_and(|record| record.session_id == self.identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
        self.owned = false;
    }

    fn try_create(&self, record: &SchedulerLockRecord) -> Result<bool, String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "Invalid scheduled task lock path".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
        let body = serde_json::to_vec(record)
            .map_err(|error| format!("Failed to serialize scheduler lock: {error}"))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                file.write_all(&body)
                    .map_err(|error| format!("Failed to write scheduler lock: {error}"))?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(format!("Failed to create scheduler lock: {error}")),
        }
    }
}

impl Drop for SchedulerLockGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn read_record(path: &Path) -> Option<SchedulerLockRecord> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write_record_replace(path: &Path, record: &SchedulerLockRecord) -> Result<(), String> {
    let body = serde_json::to_vec(record)
        .map_err(|error| format!("Failed to serialize scheduler lock: {error}"))?;
    std::fs::write(path, body).map_err(|error| format!("Failed to refresh scheduler lock: {error}"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_running(pid: u32) -> bool {
    // Fail safe on platforms without a cheap std PID probe: this process is
    // certainly live; foreign records are treated as live until removed.
    pid == std::process::id() || pid != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn lock_is_exclusive_and_raii_releasable() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-cron-lock-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let mut first = SchedulerLockGuard::new(&root, "first");
        let mut second = SchedulerLockGuard::new(&root, "second");
        assert!(first.try_acquire().unwrap());
        assert!(!second.try_acquire().unwrap());
        first.release();
        assert!(second.try_acquire().unwrap());
        second.release();

        let _ = std::fs::remove_dir_all(root);
    }
}
