//! Maps to: CC `utils/fileHistory.ts` — file checkpointing behind /rewind.
//!
//! Backups live under `{configDir}/file-history/{sessionId}/{sha256[..16]}@vN`
//! and are recorded per user-message snapshot; rewinding applies a snapshot
//! back onto disk. State lives in `AppState.file_history` (CC
//! `AppState.fileHistory`); CC's injected `updateFileHistoryState` updater is
//! represented by passing `&AppStore` — capture uses `store.get()` (CC's
//! no-op updater phase) and commits go through `store.set_state` with an
//! explicit unchanged→Same guard (CC's same-ref no-op semantics).
//!
//! Session-storage `recordFileHistorySnapshot` linkage is preserved here;
//! analytics and VS Code rewind notifications remain non-semantic seams.

use crate::bootstrap::state::{get_is_non_interactive_session, get_original_cwd, get_session_id};
use crate::state::store::AppStore;
use crate::utils::config::load_global_config;
use crate::utils::env_utils::get_claude_config_home_dir;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Maps to: CC `MAX_SNAPSHOTS = 100`.
pub const MAX_SNAPSHOTS: usize = 100;

/// Maps to: CC `FileHistoryBackup`. `backup_file_name: None` is CC's `null`
/// marker — the file did not exist in this version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryBackup {
    pub backup_file_name: Option<String>,
    pub version: u32,
    pub backup_time: DateTime<Utc>,
}

/// Maps to: CC `FileHistorySnapshot`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshot {
    pub message_id: String,
    pub tracked_file_backups: BTreeMap<String, FileHistoryBackup>,
    pub timestamp: DateTime<Utc>,
}

/// Maps to: CC `FileHistoryState`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileHistoryState {
    pub snapshots: Vec<FileHistorySnapshot>,
    pub tracked_files: BTreeSet<String>,
    /// Monotonically-increasing counter incremented on every snapshot, even
    /// when old snapshots are evicted (CC: useGitDiffStats activity signal —
    /// snapshots.len() plateaus once the cap is reached).
    pub snapshot_sequence: u64,
}

/// Maps to: CC `DiffStats` (the resolved branch; CC's `undefined` branch is
/// `Option<DiffStats>` at the call sites).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffStats {
    pub files_changed: Vec<String>,
    pub insertions: usize,
    pub deletions: usize,
}

/// Maps to: CC `BackupFileName | undefined` three-state resolution:
/// - `Some(Some(name))` — backup file exists for this version
/// - `Some(None)` — CC `null`: the file did not exist in this version
/// - `None` — CC `undefined`: the backup could not be resolved (error)
type ResolvedBackupFileName = Option<Option<String>>;

/// Maps to: CC `fileHistoryEnabled()`.
pub fn file_history_enabled() -> bool {
    if get_is_non_interactive_session() {
        return file_history_enabled_sdk();
    }
    load_global_config().file_checkpointing_enabled != Some(false)
        && !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING")
                .as_deref(),
        )
}

/// Maps to: CC `fileHistoryEnabledSdk()`.
fn file_history_enabled_sdk() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING")
            .as_deref(),
    ) && !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING")
            .as_deref(),
    )
}

/// CC phase-1 capture (`updateFileHistoryState(state => { captured = state;
/// return state })`) — a lock-free snapshot read here.
fn capture_state(store: &AppStore) -> Arc<FileHistoryState> {
    Arc::clone(&store.get().file_history)
}

