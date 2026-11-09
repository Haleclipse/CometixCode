//! Read/write safety boundary for auto-memory files.
//!
//! @nyan: memory content is checked at the Write/Edit boundary and again just
//! before model injection. The second check covers Bash and external-editor
//! writes that cannot be inspected reliably by the tool permission parser.

use crate::services::team_memory_sync::secret_scanner::{SecretMatch, scan_for_secrets};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryGuardError {
    pub rule_ids: Vec<String>,
}

impl fmt::Display for MemoryGuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Memory content contains potential secrets (rules: {}) and cannot be written. Remove the sensitive content and try again.",
            self.rule_ids.join(", ")
        )
    }
}

impl std::error::Error for MemoryGuardError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedMemoryFile {
    pub path: PathBuf,
    pub rule_ids: Vec<String>,
    pub labels: Vec<String>,
}

static BLOCKED_MEMORY_FILES: LazyLock<Mutex<BTreeMap<PathBuf, BlockedMemoryFile>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn secret_scan_enabled() -> bool {
    crate::utils::settings::get_initial_settings()
        .nyan
        .as_ref()
        .map(|settings| settings.memory().secret_scan())
        .unwrap_or(true)
}

fn normalize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn matches_to_error(matches: &[SecretMatch]) -> MemoryGuardError {
    MemoryGuardError {
        rule_ids: matches
            .iter()
            .map(|secret| secret.rule_id.clone())
            .collect(),
    }
}

/// Check a complete memory file before it is written or injected.
///
/// Non-memory paths are deliberately ignored so ordinary Read/Grep and normal
/// project-file writes keep their existing behavior. A blocked result contains
/// rule identifiers only; matched secret values never cross this boundary.
pub fn check_memory_content(path: &Path, content: &str) -> Result<(), MemoryGuardError> {
    if !secret_scan_enabled() || !crate::memdir::paths::is_auto_mem_path(path) {
        return Ok(());
    }
    let matches = scan_for_secrets(content);
    if matches.is_empty() {
        Ok(())
    } else {
        Err(matches_to_error(&matches))
    }
}

/// Record a read-time isolation event without modifying or deleting the file.
pub fn record_memory_guard_blocked(path: &Path, error: &MemoryGuardError) {
    let path = normalize_path(path);
    let labels = error
        .rule_ids
        .iter()
        .map(|rule_id| crate::services::team_memory_sync::secret_scanner::get_secret_label(rule_id))
        .collect::<Vec<_>>();
    let blocked = BlockedMemoryFile {
        path: path.clone(),
        rule_ids: error.rule_ids.clone(),
        labels,
    };
    BLOCKED_MEMORY_FILES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.clone(), blocked);
    tracing::warn!(
        target: "nyan::memory",
        memory_guard_blocked = true,
        path = %path.display(),
        rules = ?error.rule_ids,
        "memory_guard_blocked"
    );
}

/// Check content immediately before model injection and isolate violations.
pub fn check_memory_injection(path: &Path, content: &str) -> bool {
    match check_memory_content(path, content) {
        Ok(()) => {
            BLOCKED_MEMORY_FILES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&normalize_path(path));
            true
        }
        Err(error) => {
            record_memory_guard_blocked(path, &error);
            false
        }
    }
}

pub fn blocked_memory_files() -> Vec<BlockedMemoryFile> {
    BLOCKED_MEMORY_FILES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect()
}

pub fn blocked_memory_file(path: &Path) -> Option<BlockedMemoryFile> {
    let path = normalize_path(path);
    BLOCKED_MEMORY_FILES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&path)
        .cloned()
}

#[cfg(test)]
pub(crate) fn clear_blocked_memory_files_for_test() {
    BLOCKED_MEMORY_FILES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn memory_guard_rejects_public_build_heuristics_as_a_whole_file() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("nyan-memory-guard-{}", uuid::Uuid::new_v4()));
        let _override = EnvVarGuard::set("NYAN_MEMORY_PATH_OVERRIDE", &root);
        let path = root.join("candidate.md");
        let error = check_memory_content(&path, "safe\npassword: \"secret-value\"\n").unwrap_err();
        assert!(error.rule_ids.contains(&"sensitive-assignment".to_string()));
        assert!(!error.to_string().contains("secret-value"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ordinary_paths_are_not_intercepted() {
        let path = Path::new("/tmp/not-auto-memory.md");
        assert!(check_memory_content(path, "authorization: Bearer abcdefgh").is_ok());
    }
}