/// CC commit phase — clone-mutate-swap the Arc field.
///
/// P3 §3a: unchanged commits must stay side-effect free after the B3 flip;
/// the equality check `AppStore::update` used to provide globally is now
/// explicit at this site (CC's updater returns `prev` when the mutation
/// left the history untouched).
fn commit_state(store: &AppStore, mutate: impl FnOnce(&mut FileHistoryState)) {
    store.set_state(|prev| {
        let mut history = (*prev.file_history).clone();
        mutate(&mut history);
        if history == *prev.file_history {
            return crate::state::store::UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.file_history = Arc::new(history);
        crate::state::store::UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `fileHistoryTrackEdit` — tracks a file edit (or add) by
/// backing up its current contents into the most recent snapshot. Must be
/// called before the file is actually written.
pub async fn file_history_track_edit(store: &AppStore, file_path: &str, message_id: &str) {
    if !file_history_enabled() {
        return;
    }

    let tracking_path = maybe_shorten_file_path(file_path);

    // Phase 1: check if a backup is needed. Speculative writes would
    // overwrite the deterministic {hash}@v1 backup on every repeat call.
    let captured = capture_state(store);
    let Some(most_recent) = captured.snapshots.last() else {
        // CC: logError('FileHistory: Missing most recent snapshot') +
        // tengu_file_history_track_edit_failed.
        return;
    };
    if most_recent
        .tracked_file_backups
        .contains_key(&tracking_path)
    {
        // Already tracked in the most recent snapshot; the next makeSnapshot
        // re-checks mtime and re-backups if changed. Do not touch v1.
        return;
    }

    // Phase 2: async backup.
    let backup = match create_backup(Some(file_path), 1).await {
        Ok(backup) => backup,
        Err(_) => {
            // CC: logError + tengu_file_history_track_edit_failed.
            return;
        }
    };

    // Phase 3: commit. Re-check tracked (another trackEdit may have raced).
    let mut snapshot_to_record = None;
    commit_state(store, |state| {
        let Some(most_recent) = state.snapshots.last_mut() else {
            return;
        };
        if most_recent
            .tracked_file_backups
            .contains_key(&tracking_path)
        {
            return;
        }
        most_recent
            .tracked_file_backups
            .insert(tracking_path.clone(), backup);
        state.tracked_files.insert(tracking_path.clone());
        snapshot_to_record = Some(most_recent.clone());
        // CC: tengu_file_history_track_edit_success analytics.
    });
    if let Some(snapshot) = snapshot_to_record {
        let _ = crate::utils::session_storage::record_file_history_snapshot(
            message_id, &snapshot, true,
        );
    }
}

/// Maps to: CC `fileHistoryMakeSnapshot` — adds a snapshot and backs up any
/// modified tracked files.
pub async fn file_history_make_snapshot(store: &AppStore, message_id: &str) {
    if !file_history_enabled() {
        return;
    }

    // Phase 1: capture which files to back up.
    let captured = capture_state(store);

    // Phase 2: all IO async, outside the updater. (CC runs the per-file
    // backups through Promise.all; sequential awaits are semantically
    // identical here — no result depends on ordering.)
    let mut tracked_file_backups: BTreeMap<String, FileHistoryBackup> = BTreeMap::new();
    if let Some(most_recent) = captured.snapshots.last() {
        for tracking_path in &captured.tracked_files {
            let file_path = maybe_expand_file_path(tracking_path);
            let latest_backup = most_recent.tracked_file_backups.get(tracking_path);
            let next_version = latest_backup.map(|backup| backup.version + 1).unwrap_or(1);

            // Stat once; ENOENT means the tracked file was deleted.
            let file_stats = match tokio::fs::metadata(&file_path).await {
                Ok(stats) => Some(stats),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(_) => {
                    // CC: logError + tengu_file_history_backup_file_failed.
                    continue;
                }
            };

            let Some(file_stats) = file_stats else {
                tracked_file_backups.insert(
                    tracking_path.clone(),
                    FileHistoryBackup {
                        backup_file_name: None,
                        version: next_version,
                        backup_time: Utc::now(),
                    },
                );
                // CC: tengu_file_history_backup_deleted_file analytics.
                continue;
            };

            // File exists — reuse the latest backup when unchanged.
            if let Some(latest) = latest_backup {
                if let Some(backup_name) = latest.backup_file_name.as_deref() {
                    if !check_origin_file_changed(&file_path, backup_name, Some(&file_stats)).await
                    {
                        tracked_file_backups.insert(tracking_path.clone(), latest.clone());
                        continue;
                    }
                }
            }

            // Newer than the latest backup — create a new one.
            match create_backup(Some(&file_path), next_version).await {
                Ok(backup) => {
                    tracked_file_backups.insert(tracking_path.clone(), backup);
                }
                Err(_) => {
                    // CC: logError + tengu_file_history_backup_file_failed.
                }
            }
        }
    }

    // Phase 3: commit. Read tracked files FRESH — a racing trackEdit during
    // phase 2 wrote its backup into snapshots[-1]; inherit those entries so
    // the new snapshot covers every currently-tracked file.
    let mut snapshot_to_record = None;
    commit_state(store, |state| {
        if let Some(last_snapshot) = state.snapshots.last() {
            for tracking_path in &state.tracked_files {
                if tracked_file_backups.contains_key(tracking_path) {
                    continue;
                }
                if let Some(inherited) = last_snapshot.tracked_file_backups.get(tracking_path) {
                    tracked_file_backups.insert(tracking_path.clone(), inherited.clone());
                }
            }
        }
        let new_snapshot = FileHistorySnapshot {
            message_id: message_id.to_string(),
            tracked_file_backups: std::mem::take(&mut tracked_file_backups),
            timestamp: Utc::now(),
        };
        state.snapshots.push(new_snapshot.clone());
        snapshot_to_record = Some(new_snapshot);
        if state.snapshots.len() > MAX_SNAPSHOTS {
            let excess = state.snapshots.len() - MAX_SNAPSHOTS;
            state.snapshots.drain(..excess);
        }
        state.snapshot_sequence += 1;
        // CC: notifyVscodeSnapshotFilesUpdated(old, new) — joins with the
        // vscode SDK MCP subsystem.
        // CC: tengu_file_history_snapshot_success analytics.
    });
    if let Some(snapshot) = snapshot_to_record {
        let _ = crate::utils::session_storage::record_file_history_snapshot(
            message_id, &snapshot, false,
        );
    }
}

/// Maps to: CC `fileHistoryRewind` — rewinds the file system to the snapshot
/// recorded for `message_id`. Pure filesystem side effect; does not mutate
/// FileHistoryState.
pub async fn file_history_rewind(store: &AppStore, message_id: &str) -> Result<(), String> {
    if !file_history_enabled() {
        return Ok(());
    }

    let captured = capture_state(store);
    let Some(target_snapshot) = captured
        .snapshots
        .iter()
        .rev()
        .find(|snapshot| snapshot.message_id == message_id)
    else {
        // CC: logError + tengu_file_history_rewind_failed(snapshotFound: false).
        return Err("The selected snapshot was not found".to_string());
    };

    let _files_changed = apply_snapshot(&captured, target_snapshot).await;
    // CC: tengu_file_history_rewind_success analytics.
    Ok(())
}

/// Maps to: CC `fileHistoryCanRestore`.
pub fn file_history_can_restore(state: &FileHistoryState, message_id: &str) -> bool {
    if !file_history_enabled() {
        return false;
    }
    state
        .snapshots
        .iter()
        .any(|snapshot| snapshot.message_id == message_id)
}

/// Maps to: CC `fileHistoryGetDiffStats` — counts the files/lines that would
/// change if reverting to the snapshot for `message_id`.
pub async fn file_history_get_diff_stats(
    state: &FileHistoryState,
    message_id: &str,
) -> Option<DiffStats> {
    if !file_history_enabled() {
        return None;
    }

    let target_snapshot = state
        .snapshots
        .iter()
        .rev()
        .find(|snapshot| snapshot.message_id == message_id)?;

    let mut files_changed: Vec<String> = Vec::new();
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for tracking_path in &state.tracked_files {
        let file_path = maybe_expand_file_path(tracking_path);
        let Some(backup_file_name) =
            resolve_backup_for_snapshot(tracking_path, target_snapshot, state)
        else {
            // CC: logError('FileHistory: Error finding the backup file to
            // apply') + tengu_file_history_rewind_restore_file_failed(dryRun).
            continue;
        };

        let stats = compute_diff_stats_for_file(&file_path, backup_file_name.as_deref()).await;
        if stats.insertions > 0 || stats.deletions > 0 {
            files_changed.push(file_path.clone());
            insertions += stats.insertions;
            deletions += stats.deletions;
            continue;
        }
        if backup_file_name.is_none() && tokio::fs::metadata(&file_path).await.is_ok() {
            // Zero-byte file created after the snapshot: counts as changed
            // even though the line diff reports 0/0.
            files_changed.push(file_path);
        }
    }
    Some(DiffStats {
        files_changed,
        insertions,
        deletions,
    })
}

/// Maps to: CC `fileHistoryHasAnyChanges` — boolean-only dry run using the
/// stat/content comparison (never diffs lines); early-exits on the first
/// changed file.
pub async fn file_history_has_any_changes(state: &FileHistoryState, message_id: &str) -> bool {
    if !file_history_enabled() {
        return false;
    }
    let Some(target_snapshot) = state
        .snapshots
        .iter()
        .rev()
        .find(|snapshot| snapshot.message_id == message_id)
    else {
        return false;
    };

    for tracking_path in &state.tracked_files {
        let file_path = maybe_expand_file_path(tracking_path);
        let Some(backup_file_name) =
            resolve_backup_for_snapshot(tracking_path, target_snapshot, state)
        else {
            continue;
        };
        match backup_file_name {
            None => {
                // Backup says the file did not exist; probe via stat.
                if tokio::fs::metadata(&file_path).await.is_ok() {
                    return true;
                }
            }
            Some(backup_name) => {
                if check_origin_file_changed(&file_path, &backup_name, None).await {
                    return true;
                }
            }
        }
    }
    false
}

/// CC's shared lookup: the target snapshot's backup for a file, falling back
/// to the first-version backup when the file was not tracked yet at the
/// target point (fileHistory.ts:436-438 et al).
fn resolve_backup_for_snapshot(
    tracking_path: &str,
    target_snapshot: &FileHistorySnapshot,
    state: &FileHistoryState,
) -> ResolvedBackupFileName {
    match target_snapshot.tracked_file_backups.get(tracking_path) {
        Some(backup) => Some(backup.backup_file_name.clone()),
        None => get_backup_file_name_first_version(tracking_path, state),
    }
}

/// Maps to: CC `applySnapshot` — writes/deletes tracked files on disk to
/// match the target snapshot; returns the changed paths. Per-file errors are
/// swallowed (CC try/catch continue).
async fn apply_snapshot(
    state: &FileHistoryState,
    target_snapshot: &FileHistorySnapshot,
) -> Vec<String> {
    let mut files_changed = Vec::new();
    for tracking_path in &state.tracked_files {
        let file_path = maybe_expand_file_path(tracking_path);
        let Some(backup_file_name) =
            resolve_backup_for_snapshot(tracking_path, target_snapshot, state)
        else {
            // CC: logError + tengu_file_history_rewind_restore_file_failed.
            continue;
        };

        match backup_file_name {
            None => {
                // File did not exist at the target version; delete if present.
                match tokio::fs::remove_file(&file_path).await {
                    Ok(()) => files_changed.push(file_path),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => {
                        // CC: logError + restore_file_failed analytics.
                    }
                }
            }
            Some(backup_name) => {
                // Restore only if it differs.
                if check_origin_file_changed(&file_path, &backup_name, None).await {
                    match restore_backup(&file_path, &backup_name).await {
                        Ok(()) => files_changed.push(file_path),
                        Err(_) => {
                            // CC: logError + restore_file_failed analytics.
                        }
                    }
                }
            }
        }
    }
    files_changed
}

/// Outcome of the stat-level comparison (CC `compareStatsAndContent` before
/// its content callback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatComparison {
    Changed,
    Unchanged,
    NeedContentCompare,
}

/// Maps to: CC `compareStatsAndContent` stat phase — one missing = changed,
/// both missing = unchanged, mode/size mismatch = changed, original older
/// than the backup = unchanged, otherwise compare content.
fn compare_stats(
    original: Option<&std::fs::Metadata>,
    backup: Option<&std::fs::Metadata>,
) -> StatComparison {
    match (original, backup) {
        (None, None) => StatComparison::Unchanged,
        (Some(_), None) | (None, Some(_)) => StatComparison::Changed,
        (Some(original), Some(backup)) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if original.mode() != backup.mode() {
                    return StatComparison::Changed;
                }
            }
            if original.len() != backup.len() {
                return StatComparison::Changed;
            }
            // mtime optimization: if the original was last modified before
            // the backup was taken, the content cannot have diverged.
            if let (Ok(original_mtime), Ok(backup_mtime)) = (original.modified(), backup.modified())
            {
                if original_mtime < backup_mtime {
                    return StatComparison::Unchanged;
                }
            }
            StatComparison::NeedContentCompare
        }
    }
}

/// Maps to: CC `checkOriginFileChanged` — stat comparison with an optional
/// pre-fetched stat for the original, falling back to full content compare.
/// Non-ENOENT stat errors count as changed (CC returns true).
pub async fn check_origin_file_changed(
    original_file: &str,
    backup_file_name: &str,
    original_stats_hint: Option<&std::fs::Metadata>,
) -> bool {
    let backup_path = resolve_backup_path(backup_file_name, None);

    let original_stats_owned;
    let original_stats = match original_stats_hint {
        Some(stats) => Some(stats),
        None => match tokio::fs::metadata(original_file).await {
            Ok(stats) => {
                original_stats_owned = stats;
                Some(&original_stats_owned)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return true,
        },
    };
    let backup_stats = match tokio::fs::metadata(&backup_path).await {
        Ok(stats) => Some(stats),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return true,
    };

    match compare_stats(original_stats, backup_stats.as_ref()) {
        StatComparison::Changed => true,
        StatComparison::Unchanged => false,
        StatComparison::NeedContentCompare => {
            let original_content = tokio::fs::read_to_string(original_file).await;
            let backup_content = tokio::fs::read_to_string(&backup_path).await;
            match (original_content, backup_content) {
                (Ok(original), Ok(backup)) => original != backup,
                // File deleted between stat and read → treat as changed.
                _ => true,
            }
        }
    }
}

/// Maps to: CC `computeDiffStatsForFile` — line-diff counts between the
/// current file and its backup (either side may be absent).
async fn compute_diff_stats_for_file(
    original_file: &str,
    backup_file_name: Option<&str>,
) -> DiffStats {
    let backup_path = backup_file_name.map(|name| resolve_backup_path(name, None));

    let original_content = read_file_or_null(Path::new(original_file)).await;
    let backup_content = match &backup_path {
        Some(path) => read_file_or_null(path).await,
        None => None,
    };

    if original_content.is_none() && backup_content.is_none() {
        return DiffStats::default();
    }

    let mut stats = DiffStats {
        files_changed: vec![original_file.to_string()],
        insertions: 0,
        deletions: 0,
    };

    // CC diffLines(original, backup): added = lines the backup would add
    // back, removed = lines the rewind would drop.
    let original = original_content.unwrap_or_default();
    let backup = backup_content.unwrap_or_default();
    let diff = similar::TextDiff::from_lines(&original, &backup);
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => stats.insertions += 1,
            similar::ChangeTag::Delete => stats.deletions += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    stats
}

/// Maps to: CC `getBackupFileName` — `sha256(filePath)[..16]@v{version}`.
pub fn get_backup_file_name(file_path: &str, version: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_path.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}@v{version}", &hex[..16])
}

/// Maps to: CC `resolveBackupPath` —
/// `{configDir}/file-history/{sessionId}/{backupFileName}`.
pub fn resolve_backup_path(backup_file_name: &str, session_id: Option<&str>) -> PathBuf {
    let session = session_id
        .map(str::to_string)
        .unwrap_or_else(get_session_id);
    get_claude_config_home_dir()
        .join("file-history")
        .join(session)
        .join(backup_file_name)
}

/// Maps to: CC `createBackup` — copies the file to its versioned backup path
/// (preserving permissions). ENOENT on the source records a null backup;
/// lazy mkdir on the destination (copy first, mkdir+retry on ENOENT).
async fn create_backup(file_path: Option<&str>, version: u32) -> io::Result<FileHistoryBackup> {
    let Some(file_path) = file_path else {
        return Ok(FileHistoryBackup {
            backup_file_name: None,
            version,
            backup_time: Utc::now(),
        });
    };

    let backup_file_name = get_backup_file_name(file_path, version);
    let backup_path = resolve_backup_path(&backup_file_name, None);

    // Stat first: source missing → null backup, cleanly separated from a
    // missing backup directory.
    let src_stats = match tokio::fs::metadata(file_path).await {
        Ok(stats) => stats,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(FileHistoryBackup {
                backup_file_name: None,
                version,
                backup_time: Utc::now(),
            });
        }
        Err(error) => return Err(error),
    };

    // copyFile keeps content off the heap; lazy mkdir hits the fast path in
    // the common case.
    if let Err(error) = tokio::fs::copy(file_path, &backup_path).await {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
        if let Some(parent) = backup_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::copy(file_path, &backup_path).await?;
    }

    // Preserve file permissions on the backup.
    tokio::fs::set_permissions(&backup_path, src_stats.permissions()).await?;
    // CC: tengu_file_history_backup_file_created analytics.

    Ok(FileHistoryBackup {
        backup_file_name: Some(backup_file_name),
        version,
        backup_time: Utc::now(),
    })
}

/// Maps to: CC `restoreBackup` — copies the backup over the original path
/// (lazy mkdir) and restores permissions. A missing backup logs and bails
/// without error (CC returns after logEvent).
async fn restore_backup(file_path: &str, backup_file_name: &str) -> io::Result<()> {
    let backup_path = resolve_backup_path(backup_file_name, None);

    let backup_stats = match tokio::fs::metadata(&backup_path).await {
        Ok(stats) => stats,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // CC: logError('Backup file not found') + restore_file_failed.
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    if let Err(error) = tokio::fs::copy(&backup_path, file_path).await {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
        if let Some(parent) = Path::new(file_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::copy(&backup_path, file_path).await?;
    }

    tokio::fs::set_permissions(file_path, backup_stats.permissions()).await
}

/// Maps to: CC `getBackupFileNameFirstVersion` — the earliest (v1) backup for
/// a file, used when rewinding to a point before the file was tracked.
/// `Some(None)` means the file did not exist in v1; `None` means no v1 was
/// found at all (resolution error).
fn get_backup_file_name_first_version(
    tracking_path: &str,
    state: &FileHistoryState,
) -> ResolvedBackupFileName {
    for snapshot in &state.snapshots {
        if let Some(backup) = snapshot.tracked_file_backups.get(tracking_path) {
            if backup.version == 1 {
                return Some(backup.backup_file_name.clone());
            }
        }
    }
    None
}

/// Maps to: CC `maybeShortenFilePath` — tracking keys are cwd-relative when
/// possible to reduce session-storage space.
pub fn maybe_shorten_file_path(file_path: &str) -> String {
    let path = Path::new(file_path);
    if !path.is_absolute() {
        return file_path.to_string();
    }
    let cwd = get_original_cwd();
    match path.strip_prefix(&cwd) {
        Ok(relative) => relative.to_string_lossy().to_string(),
        Err(_) => file_path.to_string(),
    }
}

/// Maps to: CC `maybeExpandFilePath`.
pub fn maybe_expand_file_path(file_path: &str) -> String {
    let path = Path::new(file_path);
    if path.is_absolute() {
        return file_path.to_string();
    }
    Path::new(&get_original_cwd())
        .join(path)
        .to_string_lossy()
        .to_string()
}

/// Maps to: CC `fileHistoryRestoreStateFromLog` — rebuilds tracked files from
/// resumed snapshots, migrating absolute paths to shortened tracking keys.
pub fn file_history_restore_state_from_log(
    file_history_snapshots: &[FileHistorySnapshot],
    on_update_state: impl FnOnce(FileHistoryState),
) {
    if !file_history_enabled() {
        return;
    }
    let mut snapshots = Vec::with_capacity(file_history_snapshots.len());
    let mut tracked_files = BTreeSet::new();
    for snapshot in file_history_snapshots {
        let mut tracked_file_backups = BTreeMap::new();
        for (path, backup) in &snapshot.tracked_file_backups {
            let tracking_path = maybe_shorten_file_path(path);
            tracked_files.insert(tracking_path.clone());
            tracked_file_backups.insert(tracking_path, backup.clone());
        }
        snapshots.push(FileHistorySnapshot {
            message_id: snapshot.message_id.clone(),
            tracked_file_backups,
            timestamp: snapshot.timestamp,
        });
    }
    let snapshot_sequence = snapshots.len() as u64;
    on_update_state(FileHistoryState {
        snapshots,
        tracked_files,
        snapshot_sequence,
    });
}

/// Maps to: CC `copyFileHistoryForResume` — hard-links (copy fallback) the
/// previous session's backup files into the current session's directory.
/// CC unpacks `LogOption` (snapshots + last message's sessionId); the caller
/// does that unpacking here.
pub async fn copy_file_history_for_resume(
    file_history_snapshots: &[FileHistorySnapshot],
    previous_session_id: &str,
) {
    if !file_history_enabled() {
        return;
    }
    if file_history_snapshots.is_empty() {
        return;
    }

    let session_id = get_session_id();
    if previous_session_id == session_id {
        // Same session id — the backups are already in place.
        return;
    }

    // All backups share {configDir}/file-history/{sessionId}/ — create once.
    let new_backup_dir = get_claude_config_home_dir()
        .join("file-history")
        .join(&session_id);
    if tokio::fs::create_dir_all(&new_backup_dir).await.is_err() {
        // CC: logError.
        return;
    }

    for snapshot in file_history_snapshots {
        let mut copy_failed = false;
        for backup in snapshot.tracked_file_backups.values() {
            let Some(backup_file_name) = backup.backup_file_name.as_deref() else {
                continue;
            };
            let old_backup_path = resolve_backup_path(backup_file_name, Some(previous_session_id));
            let new_backup_path = new_backup_dir.join(backup_file_name);

            match tokio::fs::hard_link(&old_backup_path, &new_backup_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    // Already migrated, skip.
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // CC: logError('backup file does not exist in previous
                    // session') — snapshot counts as failed.
                    copy_failed = true;
                }
                Err(_) => {
                    // Fallback to copy if hard link fails.
                    if tokio::fs::copy(&old_backup_path, &new_backup_path)
                        .await
                        .is_err()
                    {
                        // CC: logError.
                        copy_failed = true;
                    }
                }
            }
        }
        if !copy_failed {
            // CC: recordFileHistorySnapshot(snapshot.messageId, snapshot,
            // false) — joins with session-storage resume persistence.
        }
        // CC: tengu_file_history_resume_copy_failed analytics when any
        // snapshot failed.
    }
}

/// Maps to: CC `readFileAsyncOrNull` — best-effort read.
async fn read_file_or_null(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(message_id: &str, backups: &[(&str, Option<&str>, u32)]) -> FileHistorySnapshot {
        FileHistorySnapshot {
            message_id: message_id.to_string(),
            tracked_file_backups: backups
                .iter()
                .map(|(path, name, version)| {
                    (
                        path.to_string(),
                        FileHistoryBackup {
                            backup_file_name: name.map(str::to_string),
                            version: *version,
                            backup_time: Utc::now(),
                        },
                    )
                })
                .collect(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn backup_file_name_matches_official_hash_shape() {
        let name = get_backup_file_name("/repo/src/main.rs", 3);
        let (hash, version) = name.split_once("@v").expect("has @v separator");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(version, "3");
        // Deterministic per path+version.
        assert_eq!(name, get_backup_file_name("/repo/src/main.rs", 3));
        assert_ne!(name, get_backup_file_name("/repo/src/main.rs", 4));
    }

    #[test]
    fn resolve_backup_path_matches_official_layout() {
        let path = resolve_backup_path("abcd@v1", Some("session-x"));
        assert_eq!(
            path,
            get_claude_config_home_dir()
                .join("file-history")
                .join("session-x")
                .join("abcd@v1")
        );
    }

    #[test]
    fn first_version_lookup_matches_official_three_states() {
        let state = FileHistoryState {
            snapshots: vec![
                snapshot("m1", &[("a.rs", Some("aaaa@v1"), 1), ("b.rs", None, 1)]),
                snapshot("m2", &[("a.rs", Some("aaaa@v2"), 2)]),
            ],
            tracked_files: ["a.rs", "b.rs", "c.rs"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            snapshot_sequence: 2,
        };
        // Backup exists at v1.
        assert_eq!(
            get_backup_file_name_first_version("a.rs", &state),
            Some(Some("aaaa@v1".to_string()))
        );
        // v1 recorded the file as absent (CC null).
        assert_eq!(
            get_backup_file_name_first_version("b.rs", &state),
            Some(None)
        );
        // No v1 at all (CC undefined).
        assert_eq!(get_backup_file_name_first_version("c.rs", &state), None);
    }

    #[test]
    fn restore_state_from_log_rebuilds_tracked_files_and_sequence() {
        let snapshots = vec![
            snapshot("m1", &[("src/a.rs", Some("aaaa@v1"), 1)]),
            snapshot("m2", &[("src/b.rs", None, 1)]),
        ];
        let mut restored: Option<FileHistoryState> = None;
        file_history_restore_state_from_log(&snapshots, |state| restored = Some(state));
        let restored = restored.expect("gate enabled by default");
        assert_eq!(restored.snapshots.len(), 2);
        assert_eq!(restored.snapshot_sequence, 2);
        assert!(restored.tracked_files.contains("src/a.rs"));
        assert!(restored.tracked_files.contains("src/b.rs"));
    }

    #[test]
    fn compare_stats_handles_official_presence_matrix() {
        assert_eq!(compare_stats(None, None), StatComparison::Unchanged);
        let temp =
            std::env::temp_dir().join(format!("cometix-fh-compare-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&temp, "hello\n").expect("write temp");
        let stats = std::fs::metadata(&temp).expect("stat temp");
        assert_eq!(
            compare_stats(Some(&stats), None),
            StatComparison::Changed,
            "one side missing = changed"
        );
        assert_eq!(compare_stats(None, Some(&stats)), StatComparison::Changed);
        let _ = std::fs::remove_file(&temp);
    }

    #[tokio::test]
    async fn diff_stats_count_deleted_lines_against_missing_backup() {
        // With no backup (file absent at target), every current line counts
        // as a deletion on rewind — CC diffLines(original, '').
        let temp =
            std::env::temp_dir().join(format!("cometix-fh-diff-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&temp, "one\ntwo\nthree\n").expect("write temp");
        let stats = compute_diff_stats_for_file(&temp.to_string_lossy(), None).await;
        assert_eq!(stats.files_changed.len(), 1);
        assert_eq!(stats.insertions, 0);
        assert_eq!(stats.deletions, 3);
        let _ = std::fs::remove_file(&temp);

        // Both sides absent → empty stats, no filesChanged entry.
        let missing =
            std::env::temp_dir().join(format!("cometix-fh-missing-{}.txt", uuid::Uuid::new_v4()));
        let stats = compute_diff_stats_for_file(&missing.to_string_lossy(), None).await;
        assert_eq!(stats, DiffStats::default());
    }

    #[test]
    fn can_restore_matches_official_snapshot_lookup() {
        let state = FileHistoryState {
            snapshots: vec![snapshot("m1", &[])],
            tracked_files: BTreeSet::new(),
            snapshot_sequence: 1,
        };
        assert!(file_history_can_restore(&state, "m1"));
        assert!(!file_history_can_restore(&state, "missing"));
    }
}
