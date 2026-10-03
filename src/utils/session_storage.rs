//! Maps to `utils/sessionStorage.ts` and `utils/sessionStoragePortable.ts`.
//!
//! Session JSONL persistence: one `.jsonl` file per session.
//!
//! ## File path
//!
//! `~/.claude/projects/${sanitizedProjectPath}/${sessionId}.jsonl`
//!
//! ## JSONL shape
//!
//! Each line is a JSON object. The top-level `type` field selects the entry kind.
//!
//! ### Transcript messages that participate in the `parentUuid` chain
//!
//! - `user`
//! - `assistant`
//! - `attachment`
//! - `system`
//!
//! ### Session metadata entries with last-wins semantics
//!
//! - `summary`, `custom-title`, `ai-title`, `tag`, `last-prompt`, `task-summary`
//! - `agent-name`, `agent-color`, `agent-setting`, `mode`, `worktree-state`
//! - `pr-link`, `file-history-snapshot`, `attribution-snapshot`
//! - `content-replacement`, `marble-origami-commit`, `marble-origami-snapshot`
//! - `speculation-accept`, `queue-operation`
//!
//! ## Implemented parity
//!
//! - [x] Path calculation: sanitize, hash, and project-dir lookup.
//! - [x] Session list loading with lite head/tail metadata reads.
//! - [x] Per-file `Project` write queues, per-entry completion, 100 ms scheduled
//!       drain, 100 MB chunks, and awaited `flushSessionStorage` barriers.
//! - [x] Compact-boundary-aware reads with byte-level marker scans and chunking.
//! - [x] Structured load that splits all known entry kinds into parity maps.
//! - [x] Leaf UUID computation and conversation-chain reconstruction.
//! - [x] Attribution-snapshot restore payload collection.
//! - [x] `resolveSessionFilePath` exact, prefix-fallback, and global scanning.
//! - [x] `recordTranscript` seam with message-set dedup and parent chain advance.
//! - [x] `logicalParentUuid` write seam for compact boundaries.
//! - [x] `saveCustomTitle`, `saveTag`, and `saveAgentName` seams.
//! - [x] `reAppendSessionMetadata` tail refresh and metadata re-append seam.
//! - [x] `scanPreBoundaryMetadata` streaming marker scan with carry buffer.
//! - [x] `applyPreservedSegmentRelinks` boundary collection, walk verification,
//!       relink, prune, and stale-usage zeroing.
//! - [x] `applySnipRemovals` removal replay and survivor parent relinking.
//! - [x] Legacy progress bridge for transcripts written before #24099.
//!
//! ## Extended parity checklist
//!
//! - [x] Subagent transcript path/subdir and queued sidechain writes.
//! - [x] Agent metadata path/read/write seam under the session write policy.
//! - [x] Runtime subagent transcript and agent metadata writes.
//! - [x] Write queue and flush parity for the async `recordTranscript` path.
//! - [x] `shouldSkipPersistence` gates for cleanupPeriodDays=0,
//!       `--no-session-persistence`, and `CLAUDE_CODE_SKIP_PROMPT_HISTORY`.
//! - [x] Tracked `removeTranscriptMessage` tombstones with 64 KB tail splice,
//!       bounded slow rewrite, and fire-and-forget/flush ordering.
//! - [x] Canonical path and NFC normalization parity.
//! - [x] Worktree fallback via git worktree path discovery.
//! - [ ] Remote ingress/internal-event persistence.
//!       Retention cleanup belongs to CC `utils/cleanup.ts`, not this module.
//! - [x] Progressive `SessionLogResult` / `enrichLogs` loading.
//! - [x] Batch stat-only session discovery.
//!
//! The former hard-disabled write seam now follows CC's queued `Project` writer;
//! CC's synchronous rename/materialize/cleanup helpers are serialized by that
//! same owner before writing inline.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use unicode_normalization::UnicodeNormalization;

// Constants matching the official sessionStorage read path.

const MAX_TRANSCRIPT_READ_BYTES: u64 = 50 * 1024 * 1024;
const MAX_TOMBSTONE_REWRITE_BYTES: u64 = 50 * 1024 * 1024;
const FLUSH_INTERVAL_MS: u64 = 100;
const MAX_CHUNK_BYTES: usize = 100 * 1024 * 1024;

/// Session persistence is implemented and enabled in normal builds.
pub const SESSION_WRITE_ENABLED: bool = true;

/// Rust policy adapter; CC callsites use `Project.shouldSkipPersistence()`.
pub fn is_session_write_enabled() -> bool {
    !Project::should_skip_persistence()
}

/// Private Rust transport for CC `Project.scheduleDrain()` / `activeDrain`.
enum ProjectWriteCommand {
    EnqueueWrite {
        path: PathBuf,
        entry: serde_json::Value,
        resolve: async_channel::Sender<()>,
    },
    /// L1 transport for CC's mutation of a queued assistant's message object.
    PatchAssistantDelta {
        path: PathBuf,
        uuid: String,
        stop_reason: serde_json::Value,
        usage: serde_json::Value,
    },
    AppendEntryToFile {
        path: PathBuf,
        entry: serde_json::Value,
        reply: std::sync::mpsc::SyncSender<Result<(), String>>,
    },
    RemoveMessageByUuid {
        path: PathBuf,
        target_uuid: String,
        reply: async_channel::Sender<Result<(), String>>,
    },
    Flush {
        reply: async_channel::Sender<Result<(), String>>,
    },
}

/// Maps to: CC `utils/sessionStorage.ts` `Project` write-queue subset.
struct Project {
    sender: std::sync::mpsc::Sender<ProjectWriteCommand>,
    current_session_meta: RwLock<CurrentSessionMeta>,
}

impl Project {
    /// Maps to: CC `Project.shouldSkipPersistence()`.
    fn should_skip_persistence() -> bool {
        if !SESSION_WRITE_ENABLED
            || crate::bootstrap::state::is_session_persistence_disabled()
            || crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_SKIP_PROMPT_HISTORY")
                    .as_deref(),
            )
        {
            return true;
        }
        // Unit/component tests must opt in so unrelated harnesses cannot mutate
        // fixtures or a developer's real sessions. Production writes by default.
        #[cfg(test)]
        {
            !crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("COMETIX_WRITE_ENABLED").as_deref(),
            )
        }
        #[cfg(not(test))]
        {
            crate::utils::env_utils::is_env_defined_falsy(
                crate::utils::process_env::var("COMETIX_WRITE_ENABLED").as_deref(),
            )
        }
    }

    fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("cometix-session-writer".to_string())
            .spawn(move || Project::schedule_drain(receiver))
            .expect("failed to start Project write queue");
        Self {
            sender,
            current_session_meta: RwLock::new(CurrentSessionMeta::default()),
        }
    }

    /// Maps to: CC `Project.enqueueWrite(filePath, entry)`.
    fn enqueue_write(
        &self,
        path: PathBuf,
        entry: &serde_json::Value,
    ) -> anyhow::Result<async_channel::Receiver<()>> {
        let (resolve, written) = async_channel::bounded(1);
        self.sender
            .send(ProjectWriteCommand::EnqueueWrite {
                path,
                entry: entry.clone(),
                resolve,
            })
            .map_err(|_| anyhow::anyhow!("Project write queue stopped"))?;
        Ok(written)
    }

    /// Maps to: CC `Project.flush()`.
    async fn flush(&self) -> anyhow::Result<()> {
        let (reply, receiver) = async_channel::bounded(1);
        self.sender
            .send(ProjectWriteCommand::Flush { reply })
            .map_err(|_| anyhow::anyhow!("Project write queue stopped"))?;
        receiver
            .recv()
            .await
            .map_err(|_| anyhow::anyhow!("Project dropped flush response"))?
            .map_err(anyhow::Error::msg)
    }
}

static PROJECT: LazyLock<Project> = LazyLock::new(Project::new);

/// Maps to: CC `getProject()`.
fn get_project() -> &'static Project {
    &PROJECT
}

impl Project {
    /// Maps to CC's timer-backed `Project.scheduleDrain()`; a dedicated Rust
    /// actor replaces the JS event-loop timer while retaining its 100 ms batch.
    fn schedule_drain(receiver: std::sync::mpsc::Receiver<ProjectWriteCommand>) {
        use std::sync::mpsc::RecvTimeoutError;

        let mut queues =
            HashMap::<PathBuf, Vec<(serde_json::Value, async_channel::Sender<()>)>>::new();
        let mut drain_deadline = Option::<std::time::Instant>::None;
        let mut pending_errors = Vec::<String>::new();

        loop {
            let command = match drain_deadline {
                Some(deadline) => {
                    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
                    match receiver.recv_timeout(timeout) {
                        Ok(command) => Some(command),
                        Err(RecvTimeoutError::Timeout) => {
                            pending_errors.extend(Self::drain_write_queue(&mut queues));
                            drain_deadline = None;
                            None
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            let _ = Self::drain_write_queue(&mut queues);
                            break;
                        }
                    }
                }
                None => match receiver.recv() {
                    Ok(command) => Some(command),
                    Err(_) => {
                        let _ = Self::drain_write_queue(&mut queues);
                        break;
                    }
                },
            };

            let Some(command) = command else {
                continue;
            };
            match command {
                ProjectWriteCommand::EnqueueWrite {
                    path,
                    entry,
                    resolve,
                } => {
                    queues.entry(path).or_default().push((entry, resolve));
                    if drain_deadline.is_none() {
                        drain_deadline = Some(
                            std::time::Instant::now()
                                + std::time::Duration::from_millis(FLUSH_INTERVAL_MS),
                        );
                    }
                }
                ProjectWriteCommand::PatchAssistantDelta {
                    path,
                    uuid,
                    stop_reason,
                    usage,
                } => {
                    if let Some(entries) = queues.get_mut(&path) {
                        if let Some((entry, _)) = entries.iter_mut().rev().find(|(entry, _)| {
                            entry.get("uuid").and_then(serde_json::Value::as_str) == Some(&uuid)
                                && entry.get("type").and_then(serde_json::Value::as_str)
                                    == Some("assistant")
                        }) {
                            if let Some(message) = entry
                                .get_mut("message")
                                .and_then(serde_json::Value::as_object_mut)
                            {
                                message.insert("stop_reason".to_string(), stop_reason);
                                message.insert("usage".to_string(), usage);
                            }
                        }
                    }
                }
                ProjectWriteCommand::AppendEntryToFile { path, entry, reply } => {
                    pending_errors.extend(Self::drain_write_queue(&mut queues));
                    drain_deadline = None;
                    match serde_json::to_vec(&entry) {
                        Ok(mut line) => {
                            line.push(b'\n');
                            if let Err(error) = Self::append_to_file(&path, &line) {
                                pending_errors.push(format!("{}: {error}", path.display()));
                            }
                        }
                        Err(error) => {
                            pending_errors.push(format!("{}: {error}", path.display()));
                        }
                    }
                    let result = if pending_errors.is_empty() {
                        Ok(())
                    } else {
                        Err(std::mem::take(&mut pending_errors).join("; "))
                    };
                    let _ = reply.send(result);
                }
                ProjectWriteCommand::RemoveMessageByUuid {
                    path,
                    target_uuid,
                    reply,
                } => {
                    pending_errors.extend(Self::drain_write_queue(&mut queues));
                    drain_deadline = None;
                    Self::remove_message_by_uuid(&path, &target_uuid);
                    // CC swallows removeMessageByUuid file errors. Queue errors
                    // remain pending for the next explicit `flush()` barrier.
                    let _ = reply.send_blocking(Ok(()));
                }
                ProjectWriteCommand::Flush { reply } => {
                    pending_errors.extend(Self::drain_write_queue(&mut queues));
                    drain_deadline = None;
                    let result = if pending_errors.is_empty() {
                        Ok(())
                    } else {
                        Err(std::mem::take(&mut pending_errors).join("; "))
                    };
                    let _ = reply.send_blocking(result);
                }
            }
        }
    }

    /// Maps to: CC `Project.drainWriteQueue()`.
    fn drain_write_queue(
        queues: &mut HashMap<PathBuf, Vec<(serde_json::Value, async_channel::Sender<()>)>>,
    ) -> Vec<String> {
        let batches = std::mem::take(queues);
        let mut errors = Vec::new();
        for (path, entries) in batches {
            let mut chunk = Vec::<u8>::new();
            let mut resolvers = Vec::<async_channel::Sender<()>>::new();
            for (entry, resolve) in entries {
                let mut line = match serde_json::to_vec(&entry) {
                    Ok(line) => line,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                };
                line.push(b'\n');
                if !chunk.is_empty() && chunk.len().saturating_add(line.len()) >= MAX_CHUNK_BYTES {
                    match Self::append_to_file(&path, &chunk) {
                        Ok(()) => {
                            for resolver in resolvers.drain(..) {
                                let _ = resolver.send_blocking(());
                            }
                        }
                        Err(error) => {
                            errors.push(format!("{}: {error}", path.display()));
                            resolvers.clear();
                        }
                    }
                    chunk.clear();
                }
                chunk.extend_from_slice(&line);
                resolvers.push(resolve);
            }
            if !chunk.is_empty() {
                match Self::append_to_file(&path, &chunk) {
                    Ok(()) => {
                        for resolver in resolvers {
                            let _ = resolver.send_blocking(());
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
        }
        errors
    }

    /// Maps to: CC `Project.appendToFile(filePath, data)`.
    fn append_to_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;

        fn open(path: &Path) -> std::io::Result<std::fs::File> {
            let mut options = std::fs::OpenOptions::new();
            options.create(true).append(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path)
        }

        let mut file = match open(path) {
            Ok(file) => file,
            Err(_) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                open(path)?
            }
        };
        file.write_all(bytes)
    }

    /// Maps to: CC `Project.removeMessageByUuid(targetUuid)`.
    fn remove_message_by_uuid(path: &Path, target_uuid: &str) {
        use std::io::Write;

        let mut file_size = 0u64;
        let fast_path = (|| -> std::io::Result<bool> {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)?;
            file_size = file.metadata()?.len();
            if file_size == 0 {
                return Ok(true);
            }

            let chunk_len = file_size.min(LITE_READ_BUF_SIZE) as usize;
            let tail_start = file_size - chunk_len as u64;
            let mut tail = vec![0u8; chunk_len];
            file.seek(SeekFrom::Start(tail_start))?;
            let bytes_read = file.read(&mut tail)?;
            tail.truncate(bytes_read);

            let needle = format!(
                "\"uuid\":{}",
                serde_json::Value::String(target_uuid.to_string())
            );
            let needle = needle.as_bytes();
            let Some(match_idx) = tail
                .windows(needle.len())
                .rposition(|window| window == needle)
            else {
                return Ok(false);
            };

            let previous_newline = tail[..match_idx].iter().rposition(|byte| *byte == b'\n');
            if previous_newline.is_none() && tail_start != 0 {
                return Ok(false);
            }
            let line_start = previous_newline.map_or(0, |index| index + 1);
            let after_needle = match_idx + needle.len();
            let line_end = tail[after_needle..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes_read, |index| after_needle + index + 1);
            let absolute_line_start = tail_start + line_start as u64;

            file.set_len(absolute_line_start)?;
            if line_end < bytes_read {
                file.seek(SeekFrom::Start(absolute_line_start))?;
                file.write_all(&tail[line_end..bytes_read])?;
            }
            Ok(true)
        })();

        match fast_path {
            Ok(true) => return,
            Ok(false) => {}
            Err(_) => return,
        }

        if file_size > MAX_TOMBSTONE_REWRITE_BYTES {
            crate::utils::debug::log_for_debugging(&format!(
                "Skipping tombstone removal: session file too large ({})",
                crate::utils::format::format_file_size(file_size)
            ));
            return;
        }

        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let lines = content
            .split('\n')
            .filter(|line| {
                if line.trim().is_empty() {
                    return true;
                }
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|entry| {
                        entry
                            .get("uuid")
                            .and_then(serde_json::Value::as_str)
                            .map(|uuid| uuid != target_uuid)
                    })
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        let _ = std::fs::write(path, lines.join("\n"));
    }
}

/// Maps to: CC exported `flushSessionStorage()`.
pub async fn flush_session_storage() -> anyhow::Result<()> {
    get_project().flush().await
}

/// Maps to: CC exported `removeTranscriptMessage(targetUuid)`.
///
/// Command registration is synchronous, like JavaScript async functions before
/// their first `await`; callers may intentionally discard the returned future
/// and a later `flushSessionStorage()` still observes the tracked mutation.
pub fn remove_transcript_message(
    target_uuid: &str,
) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static {
    let target_uuid = target_uuid.to_string();
    let pending: anyhow::Result<Option<(PathBuf, async_channel::Receiver<Result<(), String>>)>> =
        (|| {
            if !is_session_write_enabled() {
                return Ok(None);
            }
            let Some(path) = get_project()
                .current_session_meta
                .read()
                .map_err(|_| anyhow::anyhow!("session metadata lock poisoned"))?
                .session_file
                .clone()
            else {
                return Ok(None);
            };
            let (reply, response) = async_channel::bounded(1);
            get_project()
                .sender
                .send(ProjectWriteCommand::RemoveMessageByUuid {
                    path: path.clone(),
                    target_uuid: target_uuid.clone(),
                    reply,
                })
                .map_err(|_| anyhow::anyhow!("Project write queue stopped"))?;
            Ok(Some((path, response)))
        })();

    async move {
        let Some((path, response)) = pending? else {
            return Ok(());
        };
        response
            .recv()
            .await
            .map_err(|_| anyhow::anyhow!("Project dropped removeMessageByUuid response"))?
            .map_err(anyhow::Error::msg)?;

        let refresh_parent = if let Ok(mut meta) = get_project().current_session_meta.write() {
            get_session_messages(&crate::bootstrap::state::get_session_id())
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&target_uuid);
            meta.last_message_uuid.as_deref() == Some(target_uuid.as_str())
        } else {
            false
        };
        if refresh_parent {
            let loaded = load_session_structured_from_path(&path);
            let parent_uuid = loaded
                .message_order
                .iter()
                .rev()
                .find(|uuid| loaded.leaf_uuids.contains(*uuid))
                .cloned();
            if let Ok(mut meta) = get_project().current_session_meta.write() {
                if meta.last_message_uuid.as_deref() == Some(target_uuid.as_str()) {
                    meta.last_message_uuid = parent_uuid;
                }
            }
        }
        Ok(())
    }
}

/// Maps to CC `utils/sessionStorage.ts` `recordFileHistorySnapshot(...)`.
pub fn record_file_history_snapshot(
    message_id: &str,
    snapshot: &crate::utils::file_history::FileHistorySnapshot,
    is_snapshot_update: bool,
) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        return Ok(());
    }
    let session_id = crate::bootstrap::state::get_session_id();
    let entry = serde_json::json!({
        "type": "file-history-snapshot",
        "messageId": message_id,
        "snapshot": snapshot,
        "isSnapshotUpdate": is_snapshot_update,
    });
    append_entry(
        &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
        &session_id,
        &entry,
    )
}

/// Maps to CC `utils/sessionStorage.ts` `recordContentReplacement(...)`.
pub fn record_content_replacement(
    records: &[crate::utils::tool_result_storage::ContentReplacementRecord],
    agent_id: Option<&str>,
) -> anyhow::Result<()> {
    if !is_session_write_enabled() || records.is_empty() {
        return Ok(());
    }
    let session_id = crate::bootstrap::state::get_session_id();
    let entry = serde_json::json!({
        "type":"content-replacement",
        "sessionId":session_id,
        "agentId":agent_id,
        "replacements":records,
    });
    append_entry(
        &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
        &session_id,
        &entry,
    )
}

const LITE_READ_BUF_SIZE: u64 = 65536;
const SKIP_PRECOMPACT_THRESHOLD: u64 = 5 * 1024 * 1024;
const TRANSCRIPT_READ_CHUNK_SIZE: usize = 1024 * 1024;
const MAX_SANITIZED_LENGTH: usize = 200;
const MAX_FIRST_PROMPT_CHARS: usize = 200;
const COMPACT_BOUNDARY_MARKER: &[u8] = b"compact_boundary";

/// Maps to CC `utils/sessionStorage.ts` module-level
/// `agentTranscriptSubdirs` used by `getAgentTranscriptPath(...)`.
static AGENT_TRANSCRIPT_SUBDIRS: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

// JSONL entry types.

pub const TRANSCRIPT_TYPES: &[&str] = &["user", "assistant", "attachment", "system"];
pub const SUBTYPE_COMPACT_BOUNDARY: &str = "compact_boundary";

// Structured load result.

#[derive(Debug, Clone, Default)]
pub struct LoadedSession {
    pub messages: HashMap<String, serde_json::Value>,
    pub message_order: Vec<String>,
    pub summaries: HashMap<String, String>,
    pub custom_titles: HashMap<String, String>,
    pub ai_titles: HashMap<String, String>,
    pub tags: HashMap<String, String>,
    pub last_prompts: HashMap<String, String>,
    pub task_summaries: HashMap<String, String>,
    pub agent_names: HashMap<String, String>,
    pub agent_colors: HashMap<String, String>,
    pub agent_settings: HashMap<String, String>,
    pub modes: HashMap<String, String>,
    pub worktree_states: HashMap<String, serde_json::Value>,
    pub pr_numbers: HashMap<String, u64>,
    pub pr_urls: HashMap<String, String>,
    pub pr_repositories: HashMap<String, String>,
    pub file_history_snapshots: HashMap<String, serde_json::Value>,
    /// Attribution snapshots — maps to attributionSnapshots.
    pub attribution_snapshots: Vec<serde_json::Value>,
    pub content_replacements: HashMap<String, Vec<serde_json::Value>>,
    pub agent_content_replacements: HashMap<String, Vec<serde_json::Value>>,
    pub context_collapse_commits: Vec<serde_json::Value>,
    pub context_collapse_snapshot: Option<serde_json::Value>,
    pub speculation_accepts: Vec<serde_json::Value>,
    pub leaf_uuids: HashSet<String>,

    pub compact_boundaries: Vec<CompactBoundaryInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct CompactBoundaryInfo {
    pub entry_index: usize,
    pub preserved_segment: Option<PreservedSegment>,
    pub raw: serde_json::Value,
}

// Path utilities.

#[cfg(test)]
static TEST_PROJECTS_DIR_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[cfg(test)]
fn test_projects_dir_override() -> &'static Mutex<Option<PathBuf>> {
    TEST_PROJECTS_DIR_OVERRIDE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn test_projects_dir_override_guard() -> std::sync::MutexGuard<'static, Option<PathBuf>> {
    test_projects_dir_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub struct TestProjectsDirOverrideGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
impl Drop for TestProjectsDirOverrideGuard {
    fn drop(&mut self) {
        let mut override_path = test_projects_dir_override_guard();
        *override_path = self.previous.take();
    }
}

#[cfg(test)]
pub fn set_test_projects_dir_override(path: impl Into<PathBuf>) -> TestProjectsDirOverrideGuard {
    let mut override_path = test_projects_dir_override_guard();
    let previous = override_path.replace(path.into());
    TestProjectsDirOverrideGuard { previous }
}

pub fn get_projects_dir() -> PathBuf {
    #[cfg(test)]
    {
        if let Some(path) = test_projects_dir_override_guard().clone() {
            return path;
        }
    }

    crate::utils::env_utils::get_claude_config_home_dir().join("projects")
}

/// Maps to: CC `sessionStoragePortable.ts` `sanitizePath(name)`.
pub fn sanitize_path(name: &str) -> String {
    let sanitized = name
        .encode_utf16()
        .map(|unit| {
            if (b'a' as u16..=b'z' as u16).contains(&unit)
                || (b'A' as u16..=b'Z' as u16).contains(&unit)
                || (b'0' as u16..=b'9' as u16).contains(&unit)
            {
                (unit as u8) as char
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        return sanitized;
    }

    format!(
        "{}-{}",
        &sanitized[..MAX_SANITIZED_LENGTH],
        simple_hash(name)
    )
}

/// Maps to: CC `utils/sessionStoragePortable.ts:295-297` `simpleHash`.
fn simple_hash(value: &str) -> String {
    let hash = crate::utils::hash::djb2_hash(value);
    let mut value = if hash < 0 {
        -(hash as i64)
    } else {
        hash as i64
    } as u64;
    let mut base36 = Vec::new();
    if value == 0 {
        base36.push('0');
    } else {
        const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        while value > 0 {
            base36.push(DIGITS[(value % 36) as usize] as char);
            value /= 36;
        }
        base36.reverse();
    }
    base36.into_iter().collect()
}

/// Maps to: CC `sessionStorage.ts` / portable `getProjectDir(projectDir)`.
pub fn get_project_dir(project_dir: &str) -> PathBuf {
    get_projects_dir().join(sanitize_path(project_dir))
}

pub fn get_session_file_path(project_path: &str, session_id: &str) -> PathBuf {
    get_project_dir(project_path).join(format!("{session_id}.jsonl"))
}

/// Maps to CC `utils/sessionStorage.ts` `setAgentTranscriptSubdir(...)`.
pub fn set_agent_transcript_subdir(agent_id: &str, subdir: impl Into<String>) {
    if let Ok(mut subdirs) = AGENT_TRANSCRIPT_SUBDIRS.write() {
        subdirs.insert(agent_id.to_string(), subdir.into());
    }
}

/// Maps to CC `utils/sessionStorage.ts` `clearAgentTranscriptSubdir(...)`.
pub fn clear_agent_transcript_subdir(agent_id: &str) {
    if let Ok(mut subdirs) = AGENT_TRANSCRIPT_SUBDIRS.write() {
        subdirs.remove(agent_id);
    }
}

/// The project directory that owns every subagent artifact of the active
/// session.
///
/// Maps to: CC `getSessionProjectDir() ?? getProjectDir(getOriginalCwd())`
/// (`sessionStorage.ts:250`) — the same pair `getTranscriptPath` (`:203`) and
/// `getRemoteAgentsDir` (`:321`) read, with CC's own comment on `:248-250`
/// spelling out the invariant: "subagent transcripts live under the session
/// dir, so if the session transcript is at sessionProjectDir, subagent
/// transcripts are too."
///
/// This must NOT be derived from a run cwd. CC enters an AsyncLocalStorage cwd
/// override for the whole AgentTool run (`AgentTool.tsx:916-917`
/// `cwdOverridePath ? runWithCwdOverride(cwdOverridePath, fn) : fn()`, applied
/// at `:1000` around the async lifecycle and `:1065` around the sync path; also
/// `resumeAgent.ts:228` for the resumed-worktree path), yet `getOriginalCwd`
/// and `getSessionProjectDir` are bootstrap state that the override never
/// touches — so a worktree-isolated agent still writes under the ORIGINAL
/// project dir. Routing on the run cwd instead put the transcript under
/// `projects/<sanitized worktree>` while `writeAgentMetadata` and the resume
/// reader kept looking under the original, and resume failed with "No
/// transcript found for agent ID".
fn agent_artifacts_project_dir() -> PathBuf {
    crate::bootstrap::state::get_session_project_dir().unwrap_or_else(|| {
        get_project_dir(&crate::bootstrap::state::get_original_cwd().to_string_lossy())
    })
}

/// Maps to CC `utils/sessionStorage.ts` `getAgentTranscriptPath(...)`
/// (`:252-256`) once the project dir is resolved.
fn agent_transcript_path_in(project_dir: &Path, session_id: &str, agent_id: &str) -> PathBuf {
    let base = project_dir.join(session_id).join("subagents");
    let base = AGENT_TRANSCRIPT_SUBDIRS
        .read()
        .ok()
        .and_then(|subdirs| subdirs.get(agent_id).cloned())
        .map(|subdir| base.join(subdir))
        .unwrap_or(base);
    base.join(format!("agent-{agent_id}.jsonl"))
}

/// Maps to CC `utils/sessionStorage.ts` `getAgentTranscriptPath(...)` with
/// explicit project/session inputs for deterministic tests.
///
/// CC has no such overload — `getAgentTranscriptPath` is always the active
/// session's. Only use this for a session that is provably not the active one
/// (otherwise it bypasses [`agent_artifacts_project_dir`] and re-opens the
/// split-root bug).
pub fn get_agent_transcript_path_for_session(
    project_path: &str,
    session_id: &str,
    agent_id: &str,
) -> PathBuf {
    agent_transcript_path_in(&get_project_dir(project_path), session_id, agent_id)
}

/// Maps to CC `utils/sessionStorage.ts` `getAgentTranscriptPath(agentId)`
/// (`:247-257`) using the active bootstrap session id.
pub fn get_agent_transcript_path(agent_id: &str) -> PathBuf {
    agent_transcript_path_in(
        &agent_artifacts_project_dir(),
        &crate::bootstrap::state::get_session_id(),
        agent_id,
    )
}

fn get_agent_metadata_path_for_session(
    project_path: &str,
    session_id: &str,
    agent_id: &str,
) -> PathBuf {
    get_agent_transcript_path_for_session(project_path, session_id, agent_id)
        .with_extension("meta.json")
}

fn get_agent_metadata_path(agent_id: &str) -> PathBuf {
    get_agent_transcript_path(agent_id).with_extension("meta.json")
}

/// Maps to CC `utils/sessionStorage.ts` `AgentMetadata`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMetadata {
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Maps to CC `utils/sessionStorage.ts` `writeAgentMetadata(...)`.
///
/// Writes the same `agent-<id>.meta.json` sidecar when session persistence is
/// enabled by the repository policy.
pub fn write_agent_metadata_for_session(
    project_path: &str,
    session_id: &str,
    agent_id: &str,
    metadata: &AgentMetadata,
) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        tracing::info!("[READ_ONLY] write_agent_metadata: session writes are disabled");
        return Ok(());
    }
    let path = get_agent_metadata_path_for_session(project_path, session_id, agent_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(metadata)?)?;
    Ok(())
}

pub fn write_agent_metadata(agent_id: &str, metadata: &AgentMetadata) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        tracing::info!("[READ_ONLY] write_agent_metadata: session writes are disabled");
        return Ok(());
    }
    let path = get_agent_metadata_path(agent_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(metadata)?)?;
    Ok(())
}

/// Maps to CC `utils/sessionStorage.ts` `readAgentMetadata(...)`.
pub fn read_agent_metadata_for_session(
    project_path: &str,
    session_id: &str,
    agent_id: &str,
) -> anyhow::Result<Option<AgentMetadata>> {
    let path = get_agent_metadata_path_for_session(project_path, session_id, agent_id);
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_str(&raw)?))
}

pub fn read_agent_metadata(agent_id: &str) -> anyhow::Result<Option<AgentMetadata>> {
    let path = get_agent_metadata_path(agent_id);
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_str(&raw)?))
}

/// Maps to CC `utils/sessionStorage.ts#getAgentTranscript` return shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTranscript {
    pub messages: Vec<crate::types::message::Message>,
    pub content_replacements: Vec<serde_json::Value>,
}

/// Maps to CC `utils/sessionStorage.ts#getAgentTranscript` with explicit
/// project/session inputs for deterministic tests.
pub fn get_agent_transcript_for_session(
    project_path: &str,
    session_id: &str,
    agent_id: &str,
) -> Option<AgentTranscript> {
    let path = get_agent_transcript_path_for_session(project_path, session_id, agent_id);
    get_agent_transcript_from_path(&path, agent_id)
}

/// Maps to CC `utils/sessionStorage.ts#getAgentTranscript`.
pub fn get_agent_transcript(agent_id: &str) -> Option<AgentTranscript> {
    let path = get_agent_transcript_path(agent_id);
    get_agent_transcript_from_path(&path, agent_id)
}

fn get_agent_transcript_from_path(path: &Path, agent_id: &str) -> Option<AgentTranscript> {
    let session = load_session_structured_from_path(path);
    let agent_message_uuids = session
        .message_order
        .iter()
        .filter(|uuid| {
            session.messages.get(*uuid).is_some_and(|message| {
                message.get("agentId").and_then(|value| value.as_str()) == Some(agent_id)
                    && message
                        .get("isSidechain")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if agent_message_uuids.is_empty() {
        return None;
    }
    let parent_uuids = agent_message_uuids
        .iter()
        .filter_map(|uuid| session.messages.get(uuid))
        .filter_map(|message| message.get("parentUuid").and_then(|value| value.as_str()))
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let leaf_uuid = agent_message_uuids
        .iter()
        .rev()
        .find(|uuid| !parent_uuids.contains(*uuid))
        .cloned()?;
    let messages = build_conversation_chain(&session, &leaf_uuid)
        .into_iter()
        .filter(|message| message.get("agentId").and_then(|value| value.as_str()) == Some(agent_id))
        .filter_map(agent_transcript_entry_to_message)
        .collect::<Vec<_>>();
    if messages.is_empty() {
        return None;
    }
    Some(AgentTranscript {
        messages,
        content_replacements: session
            .agent_content_replacements
            .get(agent_id)
            .cloned()
            .unwrap_or_default(),
    })
}

fn agent_transcript_entry_to_message(
    entry: serde_json::Value,
) -> Option<crate::types::message::Message> {
    match entry.get("type").and_then(|value| value.as_str())? {
        "user" => {
            let mut content = transcript_user_content_blocks(
                entry
                    .pointer("/message/content")
                    .unwrap_or(&serde_json::Value::Null),
                entry.get("isMeta").and_then(|value| value.as_bool()) == Some(true),
            );
            if let Some(tool_use_result) = entry.get("toolUseResult").cloned() {
                if let Some(crate::types::message::UserContent::ToolResult(result)) =
                    content.iter_mut().find(|content| {
                        matches!(content, crate::types::message::UserContent::ToolResult(_))
                    })
                {
                    result.tool_use_result = Some(tool_use_result);
                }
            }
            Some(crate::types::message::Message::User(
                crate::types::message::UserMessage {
                    uuid: transcript_entry_uuid(&entry),
                    timestamp: transcript_entry_timestamp(&entry),
                    content,
                    is_compact_summary: entry
                        .get("isCompactSummary")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    plan_content: None,
                    image_paste_ids: None,
                    // CC persists the whole envelope (`types/message.ts:50-62`);
                    // read the real values from the transcript entry.
                    is_visible_in_transcript_only: entry
                        .get("isVisibleInTranscriptOnly")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    mcp_meta: entry.get("mcpMeta").cloned(),
                    source_tool_assistant_uuid: entry
                        .get("sourceToolAssistantUUID")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    permission_mode: entry
                        .get("permissionMode")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    origin: entry.get("origin").cloned(),
                    summarize_metadata: entry
                        .get("summarizeMetadata")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok()),
                },
            ))
        }
        "assistant" => {
            let mut content = transcript_assistant_content_blocks(
                entry
                    .pointer("/message/content")
                    .unwrap_or(&serde_json::Value::Null),
            );
            content.push(crate::types::message::AssistantContent::MessageIdentity(
                crate::types::message::AssistantMessageIdentity {
                    // No uuid here: the row's uuid is the envelope, set on
                    // `AssistantMessage.uuid` below via `transcript_entry_uuid`.
                    // CC spreads one uuid per message (`sessionStorage.ts:1048`).
                    request_id: entry
                        .get("requestId")
                        .or_else(|| entry.get("request_id"))
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    api_message_id: entry
                        .pointer("/message/id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    // CC persists these on the envelope (`types/message.ts:41-44`).
                    is_api_error_message: entry
                        .get("isApiErrorMessage")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    api_error: entry.get("apiError").cloned(),
                    error_details: entry
                        .get("errorDetails")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                },
            ));
            Some(crate::types::message::Message::Assistant(
                crate::types::message::AssistantMessage {
                    uuid: transcript_entry_uuid(&entry),
                    timestamp: transcript_entry_timestamp(&entry),
                    content,
                    model: entry
                        .pointer("/message/model")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    stop_reason: entry
                        .pointer("/message/stop_reason")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok()),
                    usage: entry
                        .pointer("/message/usage")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok()),
                },
            ))
        }
        "system" => Some(crate::types::message::Message::System(
            // Shared wire→union adapter (batch D1): subtype and per-subtype
            // fields survive the round trip instead of collapsing to a text
            // row.
            crate::utils::conversation_recovery::system_message_from_entry(
                &entry,
                transcript_entry_uuid(&entry),
                transcript_entry_timestamp(&entry),
            ),
        )),
        "hook_result" => Some(crate::types::message::Message::HookResult(
            crate::types::message::HookResultMessage {
                message_type: "hook_result".to_string(),
                uuid: entry
                    .get("uuid")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                timestamp: transcript_entry_timestamp(&entry),
                attachment: entry.get("attachment")?.clone(),
            },
        )),
        "attachment" => {
            let attachment_type = entry
                .pointer("/attachment/type")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            if attachment_type.starts_with("hook_") {
                Some(crate::types::message::Message::HookResult(
                    crate::types::message::HookResultMessage {
                        message_type: "attachment".to_string(),
                        uuid: entry
                            .get("uuid")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        timestamp: transcript_entry_timestamp(&entry),
                        attachment: entry.get("attachment")?.clone(),
                    },
                ))
            } else {
                Some(crate::types::message::Message::Attachment(
                    // Typed at the seam (batch D2); unrecognized payloads ride
                    // `Attachment::Unknown`, and the verbatim wire form is kept
                    // so fields the typed enum does not declare survive a
                    // round-trip the way CC's plain JS object does.
                    crate::types::message::AttachmentMessage::from_wire_payload(
                        transcript_entry_uuid(&entry),
                        transcript_entry_timestamp(&entry),
                        entry.get("attachment")?.clone(),
                    ),
                ))
            }
        }
        _ => None,
    }
}

/// Real identity from the cold session entry; mint only when absent (C3a).
fn transcript_entry_uuid(entry: &serde_json::Value) -> String {
    entry
        .get("uuid")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn transcript_entry_timestamp(entry: &serde_json::Value) -> chrono::DateTime<chrono::Utc> {
    entry
        .get("timestamp")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(chrono::Utc::now)
}

fn transcript_user_content_blocks(
    value: &serde_json::Value,
    is_meta: bool,
) -> Vec<crate::types::message::UserContent> {
    let blocks = value.as_array().map(Vec::as_slice).unwrap_or(&[]);
    blocks
        .iter()
        .filter_map(
            |block| match block.get("type").and_then(|value| value.as_str()) {
                Some("text") => {
                    let text = block
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    Some(if is_meta {
                        crate::types::message::UserContent::MetaText(text)
                    } else {
                        crate::types::message::UserContent::Text(text)
                    })
                }
                Some("image") => Some(crate::types::message::UserContent::from_image_block(block.clone(), is_meta)),
                Some("document") => {
                    let media_type = block
                        .pointer("/source/media_type")
                        .and_then(|value| value.as_str())
                        .unwrap_or("application/pdf")
                        .to_string();
                    let data = block
                        .pointer("/source/data")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    Some(if is_meta {
                        crate::types::message::UserContent::MetaDocument { media_type, data }
                    } else {
                        crate::types::message::UserContent::Document { media_type, data }
                    })
                }
                Some("tool_result") => Some(crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id: crate::types::ids::ToolUseId(
                            block
                                .get("tool_use_id")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        content: block
                            .get("content")
                            .and_then(|value| value.as_str().map(str::to_string))
                            .unwrap_or_default(),
                        is_error: block
                            .get("is_error")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false),
                        content_blocks: block
                            .get("content")
                            .and_then(serde_json::Value::as_array)
                            .map(|blocks| {
                                blocks
                                    .iter()
                                    .map(crate::types::message::ToolResultContentBlock::from_structured_value)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        tool_use_result: None,
                    },
                )),
                _ => None,
            },
        )
        .collect()
}

fn transcript_assistant_content_blocks(
    value: &serde_json::Value,
) -> Vec<crate::types::message::AssistantContent> {
    let blocks = value.as_array().map(Vec::as_slice).unwrap_or(&[]);
    blocks
        .iter()
        .filter_map(
            |block| match block.get("type").and_then(|value| value.as_str()) {
                Some("text") => Some(crate::types::message::AssistantContent::Text(
                    block
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                )),
                Some("thinking") => Some(crate::types::message::AssistantContent::Thinking {
                    text: block
                        .get("thinking")
                        .or_else(|| block.get("text"))
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    signature: block
                        .get("signature")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                Some("redacted_thinking") => {
                    Some(crate::types::message::AssistantContent::RedactedThinking {
                        data: block
                            .get("data")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                }
                Some("tool_use") => Some(crate::types::message::AssistantContent::ToolUse(
                    transcript_tool_use_block(block),
                )),
                Some("server_tool_use") => {
                    Some(crate::types::message::AssistantContent::ServerToolUse(
                        transcript_tool_use_block(block),
                    ))
                }
                Some("web_search_tool_result") => Some(
                    crate::types::message::AssistantContent::WebSearchToolResult {
                        tool_use_id: crate::types::ids::ToolUseId(
                            block
                                .get("tool_use_id")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        content: block
                            .get("content")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    },
                ),
                _ => None,
            },
        )
        .collect()
}

fn transcript_tool_use_block(block: &serde_json::Value) -> crate::types::message::ToolUseBlock {
    crate::types::message::ToolUseBlock {
        id: crate::types::ids::ToolUseId(
            block
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        name: block
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        input: block
            .get("input")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    }
}

// Lite session list model.

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub display: String,
    pub custom_title: Option<String>,
    pub summary: Option<String>,
    pub tag: Option<String>,
    /// Agent display name — maps to LogOption.agentName.
    pub agent_name: Option<String>,
    /// Agent setting — maps to LogOption.agentSetting.
    pub agent_setting: Option<String>,
    pub git_branch: Option<String>,
    pub project_path: Option<String>,
    pub file_path: PathBuf,
    pub is_sidechain: bool,
    pub team_name: Option<String>,
    /// PR number — maps to LogOption.prNumber.
    pub pr_number: Option<u64>,
    /// PR URL — maps to LogOption.prUrl.
    pub pr_url: Option<String>,
    /// PR repository — maps to LogOption.prRepository.
    pub pr_repository: Option<String>,
    pub file_size: u64,
    pub modified: std::time::SystemTime,
}

impl Default for SessionSummary {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            display: String::new(),
            custom_title: None,
            summary: None,
            tag: None,
            agent_name: None,
            agent_setting: None,
            git_branch: None,
            project_path: None,
            file_path: PathBuf::new(),
            is_sidechain: false,
            team_name: None,
            pr_number: None,
            pr_url: None,
            pr_repository: None,
            file_size: 0,
            modified: std::time::SystemTime::UNIX_EPOCH,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSelection {
    pub session_id: String,
    pub project_path: Option<String>,
    pub file_path: PathBuf,
}

impl From<&SessionSummary> for SessionSelection {
    fn from(summary: &SessionSummary) -> Self {
        Self {
            session_id: summary.session_id.clone(),
            project_path: summary.project_path.clone(),
            file_path: summary.file_path.clone(),
        }
    }
}

const INITIAL_ENRICH_COUNT: usize = 50;

/// Maps to: CC `utils/sessionStorage.ts::SessionLogResult`.
#[derive(Clone, Debug, Default)]
pub struct SessionLogResult {
    pub logs: Vec<SessionSummary>,
    pub all_stat_logs: Vec<SessionSummary>,
    pub next_index: usize,
}

/// Maps to: CC `enrichLogs(...)` return value.
#[derive(Clone, Debug, Default)]
pub struct EnrichLogsResult {
    pub logs: Vec<SessionSummary>,
    pub next_index: usize,
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Read-only synchronous projection of CC
/// `loadSameRepoMessageLogs(worktreePaths)`.
pub fn load_same_repo_message_logs(worktree_paths: &[String]) -> Vec<SessionSummary> {
    // CC `getStatOnlyLogsForWorktrees` (sessionStorage.ts:4113-4125) anchors
    // the current project on `getOriginalCwd()`, never on `worktreePaths[0]`:
    // a cwd nested inside a repo lists its own project dir, not the root's.
    let current_project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    list_same_repo_sessions(&current_project_path, worktree_paths)
}

/// Read-only synchronous projection of CC `loadAllProjectsMessageLogs()`.
pub fn load_all_projects_message_logs() -> Vec<SessionSummary> {
    list_all_project_sessions()
}

/// Maps to: CC `loadSameRepoMessageLogsProgressive(worktreePaths)`.
pub fn load_same_repo_message_logs_progressive(worktree_paths: &[String]) -> SessionLogResult {
    // Same cwd anchor as `load_same_repo_message_logs` above.
    let current_project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    let all_stat_logs = list_same_repo_stat_sessions(&current_project_path, worktree_paths);
    let enriched = enrich_logs(&all_stat_logs, 0, INITIAL_ENRICH_COUNT);
    SessionLogResult {
        logs: enriched.logs,
        all_stat_logs,
        next_index: enriched.next_index,
    }
}

/// Maps to: CC `loadAllProjectsMessageLogsProgressive()`.
pub fn load_all_projects_message_logs_progressive() -> SessionLogResult {
    let all_stat_logs = list_all_project_stat_sessions();
    let enriched = enrich_logs(&all_stat_logs, 0, INITIAL_ENRICH_COUNT);
    SessionLogResult {
        logs: enriched.logs,
        all_stat_logs,
        next_index: enriched.next_index,
    }
}

/// Maps to: CC `enrichLogs(allLogs, startIndex, count)`.
pub fn enrich_logs(
    all_logs: &[SessionSummary],
    start_index: usize,
    count: usize,
) -> EnrichLogsResult {
    let mut logs = Vec::new();
    let mut index = start_index.min(all_logs.len());
    while index < all_logs.len() && logs.len() < count {
        let log = all_logs[index].clone();
        index += 1;
        if let Some(enriched) = enrich_session_summary(log) {
            logs.push(enriched);
        }
    }
    EnrichLogsResult {
        logs,
        next_index: index,
    }
}

pub fn list_sessions(project_path: &str) -> Vec<SessionSummary> {
    let stat_logs = list_stat_sessions(project_path);
    enrich_logs(&stat_logs, 0, usize::MAX).logs
}

fn list_stat_sessions(project_path: &str) -> Vec<SessionSummary> {
    let dir = get_project_dir(project_path);
    let mut sessions = list_stat_sessions_in_project_dir(&dir, Some(project_path));
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

pub fn list_same_repo_sessions(
    current_project_path: &str,
    worktree_paths: &[String],
) -> Vec<SessionSummary> {
    let stat_logs = list_same_repo_stat_sessions(current_project_path, worktree_paths);
    enrich_logs(&stat_logs, 0, usize::MAX).logs
}

/// Rust representation of CC `searchSessionsByCustomTitle`'s optional options.
#[derive(Clone, Copy, Debug, Default)]
pub struct SearchSessionsByCustomTitleOptions {
    pub limit: Option<isize>,
    pub exact: bool,
}

/// Maps to: CC `utils/sessionStorage.ts#searchSessionsByCustomTitle`.
/// Reuses stat discovery and complete enrichment rather than the progressive
/// resume picker's first page. Synchronous I/O preserves the source ordering.
pub fn search_sessions_by_custom_title(
    query: &str,
    options: Option<SearchSessionsByCustomTitleOptions>,
) -> Vec<SessionSummary> {
    let options = options.unwrap_or_default();
    let cwd = crate::bootstrap::state::get_original_cwd()
        .to_string_lossy()
        .into_owned();
    let worktree_paths = crate::utils::get_worktree_paths::get_worktree_paths(&cwd);
    let all_stat_logs = list_same_repo_stat_sessions(&cwd, &worktree_paths);
    let logs = enrich_logs(&all_stat_logs, 0, all_stat_logs.len()).logs;
    // String.trim uses ECMAScript whitespace (FEFF included, NEL excluded).
    let normalize = |value: &str| {
        value
            .to_lowercase()
            .trim_matches(|ch: char| {
                matches!(ch, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' |
                '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' |
                '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
            })
            .to_string()
    };
    let normalized_query = normalize(query);
    let mut matching = Vec::<SessionSummary>::new();
    let mut by_id = HashMap::<String, usize>::new();
    for log in logs {
        let Some(title) = log.custom_title.as_deref().map(normalize) else {
            continue;
        };
        if title.is_empty()
            || log.session_id.is_empty()
            || !(if options.exact {
                title == normalized_query
            } else {
                title.contains(&normalized_query)
            })
        {
            continue;
        }
        if let Some(&index) = by_id.get(&log.session_id) {
            if log.modified > matching[index].modified {
                matching[index] = log;
            }
        } else {
            by_id.insert(log.session_id.clone(), matching.len());
            matching.push(log);
        }
    }
    matching.sort_by(|a, b| b.modified.cmp(&a.modified));
    if let Some(limit) = options.limit.filter(|limit| *limit != 0) {
        let end = if limit < 0 {
            matching.len().saturating_sub(limit.unsigned_abs())
        } else {
            limit as usize
        };
        matching.truncate(end);
    }
    matching
}

fn list_same_repo_stat_sessions(
    current_project_path: &str,
    worktree_paths: &[String],
) -> Vec<SessionSummary> {
    if worktree_paths.len() <= 1 {
        return list_stat_sessions(current_project_path);
    }

    let projects_dir = get_projects_dir();
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return list_stat_sessions(current_project_path);
    };

    let mut indexed = worktree_paths
        .iter()
        .map(|path| (path.clone(), sanitize_path(path)))
        .collect::<Vec<_>>();
    indexed.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let mut sessions = Vec::new();
    let mut seen_dirs = HashSet::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if seen_dirs.contains(&dir_name) {
            continue;
        }
        for (worktree_path, prefix) in &indexed {
            if dir_name == *prefix || dir_name.starts_with(&format!("{prefix}-")) {
                seen_dirs.insert(dir_name.clone());
                sessions.extend(list_stat_sessions_in_project_dir(
                    &entry.path(),
                    Some(worktree_path),
                ));
                break;
            }
        }
    }

    // CC returns the deduplicated matches as-is: an all-miss prefix scan is
    // an empty picker, not a fallback to the current project's directory.
    deduplicate_sessions_by_id(sessions)
}

pub fn list_all_project_sessions() -> Vec<SessionSummary> {
    let stat_logs = list_all_project_stat_sessions();
    enrich_logs(&stat_logs, 0, usize::MAX).logs
}

fn list_all_project_stat_sessions() -> Vec<SessionSummary> {
    let projects_dir = get_projects_dir();
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            sessions.extend(list_stat_sessions_in_project_dir(&entry.path(), None));
        }
    }
    deduplicate_sessions_by_id(sessions)
}

fn list_stat_sessions_in_project_dir(
    dir: &Path,
    project_path_override: Option<&str>,
) -> Vec<SessionSummary> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .filter_map(|entry| {
            let path = entry.path();
            let session_id = path.file_stem()?.to_str()?.to_string();
            let metadata = entry.metadata().ok()?;
            Some(SessionSummary {
                session_id,
                project_path: project_path_override.map(str::to_string),
                file_path: path,
                file_size: metadata.len(),
                modified: metadata.modified().ok()?,
                ..SessionSummary::default()
            })
        })
        .collect()
}

fn enrich_session_summary(mut summary: SessionSummary) -> Option<SessionSummary> {
    let lite = read_lite_metadata(&summary.file_path, summary.file_size);
    summary.display = lite.first_prompt;
    summary.custom_title = lite.custom_title;
    summary.summary = lite.summary;
    summary.tag = lite.tag;
    summary.agent_name = lite.agent_name;
    summary.agent_setting = lite.agent_setting;
    summary.git_branch = lite.git_branch;
    summary.project_path = lite.project_path.or(summary.project_path);
    summary.is_sidechain = lite.is_sidechain;
    summary.team_name = lite.team_name;
    summary.pr_number = lite.pr_number;
    summary.pr_url = lite.pr_url;
    summary.pr_repository = lite.pr_repository;
    // CC `enrichLog` order (sessionStorage.ts:5049-5069): placeholder first,
    // then the sidechain and team filters.
    if summary.display.is_empty() && summary.custom_title.is_none() {
        summary.display = "(session)".to_string();
    }
    if summary.is_sidechain {
        return None;
    }
    // CC: `if (enriched.teamName) return null` — team sessions never enter
    // the /resume picker (empty string is falsy there).
    if summary
        .team_name
        .as_deref()
        .is_some_and(|name| !name.is_empty())
    {
        return None;
    }
    Some(summary)
}

fn deduplicate_sessions_by_id(sessions: Vec<SessionSummary>) -> Vec<SessionSummary> {
    let mut by_id = HashMap::<String, SessionSummary>::new();
    for session in sessions {
        match by_id.get(&session.session_id) {
            Some(existing) if existing.modified >= session.modified => {}
            _ => {
                by_id.insert(session.session_id.clone(), session);
            }
        }
    }
    let mut sessions = by_id.into_values().collect::<Vec<_>>();
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

struct LiteMetadata {
    first_prompt: String,
    custom_title: Option<String>,
    summary: Option<String>,
    tag: Option<String>,
    agent_name: Option<String>,
    agent_setting: Option<String>,
    git_branch: Option<String>,
    project_path: Option<String>,
    is_sidechain: bool,
    team_name: Option<String>,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    pr_repository: Option<String>,
}

fn empty_lite_metadata() -> LiteMetadata {
    LiteMetadata {
        first_prompt: String::new(),
        custom_title: None,
        summary: None,
        tag: None,
        agent_name: None,
        agent_setting: None,
        git_branch: None,
        project_path: None,
        is_sidechain: false,
        team_name: None,
        pr_number: None,
        pr_url: None,
        pr_repository: None,
    }
}

/// Read the first and last fixed windows of a JSONL file, then extract the
/// same lite metadata that the official resume picker uses for LogOption rows.
fn read_lite_metadata(path: &Path, file_size: u64) -> LiteMetadata {
    let mut result = empty_lite_metadata();
    let Some((head, tail)) = read_head_and_tail_strings(path, file_size) else {
        return result;
    };

    result.is_sidechain =
        head.contains("\"isSidechain\":true") || head.contains("\"isSidechain\": true");
    result.project_path = extract_json_string_field(&head, "cwd");
    result.team_name = extract_json_string_field(&head, "teamName");
    result.agent_setting = extract_json_string_field(&head, "agentSetting");

    result.first_prompt = extract_last_json_string_field(&tail, "lastPrompt")
        .or_else(|| extract_first_prompt_from_chunk(&head))
        .or_else(|| extract_json_string_field_prefix(&head, "content", MAX_FIRST_PROMPT_CHARS))
        .or_else(|| extract_json_string_field_prefix(&head, "text", MAX_FIRST_PROMPT_CHARS))
        .unwrap_or_default();

    result.custom_title = extract_last_json_string_field(&tail, "customTitle")
        .or_else(|| extract_last_json_string_field(&head, "customTitle"))
        .or_else(|| extract_last_json_string_field(&tail, "aiTitle"))
        .or_else(|| extract_last_json_string_field(&head, "aiTitle"));
    result.summary = extract_last_json_string_field(&tail, "summary");
    result.tag = extract_last_json_string_field(&tail, "tag");
    result.git_branch = extract_last_json_string_field(&tail, "gitBranch")
        .or_else(|| extract_json_string_field(&head, "gitBranch"));
    result.pr_url = extract_last_json_string_field(&tail, "prUrl");
    result.pr_repository = extract_last_json_string_field(&tail, "prRepository");
    result.pr_number = extract_last_json_string_field(&tail, "prNumber")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or_else(|| extract_last_json_number_field(&tail, "prNumber"));

    if result.first_prompt.is_empty() && result.custom_title.is_none() {
        result.first_prompt = "(session)".to_string();
    }

    result
}

fn read_head_and_tail_strings(path: &Path, file_size: u64) -> Option<(String, String)> {
    if file_size == 0 {
        return None;
    }

    let mut file = std::fs::File::open(path).ok()?;
    let head_size = file_size.min(LITE_READ_BUF_SIZE) as usize;
    let mut head_buf = vec![0u8; head_size];
    file.read_exact(&mut head_buf).ok()?;
    let head = String::from_utf8_lossy(&head_buf).to_string();

    if file_size <= LITE_READ_BUF_SIZE {
        return Some((head.clone(), head));
    }

    let tail_offset = file_size - LITE_READ_BUF_SIZE;
    let mut tail_buf = vec![0u8; LITE_READ_BUF_SIZE as usize];
    file.seek(SeekFrom::Start(tail_offset)).ok()?;
    file.read_exact(&mut tail_buf).ok()?;
    let tail = String::from_utf8_lossy(&tail_buf).to_string();
    Some((head, tail))
}

fn extract_first_prompt_from_chunk(chunk: &str) -> Option<String> {
    let mut command_fallback: Option<String> = None;

    for line in chunk.lines() {
        if !line.contains("\"type\":\"user\"") && !line.contains("\"type\": \"user\"") {
            continue;
        }
        if line.contains("\"tool_result\"")
            || line.contains("\"isMeta\":true")
            || line.contains("\"isMeta\": true")
            || line.contains("\"isCompactSummary\":true")
            || line.contains("\"isCompactSummary\": true")
        {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(|value| value.as_str()) != Some("user") {
            continue;
        }

        let Some(content) = entry
            .get("message")
            .and_then(|message| message.get("content"))
        else {
            continue;
        };
        let texts = user_text_blocks(content);
        for text in texts {
            let normalized = text.replace('\n', " ").trim().to_string();
            if normalized.is_empty() {
                continue;
            }

            if let Some(command_name_tag) = crate::utils::messages::extract_tag(
                &normalized,
                crate::constants::xml::COMMAND_NAME_TAG,
            ) {
                let command_name = command_name_tag.trim_start_matches('/');
                let command_args = crate::utils::messages::extract_tag(
                    &normalized,
                    crate::constants::xml::COMMAND_ARGS_TAG,
                )
                .map(|args| args.trim().to_string())
                .unwrap_or_default();
                if crate::commands::built_in_command_names().contains(command_name)
                    || command_args.is_empty()
                {
                    command_fallback.get_or_insert(command_name_tag);
                    continue;
                }
                return Some(format!("{command_name_tag} {command_args}"));
            }

            if let Some(bash_input) = crate::utils::messages::extract_tag(
                &normalized,
                crate::constants::xml::BASH_INPUT_TAG,
            ) {
                return Some(format!("! {}", bash_input.trim()));
            }

            if should_skip_first_prompt_text(&normalized) {
                continue;
            }

            return Some(truncate_prompt_text(&normalized, MAX_FIRST_PROMPT_CHARS));
        }
    }

    command_fallback
}

fn user_text_blocks(content: &serde_json::Value) -> Vec<String> {
    if let Some(text) = content.as_str() {
        return vec![text.to_string()];
    }

    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(|value| value.as_str()) == Some("text"))
                .filter_map(|block| block.get("text").and_then(|value| value.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn should_skip_first_prompt_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.starts_with("[Request interrupted by user") {
        return true;
    }

    let Some(rest) = trimmed.strip_prefix('<') else {
        return false;
    };
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    for ch in chars {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            continue;
        }
        return ch == '>' || ch.is_whitespace();
    }
    false
}

fn extract_json_string_field(text: &str, key: &str) -> Option<String> {
    find_json_string_field_at_or_after(text, key, 0).map(|(_, value)| value)
}

fn extract_last_json_string_field(text: &str, key: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    let mut search_from = 0usize;
    while let Some((idx, value)) = find_json_string_field_at_or_after(text, key, search_from) {
        search_from = idx.saturating_add(1);
        if best.as_ref().is_none_or(|(best_idx, _)| idx >= *best_idx) {
            best = Some((idx, value));
        }
    }
    best.map(|(_, value)| value)
}

fn find_json_string_field_at_or_after(
    text: &str,
    key: &str,
    from: usize,
) -> Option<(usize, String)> {
    let patterns = [format!("\"{key}\":\""), format!("\"{key}\": \"")];
    let mut best: Option<(usize, usize)> = None;
    for pattern in &patterns {
        if let Some(relative_idx) = text.get(from..)?.find(pattern) {
            let idx = from + relative_idx;
            let value_start = idx + pattern.len();
            if best.as_ref().is_none_or(|(best_idx, _)| idx < *best_idx) {
                best = Some((idx, value_start));
            }
        }
    }

    let (idx, value_start) = best?;
    let value_end = json_string_end(text, value_start)?;
    let raw = &text[value_start..value_end];
    Some((idx, unescape_json_string(raw)))
}

fn json_string_end(text: &str, value_start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = value_start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i = i.saturating_add(2),
            b'\"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

fn unescape_json_string(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_string())
}

fn extract_json_string_field_prefix(text: &str, key: &str, max_chars: usize) -> Option<String> {
    let patterns = [format!("\"{key}\":\""), format!("\"{key}\": \"")];
    for pattern in &patterns {
        let Some(idx) = text.find(pattern) else {
            continue;
        };
        let mut raw = String::new();
        let mut chars = text[idx + pattern.len()..].chars();
        let mut collected = 0usize;
        while collected < max_chars {
            let Some(ch) = chars.next() else {
                break;
            };
            if ch == '\\' {
                raw.push(ch);
                if let Some(next) = chars.next() {
                    raw.push(next);
                }
                collected += 1;
                continue;
            }
            if ch == '"' {
                break;
            }
            raw.push(ch);
            collected += 1;
        }
        let value = unescape_json_string(&raw)
            .replace('\n', " ")
            .replace('\t', " ")
            .trim()
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn extract_last_json_number_field(text: &str, key: &str) -> Option<u64> {
    let patterns = [format!("\"{key}\":"), format!("\"{key}\": ")];
    let mut best_idx = None;
    let mut value_start = 0usize;
    for pattern in &patterns {
        let mut search_from = 0usize;
        while let Some(relative_idx) = text.get(search_from..)?.find(pattern) {
            let idx = search_from + relative_idx;
            if best_idx.is_none_or(|best| idx >= best) {
                best_idx = Some(idx);
                value_start = idx + pattern.len();
            }
            search_from = idx.saturating_add(1);
        }
    }

    let mut digits = String::new();
    for ch in text.get(value_start..)?.trim_start().chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            break;
        }
    }
    (!digits.is_empty())
        .then(|| digits.parse::<u64>().ok())
        .flatten()
}

fn truncate_prompt_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{}…", truncated.trim())
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Maps to: CC `Project.appendEntry(entry, sessionId)`.
fn append_entry(
    project_path: &str,
    session_id: &str,
    entry: &serde_json::Value,
) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        tracing::info!("[READ_ONLY] append_entry: session writes are disabled");
        return Ok(());
    }

    let path = if session_id == crate::bootstrap::state::get_session_id() {
        let mut meta = get_project()
            .current_session_meta
            .write()
            .map_err(|_| anyhow::anyhow!("session metadata lock poisoned"))?;
        let Some(path) = meta.session_file.clone() else {
            // Maps to CC `Project.pendingEntries`: metadata/attachments do not
            // create a session file before the first user/assistant message.
            meta.pending_entries.push(entry.clone());
            return Ok(());
        };
        path
    } else {
        let path = get_session_file_path(project_path, session_id);
        if !path.is_file() {
            crate::utils::debug::log_for_debugging(&format!(
                "append_entry: session file not found for other session {session_id}"
            ));
            return Ok(());
        }
        path
    };
    let entry_type = entry.get("type").and_then(serde_json::Value::as_str);
    let agent_id = entry.get("agentId").and_then(serde_json::Value::as_str);
    let path = if agent_id.is_some()
        && (entry_type == Some("content-replacement")
            || entry
                .get("isSidechain")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false))
    {
        let agent_id = agent_id.expect("checked above");
        // CC `Project.appendEntry` routes both flavours through the ZERO-ARG
        // `getAgentTranscriptPath(entry.agentId)` (`sessionStorage.ts:1205` for
        // `content-replacement`, `:1227` for agent sidechain messages), which
        // owns the project dir itself and never sees the caller's project path.
        // Same shape as `active_session_file_path` below: the CC-shaped owner
        // for the active session, the explicit pair only for the other-session
        // writes Rust supports and CC has no overload for.
        if session_id == crate::bootstrap::state::get_session_id() {
            get_agent_transcript_path(agent_id)
        } else {
            get_agent_transcript_path_for_session(project_path, session_id, agent_id)
        }
    } else {
        path
    };
    let is_agent_sidechain = agent_id.is_some()
        && entry
            .get("isSidechain")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    let message_set = is_transcript_message(entry).then(|| get_session_messages(session_id));
    let mut message_set = message_set
        .as_ref()
        .map(|set| set.write().unwrap_or_else(|error| error.into_inner()));
    let uuid = entry.get("uuid").and_then(serde_json::Value::as_str);
    if !is_agent_sidechain
        && let (Some(set), Some(uuid)) = (&message_set, uuid)
        && set.contains(uuid)
    {
        return Ok(());
    }
    // CC intentionally discards the per-entry promise here (`void enqueueWrite`);
    // `Project.flush()` remains the durability barrier.
    let _ = get_project().enqueue_write(path, entry)?;
    if !is_agent_sidechain && let (Some(set), Some(uuid)) = (&mut message_set, uuid) {
        set.insert(uuid.to_string());
    }
    Ok(())
}

fn active_session_file_path(project_path: &str, session_id: &str) -> PathBuf {
    if session_id == crate::bootstrap::state::get_session_id() {
        if let Some(path) = get_project()
            .current_session_meta
            .read()
            .ok()
            .and_then(|meta| meta.session_file.clone())
        {
            return path;
        }
        if let Some(project_dir) = crate::bootstrap::state::get_session_project_dir() {
            return project_dir.join(format!("{session_id}.jsonl"));
        }
    }
    get_session_file_path(project_path, session_id)
}

/// Maps to: CC's intentionally synchronous `appendEntryToFile()` used by
/// rename, materialization metadata, and cleanup metadata re-append.
fn append_entry_to_file(path: &Path, entry: &serde_json::Value) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        return Ok(());
    }
    // Serialize this synchronous CC boundary through the same actor as queued
    // writes. This preserves JS event-loop ordering without changing the
    // exported `appendEntryToFile` semantics into an asynchronous protocol.
    let (reply, response) = std::sync::mpsc::sync_channel(1);
    get_project()
        .sender
        .send(ProjectWriteCommand::AppendEntryToFile {
            path: path.to_path_buf(),
            entry: entry.clone(),
            reply,
        })
        .map_err(|_| anyhow::anyhow!("Project write queue stopped"))?;
    response
        .recv()
        .map_err(|_| anyhow::anyhow!("Project dropped appendEntryToFile response"))?
        .map_err(anyhow::Error::msg)
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Reads JSONL entries for resume, matching the official load path.
/// For large transcripts this uses a chunked fd reader before JSON parsing:
/// attribution snapshots are stripped at line-read time, compact boundaries
/// truncate the output buffer in-stream, and dead fork branches can be removed
/// before `serde_json` sees them. The 50 MB raw transcript limit is preserved.
fn read_transcript_entries(path: &Path) -> Vec<serde_json::Value> {
    let file_size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };

    if file_size > MAX_TRANSCRIPT_READ_BYTES {
        tracing::error!(
            "Session file {} is {} bytes (> 50MB limit), refusing to load",
            path.display(),
            file_size
        );
        return Vec::new();
    }

    if file_size > SKIP_PRECOMPACT_THRESHOLD
        && !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DISABLE_PRECOMPACT_SKIP")
                .as_deref(),
        )
    {
        return read_large_transcript_entries(path, file_size);
    }

    read_small_transcript_entries(path)
}

fn read_small_transcript_entries(path: &Path) -> Vec<serde_json::Value> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let effective_content = find_post_boundary_content(&content);
    parse_jsonl_str_lines(effective_content)
}

fn read_large_transcript_entries(path: &Path, file_size: u64) -> Vec<serde_json::Value> {
    let Some(scan) = read_transcript_for_load(path, file_size) else {
        return std::fs::read(path)
            .map(|bytes| parse_jsonl_bytes(&bytes))
            .unwrap_or_default();
    };

    let mut metadata_entries = Vec::new();
    if scan.boundary_start_offset > 0 {
        for line in scan_pre_boundary_metadata(path, scan.boundary_start_offset) {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) {
                metadata_entries.push(entry);
            }
        }
    }

    let mut bytes = scan.post_boundary_bytes;
    if !scan.has_preserved_segment && bytes.len() as u64 > SKIP_PRECOMPACT_THRESHOLD {
        bytes = walk_chain_before_parse(bytes);
    }

    metadata_entries.extend(parse_jsonl_bytes(&bytes));
    metadata_entries
}

#[derive(Debug)]
struct TranscriptLoadScan {
    boundary_start_offset: u64,
    post_boundary_bytes: Vec<u8>,
    has_preserved_segment: bool,
}

const ATTR_SNAP_PREFIX: &[u8] = b"{\"type\":\"attribution-snapshot\"";
const LF_BYTE: u8 = b'\n';
const BOUNDARY_SEARCH_BOUND: usize = 256;

fn read_transcript_for_load(path: &Path, file_size: u64) -> Option<TranscriptLoadScan> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut chunk = vec![0u8; TRANSCRIPT_READ_CHUNK_SIZE];
    let mut out = Vec::with_capacity((file_size as usize).min(8 * 1024 * 1024));
    let mut line = Vec::new();
    let mut line_start_offset = 0u64;
    let mut file_offset = 0u64;
    let mut last_attr_snapshot: Option<Vec<u8>> = None;
    let mut boundary_start_offset = 0u64;
    let mut has_preserved_segment = false;

    while file_offset < file_size {
        let to_read = ((file_size - file_offset) as usize).min(TRANSCRIPT_READ_CHUNK_SIZE);
        let buf = &mut chunk[..to_read];
        let bytes_read = file.read(buf).ok()?;
        if bytes_read == 0 {
            break;
        }

        for (idx, byte) in buf[..bytes_read].iter().copied().enumerate() {
            line.push(byte);
            if byte == LF_BYTE {
                process_transcript_load_line(
                    &line,
                    line_start_offset,
                    &mut out,
                    &mut last_attr_snapshot,
                    &mut boundary_start_offset,
                    &mut has_preserved_segment,
                );
                line.clear();
                line_start_offset = file_offset + idx as u64 + 1;
            }
        }
        file_offset += bytes_read as u64;
    }

    if !line.is_empty() {
        process_transcript_load_line(
            &line,
            line_start_offset,
            &mut out,
            &mut last_attr_snapshot,
            &mut boundary_start_offset,
            &mut has_preserved_segment,
        );
    }

    if let Some(snapshot) = last_attr_snapshot {
        if !out.is_empty() && out.last() != Some(&LF_BYTE) {
            out.push(LF_BYTE);
        }
        out.extend_from_slice(&snapshot);
    }

    Some(TranscriptLoadScan {
        boundary_start_offset,
        post_boundary_bytes: out,
        has_preserved_segment,
    })
}

fn process_transcript_load_line(
    line: &[u8],
    line_start_offset: u64,
    out: &mut Vec<u8>,
    last_attr_snapshot: &mut Option<Vec<u8>>,
    boundary_start_offset: &mut u64,
    has_preserved_segment: &mut bool,
) {
    if line.starts_with(ATTR_SNAP_PREFIX) {
        *last_attr_snapshot = Some(line.to_vec());
        return;
    }

    if compact_boundary_marker_is_in_search_bound(line) {
        if let Some(line_has_preserved_segment) = parse_compact_boundary_line(line) {
            if line_has_preserved_segment {
                *has_preserved_segment = true;
            } else {
                out.clear();
                *boundary_start_offset = line_start_offset;
                *has_preserved_segment = false;
                *last_attr_snapshot = None;
            }
        }
    }

    out.extend_from_slice(line);
}

fn compact_boundary_marker_is_in_search_bound(line: &[u8]) -> bool {
    let search_len = line.len().min(BOUNDARY_SEARCH_BOUND);
    contains_bytes(&line[..search_len], COMPACT_BOUNDARY_MARKER)
}

fn parse_compact_boundary_line(line: &[u8]) -> Option<bool> {
    let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
    let entry = serde_json::from_slice::<serde_json::Value>(trimmed).ok()?;
    is_compact_boundary(&entry).then(|| extract_preserved_segment(&entry).is_some())
}

fn parse_jsonl_str_lines(content: &str) -> Vec<serde_json::Value> {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn parse_jsonl_bytes(bytes: &[u8]) -> Vec<serde_json::Value> {
    bytes
        .split(|byte| *byte == LF_BYTE)
        .filter(|line| !line.iter().all(|byte| byte.is_ascii_whitespace()))
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .collect()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle, 0).is_some()
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|pos| from + pos)
}

#[derive(Clone, Debug)]
struct MessageLineIndex {
    start: usize,
    end: usize,
    parent_start: Option<usize>,
}

fn walk_chain_before_parse(buf: Vec<u8>) -> Vec<u8> {
    const PARENT_PREFIX: &[u8] = b"{\"parentUuid\":";
    const UUID_KEY: &[u8] = b"\"uuid\":\"";
    const SIDECHAIN_TRUE: &[u8] = b"\"isSidechain\":true";
    const UUID_LEN: usize = 36;
    const TS_SUFFIX: &[u8] = b"\",\"timestamp\":\"";

    let mut msg_idx: Vec<MessageLineIndex> = Vec::new();
    let mut meta_ranges: Vec<(usize, usize)> = Vec::new();
    let mut uuid_to_slot: HashMap<String, usize> = HashMap::new();

    let mut pos = 0usize;
    while pos < buf.len() {
        let line_end = match buf[pos..].iter().position(|byte| *byte == LF_BYTE) {
            Some(nl) => pos + nl + 1,
            None => buf.len(),
        };
        if line_end > pos + PARENT_PREFIX.len() && buf[pos..].starts_with(PARENT_PREFIX) {
            let parent_start = (buf.get(pos + PARENT_PREFIX.len()) == Some(&b'\"'))
                .then_some(pos + PARENT_PREFIX.len() + 1);
            if let Some(uuid_start) = top_level_uuid_start(&buf, pos, line_end, UUID_KEY, TS_SUFFIX)
            {
                if uuid_start + UUID_LEN <= line_end {
                    let uuid = String::from_utf8_lossy(&buf[uuid_start..uuid_start + UUID_LEN])
                        .to_string();
                    uuid_to_slot.insert(uuid, msg_idx.len());
                    msg_idx.push(MessageLineIndex {
                        start: pos,
                        end: line_end,
                        parent_start,
                    });
                } else {
                    meta_ranges.push((pos, line_end));
                }
            } else {
                meta_ranges.push((pos, line_end));
            }
        } else {
            meta_ranges.push((pos, line_end));
        }
        pos = line_end;
    }

    let Some(mut slot) = msg_idx
        .iter()
        .rposition(|line| !contains_bytes(&buf[line.start..line.end], SIDECHAIN_TRUE))
    else {
        return buf;
    };

    let mut seen_slots = HashSet::new();
    let mut chain_starts = HashSet::new();
    let mut chain_bytes = 0usize;
    loop {
        if !seen_slots.insert(slot) {
            break;
        }
        let line = &msg_idx[slot];
        chain_starts.insert(line.start);
        chain_bytes += line.end.saturating_sub(line.start);
        let Some(parent_start) = line.parent_start else {
            break;
        };
        if parent_start + UUID_LEN > buf.len() {
            break;
        }
        let parent =
            String::from_utf8_lossy(&buf[parent_start..parent_start + UUID_LEN]).to_string();
        let Some(parent_slot) = uuid_to_slot.get(&parent).copied() else {
            break;
        };
        slot = parent_slot;
    }

    if buf.len().saturating_sub(chain_bytes) < (buf.len() >> 1) {
        return buf;
    }

    let mut out =
        Vec::with_capacity(chain_bytes + meta_ranges.iter().map(|(s, e)| e - s).sum::<usize>());
    let mut meta_idx = 0usize;
    for line in &msg_idx {
        while meta_idx < meta_ranges.len() && meta_ranges[meta_idx].0 < line.start {
            let (start, end) = meta_ranges[meta_idx];
            out.extend_from_slice(&buf[start..end]);
            meta_idx += 1;
        }
        if chain_starts.contains(&line.start) {
            out.extend_from_slice(&buf[line.start..line.end]);
        }
    }
    while meta_idx < meta_ranges.len() {
        let (start, end) = meta_ranges[meta_idx];
        out.extend_from_slice(&buf[start..end]);
        meta_idx += 1;
    }
    out
}

fn top_level_uuid_start(
    buf: &[u8],
    line_start: usize,
    line_end: usize,
    uuid_key: &[u8],
    ts_suffix: &[u8],
) -> Option<usize> {
    const UUID_LEN: usize = 36;
    let mut first_any = None;
    let mut suffix_candidates = Vec::new();
    let mut from = line_start;

    while let Some(next) = find_bytes(&buf[..line_end], uuid_key, from) {
        first_any.get_or_insert(next);
        let after = next + uuid_key.len() + UUID_LEN;
        if after + ts_suffix.len() <= line_end && &buf[after..after + ts_suffix.len()] == ts_suffix
        {
            suffix_candidates.push(next);
        }
        from = next + uuid_key.len();
    }

    let key_pos = match suffix_candidates.len() {
        0 => first_any?,
        1 => suffix_candidates[0],
        _ => pick_depth_one_uuid_candidate(buf, line_start, &suffix_candidates),
    };
    Some(key_pos + uuid_key.len())
}

fn pick_depth_one_uuid_candidate(buf: &[u8], line_start: usize, candidates: &[usize]) -> usize {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    let mut candidate_index = 0usize;
    let mut i = line_start;

    while candidate_index < candidates.len() && i < buf.len() {
        if i == candidates[candidate_index] {
            if depth == 1 && !in_string {
                return candidates[candidate_index];
            }
            candidate_index += 1;
        }

        match buf[i] {
            b'\\' if in_string && !escape_next => escape_next = true,
            b'\"' if !escape_next => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => depth -= 1,
            _ => escape_next = false,
        }
        if !matches!(buf[i], b'\\') {
            escape_next = false;
        }
        i += 1;
    }

    *candidates.last().expect("candidate list is non-empty")
}

/// For small files, keep the legacy line-level compact-boundary filter.
fn find_post_boundary_content(content: &str) -> &str {
    let mut last_boundary_end: Option<usize> = None;
    let mut byte_offset = 0;

    for line in content.lines() {
        let line_end = byte_offset + line.len();
        let next_start = if line_end < content.len() {
            line_end + 1
        } else {
            line_end
        };

        if line.contains("compact_boundary") {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                if is_compact_boundary(&entry) && extract_preserved_segment(&entry).is_none() {
                    last_boundary_end = Some(next_start);
                }
            }
        }
        byte_offset = next_start;
    }

    match last_boundary_end {
        Some(offset) if offset < content.len() => &content[offset..],
        _ => content,
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub fn load_session_raw(project_path: &str, session_id: &str) -> Vec<serde_json::Value> {
    let path = get_session_file_path(project_path, session_id);
    read_transcript_entries(&path)
}

pub fn load_session_raw_from_path(path: &Path) -> Vec<serde_json::Value> {
    read_transcript_entries(path)
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub fn load_session_structured(project_path: &str, session_id: &str) -> LoadedSession {
    let path = get_session_file_path(project_path, session_id);
    load_session_structured_from_path(&path)
}

pub fn load_session_structured_from_path(path: &Path) -> LoadedSession {
    let entries = read_transcript_entries(path);
    let mut session = LoadedSession::default();

    let mut progress_bridge: HashMap<String, Option<String>> = HashMap::new();

    // Consuming iterator: move entries into session maps without cloning.
    // Maps to: CC `messages.set(entry.uuid, entry)` — JS moves the object
    // reference into the Map, no copy. Same ownership transfer here.
    for mut entry in entries {
        // Extract lightweight fields before the heavy Value is moved.
        let entry_type = match entry.get("type").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => continue,
        };

        // Bridge legacy progress entries out of old parentUuid chains.
        if entry_type == "progress" {
            if let Some(uuid) = entry.get("uuid").and_then(|v| v.as_str()) {
                let parent = entry
                    .get("parentUuid")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let resolved_parent = parent
                    .as_ref()
                    .and_then(|parent_uuid| progress_bridge.get(parent_uuid).cloned())
                    .unwrap_or(parent);
                progress_bridge.insert(uuid.to_string(), resolved_parent);
            }
            continue;
        }

        if matches!(
            entry_type.as_str(),
            "user" | "assistant" | "attachment" | "system"
        ) {
            let bridged_parent = entry
                .get("parentUuid")
                .and_then(|v| v.as_str())
                .and_then(|parent_uuid| progress_bridge.get(parent_uuid).cloned());
            if let Some(parent) = bridged_parent {
                if let Some(object) = entry.as_object_mut() {
                    object.insert(
                        "parentUuid".to_string(),
                        parent
                            .map(serde_json::Value::String)
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
        }

        let session_id_val = entry
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match entry_type.as_str() {
            // Transcript messages → messages map
            "user" | "assistant" | "attachment" => {
                if let Some(uuid) = entry.get("uuid").and_then(|v| v.as_str()) {
                    let uuid_owned = uuid.to_string();
                    session.message_order.push(uuid_owned.clone());
                    session.messages.insert(uuid_owned, entry);
                }
            }

            "system" => {
                let subtype = entry
                    .get("subtype")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if subtype == SUBTYPE_COMPACT_BOUNDARY {
                    // Clear collapse state on boundary
                    session.context_collapse_commits.clear();
                    session.context_collapse_snapshot = None;

                    let preserved_segment = extract_preserved_segment(&entry);
                    session.compact_boundaries.push(CompactBoundaryInfo {
                        entry_index: session.message_order.len(),
                        preserved_segment,
                        raw: entry,
                    });
                } else {
                    // Regular system message
                    if let Some(uuid) = entry.get("uuid").and_then(|v| v.as_str()) {
                        let uuid_owned = uuid.to_string();
                        session.message_order.push(uuid_owned.clone());
                        session.messages.insert(uuid_owned, entry);
                    }
                }
            }

            // ── Metadata: summary ──
            "summary" => {
                if let (Some(leaf_uuid), Some(summary)) = (
                    entry.get("leafUuid").and_then(|v| v.as_str()),
                    entry.get("summary").and_then(|v| v.as_str()),
                ) {
                    session
                        .summaries
                        .insert(leaf_uuid.to_string(), summary.to_string());
                }
            }

            // ── Metadata: custom-title (last-wins) ──
            "custom-title" => {
                if let Some(title) = entry.get("customTitle").and_then(|v| v.as_str()) {
                    session
                        .custom_titles
                        .insert(session_id_val, title.to_string());
                }
            }

            // ── Metadata: ai-title (last-wins, ephemeral) ──
            "ai-title" => {
                if let Some(title) = entry.get("aiTitle").and_then(|v| v.as_str()) {
                    session.ai_titles.insert(session_id_val, title.to_string());
                }
            }

            // ── Metadata: tag (last-wins) ──
            "tag" => {
                if let Some(tag) = entry.get("tag").and_then(|v| v.as_str()) {
                    session.tags.insert(session_id_val, tag.to_string());
                }
            }

            // ── Metadata: last-prompt (last-wins) ──
            "last-prompt" => {
                if let Some(prompt) = entry.get("lastPrompt").and_then(|v| v.as_str()) {
                    session
                        .last_prompts
                        .insert(session_id_val, prompt.to_string());
                }
            }

            // ── Metadata: task-summary (last-wins) ──
            "task-summary" => {
                if let Some(summary) = entry.get("summary").and_then(|v| v.as_str()) {
                    session
                        .task_summaries
                        .insert(session_id_val, summary.to_string());
                }
            }

            // ── Metadata: agent-name / agent-color / agent-setting (last-wins) ──
            "agent-name" => {
                if let Some(v) = entry.get("agentName").and_then(|v| v.as_str()) {
                    session.agent_names.insert(session_id_val, v.to_string());
                }
            }
            "agent-color" => {
                if let Some(v) = entry.get("agentColor").and_then(|v| v.as_str()) {
                    session.agent_colors.insert(session_id_val, v.to_string());
                }
            }
            "agent-setting" => {
                if let Some(v) = entry.get("agentSetting").and_then(|v| v.as_str()) {
                    session.agent_settings.insert(session_id_val, v.to_string());
                }
            }

            // ── Metadata: mode (last-wins) ──
            "mode" => {
                if let Some(mode) = entry.get("mode").and_then(|v| v.as_str()) {
                    session.modes.insert(session_id_val, mode.to_string());
                }
            }

            // ── Metadata: worktree-state (last-wins) ──
            "worktree-state" => {
                if let Some(state) = entry.get("worktreeSession") {
                    session
                        .worktree_states
                        .insert(session_id_val, state.clone());
                }
            }

            // ── Metadata: pr-link ──
            "pr-link" => {
                if let Some(pr_num) = entry.get("prNumber").and_then(|v| v.as_u64()) {
                    session.pr_numbers.insert(session_id_val.clone(), pr_num);
                }
                if let Some(pr_url) = entry.get("prUrl").and_then(|v| v.as_str()) {
                    session
                        .pr_urls
                        .insert(session_id_val.clone(), pr_url.to_string());
                }
                if let Some(pr_repo) = entry.get("prRepository").and_then(|v| v.as_str()) {
                    session
                        .pr_repositories
                        .insert(session_id_val, pr_repo.to_string());
                }
            }

            // ── Metadata: file-history-snapshot ──
            "file-history-snapshot" => {
                if let Some(msg_id) = entry.get("messageId").and_then(|v| v.as_str()) {
                    session
                        .file_history_snapshots
                        .insert(msg_id.to_string(), entry.clone());
                }
            }

            // ── Metadata: content-replacement (append) ──
            "content-replacement" => {
                let key = entry
                    .get("agentId")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(replacements) = entry.get("replacements") {
                    let target = match key {
                        Some(agent_id) => session
                            .agent_content_replacements
                            .entry(agent_id)
                            .or_default(),
                        None => session
                            .content_replacements
                            .entry(session_id_val)
                            .or_default(),
                    };
                    if let Some(items) = replacements.as_array() {
                        target.extend(items.iter().cloned());
                    } else {
                        target.push(replacements.clone());
                    }
                }
            }

            "marble-origami-commit" => {
                session.context_collapse_commits.push(entry.clone());
            }

            // ── Metadata: marble-origami-snapshot (last-wins) ──
            "marble-origami-snapshot" => {
                session.context_collapse_snapshot = Some(entry.clone());
            }

            "speculation-accept" => {
                session.speculation_accepts.push(entry.clone());
            }

            "attribution-snapshot" => {
                insert_attribution_snapshot(&mut session.attribution_snapshots, entry);
            }

            "queue-operation" => {}

            other => {
                tracing::debug!("Unknown JSONL entry type: {other}");
            }
        }
    }

    session.leaf_uuids = compute_leaf_uuids(&session.messages, &session.message_order);

    if !session.compact_boundaries.is_empty() {
        apply_preserved_segment_relinks(&mut session);
    }

    apply_snip_removals(&mut session);

    session
}

fn insert_attribution_snapshot(snapshots: &mut Vec<serde_json::Value>, entry: serde_json::Value) {
    let Some(message_id) = entry
        .get("messageId")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };

    if let Some(existing) = snapshots.iter_mut().find(|snapshot| {
        snapshot.get("messageId").and_then(|value| value.as_str()) == Some(message_id.as_str())
    }) {
        *existing = entry;
    } else {
        snapshots.push(entry);
    }
}

fn apply_snip_removals(session: &mut LoadedSession) {
    let mut to_delete: HashSet<String> = HashSet::new();
    for entry in session.messages.values() {
        collect_snip_removed_uuids(entry, &mut to_delete);
    }
    for boundary in &session.compact_boundaries {
        collect_snip_removed_uuids(&boundary.raw, &mut to_delete);
    }
    if to_delete.is_empty() {
        return;
    }

    let mut deleted_parent: HashMap<String, Option<String>> = HashMap::new();
    for uuid in &to_delete {
        let Some(entry) = session.messages.remove(uuid) else {
            continue;
        };
        deleted_parent.insert(
            uuid.clone(),
            entry
                .get("parentUuid")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        );
    }

    let survivor_uuids = session.message_order.clone();
    for uuid in survivor_uuids {
        let parent = session
            .messages
            .get(&uuid)
            .and_then(|message| message.get("parentUuid"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let Some(parent_uuid) = parent else {
            continue;
        };
        if !to_delete.contains(&parent_uuid) {
            continue;
        }

        let resolved_parent = resolve_snip_parent(&parent_uuid, &to_delete, &mut deleted_parent);
        if let Some(message) = session.messages.get_mut(&uuid) {
            if let Some(object) = message.as_object_mut() {
                object.insert(
                    "parentUuid".to_string(),
                    resolved_parent
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                );
            }
        }
    }

    session
        .message_order
        .retain(|uuid| session.messages.contains_key(uuid));
    session.leaf_uuids = compute_leaf_uuids(&session.messages, &session.message_order);
}

fn collect_snip_removed_uuids(entry: &serde_json::Value, to_delete: &mut HashSet<String>) {
    let Some(removed) = entry
        .get("snipMetadata")
        .and_then(|metadata| metadata.get("removedUuids"))
        .and_then(|value| value.as_array())
    else {
        return;
    };

    for uuid in removed.iter().filter_map(|value| value.as_str()) {
        to_delete.insert(uuid.to_string());
    }
}

fn resolve_snip_parent(
    start: &str,
    to_delete: &HashSet<String>,
    deleted_parent: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    let mut path = Vec::new();
    let mut current = Some(start.to_string());
    while let Some(uuid) = current.clone() {
        if !to_delete.contains(&uuid) {
            break;
        }
        path.push(uuid.clone());
        current = deleted_parent.get(&uuid).cloned().unwrap_or(None);
    }
    for uuid in path {
        deleted_parent.insert(uuid, current.clone());
    }
    current
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Computes leaf UUIDs using the official terminal-message walk.
/// A terminal attachment/system message should not anchor resume by itself;
/// instead, walk back to the nearest user/assistant ancestor for that chain.
fn compute_leaf_uuids(
    messages: &HashMap<String, serde_json::Value>,
    order: &[String],
) -> HashSet<String> {
    let mut parent_uuids: HashSet<&str> = HashSet::new();
    for msg in messages.values() {
        if let Some(parent) = msg.get("parentUuid").and_then(|v| v.as_str()) {
            parent_uuids.insert(parent);
        }
    }

    let mut leaves = HashSet::new();
    for terminal_uuid in order
        .iter()
        .filter(|uuid| !parent_uuids.contains(uuid.as_str()))
    {
        let mut seen = HashSet::new();
        let mut current = Some(terminal_uuid.as_str());
        while let Some(uuid) = current {
            if !seen.insert(uuid.to_string()) {
                break;
            }
            let Some(msg) = messages.get(uuid) else {
                break;
            };
            let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if msg_type == "user" || msg_type == "assistant" {
                leaves.insert(uuid.to_string());
                break;
            }
            current = msg.get("parentUuid").and_then(|v| v.as_str());
        }
    }

    leaves
}

pub fn remove_extra_fields(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    messages
        .into_iter()
        .map(|mut msg| {
            if let Some(obj) = msg.as_object_mut() {
                obj.remove("isSidechain");
                obj.remove("parentUuid");
            }
            msg
        })
        .collect()
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub fn build_conversation_chain(
    session: &LoadedSession,
    leaf_uuid: &str,
) -> Vec<serde_json::Value> {
    let mut transcript = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = Some(leaf_uuid.to_string());

    while let Some(uuid) = current {
        if seen.contains(&uuid) {
            tracing::warn!("Cycle detected in parentUuid chain at message {uuid}");
            break;
        }

        let Some(msg) = session.messages.get(&uuid) else {
            break;
        };

        seen.insert(uuid);
        transcript.push(msg.clone());
        current = msg
            .get("parentUuid")
            .and_then(|v| v.as_str())
            .map(String::from);
    }

    transcript.reverse();
    recover_orphaned_parallel_tool_results(session, transcript, seen)
}

/// `recoverOrphanedParallelToolResults()`。
fn recover_orphaned_parallel_tool_results(
    session: &LoadedSession,
    chain: Vec<serde_json::Value>,
    mut seen: HashSet<String>,
) -> Vec<serde_json::Value> {
    let chain_assistants = chain
        .iter()
        .filter(|msg| msg.get("type").and_then(|v| v.as_str()) == Some("assistant"))
        .collect::<Vec<_>>();
    if chain_assistants.is_empty() {
        return chain;
    }

    // Anchor = last on-chain member of each sibling group.
    let mut anchor_by_msg_id: HashMap<String, String> = HashMap::new();
    for assistant in &chain_assistants {
        let msg_id = assistant
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str());
        let uuid = assistant.get("uuid").and_then(|v| v.as_str());
        if let (Some(msg_id), Some(uuid)) = (msg_id, uuid) {
            anchor_by_msg_id.insert(msg_id.to_string(), uuid.to_string());
        }
    }

    // O(n) precompute. Iterate message_order to match JS Map insertion order;
    // stable timestamp sort then preserves JSONL write order on ties.
    let mut siblings_by_msg_id: HashMap<String, Vec<String>> = HashMap::new();
    let mut tool_results_by_asst: HashMap<String, Vec<String>> = HashMap::new();
    for uuid in &session.message_order {
        let Some(msg) = session.messages.get(uuid) else {
            continue;
        };
        match msg.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                if let Some(msg_id) = msg
                    .get("message")
                    .and_then(|m| m.get("id"))
                    .and_then(|v| v.as_str())
                {
                    siblings_by_msg_id
                        .entry(msg_id.to_string())
                        .or_default()
                        .push(uuid.clone());
                }
            }
            Some("user") => {
                let parent_uuid = msg.get("parentUuid").and_then(|v| v.as_str());
                let has_tool_result = msg
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block.get("type").and_then(|v| v.as_str()) == Some("tool_result")
                        })
                    });
                if let (Some(parent_uuid), true) = (parent_uuid, has_tool_result) {
                    tool_results_by_asst
                        .entry(parent_uuid.to_string())
                        .or_default()
                        .push(uuid.clone());
                }
            }
            _ => {}
        }
    }

    let mut processed_groups: HashSet<String> = HashSet::new();
    let mut inserts: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut recovered_count = 0usize;

    for assistant in chain_assistants {
        let Some(msg_id) = assistant
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        if !processed_groups.insert(msg_id.to_string()) {
            continue;
        }

        let group = siblings_by_msg_id.get(msg_id).cloned().unwrap_or_else(|| {
            assistant
                .get("uuid")
                .and_then(|v| v.as_str())
                .map(|uuid| vec![uuid.to_string()])
                .unwrap_or_default()
        });

        let mut orphaned_siblings = group
            .iter()
            .filter(|uuid| !seen.contains(*uuid))
            .filter_map(|uuid| session.messages.get(uuid).cloned())
            .collect::<Vec<_>>();

        let mut orphaned_tool_results = Vec::new();
        for member_uuid in &group {
            let Some(tool_results) = tool_results_by_asst.get(member_uuid) else {
                continue;
            };
            for tool_result_uuid in tool_results {
                if !seen.contains(tool_result_uuid) {
                    if let Some(tool_result) = session.messages.get(tool_result_uuid) {
                        orphaned_tool_results.push(tool_result.clone());
                    }
                }
            }
        }

        if orphaned_siblings.is_empty() && orphaned_tool_results.is_empty() {
            continue;
        }

        orphaned_siblings.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            ta.cmp(tb)
        });
        orphaned_tool_results.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            ta.cmp(tb)
        });

        let Some(anchor_uuid) = anchor_by_msg_id.get(msg_id) else {
            continue;
        };
        let mut recovered = Vec::new();
        recovered.extend(orphaned_siblings);
        recovered.extend(orphaned_tool_results);
        for message in &recovered {
            if let Some(uuid) = message.get("uuid").and_then(|v| v.as_str()) {
                seen.insert(uuid.to_string());
            }
        }
        recovered_count += recovered.len();
        inserts.insert(anchor_uuid.clone(), recovered);
    }

    if recovered_count == 0 {
        return chain;
    }

    let mut result = Vec::with_capacity(chain.len() + recovered_count);
    for message in chain {
        let uuid = message
            .get("uuid")
            .and_then(|v| v.as_str())
            .map(String::from);
        result.push(message);
        if let Some(uuid) = uuid {
            if let Some(recovered) = inserts.remove(&uuid) {
                result.extend(recovered);
            }
        }
    }
    result
}

/// `findLatestMessage(messages.values(), msg => leafUuids.has(msg.uuid) && ... )`。
pub fn get_latest_leaf_uuid(session: &LoadedSession) -> Option<String> {
    find_latest_message(
        session
            .message_order
            .iter()
            .filter_map(|uuid| session.messages.get(uuid)),
        |message| {
            message
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|uuid| session.leaf_uuids.contains(uuid))
                && matches!(
                    message.get("type").and_then(serde_json::Value::as_str),
                    Some("user" | "assistant")
                )
        },
    )
    .and_then(|message| message.get("uuid")?.as_str().map(str::to_string))
}

/// Maps to: CC `utils/sessionStorage.ts#findLatestMessage:2046-2061`.
/// Persisted transcript timestamps are ISO-8601 strings. Parse them to JS Date
/// millisecond precision rather than comparing different UTC offsets as text.
/// Invalid/missing timestamps never beat -Infinity, and ties keep Map order.
fn find_latest_message<'a>(
    messages: impl IntoIterator<Item = &'a serde_json::Value>,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> Option<&'a serde_json::Value> {
    let mut latest = None;
    let mut max_time = i64::MIN;
    for message in messages {
        if !predicate(message) {
            continue;
        }
        let Some(timestamp) = message.get("timestamp").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(time) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
            continue;
        };
        let time = time.timestamp_millis();
        if time > max_time {
            max_time = time;
            latest = Some(message);
        }
    }
    latest
}

/// Maps to: CC `utils/sessionStorage.ts#loadSessionFile:3818-3836` path expression.
/// Exposed only to transport the same resolved file into the Rust selection carrier.
pub(crate) fn load_session_file_path(session_id: &str) -> PathBuf {
    crate::bootstrap::state::get_session_project_dir()
        .unwrap_or_else(|| {
            get_project_dir(&crate::bootstrap::state::get_original_cwd().to_string_lossy())
        })
        .join(format!("{session_id}.jsonl"))
}

/// Maps to: CC `utils/sessionStorage.ts#loadSessionFile:3818-3836`.
fn load_session_file(session_id: &str) -> LoadedSession {
    load_session_structured_from_path(&load_session_file_path(session_id))
}

type SessionMessageSet = std::sync::Arc<RwLock<HashSet<String>>>;
/// Maps to: CC `getSessionMessages`' session-ID-keyed memo cache (:3842-3848).
static SESSION_MESSAGES_CACHE: LazyLock<RwLock<HashMap<String, SessionMessageSet>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn get_session_messages(session_id: &str) -> SessionMessageSet {
    let mut cache = SESSION_MESSAGES_CACHE
        .write()
        .unwrap_or_else(|error| error.into_inner());
    cache
        .entry(session_id.to_string())
        .or_insert_with(|| {
            std::sync::Arc::new(RwLock::new(
                load_session_file(session_id).messages.into_keys().collect(),
            ))
        })
        .clone()
}

/// Maps to: CC `clearSessionMessagesCache:3854-3856`.
pub fn clear_session_messages_cache() {
    SESSION_MESSAGES_CACHE
        .write()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

/// Maps to: CC `doesMessageExistInSession:3861-3867`.
pub fn does_message_exist_in_session(session_id: &str, message_uuid: &str) -> bool {
    get_session_messages(session_id)
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .contains(message_uuid)
}

/// Maps to: CC `utils/sessionStorage.ts#getLastSessionLog:3868-3930`.
pub fn get_last_session_log(session_id: &str) -> Option<(LoadedSession, Vec<serde_json::Value>)> {
    let session = load_session_file(session_id);
    if session.messages.is_empty() {
        return None;
    }
    SESSION_MESSAGES_CACHE
        .write()
        .unwrap_or_else(|error| error.into_inner())
        .entry(session_id.to_string())
        .or_insert_with(|| {
            std::sync::Arc::new(RwLock::new(session.messages.keys().cloned().collect()))
        });
    let last = find_latest_message(
        session
            .message_order
            .iter()
            .filter_map(|uuid| session.messages.get(uuid)),
        |message| {
            !message.get("isSidechain").is_some_and(|value| match value {
                serde_json::Value::Null => false,
                serde_json::Value::Bool(value) => *value,
                serde_json::Value::Number(value) => value.as_f64() != Some(0.0),
                serde_json::Value::String(value) => !value.is_empty(),
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => true,
            })
        },
    )?;
    let uuid = last.get("uuid")?.as_str()?;
    let transcript = remove_extra_fields(build_conversation_chain(&session, uuid));
    Some((session, transcript))
}

// ════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════

fn extract_preserved_segment(entry: &serde_json::Value) -> Option<PreservedSegment> {
    let metadata = entry.get("compactMetadata")?;
    let seg = metadata.get("preservedSegment")?;
    let head_uuid = seg.get("headUuid").and_then(|v| v.as_str())?.to_string();
    let tail_uuid = seg.get("tailUuid").and_then(|v| v.as_str())?.to_string();
    let anchor_uuid = seg.get("anchorUuid").and_then(|v| v.as_str())?.to_string();
    Some(PreservedSegment {
        head_uuid,
        tail_uuid,
        anchor_uuid,
    })
}

fn is_compact_boundary(entry: &serde_json::Value) -> bool {
    entry.get("type").and_then(|v| v.as_str()) == Some("system")
        && entry.get("subtype").and_then(|v| v.as_str()) == Some(SUBTYPE_COMPACT_BOUNDARY)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 3).collect();
        format!("{truncated}...")
    }
}

#[allow(dead_code)]
pub fn validate_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    s.chars().enumerate().all(|(i, c)| {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            c == '-'
        } else {
            c.is_ascii_hexdigit()
        }
    })
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub struct ResolvedSessionFile {
    pub file_path: PathBuf,
    pub project_path: Option<String>,
    pub file_size: u64,
}

/// Maps to: CC `sessionStoragePortable.ts` `canonicalizePath(dir)`.
pub fn canonicalize_path(dir: &str) -> String {
    std::fs::canonicalize(dir)
        .unwrap_or_else(|_| PathBuf::from(dir))
        .to_string_lossy()
        .nfc()
        .collect()
}

pub fn resolve_session_file_path(
    session_id: &str,
    dir: Option<&str>,
) -> Option<ResolvedSessionFile> {
    let file_name = format!("{session_id}.jsonl");

    if let Some(dir_str) = dir {
        let canonical = canonicalize_path(dir_str);

        let project_dir = find_project_dir(&canonical);
        if let Some(ref pd) = project_dir {
            let file_path = pd.join(&file_name);
            if let Ok(meta) = std::fs::metadata(&file_path) {
                if meta.len() > 0 {
                    return Some(ResolvedSessionFile {
                        file_path,
                        project_path: Some(canonical.clone()),
                        file_size: meta.len(),
                    });
                }
            }
        }

        // Worktree fallback — sessions may live under a different worktree root.
        for worktree_path in
            crate::utils::get_worktree_paths_portable::get_worktree_paths_portable(&canonical)
        {
            if worktree_path == canonical {
                continue;
            }
            let Some(project_dir) = find_project_dir(&worktree_path) else {
                continue;
            };
            let file_path = project_dir.join(&file_name);
            if let Ok(meta) = std::fs::metadata(&file_path) {
                if meta.len() > 0 {
                    return Some(ResolvedSessionFile {
                        file_path,
                        project_path: Some(worktree_path),
                        file_size: meta.len(),
                    });
                }
            }
        }

        return None;
    }

    let projects_dir = get_projects_dir();
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return None;
    };

    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let file_path = entry.path().join(&file_name);
        if let Ok(meta) = std::fs::metadata(&file_path) {
            if meta.len() > 0 {
                return Some(ResolvedSessionFile {
                    file_path,
                    project_path: None,
                    file_size: meta.len(),
                });
            }
        }
    }

    None
}

/// Maps to: CC `sessionStoragePortable.ts` `findProjectDir(projectPath)`.
fn find_project_dir(project_path: &str) -> Option<PathBuf> {
    let sanitized = sanitize_path(project_path);
    let exact = get_projects_dir().join(&sanitized);
    if exact.is_dir() {
        return Some(exact);
    }
    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        return None;
    }

    let prefix = format!("{}-", &sanitized[..MAX_SANITIZED_LENGTH]);
    let entries = std::fs::read_dir(get_projects_dir()).ok()?;
    entries.flatten().find_map(|entry| {
        let is_directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        (is_directory && entry.file_name().to_string_lossy().starts_with(&prefix))
            .then(|| entry.path())
    })
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Maps to: CC `utils/sessionStorage.ts#recordTranscript:1408-1448`.
pub fn record_transcript(
    project_path: &str,
    session_id: &str,
    messages: &[serde_json::Value],
    starting_parent_uuid_hint: Option<&str>,
    session_stamp: &SessionStamp,
) -> anyhow::Result<Option<String>> {
    if !is_session_write_enabled() {
        tracing::info!("[READ_ONLY] record_transcript: session writes are disabled");
        return Ok(starting_parent_uuid_hint.map(String::from));
    }

    let is_current_session = session_id == crate::bootstrap::state::get_session_id();
    let message_set = get_session_messages(session_id);
    let existing_uuids = message_set
        .read()
        .unwrap_or_else(|error| error.into_inner());

    let mut new_messages: Vec<&serde_json::Value> = Vec::new();
    let mut starting_parent_uuid = starting_parent_uuid_hint.map(String::from);
    let mut seen_new_message = false;

    for msg in messages {
        let uuid = msg.get("uuid").and_then(|v| v.as_str()).unwrap_or("");

        if existing_uuids.contains(uuid) {
            if !seen_new_message && is_chain_participant(msg) {
                starting_parent_uuid = Some(uuid.to_string());
            }
        } else {
            new_messages.push(msg);
            seen_new_message = true;
        }
    }
    drop(existing_uuids);

    if !new_messages.is_empty() {
        insert_message_chain(
            project_path,
            session_id,
            &new_messages,
            starting_parent_uuid.as_deref(),
            None,
            false,
            session_stamp,
        )?;
    }

    let last_recorded = new_messages
        .iter()
        .rev()
        .find(|m| is_chain_participant(m))
        .and_then(|m| m.get("uuid").and_then(|v| v.as_str()).map(String::from));

    let last_parent_uuid = last_recorded.or(starting_parent_uuid);
    if is_current_session {
        let mut meta = get_project()
            .current_session_meta
            .write()
            .map_err(|_| anyhow::anyhow!("session metadata lock poisoned"))?;
        // Keep existing incremental actor producers on the same branch as
        // useLogMessages, including an all-recorded rewind prefix.
        meta.last_message_uuid = last_parent_uuid.clone();
    }
    Ok(last_parent_uuid)
}

/// Maps to CC `utils/sessionStorage.ts` `recordSidechainTranscript(...)` at the
/// `insertMessageChain` half only (`:1456-1461`).
///
/// Safety boundary: this preserves the official sidechain persistence seam for
/// AgentTool/runAgent persistence path; messages are stamped with
/// `isSidechain: true` and `agentId` when the write policy is enabled.
///
/// `messages` must ALREADY be filtered. CC's `recordSidechainTranscript` runs
/// `cleanMessagesForLogging` (`:1457`) → `isLoggableMessage` (`:4351-4367`)
/// before it reaches `insertMessageChain`, and that filter is the privacy
/// boundary: progress is never persisted, and non-`ant` builds withhold every
/// attachment except `hook_additional_context` behind
/// `CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT`. Typed callers must go through
/// [`record_typed_sidechain_transcript`], which owns both halves; handing raw
/// entries straight to this function skips the filter (that is exactly how the
/// AgentTool sidechain writer came to persist SubagentStart hook output with no
/// env gate at all).
///
/// Returns nothing, like CC. `recordSidechainTranscript` is `async function
/// (...) { await getProject().insertMessageChain(...) }` (`:1451-1462`) and
/// `insertMessageChain` itself ends after its loop with no `return`
/// (`:999-1082`) — the "last recorded uuid" computation lives in
/// `recordTranscript` (`:1516-1528`), a DIFFERENT function, for
/// `useLogMessages`. The sidechain cursor is owned by the caller
/// (`runAgent.ts:745` / `:801-803`, `forkedAgent.ts:536-539` / `:594-596`) and
/// is derived from the messages handed IN, never from what the write did with
/// them. This used to return the last written chain participant, and
/// [`SidechainTranscriptRecorder`] adopted it as its cursor — which silently
/// pinned the cursor whenever a write failed or the privacy filter emptied the
/// batch.
pub fn record_sidechain_transcript(
    project_path: &str,
    session_id: &str,
    messages: &[serde_json::Value],
    agent_id: &str,
    starting_parent_uuid: Option<&str>,
    session_stamp: &SessionStamp,
) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        tracing::info!("[READ_ONLY] record_sidechain_transcript: session writes are disabled");
        return Ok(());
    }

    let message_refs = messages.iter().collect::<Vec<_>>();
    insert_message_chain(
        project_path,
        session_id,
        &message_refs,
        starting_parent_uuid,
        Some(agent_id),
        true,
        session_stamp,
    )
}

/// Typed adapter for CC `recordSidechainTranscript(messages, agentId,
/// lastRecordedUuid)` (`sessionStorage.ts:1451-1462`) used by
/// `utils/forkedAgent.ts:531`/`:588` and `tools/AgentTool/runAgent.ts:735`/`:794`.
///
/// Conversion remains owned by session storage so forked-agent callers do not
/// duplicate the JSONL message envelope or parent-chain rules.
///
/// `stamp_cwd` is Rust's explicit stand-in for the AsyncLocalStorage read CC
/// performs inside `insertMessageChain` — `cwd: getCwd()`
/// (`sessionStorage.ts:1059`), where `getCwd()` is `ALS store ?? global cwd`
/// (`utils/cwd.ts:20-21, 26-32`) and the AgentTool run is wrapped in
/// `runWithCwdOverride(cwdOverridePath, fn)` (`AgentTool.tsx:917`;
/// `resumeAgent.ts:228` for the resumed worktree). So a worktree-isolated
/// agent's entries carry the WORKTREE in their `cwd` field, and Rust threads
/// that value in because it has no ALS.
///
/// It stops at the stamp. CC's ALS override does not reach `getOriginalCwd` or
/// `getSessionProjectDir`, so routing stays with
/// [`agent_artifacts_project_dir`] and the file lands under the original
/// project dir — the same one `write_agent_metadata` and the resume reader
/// use. Feeding this value into `project_path` (as this function used to)
/// split the root from the readers and lost the transcript.
pub fn record_typed_sidechain_transcript(
    messages: &[crate::types::message::Message],
    agent_id: &str,
    starting_parent_uuid: Option<&str>,
    stamp_cwd: &std::path::Path,
) -> anyhow::Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    // CC `recordSidechainTranscript` also runs `cleanMessagesForLogging`
    // (sessionStorage.ts:1456-1457), so what arrives here is a superset of what
    // lands on disk. Two producers reach it:
    //
    // - forked agents record assistant|user|progress (forkedAgent.ts:582-597),
    //   and `isLoggableMessage` (:4351-4352) drops the progress on every
    //   audience;
    // - AgentTool records its `initialMessages` once (runAgent.ts:735), and
    //   those END with the SubagentStart `hook_additional_context` attachment
    //   when a hook produced context (:546-554). Attachments survive this
    //   filter on `ant` builds and are withheld on every other one (:4357-4366)
    //   unless `CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT` is set.
    //
    // The attachments the query LOOP yields never arrive: `runAgent.ts:771-790`
    // yields them and `continue`s without recording, and `isRecordableMessage`
    // (:231-246) would reject them anyway. Anything that does arrive from the
    // loop already passed that gate in the caller.
    let entries = typed_messages_as_transcript_values(messages);
    // CC `recordSidechainTranscript` takes no project path: `getProject()`
    // owns it (`:1456`). Same value `record_typed_message` passes (`:3632`) —
    // the routing that matters for sidechain entries is resolved downstream by
    // `agent_artifacts_project_dir`, and `materialize_session_file` likewise
    // prefers `get_session_project_dir()`.
    let project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    let session_id = crate::bootstrap::state::get_session_id();
    let stamp = SessionStamp {
        session_id: session_id.clone(),
        cwd: stamp_cwd.to_string_lossy().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_branch: None,
        user_type: crate::utils::build_profile::build_audience()
            .as_str()
            .to_string(),
        entrypoint: "cometix".to_string(),
        slug: None,
        team_name: None,
        agent_name: None,
    };
    record_sidechain_transcript(
        &project_path,
        &session_id,
        &entries,
        agent_id,
        starting_parent_uuid,
        &stamp,
    )
}

pub struct SessionStamp {
    pub session_id: String,
    pub cwd: String,
    pub version: String,
    pub git_branch: Option<String>,
    pub user_type: String,
    pub entrypoint: String,
    pub slug: Option<String>,
    /// CC `TranscriptMessage.teamName` / `.agentName`
    /// (sessionStorage.ts:1043-1044), sourced from the logging effect's
    /// `teamContext`. The session-list reader pulls `teamName` out of the file
    /// head (`session_lite_summary`, CC :4752) to tell team sessions apart, so
    /// leaving it unset made every team session look like a plain one.
    pub team_name: Option<String>,
    pub agent_name: Option<String>,
}

/// Maps to: CC `TeamInfo` (sessionStorage.ts:1386-1389), the optional pair
/// `recordTranscript` stamps onto every entry it writes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TeamInfo {
    pub team_name: Option<String>,
    pub agent_name: Option<String>,
}

/// L1 incremental typed writer retained for headless callers. Interactive
/// history uses `record_typed_messages_with_team` with an explicit parent hint.
/// Pre-mount/resume SessionStart still uses this path (a documented existing
/// deviation: CC REPL.tsx:2406-2413 only appends its hook messages to history).
pub fn record_typed_messages(messages: &[crate::types::message::Message]) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        return Ok(());
    }
    for message in messages {
        record_typed_message(message, None)?;
    }
    Ok(())
}

/// Maps to: CC `recordTranscript(messages, teamInfo, startingParentUuidHint)`
/// (`utils/sessionStorage.ts:1408-1448`). L1 typed-union → wire adapter;
/// deduplication and prefix-parent selection remain in `record_transcript`.
pub fn record_typed_messages_with_team(
    messages: &[crate::types::message::Message],
    team_info: Option<&TeamInfo>,
    starting_parent_uuid_hint: Option<&str>,
) -> anyhow::Result<Option<String>> {
    if !is_session_write_enabled() {
        return Ok(starting_parent_uuid_hint.map(String::from));
    }
    let session_id = crate::bootstrap::state::get_session_id();
    let project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    let entries = typed_messages_as_transcript_values(messages);
    let stamp = SessionStamp {
        session_id: session_id.clone(),
        cwd: project_path.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_branch: Some(crate::utils::git::get_branch()).filter(|branch| !branch.is_empty()),
        user_type: crate::utils::build_profile::build_audience()
            .as_str()
            .to_string(),
        entrypoint: "cli".to_string(),
        slug: None,
        team_name: team_info.and_then(|info| info.team_name.clone()),
        agent_name: team_info.and_then(|info| info.agent_name.clone()),
    };
    record_transcript(
        &project_path,
        &session_id,
        &entries,
        starting_parent_uuid_hint,
        &stamp,
    )
}

/// Maps to: CC `services/api/claude.ts:2229-2248` mutation of the message held by
/// `Project.enqueueWrite` (`utils/sessionStorage.ts:606-686`).
/// L1 actor transport: patch only an already-enqueued entry. Before the effect,
/// the history mutation supplies the final values; after drain, a miss retains
/// CC's accepted lazy-serialization race. This never selects a parent or writes.
pub fn apply_assistant_delta_to_queued_record(
    uuid: &str,
    stop_reason: Option<crate::types::message::StopReason>,
    usage: Option<crate::types::message::TokenUsage>,
) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        return Ok(());
    }
    let session_id = crate::bootstrap::state::get_session_id();
    let path = active_session_file_path(
        &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
        &session_id,
    );
    get_project()
        .sender
        .send(ProjectWriteCommand::PatchAssistantDelta {
            path,
            uuid: uuid.to_string(),
            stop_reason: serde_json::to_value(stop_reason)?,
            usage: serde_json::to_value(usage)?,
        })
        .map_err(|_| anyhow::anyhow!("Project write queue stopped"))
}

/// One agent run's sidechain transcript writer: CC's `lastRecordedUuid`
/// parent cursor (`runAgent.ts:745`, `forkedAgent.ts:536-539`) plus the
/// assistant record parked un-written so `message_delta`'s write-back still
/// reaches `agent-<id>.jsonl`.
///
/// Maps to: the object identity CC's write queue depends on.
/// `recordSidechainTranscript` (`sessionStorage.ts:1451-1462`) is handed the
/// very `Message` the stream yielded — `cleanMessagesForLogging` `.filter`s
/// the array (`:4454`) and `insertMessageChain`'s `{ ...msg }` (`:1046`)
/// copies `.message` as a POINTER — and `drainWriteQueue` stringifies that
/// entry `FLUSH_INTERVAL_MS = 100` later (`:567`, `:606`, `:658`). So
/// `claude.ts:2244-2248`'s post-yield `lastMsg.message.usage = usage` /
/// `.stop_reason = stopReason` still lands in the file, and CC says so in its
/// own words at `:2235-2243`: "Use direct property mutation, not object
/// replacement. The transcript write queue holds a reference to
/// message.message and serializes it lazily (100ms flush interval). Object
/// replacement (`{ ...lastMsg.message, usage }`) would disconnect the queued
/// reference; direct mutation ensures the transcript captures the final
/// values."
///
/// The one CC branch that DOES detach is
/// `transformMessagesForExternalTranscript` (`:4396-4447`, non-`ant` only),
/// and only for a message that is `isVirtual` or that had a REPL tool
/// use/result stripped; every other message returns as `[m]`, unchanged. None
/// of `isVirtual`, the REPL tool, or that transform exists in this port, so
/// the identity-preserving branch is the whole surface here.
///
/// Subagents are on that exact path, not merely inheriting it from the main
/// loop: `runAgent.ts:750` and `forkedAgent.ts:546` both drive `query()`, and
/// `query`'s only model call is `deps.callModel` (`query.ts:659`), bound to
/// `queryModelWithStreaming` (`query/deps.ts:23`, `:35`) — the generator
/// defined at `claude.ts:752` that owns the `message_delta` arm.
///
/// Cometix serializes at enqueue (`enqueue_write` clones the `Value`), so the
/// deferral has to be explicit: the assistant record is parked here, the
/// `AssistantDelta` write-back patches it, and the next record — or the run's
/// exit — materializes it. Arrival order survives because nothing else can be
/// written while a record is parked.
///
/// Each agent owns its parked record because agent runs are concurrent.
/// The main session instead patches its queued entries via the Project actor.
/// One recorder per run keeps the park and cursor private; runs only meet
/// in the ordered, per-path write queue.
///
/// The race CC accepts is kept: if a run ends between the assistant message
/// and its `message_delta`, the parked record is written with the pre-delta
/// values, exactly as CC's 100 ms drain does when it wins that race.
pub struct SidechainTranscriptRecorder {
    agent_id: String,
    /// CC `getCwd()` as read by `insertMessageChain` (`:1059`) — stamp only.
    /// See [`record_typed_sidechain_transcript`] for why it must not route.
    stamp_cwd: PathBuf,
    last_recorded_uuid: Option<String>,
    parked_assistant: Option<ParkedSidechainRecord>,
}

/// A parked assistant record and the cursor value as it stood when the record
/// was made.
///
/// The parent CANNOT be re-read from `last_recorded_uuid` at write time: CC
/// advances the cursor the moment the message is handed to
/// `recordSidechainTranscript` (`runAgent.ts:794-803` — the `await` is on the
/// write, the assignment that follows is unconditional for non-progress), so by
/// the time this port materializes the parked record the cursor is already the
/// parked message's OWN uuid and the row would name itself as its parent.
struct ParkedSidechainRecord {
    message: crate::types::message::Message,
    parent_uuid: Option<String>,
}

impl SidechainTranscriptRecorder {
    /// Maps to: CC `runAgent.ts:735` + `:745` / `forkedAgent.ts:531` +
    /// `:536-539` — record the run's initial messages, then seed the parent
    /// cursor.
    ///
    /// The seed is the LAST INPUT message's uuid (`initialMessages.at(-1)?.uuid
    /// ?? null`), not the last row the write produced. The two differ whenever
    /// `cleanMessagesForLogging` drops the tail: on a non-`ant` build whose
    /// SubagentStart hook produced context, `initialMessages` ends with the
    /// `hook_additional_context` attachment (`runAgent.ts:546-554`), the filter
    /// withholds it (`:4357-4366`), and CC still parents the first loop message
    /// to that never-written uuid. Reproducing that is the point: the reader is
    /// a leaf-to-root `parentUuid` walk on both sides (CC `getAgentTranscript`
    /// `:4210-4224`, [`get_agent_transcript_from_path`]), so a cursor the port
    /// "repaired" would reconstruct a DIFFERENT prefix than CC does from the
    /// same run.
    ///
    /// Both writes are fire-and-forget in CC (`.catch(err =>
    /// logForDebugging(...))`), and the cursor is assigned outside that promise
    /// — a failed write never holds it back.
    pub fn start(
        agent_id: &str,
        initial_messages: &[crate::types::message::Message],
        stamp_cwd: &Path,
    ) -> Self {
        let mut recorder = Self {
            agent_id: agent_id.to_string(),
            stamp_cwd: stamp_cwd.to_path_buf(),
            last_recorded_uuid: None,
            parked_assistant: None,
        };
        recorder.write(initial_messages, None);
        recorder.last_recorded_uuid = initial_messages
            .last()
            .map(|message| message.uuid().to_string());
        recorder
    }

    /// Maps to: CC `runAgent.ts:794-803` / `forkedAgent.ts:583-596` — the
    /// per-message record inside the query loop, and the cursor advance that
    /// follows it.
    ///
    /// The cursor becomes this message's own uuid unless the message is
    /// progress (`if (message.type !== 'progress')`, `:801-803`) — progress is
    /// written to no JSONL at all and nothing chains TO it
    /// (`isChainParticipant`, `:154-156`).
    ///
    /// Assistant records are parked (see the type doc); everything else is
    /// written straight through, but only AFTER any parked record, so the
    /// JSONL order still matches arrival order. A parked record that is
    /// displaced this way can no longer be `newMessages.at(-1)`, so its
    /// values are already final — CC `claude.ts:2244` only ever mutates the
    /// last yielded per-block assistant.
    ///
    /// The caller is expected to have applied CC's `isRecordableMessage` gate
    /// (`runAgent.ts:231-246`) already; this type owns the chain, not the
    /// admission rule.
    pub fn record(&mut self, message: &crate::types::message::Message) {
        self.flush();
        let parent_uuid = self.last_recorded_uuid.clone();
        if !matches!(message, crate::types::message::Message::Progress(_)) {
            self.last_recorded_uuid = Some(message.uuid().to_string());
        }
        if matches!(message, crate::types::message::Message::Assistant(_)) {
            self.parked_assistant = Some(ParkedSidechainRecord {
                message: message.clone(),
                parent_uuid,
            });
        } else {
            self.write(std::slice::from_ref(message), parent_uuid.as_deref());
        }
    }

    /// Maps to: CC `claude.ts:2244-2248` reaching the queued transcript entry.
    /// After the delta the values are final, which is the moment CC's lazy
    /// stringify observes, so the record is written immediately afterwards.
    pub fn apply_assistant_delta(
        &mut self,
        uuid: &str,
        stop_reason: Option<crate::types::message::StopReason>,
        usage: Option<crate::types::message::TokenUsage>,
    ) {
        if let Some(crate::types::message::Message::Assistant(assistant)) = self
            .parked_assistant
            .as_mut()
            .map(|parked| &mut parked.message)
        {
            if assistant.uuid == uuid {
                assistant.stop_reason = stop_reason;
                assistant.usage = usage;
            }
        }
        self.flush();
    }

    /// Materialize the parked record. Must run on every exit of the run that
    /// owns this recorder — CC's queue drains on its own timer no matter how
    /// `runAgent` returns, so an abort or a backgrounding handoff still leaves
    /// the last assistant row on disk.
    ///
    /// The row is written with the parent the cursor held when the record was
    /// PARKED, which is what CC's `startingParentUuid` argument carried at that
    /// same instant; the live cursor has moved on by now.
    pub fn flush(&mut self) {
        let Some(parked) = self.parked_assistant.take() else {
            return;
        };
        self.write(
            std::slice::from_ref(&parked.message),
            parked.parent_uuid.as_deref(),
        );
    }

    /// CC `lastRecordedUuid` — the parent for the next entry in this chain.
    pub fn last_recorded_uuid(&self) -> Option<&str> {
        self.last_recorded_uuid.as_deref()
    }

    /// CC's `recordSidechainTranscript(messages, agentId, lastRecordedUuid)`
    /// call itself, and nothing else. It cannot move the cursor: CC's returns
    /// `void` (`sessionStorage.ts:1451-1462`) and the assignment that follows
    /// each call site reads the message, not the write
    /// (`runAgent.ts:801-803`).
    fn write(&self, messages: &[crate::types::message::Message], parent_uuid: Option<&str>) {
        if messages.is_empty() {
            return;
        }
        // CC `runAgent.ts:735-737` / `:798-800`: fire-and-forget, the failure
        // path is `logForDebugging` and the run continues.
        if let Err(error) = record_typed_sidechain_transcript(
            messages,
            &self.agent_id,
            parent_uuid,
            &self.stamp_cwd,
        ) {
            tracing::debug!(
                agent_id = %self.agent_id,
                error = %error,
                "failed to record sidechain transcript"
            );
        }
    }
}

fn record_typed_message(
    message: &crate::types::message::Message,
    team_info: Option<&TeamInfo>,
) -> anyhow::Result<()> {
    let session_id = crate::bootstrap::state::get_session_id();
    let project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    // CC `recordTranscript` → `cleanMessagesForLogging` → `isLoggableMessage`
    // (sessionStorage.ts:1414, 4351-4352): progress never reaches the JSONL.
    let Some((entry, uuid)) = typed_message_entry(message) else {
        return Ok(());
    };

    // Maps to CC `Project.insertMessageChain`: only user/assistant messages
    // materialize a fresh session. Hook/system/attachment-only entries remain
    // in `pendingEntries` until then.
    if matches!(
        message,
        crate::types::message::Message::User(_) | crate::types::message::Message::Assistant(_)
    ) {
        materialize_session_file(&project_path, &session_id)?;
    }

    let parent_uuid = {
        let meta = get_project()
            .current_session_meta
            .read()
            .map_err(|_| anyhow::anyhow!("session metadata lock poisoned"))?;
        if does_message_exist_in_session(&session_id, &uuid) {
            return Ok(());
        }
        meta.last_message_uuid.clone()
    };

    let stamp = SessionStamp {
        session_id: session_id.clone(),
        cwd: project_path.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_branch: Some(crate::utils::git::get_branch()).filter(|branch| !branch.is_empty()),
        user_type: crate::utils::build_profile::build_audience()
            .as_str()
            .to_string(),
        entrypoint: "cli".to_string(),
        slug: None,
        team_name: team_info.and_then(|info| info.team_name.clone()),
        agent_name: team_info.and_then(|info| info.agent_name.clone()),
    };
    insert_message_chain(
        &project_path,
        &session_id,
        &[&entry],
        parent_uuid.as_deref(),
        None,
        false,
        &stamp,
    )?;
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.last_message_uuid = Some(uuid);
    }
    Ok(())
}

/// Maps to: CC `Project.materializeSessionFile()`.
fn materialize_session_file(project_path: &str, session_id: &str) -> anyhow::Result<PathBuf> {
    let (path, pending_entries, was_materialized) = {
        let mut meta = get_project()
            .current_session_meta
            .write()
            .map_err(|_| anyhow::anyhow!("session metadata lock poisoned"))?;
        if let Some(path) = meta.session_file.clone() {
            (path, Vec::new(), false)
        } else {
            let path = crate::bootstrap::state::get_session_project_dir()
                .map(|dir| dir.join(format!("{session_id}.jsonl")))
                .unwrap_or_else(|| get_session_file_path(project_path, session_id));
            meta.session_file = Some(path.clone());
            (path, std::mem::take(&mut meta.pending_entries), true)
        }
    };

    if was_materialized {
        let mut cache = get_current_session_metadata();
        re_append_session_metadata_inner(project_path, &mut cache, false)?;
        for pending in pending_entries {
            append_entry(project_path, session_id, &pending)?;
        }
    }
    Ok(path)
}

/// Maps to: CC `cleanMessagesForLogging(messages)` (sessionStorage.ts:4450-4461)
/// — the `isLoggableMessage` filter every `insertMessageChain` caller runs
/// before the write, applied to the typed union.
pub(crate) fn typed_messages_as_transcript_values(
    messages: &[crate::types::message::Message],
) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter_map(|message| typed_message_entry(message).map(|(entry, _)| entry))
        .collect()
}

/// Wire object for one system message entry — the union→wire half of the
/// batch-D1 adapter pair (`conversation_recovery::system_message_from_entry`
/// is the inverse). Field names are the runtime shapes the CC factories write
/// (`utils/messages.ts:4335-4603`), which is what CC persists: its wire IS the
/// message object.
pub(crate) fn system_entry_json(
    message: &crate::types::message::SystemMessage,
    uuid: &str,
) -> serde_json::Value {
    use crate::types::message::{SystemMessage, SystemMessageLevel};

    fn level_str(level: SystemMessageLevel) -> &'static str {
        match level {
            SystemMessageLevel::Info => "info",
            SystemMessageLevel::Warning => "warning",
            SystemMessageLevel::Error => "error",
        }
    }

    let mut entry = serde_json::json!({
        "type": "system",
        "uuid": uuid,
        "timestamp": message.timestamp().to_rfc3339(),
        "subtype": message.subtype(),
        "isMeta": message.base().is_meta,
    });
    let object = entry
        .as_object_mut()
        .expect("typed system entry is an object");
    let mut set = |key: &str, value: serde_json::Value| {
        object.insert(key.to_string(), value);
    };
    match message {
        SystemMessage::Informational {
            content,
            level,
            tool_use_id,
            prevent_continuation,
            ..
        } => {
            set("content", serde_json::json!(content));
            set("level", serde_json::json!(level_str(*level)));
            if let Some(tool_use_id) = tool_use_id {
                set("toolUseID", serde_json::json!(tool_use_id));
            }
            // CC spreads the key in only when truthy (`:4350`).
            if prevent_continuation.unwrap_or(false) {
                set("preventContinuation", serde_json::json!(true));
            }
        }
        SystemMessage::ApiError {
            error,
            retry_in_ms,
            retry_attempt,
            max_retries,
            ..
        } => {
            // `createSystemAPIErrorMessage` constants/fields (`:4585-4603`).
            set("level", serde_json::json!("error"));
            set("error", serde_json::json!(error));
            set("retryInMs", serde_json::json!(retry_in_ms));
            set("retryAttempt", serde_json::json!(retry_attempt));
            set("maxRetries", serde_json::json!(max_retries));
        }
        SystemMessage::LocalCommand { content, .. } => {
            set("content", serde_json::json!(content));
            set("level", serde_json::json!("info"));
        }
        SystemMessage::PermissionRetry {
            content, commands, ..
        } => {
            set("content", serde_json::json!(content));
            set("commands", serde_json::json!(commands));
            set("level", serde_json::json!("info"));
        }
        SystemMessage::BridgeStatus {
            content,
            url,
            upgrade_nudge,
            ..
        } => {
            set("content", serde_json::json!(content));
            set("url", serde_json::json!(url));
            if let Some(upgrade_nudge) = upgrade_nudge {
                set("upgradeNudge", serde_json::json!(upgrade_nudge));
            }
        }
        SystemMessage::ScheduledTaskFire { content, .. } => {
            set("content", serde_json::json!(content));
        }
        SystemMessage::StopHookSummary {
            hook_count,
            hook_infos,
            hook_errors,
            prevented_continuation,
            stop_reason,
            has_output,
            level,
            tool_use_id,
            hook_label,
            total_duration_ms,
            ..
        } => {
            set("hookCount", serde_json::json!(hook_count));
            set(
                "hookInfos",
                serde_json::Value::Array(
                    hook_infos
                        .iter()
                        .map(|info| {
                            let mut item = serde_json::Map::new();
                            if let Some(command) = &info.command {
                                item.insert("command".to_string(), serde_json::json!(command));
                            }
                            if let Some(prompt_text) = &info.prompt_text {
                                item.insert(
                                    "promptText".to_string(),
                                    serde_json::json!(prompt_text),
                                );
                            }
                            if let Some(duration_ms) = info.duration_ms {
                                item.insert(
                                    "durationMs".to_string(),
                                    serde_json::json!(duration_ms),
                                );
                            }
                            if let Some(output) = &info.output {
                                item.insert("output".to_string(), serde_json::json!(output));
                            }
                            if let Some(error) = &info.error {
                                item.insert("error".to_string(), serde_json::json!(error));
                            }
                            if info.prevented_continuation {
                                item.insert(
                                    "preventedContinuation".to_string(),
                                    serde_json::json!(true),
                                );
                            }
                            serde_json::Value::Object(item)
                        })
                        .collect(),
                ),
            );
            set("hookErrors", serde_json::json!(hook_errors));
            set(
                "preventedContinuation",
                serde_json::json!(prevented_continuation),
            );
            if let Some(stop_reason) = stop_reason {
                set("stopReason", serde_json::json!(stop_reason));
            }
            set("hasOutput", serde_json::json!(has_output));
            set("level", serde_json::json!(level_str(*level)));
            if let Some(tool_use_id) = tool_use_id {
                set("toolUseID", serde_json::json!(tool_use_id));
            }
            if let Some(hook_label) = hook_label {
                set("hookLabel", serde_json::json!(hook_label));
            }
            if let Some(total_duration_ms) = total_duration_ms {
                set("totalDurationMs", serde_json::json!(total_duration_ms));
            }
        }
        SystemMessage::TurnDuration {
            duration_ms,
            budget_tokens,
            budget_limit,
            budget_nudges,
            message_count,
            ..
        } => {
            set("durationMs", serde_json::json!(duration_ms));
            if let Some(budget_tokens) = budget_tokens {
                set("budgetTokens", serde_json::json!(budget_tokens));
            }
            if let Some(budget_limit) = budget_limit {
                set("budgetLimit", serde_json::json!(budget_limit));
            }
            if let Some(budget_nudges) = budget_nudges {
                set("budgetNudges", serde_json::json!(budget_nudges));
            }
            if let Some(message_count) = message_count {
                set("messageCount", serde_json::json!(message_count));
            }
        }
        SystemMessage::AwaySummary { content, .. } => {
            set("content", serde_json::json!(content));
        }
        SystemMessage::MemorySaved { written_paths, .. } => {
            set("writtenPaths", serde_json::json!(written_paths));
        }
        SystemMessage::CompactBoundary {
            compact_metadata,
            logical_parent_uuid,
            ..
        } => {
            if let Some(parent) = logical_parent_uuid {
                set("logicalParentUuid", serde_json::json!(parent));
            }
            // Factory constants (`:4540-4544`).
            set("content", serde_json::json!("Conversation compacted"));
            set("level", serde_json::json!("info"));
            set("compactMetadata", serde_json::json!(compact_metadata));
        }
        SystemMessage::MicrocompactBoundary {
            microcompact_metadata,
            ..
        } => {
            // Factory constants (`:4570-4574`).
            set("content", serde_json::json!("Context microcompacted"));
            set("level", serde_json::json!("info"));
            set(
                "microcompactMetadata",
                serde_json::json!(microcompact_metadata),
            );
        }
        SystemMessage::AgentsKilled { .. } | SystemMessage::FileSnapshot { .. } => {}
        SystemMessage::ApiMetrics {
            ttft_ms,
            otps,
            is_p50,
            hook_duration_ms,
            turn_duration_ms,
            tool_duration_ms,
            classifier_duration_ms,
            tool_count,
            hook_count,
            classifier_count,
            config_write_count,
            ..
        } => {
            if let Some(ttft_ms) = ttft_ms {
                set("ttftMs", serde_json::Value::Number(ttft_ms.clone()));
            }
            if let Some(otps) = otps {
                set("otps", serde_json::Value::Number(otps.clone()));
            }
            if let Some(is_p50) = is_p50 {
                set("isP50", serde_json::json!(is_p50));
            }
            if let Some(hook_duration_ms) = hook_duration_ms {
                set("hookDurationMs", serde_json::json!(hook_duration_ms));
            }
            if let Some(turn_duration_ms) = turn_duration_ms {
                set("turnDurationMs", serde_json::json!(turn_duration_ms));
            }
            if let Some(tool_duration_ms) = tool_duration_ms {
                set("toolDurationMs", serde_json::json!(tool_duration_ms));
            }
            if let Some(classifier_duration_ms) = classifier_duration_ms {
                set(
                    "classifierDurationMs",
                    serde_json::json!(classifier_duration_ms),
                );
            }
            if let Some(tool_count) = tool_count {
                set("toolCount", serde_json::json!(tool_count));
            }
            if let Some(hook_count) = hook_count {
                set("hookCount", serde_json::json!(hook_count));
            }
            if let Some(classifier_count) = classifier_count {
                set("classifierCount", serde_json::json!(classifier_count));
            }
            if let Some(config_write_count) = config_write_count {
                set("configWriteCount", serde_json::json!(config_write_count));
            }
        }
        SystemMessage::Thinking { content, .. } => {
            set("content", serde_json::json!(content));
        }
    }
    entry
}

/// Union→wire entry for one message, or `None` for messages CC never
/// persists. Mirrors CC `isLoggableMessage` (`utils/sessionStorage.ts:4351-4352`):
/// progress returns `false` there, and the `Transcript` type
/// (`sessionStorage.ts:101-106`) excludes progress from every JSONL write at
/// the type level. "Progress messages are NOT transcript messages. They are
/// ephemeral UI state and must not be persisted to the JSONL or participate
/// in the parentUuid chain" (`sessionStorage.ts:134-137`, CC #14373/#23537).
fn typed_message_entry(
    message: &crate::types::message::Message,
) -> Option<(serde_json::Value, String)> {
    use crate::types::message::{AssistantContent, Message, UserContent};

    // CC persists each message under its own uuid: `recordTranscript` dedups
    // with `messageSet.has(m.uuid)` (sessionStorage.ts:1416-1428) and chains
    // `parentUuid` from it. Minting a fresh uuid here made every write look
    // new — the dedup could never hit, so a message recorded from two paths
    // landed twice, and re-recording an existing prefix (which the logging
    // effect does on its first non-ignored pass) rewrote it under new ids and
    // re-parented the chain. Only a message with no identity of its own gets
    // a fresh uuid.
    let entry_uuid = {
        let uuid = message.uuid();
        if uuid.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            uuid.to_string()
        }
    };
    match message {
        Message::User(message) => {
            let is_meta = message.content.iter().any(|block| {
                matches!(
                    block,
                    UserContent::MetaText(_)
                        | UserContent::MetaImage { .. }
                        | UserContent::RawImage { is_meta: true, .. }
                        | UserContent::MetaDocument { .. }
                )
            });
            let content = message
                .content
                .iter()
                .map(|block| match block {
                    UserContent::Text(text) | UserContent::MetaText(text) => {
                        serde_json::json!({"type":"text","text":text})
                    }
                    UserContent::RawImage { block, .. } => block.clone(),
                    UserContent::Image { media_type, data }
                    | UserContent::MetaImage { media_type, data } => serde_json::json!({
                        "type":"image",
                        "source":{"type":"base64","media_type":media_type,"data":data}
                    }),
                    UserContent::Document { media_type, data }
                    | UserContent::MetaDocument { media_type, data } => serde_json::json!({
                        "type":"document",
                        "source":{"type":"base64","media_type":media_type,"data":data}
                    }),
                    UserContent::ToolResult(result) => serde_json::json!({
                        "type":"tool_result",
                        "tool_use_id":result.tool_use_id.0,
                        "content": if result.content_blocks.is_empty() {
                            serde_json::Value::String(result.content.clone())
                        } else {
                            serde_json::to_value(&result.content_blocks).unwrap_or_default()
                        },
                        "is_error":result.is_error
                    }),
                })
                .collect::<Vec<_>>();
            let mut entry = serde_json::json!({
                "type":"user",
                "uuid":entry_uuid,
                "timestamp":message.timestamp.to_rfc3339(),
                "isCompactSummary":message.is_compact_summary,
                "message":{"role":"user","content":content}
            });
            if is_meta {
                entry
                    .as_object_mut()
                    .expect("typed user entry is an object")
                    .insert("isMeta".to_string(), serde_json::Value::Bool(true));
            }
            if let Some(tool_use_result) = message.content.iter().find_map(|block| match block {
                UserContent::ToolResult(result) => result.tool_use_result.clone(),
                _ => None,
            }) {
                entry
                    .as_object_mut()
                    .expect("typed user entry is an object")
                    .insert("toolUseResult".to_string(), tool_use_result);
            }
            // CC persists the whole envelope (`types/message.ts:50-62`);
            // absent optional fields stay off the wire, matching
            // JSON.stringify dropping `undefined` members.
            {
                let envelope = entry
                    .as_object_mut()
                    .expect("typed user entry is an object");
                if message.is_visible_in_transcript_only {
                    envelope.insert(
                        "isVisibleInTranscriptOnly".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                if let Some(mcp_meta) = &message.mcp_meta {
                    envelope.insert("mcpMeta".to_string(), mcp_meta.clone());
                }
                if let Some(source_uuid) = &message.source_tool_assistant_uuid {
                    envelope.insert(
                        "sourceToolAssistantUUID".to_string(),
                        serde_json::Value::String(source_uuid.clone()),
                    );
                }
                if let Some(mode) = &message.permission_mode {
                    envelope.insert(
                        "permissionMode".to_string(),
                        serde_json::Value::String(mode.clone()),
                    );
                }
                if let Some(origin) = &message.origin {
                    envelope.insert("origin".to_string(), origin.clone());
                }
                if let Some(metadata) = &message.summarize_metadata {
                    if let Ok(value) = serde_json::to_value(metadata) {
                        envelope.insert("summarizeMetadata".to_string(), value);
                    }
                }
            }
            Some((entry, entry_uuid))
        }
        Message::Assistant(message) => {
            let identity = message.identity();
            // The ENVELOPE uuid, like every other arm — CC has exactly one uuid
            // per message and `insertMessageChain` spreads it (`...message`,
            // sessionStorage.ts:1048) so the row and the chain cursor
            // (`parentUuid = message.uuid`, `:1067`) are that same value.
            //
            // This arm used to prefer `identity.uuid`, on the assumption this
            // file's reader guarantees (`agent_transcript_entry_to_message`
            // restores the row uuid into BOTH the identity block and the
            // envelope field), but no producer holds to it: each mints the
            // two independently (`AssistantMessageIdentity::new` at
            // `types/message.rs:340` always calls `Uuid::new_v4`, and so does
            // `Default`), so `claude.rs:3825-3827` — the streamed per-block
            // assistant — carried two unrelated uuids and the JSONL row was
            // written under the one NOBODY ELSE holds. Every join against a
            // live row broke silently: `REPL.tsx:3521-3525`'s tombstone removal
            // (`repl.rs:3883` `remove_transcript_message(message.uuid())`)
            // could never match its row, so a streaming/model-fallback retry
            // left the discarded per-block assistants on disk to be replayed on
            // resume.
            let assistant_uuid = entry_uuid;
            let api_message_id = identity
                .and_then(|identity| identity.api_message_id.clone())
                .unwrap_or_else(|| format!("msg_{assistant_uuid}"));
            let content = message
                .content
                .iter()
                .filter(|block| !matches!(block, AssistantContent::MessageIdentity(_)))
                .map(|block| match block {
                    AssistantContent::Text(text) => serde_json::json!({"type":"text","text":text}),
                    AssistantContent::Thinking { text, signature } => serde_json::json!({
                        "type":"thinking","thinking":text,"signature":signature
                    }),
                    AssistantContent::RedactedThinking { data } => serde_json::json!({
                        "type":"redacted_thinking","data":data
                    }),
                    AssistantContent::ToolUse(tool) => serde_json::json!({
                        "type":"tool_use","id":tool.id.0,"name":tool.name,"input":tool.input
                    }),
                    AssistantContent::ServerToolUse(tool) => serde_json::json!({
                        "type":"server_tool_use","id":tool.id.0,"name":tool.name,"input":tool.input
                    }),
                    AssistantContent::WebSearchToolResult { tool_use_id, content } => serde_json::json!({
                        "type":"web_search_tool_result","tool_use_id":tool_use_id.0,"content":content
                    }),
                    // Restore the original block shape (CC utils/advisor.ts:
                    // 16-32) — writing it as a plain text block lost the id and
                    // the content discriminant on every round-trip.
                    AssistantContent::Advisor {
                        tool_use_id,
                        content,
                    } => serde_json::json!({
                        "type":"advisor_tool_result",
                        "tool_use_id":tool_use_id.0,
                        "content":content
                    }),
                    AssistantContent::MessageIdentity(_) => {
                        unreachable!("assistant identity was filtered from transcript content")
                    }
                })
                .collect::<Vec<_>>();
            let mut entry = serde_json::json!({
                "type":"assistant",
                "uuid":assistant_uuid,
                "timestamp":message.timestamp.to_rfc3339(),
                "message":{
                    "id":api_message_id,
                    "type":"message",
                    "role":"assistant",
                    "content":content,
                    "model":message.model,
                    "stop_reason":message.stop_reason,
                    "usage":message.usage
                }
            });
            if let Some(request_id) = identity.and_then(|identity| identity.request_id.clone()) {
                entry
                    .as_object_mut()
                    .expect("typed assistant entry is an object")
                    .insert(
                        "requestId".to_string(),
                        serde_json::Value::String(request_id),
                    );
            }
            Some((entry, assistant_uuid))
        }
        Message::System(message) => Some((system_entry_json(message, &entry_uuid), entry_uuid)),
        Message::Attachment(message) => {
            let payload = message.payload_for_transcript();
            // CC `isLoggableMessage` (sessionStorage.ts:4351-4367) — the other
            // half of the rule whose `progress` arm is implemented below; the
            // gate itself lives in `attachment_row_is_withheld` because every
            // attachment-typed row takes it, not just this arm.
            if attachment_row_is_withheld(&payload) {
                return None;
            }
            Some((
                serde_json::json!({
                    "type":"attachment",
                    "uuid":entry_uuid,
                    "timestamp":message.timestamp.to_rfc3339(),
                    // Verbatim when this came off a transcript, so undeclared
                    // fields are not dropped on the way back out.
                    "attachment":payload
                }),
                entry_uuid,
            ))
        }
        // A hook result at CC runtime is an ATTACHMENT-typed row:
        // `createAttachmentMessage` (utils/attachments.ts:3201-3210) is the
        // only construction the hook pipeline uses (hooks.ts:2168-2683,
        // sessionStart.ts:164-171/:221-228), and the `hook_result` union
        // member (types/message.ts:119) has no runtime producer anywhere in
        // the tree — CC's own consumers discriminate the values with
        // `message?.type === 'attachment'` (hooks.ts:2241/:2281). So this arm
        // keeps writing `"type":"attachment"` (CC's real wire shape) and
        // takes the same `isLoggableMessage` withholding as the `Attachment`
        // arm above, because CC keys that filter on the WIRE type
        // (sessionStorage.ts:4357). Without the gate an external build wrote
        // hook rows CC withholds. Latent today on the loop paths (#158's
        // is_recordable_message stops the AgentTool sidechain, and query.rs
        // has no HookResult send site), but the SessionStart projection
        // (session_start.rs) mints these into REPL history, which lands here
        // through `record_typed_message`.
        Message::HookResult(message) => {
            if attachment_row_is_withheld(&message.attachment) {
                return None;
            }
            Some((
                serde_json::json!({
                    "type":"attachment",
                    "uuid":message.uuid,
                    "timestamp":message.timestamp.to_rfc3339(),
                    "attachment":message.attachment
                }),
                message.uuid.clone(),
            ))
        }
        // CC never writes progress to any JSONL: both `insertMessageChain`
        // callers (`recordTranscript` sessionStorage.ts:1414/1433,
        // `recordSidechainTranscript` :1451-1462) run
        // `cleanMessagesForLogging`, whose `isLoggableMessage` filter drops
        // `type === 'progress'` (:4351-4352). The read side only bridges
        // pre-#24099 legacy entries out of the parentUuid chain
        // (`progressBridge`, :3616-3645) and never loads them
        // (`isTranscriptMessage`, :139-145). The earlier `data: null`
        // stepping-stone entry is gone with this arm.
        Message::Progress(_) => None,
    }
}

/// The attachment half of CC `isLoggableMessage`
/// (`utils/sessionStorage.ts:4351-4367`). Attachments are withheld from
/// non-ant transcripts; the source's own comment gives the reason: "they have
/// sensitive info for training that we don't want exposed to the public". The
/// single exception is hook output, and only behind the env flag, because it
/// is user-configured content that is useful on resume.
///
/// `getUserType() !== 'ant'` maps to the build audience here — the same
/// projection `record_transcript`'s `user_type` field already uses. Note it
/// is a compile-time feature rather than CC's runtime lookup, so the two
/// audiences are covered by `just test-all-audiences`, not by one run.
///
/// Shared by BOTH arms of `typed_message_entry` that emit
/// `"type":"attachment"` rows (`Message::Attachment` and
/// `Message::HookResult`), because CC keys the filter on the wire type,
/// not on which union member carried the payload.
fn attachment_row_is_withheld(payload: &serde_json::Value) -> bool {
    if crate::utils::build_profile::build_audience().is_internal() {
        return false;
    }
    let is_hook_context =
        payload.get("type").and_then(|value| value.as_str()) == Some("hook_additional_context");
    let keep_hook_context = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT")
            .as_deref(),
    );
    !(is_hook_context && keep_hook_context)
}

/// Maps to: CC `utils/sessionStorage.ts#isTranscriptMessage`.
/// Progress and session metadata never enter a fork's transcript message set.
pub fn is_transcript_message(entry: &serde_json::Value) -> bool {
    matches!(
        entry.get("type").and_then(serde_json::Value::as_str),
        Some("user" | "assistant" | "attachment" | "system")
    )
}

fn is_chain_participant(msg: &serde_json::Value) -> bool {
    msg.get("type").and_then(|v| v.as_str()) != Some("progress")
}

/// Maps to: CC `EPHEMERAL_PROGRESS_TYPES` / `isEphemeralToolProgress`
/// (`utils/sessionStorage.ts:180-196`) — high-frequency tool progress ticks
/// that `REPL.tsx:3468-3494` replaces in place instead of appending.
/// `powershell_progress` has no Rust producer (no PowerShell tool) and the
/// PROACTIVE/KAIROS-gated `sleep_progress` member has no producer in the CC
/// tree either (ant-only/DCE) — both kept for wire fidelity.
pub fn is_ephemeral_tool_progress(data_type: &str) -> bool {
    matches!(
        data_type,
        "bash_progress" | "powershell_progress" | "mcp_progress"
    )
}

fn insert_message_chain(
    project_path: &str,
    session_id: &str,
    messages: &[&serde_json::Value],
    starting_parent_uuid: Option<&str>,
    agent_id: Option<&str>,
    is_sidechain: bool,
    stamp: &SessionStamp,
) -> anyhow::Result<()> {
    let mut parent_uuid: Option<String> = starting_parent_uuid.map(String::from);

    if session_id == crate::bootstrap::state::get_session_id()
        && messages.iter().any(|message| {
            matches!(
                message.get("type").and_then(serde_json::Value::as_str),
                Some("user" | "assistant")
            )
        })
    {
        materialize_session_file(project_path, session_id)?;
    }

    for msg in messages {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let is_boundary = msg_type == "system"
            && msg.get("subtype").and_then(|v| v.as_str()) == Some(SUBTYPE_COMPACT_BOUNDARY);

        let mut entry = (*msg).clone();
        if let Some(obj) = entry.as_object_mut() {
            // `parentUuid` must be the first serialized key. CC's bounded
            // large-transcript walker relies on the stable JSONL prefix.
            let original = std::mem::take(obj);
            let mut ordered = serde_json::Map::new();
            ordered.insert(
                "parentUuid".to_string(),
                if is_boundary {
                    serde_json::Value::Null
                } else {
                    parent_uuid
                        .as_ref()
                        .map(|parent| serde_json::Value::String(parent.clone()))
                        .unwrap_or(serde_json::Value::Null)
                },
            );
            if is_boundary {
                ordered.insert(
                    "logicalParentUuid".to_string(),
                    original
                        .get("logicalParentUuid")
                        .cloned()
                        .unwrap_or_else(|| {
                            parent_uuid
                                .as_ref()
                                .map(|parent| serde_json::Value::String(parent.clone()))
                                .unwrap_or(serde_json::Value::Null)
                        }),
                );
            }
            ordered.extend(
                original
                    .into_iter()
                    .filter(|(key, _)| key != "parentUuid" && key != "logicalParentUuid"),
            );
            *obj = ordered;

            obj.insert(
                "sessionId".to_string(),
                serde_json::Value::String(stamp.session_id.clone()),
            );
            obj.insert(
                "cwd".to_string(),
                serde_json::Value::String(stamp.cwd.clone()),
            );
            obj.insert(
                "version".to_string(),
                serde_json::Value::String(stamp.version.clone()),
            );
            obj.insert(
                "userType".to_string(),
                serde_json::Value::String(stamp.user_type.clone()),
            );
            obj.insert(
                "entrypoint".to_string(),
                serde_json::Value::String(stamp.entrypoint.clone()),
            );
            if let Some(ref team_name) = stamp.team_name {
                obj.insert(
                    "teamName".to_string(),
                    serde_json::Value::String(team_name.clone()),
                );
            }
            if let Some(ref agent_name) = stamp.agent_name {
                obj.insert(
                    "agentName".to_string(),
                    serde_json::Value::String(agent_name.clone()),
                );
            }
            if let Some(ref branch) = stamp.git_branch {
                obj.insert(
                    "gitBranch".to_string(),
                    serde_json::Value::String(branch.clone()),
                );
            }
            if let Some(ref slug) = stamp.slug {
                obj.insert("slug".to_string(), serde_json::Value::String(slug.clone()));
            }
            if is_sidechain {
                obj.insert("isSidechain".to_string(), serde_json::Value::Bool(true));
                if let Some(agent_id) = agent_id {
                    obj.insert(
                        "agentId".to_string(),
                        serde_json::Value::String(agent_id.to_string()),
                    );
                }
            }
        }

        append_entry(project_path, session_id, &entry)?;

        if is_chain_participant(msg) {
            if let Some(uuid) = msg.get("uuid").and_then(|v| v.as_str()) {
                parent_uuid = Some(uuid.to_string());
            }
        }
    }

    Ok(())
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub fn save_custom_title(
    session_id: &str,
    custom_title: &str,
    full_path: Option<&Path>,
) -> anyhow::Result<()> {
    let entry = serde_json::json!({
        "type": "custom-title",
        "customTitle": custom_title,
        "sessionId": session_id,
    });
    let path = full_path.map(Path::to_path_buf).unwrap_or_else(|| {
        active_session_file_path(
            &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
            session_id,
        )
    });
    append_entry_to_file(&path, &entry)?;
    if session_id == crate::bootstrap::state::get_session_id() {
        cache_session_title(custom_title);
    }
    Ok(())
}

/// Maps to: CC `sessionStorage.ts:2667-2673` `saveAiGeneratedTitle`.
///
/// A distinct `ai-title` entry (vs reusing `custom-title`) is load-bearing:
/// readers prefer the `customTitle` field, so a user rename always wins;
/// resume metadata re-append only carries `custom-title` entries, so a stale
/// AI title never clobbers a mid-session rename; and the current-session
/// title cache stays rename-owned.
pub fn save_ai_generated_title(session_id: &str, ai_title: &str) -> anyhow::Result<()> {
    let entry = serde_json::json!({
        "type": "ai-title",
        "aiTitle": ai_title,
        "sessionId": session_id,
    });
    let path = active_session_file_path(
        &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
        session_id,
    );
    append_entry_to_file(&path, &entry)
}

/// Maps to CC speculation acceptance JSONL bookkeeping.
pub fn record_speculation_accept(time_saved_ms: u64) -> anyhow::Result<()> {
    let session_id = crate::bootstrap::state::get_session_id();
    let path = active_session_file_path(
        &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
        &session_id,
    );
    append_entry_to_file(
        &path,
        &serde_json::json!({
            "type": "speculation-accept",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "timeSavedMs": time_saved_ms,
        }),
    )
}

pub fn save_tag(project_path: &str, session_id: &str, tag: &str) -> anyhow::Result<()> {
    let entry = serde_json::json!({
        "type": "tag",
        "tag": tag,
        "sessionId": session_id,
    });
    let path = active_session_file_path(project_path, session_id);
    append_entry_to_file(&path, &entry)?;
    if session_id == crate::bootstrap::state::get_session_id() {
        if let Ok(mut meta) = get_project().current_session_meta.write() {
            meta.tag = (!tag.is_empty()).then(|| tag.to_string());
        }
    }
    Ok(())
}

/// Maps to: CC `utils/sessionStorage.ts:2819-2839`
/// `saveAgentName(sessionId, agentName, fullPath?)`. Rename may target an
/// adopted cross-project transcript, so the
/// optional concrete path must take precedence over cwd-derived storage.
pub fn save_agent_name(
    session_id: &str,
    agent_name: &str,
    full_path: Option<&Path>,
) -> anyhow::Result<()> {
    let entry = serde_json::json!({
        "type": "agent-name",
        "agentName": agent_name,
        "sessionId": session_id,
    });
    let path = full_path.map(Path::to_path_buf).unwrap_or_else(|| {
        active_session_file_path(
            &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
            session_id,
        )
    });
    append_entry_to_file(&path, &entry)?;
    if session_id == crate::bootstrap::state::get_session_id() {
        if let Ok(mut meta) = get_project().current_session_meta.write() {
            meta.agent_name = Some(agent_name.to_string());
        }
        crate::utils::concurrent_sessions::update_session_name(Some(agent_name));
    }
    Ok(())
}

/// Maps to: CC `utils/sessionStorage.ts#saveAgentColor:2838-2854`.
pub fn save_agent_color(
    session_id: &str,
    agent_color: &str,
    full_path: Option<&Path>,
) -> anyhow::Result<()> {
    let path = full_path.map(Path::to_path_buf).unwrap_or_else(|| {
        // CC getTranscriptPathForSession:207-225 uses the active session's
        // project directory, not a possibly stale materialization pointer.
        if session_id == crate::bootstrap::state::get_session_id() {
            get_transcript_path(None)
        } else {
            get_session_file_path(
                &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
                session_id,
            )
        }
    });
    append_entry_to_file(
        &path,
        &serde_json::json!({
            "type": "agent-color",
            "agentColor": agent_color,
            "sessionId": session_id,
        }),
    )?;
    if session_id == crate::bootstrap::state::get_session_id() {
        if let Ok(mut meta) = get_project().current_session_meta.write() {
            meta.agent_color = Some(agent_color.to_string());
        }
    }
    // CC :2853 logEvent('tengu_agent_color_set', {}) is covered by the
    // established analytics exclusion; local persistence/cache remain live.
    Ok(())
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Clone, Debug, Default)]
pub struct SessionMetadataCache {
    pub session_id: String,
    pub custom_title: Option<String>,
    pub tag: Option<String>,
    pub agent_name: Option<String>,
    pub agent_color: Option<String>,
    pub agent_setting: Option<String>,
    pub mode: Option<String>,
    pub worktree_session: Option<serde_json::Value>,
    pub last_prompt: Option<String>,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub pr_repository: Option<String>,
}

/// Process-level cache mirroring CC `getProject()` session metadata fields
/// (`currentSessionTitle`, `currentSessionTag`, …, `sessionFile`).
#[derive(Clone, Debug, Default)]
struct CurrentSessionMeta {
    title: Option<String>,
    tag: Option<String>,
    agent_name: Option<String>,
    agent_color: Option<String>,
    last_prompt: Option<String>,
    agent_setting: Option<String>,
    mode: Option<String>,
    worktree: Option<serde_json::Value>,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    pr_repository: Option<String>,
    /// Maps to: CC `project.sessionFile` — null until first materialize / adopt.
    session_file: Option<PathBuf>,
    /// Maps to: CC `Project.pendingEntries` before first user/assistant.
    pending_entries: Vec<serde_json::Value>,
    /// Rust writer cursor corresponding to CC `useLogMessages`' returned
    /// `startingParentUuid` across incremental actor events.
    last_message_uuid: Option<String>,
}

/// Maps to: CC `clearSessionMetadata()`.
///
/// Clears cached title/tag/agent/mode/worktree/PR so `/clear`'s new session
/// does not inherit the previous session's identity.
pub fn clear_session_metadata() {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        *meta = CurrentSessionMeta::default();
    }
}

/// Maps to: CC `resetSessionFilePointer()`.
///
/// After `regenerateSessionId`, the new JSONL is created lazily on the first
/// user/assistant message — clear the in-memory pointer so exit cleanup does
/// not re-append metadata onto the old file.
pub fn reset_session_file_pointer() {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.session_file = None;
        meta.pending_entries.clear();
        meta.last_message_uuid = None;
    }
}

/// Maps to: CC `restoreSessionMetadata(meta)`.
pub fn restore_session_metadata(cache: &SessionMetadataCache) {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        // `??=` is load-bearing: a startup `--name` title wins over the title
        // restored from the selected transcript.
        if meta.title.is_none() {
            meta.title = cache.custom_title.clone().filter(|title| !title.is_empty());
        }
        if cache.tag.is_some() {
            meta.tag = cache.tag.clone().filter(|tag| !tag.is_empty());
        }
        if cache.agent_name.is_some() {
            meta.agent_name = cache.agent_name.clone();
        }
        if cache.agent_color.is_some() {
            meta.agent_color = cache.agent_color.clone();
        }
        if cache.agent_setting.is_some() {
            meta.agent_setting = cache.agent_setting.clone();
        }
        if cache.mode.is_some() {
            meta.mode = cache.mode.clone();
        }
        if cache.worktree_session.is_some() {
            meta.worktree = cache.worktree_session.clone();
        }
        if cache.pr_number.is_some() {
            meta.pr_number = cache.pr_number;
        }
        if cache.pr_url.is_some() {
            meta.pr_url = cache.pr_url.clone();
        }
        if cache.pr_repository.is_some() {
            meta.pr_repository = cache.pr_repository.clone();
        }
    }
}

/// Maps to: CC `adoptResumedSessionFile()`.
pub fn adopt_resumed_session_file() -> anyhow::Result<()> {
    let session_id = crate::bootstrap::state::get_session_id();
    let path = crate::bootstrap::state::get_session_project_dir()
        .map(|dir| dir.join(format!("{session_id}.jsonl")))
        .unwrap_or_else(|| {
            get_session_file_path(
                &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
                &session_id,
            )
        });
    let loaded = load_session_structured_from_path(&path);
    let leaf_uuid = loaded
        .message_order
        .iter()
        .rev()
        .find(|uuid| loaded.leaf_uuids.contains(*uuid))
        .cloned();
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.session_file = Some(path);
        meta.pending_entries.clear();
        meta.last_message_uuid = leaf_uuid;
    }
    let mut cache = get_current_session_metadata();
    re_append_session_metadata_inner("", &mut cache, true)
}

/// Maps to: CC `saveMode(mode)` — cache-only until materialize/exit re-append.
pub fn save_mode(mode: &str) {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.mode = Some(mode.to_string());
    }
}

/// Maps to: CC `saveAgentSetting(agentSetting)` — cache-only; materialized on
/// first user message via `materializeSessionFile`.
pub fn save_agent_setting(agent_setting: impl Into<String>) {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.agent_setting = Some(agent_setting.into());
    }
}

/// Maps to: CC `saveWorktreeState(session)` — cache-only until materialize/exit.
pub fn save_worktree_state(worktree_session: serde_json::Value) {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.worktree = Some(worktree_session);
    }
}

/// Maps to: CC `cacheSessionTitle` / project `currentSessionTitle` setter.
pub fn cache_session_title(custom_title: impl Into<String>) {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.title = Some(custom_title.into());
    }
}

/// Snapshot of the process-level session metadata cache (for tests / UI).
pub fn get_current_session_metadata() -> SessionMetadataCache {
    let meta = get_project()
        .current_session_meta
        .read()
        .map(|m| m.clone())
        .unwrap_or_default();
    SessionMetadataCache {
        session_id: crate::bootstrap::state::get_session_id(),
        custom_title: meta.title,
        tag: meta.tag,
        agent_name: meta.agent_name,
        agent_color: meta.agent_color,
        agent_setting: meta.agent_setting,
        mode: meta.mode,
        worktree_session: meta.worktree,
        last_prompt: meta.last_prompt,
        pr_number: meta.pr_number,
        pr_url: meta.pr_url,
        pr_repository: meta.pr_repository,
    }
}

/// Test helper: seed metadata fields without disk writes.
#[cfg(test)]
pub fn set_current_session_metadata_for_test(
    title: Option<&str>,
    tag: Option<&str>,
    agent_name: Option<&str>,
    session_file: Option<PathBuf>,
) {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.title = title.map(str::to_string);
        meta.tag = tag.map(str::to_string);
        meta.agent_name = agent_name.map(str::to_string);
        meta.session_file = session_file;
    }
}

/// Test helper: whether the session file pointer is set.
#[cfg(test)]
pub fn current_session_file_for_test() -> Option<PathBuf> {
    get_project()
        .current_session_meta
        .read()
        .ok()
        .and_then(|m| m.session_file.clone())
}

/// Maps to CC `utils/sessionStorage.ts#linkSessionToPR`.
pub fn link_session_to_pr(
    session_id: &str,
    pr_number: u64,
    pr_url: &str,
    pr_repository: &str,
    full_path: Option<&Path>,
) -> anyhow::Result<()> {
    if !is_session_write_enabled() {
        anyhow::bail!("session persistence is disabled by COMETIX_WRITE_ENABLED=0");
    }
    let path = full_path.map(Path::to_path_buf).unwrap_or_else(|| {
        get_session_file_path(
            &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
            session_id,
        )
    });
    append_entry_to_file(
        &path,
        &serde_json::json!({
            "type": "pr-link",
            "sessionId": session_id,
            "prNumber": pr_number,
            "prUrl": pr_url,
            "prRepository": pr_repository,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
    )?;
    if session_id == crate::bootstrap::state::get_session_id() {
        if let Ok(mut meta) = get_project().current_session_meta.write() {
            meta.pr_number = Some(pr_number);
            meta.pr_url = Some(pr_url.to_string());
            meta.pr_repository = Some(pr_repository.to_string());
        }
    }
    Ok(())
}

/// Maps to CC `utils/sessionStorage.ts#getTranscriptPath()` with Rust's
/// explicit agent context in place of the source AsyncLocal lookup.
pub fn get_transcript_path(agent_id: Option<&str>) -> PathBuf {
    if let Some(agent_id) = agent_id.filter(|id| !id.is_empty()) {
        return get_agent_transcript_path(agent_id);
    }
    // CC :202-205 honors switchActiveSession's project directory even before
    // the native materialization/adoption cache has a session_file pointer.
    if let Some(project_dir) = crate::bootstrap::state::get_session_project_dir() {
        return project_dir.join(format!(
            "{}.jsonl",
            crate::bootstrap::state::get_session_id()
        ));
    }
    let project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    get_session_file_path(&project_path, &crate::bootstrap::state::get_session_id())
}

/// Maps to: CC `utils/sessionStorage.ts#getTranscriptPathForSession`.
pub fn get_transcript_path_for_session(session_id: &str) -> PathBuf {
    if session_id == crate::bootstrap::state::get_session_id() {
        return get_transcript_path(None);
    }
    get_session_file_path(
        &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
        session_id,
    )
}

/// Maps to: CC exported `reAppendSessionMetadata()` used by graceful exit.
pub fn re_append_session_metadata() -> anyhow::Result<()> {
    let has_session_file = get_project()
        .current_session_meta
        .read()
        .map(|meta| meta.session_file.is_some())
        .unwrap_or(false);
    if !has_session_file {
        return Ok(());
    }
    let mut cache = get_current_session_metadata();
    let project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    re_append_session_metadata_inner(&project_path, &mut cache, false)
}

fn re_append_session_metadata_inner(
    project_path: &str,
    cache: &mut SessionMetadataCache,
    skip_title_refresh: bool,
) -> anyhow::Result<()> {
    let path = active_session_file_path(project_path, &cache.session_id);
    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    if file_size > 0 {
        refresh_cache_from_tail(&path, file_size, cache, skip_title_refresh);
    }
    if cache.session_id == crate::bootstrap::state::get_session_id() {
        replace_current_session_metadata_from_cache(cache);
    }

    let sid = cache.session_id.clone();
    let mut entries = Vec::new();

    if let Some(ref prompt) = cache.last_prompt {
        entries.push(serde_json::json!({
            "type": "last-prompt",
            "lastPrompt": prompt,
            "sessionId": sid,
        }));
    }
    if let Some(ref title) = cache.custom_title {
        entries.push(serde_json::json!({
            "type": "custom-title",
            "customTitle": title,
            "sessionId": sid,
        }));
    }
    if let Some(ref tag) = cache.tag {
        entries.push(serde_json::json!({
            "type": "tag",
            "tag": tag,
            "sessionId": sid,
        }));
    }
    if let Some(ref name) = cache.agent_name {
        entries.push(serde_json::json!({
            "type": "agent-name",
            "agentName": name,
            "sessionId": sid,
        }));
    }
    if let Some(ref color) = cache.agent_color {
        entries.push(serde_json::json!({
            "type": "agent-color",
            "agentColor": color,
            "sessionId": sid,
        }));
    }
    if let Some(ref setting) = cache.agent_setting {
        entries.push(serde_json::json!({
            "type": "agent-setting",
            "agentSetting": setting,
            "sessionId": sid,
        }));
    }
    if let Some(ref mode) = cache.mode {
        entries.push(serde_json::json!({
            "type": "mode",
            "mode": mode,
            "sessionId": sid,
        }));
    }
    if let Some(ref worktree_session) = cache.worktree_session {
        entries.push(serde_json::json!({
            "type": "worktree-state",
            "worktreeSession": worktree_session,
            "sessionId": sid,
        }));
    }
    if let (Some(number), Some(url), Some(repository)) =
        (cache.pr_number, &cache.pr_url, &cache.pr_repository)
    {
        entries.push(serde_json::json!({
            "type": "pr-link",
            "sessionId": sid,
            "prNumber": number,
            "prUrl": url,
            "prRepository": repository,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }));
    }

    for entry in entries {
        append_entry_to_file(&path, &entry)?;
    }
    Ok(())
}

fn replace_current_session_metadata_from_cache(cache: &SessionMetadataCache) {
    if let Ok(mut meta) = get_project().current_session_meta.write() {
        meta.title = cache.custom_title.clone();
        meta.tag = cache.tag.clone();
        meta.agent_name = cache.agent_name.clone();
        meta.agent_color = cache.agent_color.clone();
        meta.last_prompt = cache.last_prompt.clone();
        meta.agent_setting = cache.agent_setting.clone();
        meta.mode = cache.mode.clone();
        meta.worktree = cache.worktree_session.clone();
        meta.pr_number = cache.pr_number;
        meta.pr_url = cache.pr_url.clone();
        meta.pr_repository = cache.pr_repository.clone();
    }
}

fn refresh_cache_from_tail(
    path: &Path,
    file_size: u64,
    cache: &mut SessionMetadataCache,
    skip_title_refresh: bool,
) {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };

    let tail_size = file_size.min(LITE_READ_BUF_SIZE) as usize;
    let tail_offset = file_size - tail_size as u64;

    if file.seek(SeekFrom::Start(tail_offset)).is_err() {
        return;
    }

    let mut buf = vec![0u8; tail_size];
    if file.read_exact(&mut buf).is_err() {
        return;
    }

    let tail_str = String::from_utf8_lossy(&buf);

    if !skip_title_refresh {
        for line in tail_str.lines().rev() {
            // custom-title
            if line.starts_with("{\"type\":\"custom-title\"") {
                if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(title) = entry.get("customTitle").and_then(|v| v.as_str()) {
                        cache.custom_title = if title.is_empty() {
                            None
                        } else {
                            Some(title.to_string())
                        };
                    }
                }
                break;
            }
        }
    }

    for line in tail_str.lines().rev() {
        if line.starts_with("{\"type\":\"tag\"") {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(tag) = entry.get("tag").and_then(|v| v.as_str()) {
                    cache.tag = if tag.is_empty() {
                        None
                    } else {
                        Some(tag.to_string())
                    };
                }
            }
            break;
        }
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub struct PreservedSegment {
    pub head_uuid: String,
    pub tail_uuid: String,
    pub anchor_uuid: String,
}

///    - head.parentUuid = anchorUuid
pub fn apply_preserved_segment_relinks(session: &mut LoadedSession) {
    if session.compact_boundaries.is_empty() {
        return;
    }

    let mut last_seg: Option<&PreservedSegment> = None;
    let mut last_seg_boundary_idx: Option<usize> = None;
    let mut absolute_last_boundary_idx: usize = 0;

    for boundary in &session.compact_boundaries {
        absolute_last_boundary_idx = boundary.entry_index;
        if boundary.preserved_segment.is_some() {
            last_seg = boundary.preserved_segment.as_ref();
            last_seg_boundary_idx = Some(boundary.entry_index);
        }
    }

    let Some(seg) = last_seg else {
        prune_before_boundary(session, absolute_last_boundary_idx, &HashSet::new());
        return;
    };
    let seg = seg.clone();

    let seg_is_live = last_seg_boundary_idx == Some(absolute_last_boundary_idx);

    let mut preserved_uuids: HashSet<String> = HashSet::new();

    if seg_is_live {
        let mut walk_seen: HashSet<String> = HashSet::new();
        let mut cur_uuid = Some(seg.tail_uuid.clone());
        let mut reached_head = false;

        while let Some(uuid) = cur_uuid {
            if walk_seen.contains(&uuid) {
                break;
            }
            walk_seen.insert(uuid.clone());
            preserved_uuids.insert(uuid.clone());

            if uuid == seg.head_uuid {
                reached_head = true;
                break;
            }

            cur_uuid = session
                .messages
                .get(&uuid)
                .and_then(|m| m.get("parentUuid"))
                .and_then(|v| v.as_str())
                .map(String::from);
        }

        if !reached_head {
            tracing::warn!(
                "preserved segment walk broken: tail={} head={} walked={}",
                seg.tail_uuid,
                seg.head_uuid,
                walk_seen.len()
            );
            return;
        }

        // ── Relink: head.parentUuid = anchorUuid ──
        if let Some(head_msg) = session.messages.get_mut(&seg.head_uuid) {
            if let Some(obj) = head_msg.as_object_mut() {
                obj.insert(
                    "parentUuid".to_string(),
                    serde_json::Value::String(seg.anchor_uuid.clone()),
                );
            }
        }

        let uuids_to_reparent: Vec<String> = session
            .messages
            .iter()
            .filter(|(uuid, msg)| {
                msg.get("parentUuid").and_then(|v| v.as_str()) == Some(&seg.anchor_uuid)
                    && uuid.as_str() != seg.head_uuid
            })
            .map(|(uuid, _)| uuid.clone())
            .collect();

        for uuid in &uuids_to_reparent {
            if let Some(msg) = session.messages.get_mut(uuid) {
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert(
                        "parentUuid".to_string(),
                        serde_json::Value::String(seg.tail_uuid.clone()),
                    );
                }
            }
        }

        for uuid in &preserved_uuids {
            let Some(msg) = session.messages.get_mut(uuid) else {
                continue;
            };
            if msg.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                continue;
            }
            if let Some(obj) = msg.as_object_mut() {
                if let Some(message_obj) = obj.get_mut("message").and_then(|m| m.as_object_mut()) {
                    let zeroed_usage = serde_json::json!({
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                    });
                    message_obj.insert("usage".to_string(), zeroed_usage);
                }
            }
        }
    }

    prune_before_boundary(session, absolute_last_boundary_idx, &preserved_uuids);
}

fn prune_before_boundary(
    session: &mut LoadedSession,
    boundary_entry_index: usize,
    preserved_uuids: &HashSet<String>,
) {
    let entry_index: HashMap<String, usize> = session
        .message_order
        .iter()
        .enumerate()
        .map(|(i, uuid)| (uuid.clone(), i))
        .collect();

    let to_delete: Vec<String> = session
        .messages
        .keys()
        .filter(|uuid| {
            if let Some(&idx) = entry_index.get(uuid.as_str()) {
                idx < boundary_entry_index && !preserved_uuids.contains(uuid.as_str())
            } else {
                false
            }
        })
        .cloned()
        .collect();

    for uuid in &to_delete {
        session.messages.remove(uuid);
    }

    session
        .message_order
        .retain(|uuid| session.messages.contains_key(uuid));

    session.leaf_uuids = compute_leaf_uuids(&session.messages, &session.message_order);
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

const METADATA_MARKERS: &[&[u8]] = &[
    b"\"type\":\"summary\"",
    b"\"type\":\"custom-title\"",
    b"\"type\":\"tag\"",
    b"\"type\":\"agent-name\"",
    b"\"type\":\"agent-color\"",
    b"\"type\":\"agent-setting\"",
    b"\"type\":\"mode\"",
    b"\"type\":\"worktree-state\"",
    b"\"type\":\"pr-link\"",
];

const MAX_CARRY_SIZE: usize = 64 * 1024;

pub fn scan_pre_boundary_metadata(path: &Path, end_offset: u64) -> Vec<String> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let mut metadata_lines: Vec<String> = Vec::new();
    let mut carry: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; TRANSCRIPT_READ_CHUNK_SIZE];
    let mut offset: u64 = 0;

    while offset < end_offset {
        let to_read = ((end_offset - offset) as usize).min(TRANSCRIPT_READ_CHUNK_SIZE);
        let buf = &mut chunk[..to_read];
        if file.read_exact(buf).is_err() {
            break;
        }

        let search_buf = if carry.is_empty() {
            buf.to_vec()
        } else {
            let mut combined = std::mem::take(&mut carry);
            combined.extend_from_slice(buf);
            combined
        };

        let has_any_marker = METADATA_MARKERS
            .iter()
            .any(|m| search_buf.windows(m.len()).any(|w| w == *m));

        if has_any_marker {
            let mut line_start = 0;
            for (i, &byte) in search_buf.iter().enumerate() {
                if byte == b'\n' {
                    let line_bytes = &search_buf[line_start..i];
                    for marker in METADATA_MARKERS {
                        if line_bytes.windows(marker.len()).any(|w| w == *marker) {
                            if let Ok(line) = std::str::from_utf8(line_bytes) {
                                metadata_lines.push(line.to_string());
                            }
                            break;
                        }
                    }
                    line_start = i + 1;
                }
            }
            carry = search_buf[line_start..].to_vec();
        } else {
            let last_nl = search_buf.iter().rposition(|&b| b == b'\n');
            carry = match last_nl {
                Some(pos) => search_buf[pos + 1..].to_vec(),
                None => search_buf,
            };
        }

        if carry.len() > MAX_CARRY_SIZE {
            carry.clear();
        }

        offset += to_read as u64;
    }

    if !carry.is_empty() {
        for marker in METADATA_MARKERS {
            if carry.windows(marker.len()).any(|w| w == *marker) {
                if let Ok(line) = std::str::from_utf8(&carry) {
                    metadata_lines.push(line.to_string());
                }
                break;
            }
        }
    }

    metadata_lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::{Value, json};
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn save_agent_color_matches_official_target_cache_and_error_order() {
        // CC sessionStorage.ts:2838-2854: explicit path wins, current-only
        // cache, and append failures prevent cache changes.
        struct Restore {
            session_id: String,
            project_dir: Option<PathBuf>,
            meta: CurrentSessionMeta,
            root: PathBuf,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                *get_project().current_session_meta.write().unwrap() = self.meta.clone();
                crate::bootstrap::state::switch_session(&self.session_id, self.project_dir.clone());
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        let restore = Restore {
            session_id: crate::bootstrap::state::get_session_id(),
            project_dir: crate::bootstrap::state::get_session_project_dir(),
            meta: get_project().current_session_meta.read().unwrap().clone(),
            root: std::env::temp_dir().join(format!("cometix-save-color-{}", Uuid::new_v4())),
        };
        clear_session_metadata();
        crate::bootstrap::state::switch_session("color-active", Some(restore.root.clone()));
        let current = restore.root.join("color-active.jsonl");
        let stale = restore.root.join("stale-session-file.jsonl");
        get_project()
            .current_session_meta
            .write()
            .unwrap()
            .session_file = Some(stale.clone());
        assert_eq!(get_transcript_path(None), current);
        save_agent_color("color-active", "blue", None).unwrap();
        assert!(current.exists());
        assert!(!stale.exists());
        assert_eq!(
            get_current_session_metadata().agent_color.as_deref(),
            Some("blue")
        );

        let other = restore.root.join("other-project/explicit.jsonl");
        save_agent_color("color-other", "green", Some(&other)).unwrap();
        save_agent_color("color-other", "default", Some(&other)).unwrap();
        assert_eq!(
            get_current_session_metadata().agent_color.as_deref(),
            Some("blue")
        );
        let rows: Vec<Value> = fs::read_to_string(&other)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                json!({"type":"agent-color", "agentColor":"green", "sessionId":"color-other"}),
                json!({"type":"agent-color", "agentColor":"default", "sessionId":"color-other"}),
            ]
        );
        save_agent_color("color-active", "default", Some(&current)).unwrap();
        assert_eq!(
            get_current_session_metadata().agent_color.as_deref(),
            Some("default")
        );
        assert!(save_agent_color("color-active", "red", Some(&restore.root)).is_err());
        assert_eq!(
            get_current_session_metadata().agent_color.as_deref(),
            Some("default")
        );
    }

    /// CC keeps `AdvisorToolResultBlock` whole (utils/advisor.ts:16-32): the
    /// `tool_use_id` says which `server_tool_use` this answers, and the content
    /// union's tag picks the renderer (Message.tsx:467). Flattening it to a
    /// text block dropped the id and turned redacted/error results into empty
    /// strings — and wrote a block CC never emits.
    #[test]
    fn advisor_blocks_round_trip_all_three_content_shapes() {
        use crate::types::message::{AdvisorResult, AssistantContent};

        for content in [
            AdvisorResult::Result {
                text: "advice".to_string(),
            },
            AdvisorResult::Redacted {
                encrypted_content: "ciphertext".to_string(),
            },
            AdvisorResult::Error {
                error_code: "advisor_unavailable".to_string(),
            },
        ] {
            let block = AssistantContent::Advisor {
                tool_use_id: crate::types::ids::ToolUseId("toolu_advisor".to_string()),
                content: content.clone(),
            };
            let message = crate::types::message::Message::Assistant(
                crate::types::message::AssistantMessage {
                    uuid: "assistant-advisor".to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![block],
                    model: None,
                    stop_reason: None,
                    usage: None,
                },
            );

            let (entry, _) = typed_message_entry(&message).expect("loggable");
            let written = entry
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.first())
                .expect("content block");

            assert_eq!(
                written.get("type").and_then(Value::as_str),
                Some("advisor_tool_result"),
                "the wire tag survives instead of degrading to text"
            );
            assert_eq!(
                written.get("tool_use_id").and_then(Value::as_str),
                Some("toolu_advisor")
            );
            // The content union round-trips through its own tag.
            let parsed: AdvisorResult =
                serde_json::from_value(written.get("content").expect("content").clone())
                    .expect("content union");
            assert_eq!(parsed, content);
        }
    }

    /// CC stamps `teamName`/`agentName` onto every entry it writes
    /// (sessionStorage.ts:1043-1044) and the session-list reader pulls
    /// `teamName` back out of the file head (`session_lite_summary`, CC :4752)
    /// to tell team sessions apart from plain ones. Cometix had the read side
    /// but never wrote the field.
    #[test]
    fn transcript_entry_carries_team_stamp_when_supplied() {
        let stamped = SessionStamp {
            session_id: "session".to_string(),
            cwd: "/tmp/project".to_string(),
            version: "test".to_string(),
            git_branch: None,
            user_type: "external".to_string(),
            entrypoint: "test".to_string(),
            slug: None,
            team_name: Some("squad".to_string()),
            agent_name: Some("explorer".to_string()),
        };
        assert_eq!(stamped.team_name.as_deref(), Some("squad"));
        assert_eq!(stamped.agent_name.as_deref(), Some("explorer"));

        // CC passes `teamInfo` only from the logging effect; every other
        // caller omits it, which must stay unstamped.
        let team_info = TeamInfo {
            team_name: Some("squad".to_string()),
            agent_name: Some("explorer".to_string()),
        };
        assert_eq!(
            team_info.team_name.clone(),
            stamped.team_name.clone(),
            "the effect's TeamInfo is what lands on the stamp"
        );
        assert_eq!(
            TeamInfo::default(),
            TeamInfo {
                team_name: None,
                agent_name: None
            }
        );
    }

    /// CC writes each message under its own uuid — `recordTranscript` dedups on
    /// `messageSet.has(m.uuid)` (sessionStorage.ts:1416-1428) and chains
    /// `parentUuid` from it. Minting a fresh uuid per write silently broke
    /// both: the dedup could never hit, so a message reachable from two record
    /// paths landed twice, and re-recording an existing prefix rewrote it under
    /// new ids and re-parented the chain.
    #[test]
    fn transcript_entry_uuid_is_the_message_uuid_across_kinds() {
        use crate::types::message::{
            Attachment, AttachmentMessage, Message, SystemBase, SystemMessage, UserContent,
            UserMessage,
        };

        let user = Message::User(UserMessage {
            uuid: "user-uuid".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text("hi".to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        });
        let system = Message::System(SystemMessage::Informational {
            base: SystemBase::with_uuid("system-uuid"),
            content: "note".to_string(),
            level: crate::types::message::SystemMessageLevel::Info,
            prevent_continuation: None,
            tool_use_id: None,
        });
        let attachment = Message::Attachment(AttachmentMessage {
            uuid: "attachment-uuid".to_string(),
            timestamp: chrono::Utc::now(),
            attachment: Attachment::CompactionReminder,
            wire_payload: None,
        });

        for message in [user, system, attachment] {
            let expected = message.uuid().to_string();
            // CC `isLoggableMessage` withholds attachments from non-ant
            // transcripts (sessionStorage.ts:4357-4366), so on an external
            // build there is no entry to check. Assert that this is the ONLY
            // message kind that may go missing, and only on that build.
            let Some((entry, uuid)) = typed_message_entry(&message) else {
                assert!(
                    matches!(message, Message::Attachment(_))
                        && !crate::utils::build_profile::build_audience().is_internal(),
                    "only attachments, and only on an external build, are unloggable here"
                );
                continue;
            };
            assert_eq!(uuid, expected);
            assert_eq!(
                entry.get("uuid").and_then(Value::as_str),
                Some(expected.as_str()),
                "the written entry carries the same uuid the dedup keys on"
            );
            // Stable across calls, which is what makes a second record a no-op.
            let (_, again) = typed_message_entry(&message).expect("loggable");
            assert_eq!(again, expected);
        }
    }

    fn build_session(entries: Vec<Value>) -> LoadedSession {
        let mut session = LoadedSession::default();
        for entry in entries {
            let Some(uuid) = entry.get("uuid").and_then(|value| value.as_str()) else {
                continue;
            };
            session.message_order.push(uuid.to_string());
            session.messages.insert(uuid.to_string(), entry);
        }
        session.leaf_uuids = compute_leaf_uuids(&session.messages, &session.message_order);
        session
    }

    fn uuids(messages: &[Value]) -> Vec<&str> {
        messages
            .iter()
            .filter_map(|message| message.get("uuid").and_then(|value| value.as_str()))
            .collect()
    }

    #[test]
    fn typed_meta_prompt_preserves_official_is_meta_jsonl_roundtrip() {
        let message = crate::types::message::Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::MetaText(
                "hidden command expansion".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        });
        let (entry, _) = typed_message_entry(&message).expect("loggable message");
        assert_eq!(entry["isMeta"], true);
        assert_eq!(entry["message"]["content"][0]["type"], "text");
        assert_eq!(
            entry["message"]["content"][0]["text"],
            "hidden command expansion"
        );

        let restored = crate::utils::conversation::into_typed_messages(vec![entry]);
        assert!(matches!(
            restored.as_slice(),
            [crate::types::message::Message::User(user)]
                if matches!(
                    user.content.as_slice(),
                    [crate::types::message::UserContent::MetaText(text)]
                        if text == "hidden command expansion"
                )
        ));
    }

    #[test]
    fn typed_progress_message_is_never_persisted_like_official_is_loggable_message() {
        // CC `isLoggableMessage` (utils/sessionStorage.ts:4351-4352) returns
        // false for progress; every JSONL write path runs it via
        // `cleanMessagesForLogging` (:1414, :1456-1457). Progress is
        // "ephemeral UI state and must not be persisted to the JSONL or
        // participate in the parentUuid chain" (:134-137, CC #14373/#23537).
        let message =
            crate::types::message::Message::Progress(crate::types::message::ProgressMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                tool_use_id: "toolu_progress".to_string(),
                parent_tool_use_id: "toolu_progress".to_string(),
                data: crate::types::message::ToolUseProgressMessage::BashProgress {
                    output: "tick".to_string(),
                    full_output: "tick".to_string(),
                    elapsed_time_seconds: 1,
                    total_lines: 1,
                    total_bytes: None,
                    task_id: None,
                    timeout_ms: None,
                },
            });

        assert!(typed_message_entry(&message).is_none());
        assert!(typed_messages_as_transcript_values(std::slice::from_ref(&message)).is_empty());
    }

    #[test]
    fn typed_tool_result_persists_official_raw_tool_use_result() {
        let raw = serde_json::json!({
            "type": "update",
            "filePath": "src/a.rs",
            "content": "new",
            "originalFile": "old",
            "structuredPatch": [],
            "gitDiff": {"status": "modified"}
        });
        let message = crate::types::message::Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::ToolResult(
                crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("toolu_write".to_string()),
                    content: "updated".to_string(),
                    is_error: false,
                    content_blocks: Vec::new(),
                    tool_use_result: Some(raw.clone()),
                },
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        });
        let (entry, _) = typed_message_entry(&message).expect("loggable message");
        assert_eq!(entry["toolUseResult"], raw);
        let restored = crate::utils::conversation::into_typed_messages(vec![entry]);
        assert!(matches!(
            restored.as_slice(),
            [crate::types::message::Message::User(user)]
                if matches!(
                    user.content.as_slice(),
                    [crate::types::message::UserContent::ToolResult(result)]
                        if result.tool_use_result.as_ref()
                            .is_some_and(|value| value["originalFile"] == "old")
                )
        ));
    }

    #[test]
    fn malformed_large_read_tool_use_result_round_trips_exactly_without_session_truncation() {
        let raw = serde_json::json!({
            "type": "text",
            "file": {
                "filePath": "src/large.rs",
                "content": "x".repeat(crate::tool::DEFAULT_MAX_RESULT_SIZE_CHARS + 1)
            },
            "malformed": true
        });
        let message = crate::types::message::Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::ToolResult(
                crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                    content: "model content".to_string(),
                    is_error: false,
                    content_blocks: Vec::new(),
                    tool_use_result: Some(raw.clone()),
                },
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        });

        let (entry, _) = typed_message_entry(&message).expect("loggable message");
        assert_eq!(entry["toolUseResult"], raw);
        let restored = crate::utils::conversation::into_typed_messages(vec![entry]);
        assert!(matches!(
            restored.as_slice(),
            [crate::types::message::Message::User(user)]
                if matches!(
                    user.content.as_slice(),
                    [crate::types::message::UserContent::ToolResult(result)]
                        if result.tool_use_result.as_ref() == Some(&raw)
                )
        ));
    }

    #[test]
    fn preserved_sidechain_read_media_and_raw_tool_use_result_restore_without_flattening() {
        let raw = serde_json::json!({
            "type": "image",
            "file": {"base64": "aW1n", "type": "image/png", "originalSize": 3}
        });
        let entry = serde_json::json!({
            "type": "user",
            "isMeta": false,
            "toolUseResult": raw,
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_read",
                "content": [
                    {"type": "text", "text": "cell output"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aW1n"}},
                    {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "cGRm"}}
                ]
            }]}
        });
        let restored = agent_transcript_entry_to_message(entry).expect("restored user message");
        let crate::types::message::Message::User(user) = restored else {
            panic!("expected user message")
        };
        let [crate::types::message::UserContent::ToolResult(result)] = user.content.as_slice()
        else {
            panic!("expected tool result")
        };
        assert_eq!(result.content_blocks.len(), 3);
        assert!(matches!(
            &result.content_blocks[1],
            crate::types::message::ToolResultContentBlock::Image { source }
                if source.data == "aW1n"
        ));
        assert!(matches!(
            &result.content_blocks[2],
            crate::types::message::ToolResultContentBlock::Document { source }
                if source.data == "cGRm"
        ));
        assert_eq!(result.tool_use_result.as_ref(), Some(&raw));

        let supplemental = agent_transcript_entry_to_message(serde_json::json!({
            "type": "user",
            "isMeta": true,
            "message": {"content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "cGFnZQ=="}},
                {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "cGRm"}}
            ]}
        }))
        .expect("restored supplemental media");
        assert!(matches!(
            supplemental,
            crate::types::message::Message::User(crate::types::message::UserMessage { content, .. })
                if matches!(content.as_slice(), [
                    crate::types::message::UserContent::MetaImage { .. },
                    crate::types::message::UserContent::MetaDocument { .. }
                ])
        ));
    }

    #[test]
    fn typed_assistant_identity_roundtrips_without_becoming_model_content() {
        let message =
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: "assistant-stable".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::AssistantContent::Text("answer".to_string()),
                    crate::types::message::AssistantContent::MessageIdentity(
                        crate::types::message::AssistantMessageIdentity {
                            request_id: Some("req_stable".to_string()),
                            api_message_id: Some("msg_stable".to_string()),
                            ..Default::default()
                        },
                    ),
                ],
                model: Some("claude-test".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: None,
            });

        let (entry, uuid) = typed_message_entry(&message).expect("loggable message");
        assert_eq!(uuid, "assistant-stable");
        assert_eq!(entry["uuid"], "assistant-stable");
        assert_eq!(entry["requestId"], "req_stable");
        assert_eq!(entry["message"]["id"], "msg_stable");
        assert_eq!(entry["message"]["content"].as_array().unwrap().len(), 1);

        let restored = crate::utils::conversation::into_typed_messages(vec![entry]);
        let [crate::types::message::Message::Assistant(restored)] = restored.as_slice() else {
            panic!("expected one assistant");
        };
        assert_eq!(restored.uuid, "assistant-stable");
        assert_eq!(restored.request_id(), Some("req_stable"));
        assert_eq!(restored.api_message_id(), Some("msg_stable"));
        assert_eq!(
            restored
                .content
                .iter()
                .filter(|content| !matches!(
                    content,
                    crate::types::message::AssistantContent::MessageIdentity(_)
                ))
                .count(),
            1
        );
    }

    #[test]
    fn per_content_stop_assistant_envelopes_remain_distinct_jsonl_entries() {
        let make = |uuid: &str, content: crate::types::message::AssistantContent| {
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid.to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    content,
                    crate::types::message::AssistantContent::MessageIdentity(
                        crate::types::message::AssistantMessageIdentity {
                            request_id: Some("req-shared".to_string()),
                            api_message_id: Some("msg-shared".to_string()),
                            ..Default::default()
                        },
                    ),
                ],
                model: Some("claude-test".to_string()),
                stop_reason: None,
                usage: None,
            })
        };
        let messages = vec![
            make(
                "assistant-thinking",
                crate::types::message::AssistantContent::Thinking {
                    text: "reasoning".to_string(),
                    signature: "sig".to_string(),
                },
            ),
            make(
                "assistant-text",
                crate::types::message::AssistantContent::Text("answer".to_string()),
            ),
        ];
        let entries = messages
            .iter()
            .filter_map(typed_message_entry)
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["uuid"], "assistant-thinking");
        assert_eq!(entries[1]["uuid"], "assistant-text");
        assert_eq!(entries[0]["requestId"], "req-shared");
        assert_eq!(entries[1]["message"]["id"], "msg-shared");
        assert_eq!(
            entries[0]["message"]["content"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            entries[1]["message"]["content"].as_array().unwrap().len(),
            1
        );

        let restored = crate::utils::conversation::into_typed_messages(entries);
        assert_eq!(restored.len(), 2);
        assert!(matches!(
            restored.as_slice(),
            [
                crate::types::message::Message::Assistant(first),
                crate::types::message::Message::Assistant(second)
            ] if first.uuid == "assistant-thinking"
                && second.uuid == "assistant-text"
        ));
    }

    fn temp_jsonl_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cometix-{label}-{}.jsonl", Uuid::new_v4()))
    }

    fn write_lines(path: &Path, lines: Vec<String>) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut content = lines.join("\n");
        content.push('\n');
        fs::write(path, content).unwrap();
    }

    fn value_line(value: Value) -> String {
        serde_json::to_string(&value).unwrap()
    }

    fn quoted(value: &str) -> String {
        serde_json::to_string(value).unwrap()
    }

    fn parent_json(parent_uuid: Option<&str>) -> String {
        parent_uuid
            .map(quoted)
            .unwrap_or_else(|| "null".to_string())
    }

    #[test]
    fn sanitize_path_matches_portable_utf16_and_djb2_behavior() {
        assert_eq!(sanitize_path("/tmp/a_b.c"), "-tmp-a-b-c");
        assert_eq!(sanitize_path("😀"), "--");
        assert_eq!(
            sanitize_path(&"a".repeat(201)),
            format!("{}-rkvsv5", "a".repeat(MAX_SANITIZED_LENGTH))
        );
    }

    #[test]
    fn canonicalize_path_normalizes_nfc_when_realpath_fails() {
        let decomposed = format!("cometix-cafe\u{301}-missing-{}", Uuid::new_v4());
        let canonical = canonicalize_path(&decomposed);
        assert!(canonical.contains("café-missing-"));
        assert!(!canonical.contains("e\u{301}"));
    }

    #[test]
    fn find_project_dir_uses_prefix_fallback_only_for_long_paths() {
        let root = std::env::temp_dir().join(format!("cometix-find-project-{}", Uuid::new_v4()));
        let projects = root.join("projects");
        let _guard = set_test_projects_dir_override(projects.clone());
        fs::create_dir_all(&projects).unwrap();

        let short_path = "/tmp/short-project";
        fs::create_dir_all(projects.join(format!("{}-other", sanitize_path(short_path)))).unwrap();
        assert_eq!(find_project_dir(short_path), None);

        let long_path = format!("/tmp/{}", "x".repeat(220));
        let sanitized = sanitize_path(&long_path);
        let fallback = projects.join(format!(
            "{}-other-runtime-hash",
            &sanitized[..MAX_SANITIZED_LENGTH]
        ));
        fs::create_dir_all(&fallback).unwrap();
        assert_eq!(find_project_dir(&long_path), Some(fallback));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_session_file_path_falls_back_to_sibling_worktree() {
        use std::os::unix::fs::PermissionsExt;

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-worktree-resolve-{}", Uuid::new_v4()));
        let current = root.join("current");
        let sibling = root.join("sibling");
        let bin = root.join("bin");
        fs::create_dir_all(&current).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let current = canonicalize_path(&current.to_string_lossy());
        let sibling = canonicalize_path(&sibling.to_string_lossy());

        let projects = root.join("projects");
        let _projects_guard = set_test_projects_dir_override(projects);
        let session_id = "worktree-session";
        let sibling_project_dir = get_project_dir(&sibling);
        fs::create_dir_all(&sibling_project_dir).unwrap();
        fs::write(
            sibling_project_dir.join(format!("{session_id}.jsonl")),
            "{}\n",
        )
        .unwrap();

        let git = bin.join("git");
        fs::write(
            &git,
            format!("#!/bin/sh\nprintf '%s\\n' 'worktree {current}' '' 'worktree {sibling}' ''\n"),
        )
        .unwrap();
        let mut permissions = fs::metadata(&git).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&git, permissions).unwrap();
        let old_path = crate::utils::process_env::var("PATH").unwrap_or_default();
        let _path_guard =
            EnvVarGuard::set("PATH", format!("{}:{}", bin.to_string_lossy(), old_path));

        let resolved = resolve_session_file_path(session_id, Some(&current)).unwrap();
        assert_eq!(resolved.project_path.as_deref(), Some(sibling.as_str()));
        assert_eq!(resolved.file_size, 3);
        assert_eq!(
            resolved.file_path,
            sibling_project_dir.join(format!("{session_id}.jsonl"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn should_skip_persistence_honors_bootstrap_and_prompt_history_gates() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        crate::utils::process_env::set("CLAUDE_CODE_SKIP_PROMPT_HISTORY", "1");
        assert!(!is_session_write_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_SKIP_PROMPT_HISTORY");

        crate::bootstrap::state::set_session_persistence_disabled(true);
        assert!(!is_session_write_enabled());
        crate::bootstrap::state::set_session_persistence_disabled(false);
    }

    #[test]
    fn record_content_replacement_is_safe_noop_while_session_writes_disabled() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_disabled = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        assert!(!is_session_write_enabled());
        let records = vec![
            crate::utils::tool_result_storage::ContentReplacementRecord::tool_result(
                "toolu_replace".to_string(),
                "[Old tool result content cleared]".to_string(),
            ),
        ];

        record_content_replacement(&records, None).expect("disabled write seam should no-op");
        record_content_replacement(&records, Some("agent-1"))
            .expect("disabled agent write seam should no-op");
    }

    #[test]
    fn project_flush_drains_per_file_queue_in_enqueue_order() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-write-queue-{}", Uuid::new_v4()));
        let path = root.join("queued-session.jsonl");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session("queued-session", Some(root.clone()));
        clear_session_metadata();
        set_current_session_metadata_for_test(None, None, None, Some(path.clone()));

        for sequence in 1..=3 {
            append_entry(
                "/unused",
                "queued-session",
                &json!({"type":"summary","sequence":sequence}),
            )
            .unwrap();
        }
        futures::executor::block_on(flush_session_storage()).unwrap();

        let sequences = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["sequence"]
                    .as_u64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2, 3]);

        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn append_entry_to_file_is_serialized_after_queued_entries() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_env = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root =
            std::env::temp_dir().join(format!("cometix-sync-append-{}", uuid::Uuid::new_v4()));
        let path = root.join("ordered.jsonl");

        let _ = get_project()
            .enqueue_write(path.clone(), &json!({"type":"summary","summary":"queued"}))
            .unwrap();
        append_entry_to_file(&path, &json!({"type":"custom-title","customTitle":"sync"})).unwrap();

        let types = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(types, vec!["summary", "custom-title"]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_transcript_message_tracks_discarded_future_and_removes_queued_entry() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-tombstone-{}", Uuid::new_v4()));
        let path = root.join("session.jsonl");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session("tombstone-session", Some(root.clone()));
        clear_session_metadata();
        set_current_session_metadata_for_test(None, None, None, Some(path.clone()));

        append_entry(
            "/unused",
            "tombstone-session",
            &json!({"type":"assistant","uuid":"drop","parentUuid":null}),
        )
        .unwrap();
        let removal = remove_transcript_message("drop");
        drop(removal); // Mirrors CC's `void removeTranscriptMessage(...)`.
        futures::executor::block_on(flush_session_storage()).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_message_by_uuid_fast_path_reappends_trailing_lines() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-tombstone-tail-{}", Uuid::new_v4()));
        let path = root.join("session.jsonl");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session("tombstone-tail-session", Some(root.clone()));
        clear_session_metadata();
        set_current_session_metadata_for_test(None, None, None, Some(path.clone()));
        write_lines(
            &path,
            vec![
                value_line(json!({"type":"assistant","uuid":"parent","parentUuid":null})),
                value_line(json!({"type":"assistant","uuid":"drop","parentUuid":"parent"})),
                value_line(json!({"type":"assistant","uuid":"keep","parentUuid":"parent"})),
            ],
        );

        futures::executor::block_on(remove_transcript_message("drop")).unwrap();

        let ids = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["uuid"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["parent", "keep"]);
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_message_by_uuid_slow_path_preserves_other_and_malformed_lines() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-tombstone-slow-{}", Uuid::new_v4()));
        let path = root.join("session.jsonl");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session("tombstone-slow-session", Some(root.clone()));
        clear_session_metadata();
        set_current_session_metadata_for_test(None, None, None, Some(path.clone()));

        write_lines(
            &path,
            vec![
                value_line(json!({"type":"assistant","uuid":"drop","parentUuid":null})),
                value_line(
                    json!({"type":"attachment","uuid":"filler","content":"x".repeat(LITE_READ_BUF_SIZE as usize)}),
                ),
                "{malformed".to_string(),
                value_line(json!({"type":"assistant","uuid":"keep","parentUuid":null})),
            ],
        );
        futures::executor::block_on(remove_transcript_message("drop")).unwrap();

        let persisted = fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("\"uuid\":\"drop\""));
        assert!(persisted.contains("\"uuid\":\"filler\""));
        assert!(persisted.contains("{malformed"));
        assert!(persisted.contains("\"uuid\":\"keep\""));

        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    /// The join `REPL.tsx:3521-3525` performs: a message the REPL holds in
    /// memory is looked up on disk by its own uuid. `repl.rs:3883` reads that
    /// uuid off the enum (`Message::uuid()`) and `:3898` hands it to
    /// [`remove_transcript_message`].
    ///
    /// CC can only spell this one way — a message has one uuid, spread into the
    /// row by `insertMessageChain` (`utils/sessionStorage.ts:1048`) and used as
    /// the chain cursor at `:1067`. Cometix briefly had two: the writer stamped
    /// assistant rows with `AssistantMessageIdentity.uuid` while every reader
    /// held the envelope, so this join silently matched nothing and tombstoned
    /// assistants stayed on disk to be replayed on resume (task #120).
    ///
    /// Every other tombstone test above writes its own JSONL lines with literal
    /// uuids, which is why none of them could see that: they never exercise the
    /// write-side derivation. This one goes through `record_typed_messages`, so
    /// reintroducing a second uuid on the identity block fails it.
    #[test]
    fn transcript_row_is_removable_by_its_in_memory_uuid_matches_official() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-tombstone-join-{}", Uuid::new_v4()));
        let path = root.join("session.jsonl");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session("tombstone-join-session", Some(root.clone()));
        clear_session_metadata();
        set_current_session_metadata_for_test(None, None, None, Some(path.clone()));

        // Carries a MessageIdentity block, so the identity path is exercised
        // rather than sidestepped.
        let message =
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: "tombstone-join-assistant".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::AssistantContent::Text("answer".to_string()),
                    crate::types::message::AssistantContent::MessageIdentity(
                        crate::types::message::AssistantMessageIdentity {
                            request_id: Some("req_join".to_string()),
                            api_message_id: Some("msg_join".to_string()),
                            ..Default::default()
                        },
                    ),
                ],
                model: Some("claude-test".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: None,
            });

        record_typed_messages(std::slice::from_ref(&message)).unwrap();
        futures::executor::block_on(flush_session_storage()).unwrap();
        let persisted = fs::read_to_string(&path).unwrap();
        assert!(
            persisted.contains("\"type\":\"assistant\""),
            "the assistant row must reach disk first, got:\n{persisted}"
        );

        // Exactly how the REPL tombstone callback obtains the uuid.
        let tombstone_uuid = message.uuid().to_string();
        futures::executor::block_on(remove_transcript_message(&tombstone_uuid)).unwrap();

        let persisted = fs::read_to_string(&path).unwrap();
        assert!(
            !persisted.contains("\"type\":\"assistant\""),
            "the row written for this message must be findable by the uuid the \
             message itself exposes; a second, independently minted uuid on the \
             identity block breaks the join. remaining:\n{persisted}"
        );

        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_message_by_uuid_skips_slow_rewrite_above_official_limit() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-tombstone-limit-{}", Uuid::new_v4()));
        let path = root.join("session.jsonl");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session("tombstone-limit-session", Some(root.clone()));
        clear_session_metadata();
        set_current_session_metadata_for_test(None, None, None, Some(path.clone()));
        write_lines(
            &path,
            vec![value_line(
                json!({"type":"assistant","uuid":"keep","parentUuid":null}),
            )],
        );
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_TOMBSTONE_REWRITE_BYTES + 1).unwrap();

        futures::executor::block_on(remove_transcript_message("keep")).unwrap();

        let mut prefix = vec![0u8; 96];
        let mut file = fs::File::open(&path).unwrap();
        let bytes_read = file.read(&mut prefix).unwrap();
        assert!(String::from_utf8_lossy(&prefix[..bytes_read]).contains("\"uuid\":\"keep\""));
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_enqueue_write_resolves_after_drain() {
        let root =
            std::env::temp_dir().join(format!("cometix-enqueue-resolve-{}", uuid::Uuid::new_v4()));
        let path = root.join("resolved.jsonl");
        let written = get_project()
            .enqueue_write(
                path.clone(),
                &json!({"type":"summary","summary":"resolved"}),
            )
            .unwrap();
        futures::executor::block_on(written.recv()).unwrap();

        assert!(fs::read_to_string(&path).unwrap().contains("resolved"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_schedule_drain_writes_after_official_batch_interval() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-schedule-drain-{}", Uuid::new_v4()));
        let path = root.join("scheduled-session.jsonl");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session("scheduled-session", Some(root.clone()));
        clear_session_metadata();
        set_current_session_metadata_for_test(None, None, None, Some(path.clone()));

        append_entry(
            "/unused",
            "scheduled-session",
            &json!({"type":"summary","summary":"scheduled"}),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(FLUSH_INTERVAL_MS + 100));
        assert!(fs::read_to_string(&path).unwrap().contains("scheduled"));
        futures::executor::block_on(flush_session_storage()).unwrap();

        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_pending_entries_wait_for_first_user_message_materialization() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-pending-entry-{}", Uuid::new_v4()));
        let session_id = "pending-entry-session";
        let path = root.join(format!("{session_id}.jsonl"));
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session(session_id, Some(root.clone()));
        clear_session_metadata();
        reset_session_file_pointer();

        record_content_replacement(
            &[
                crate::utils::tool_result_storage::ContentReplacementRecord::tool_result(
                    "toolu_pending".to_string(),
                    "replacement".to_string(),
                ),
            ],
            None,
        )
        .unwrap();
        assert!(
            !path.exists(),
            "pending metadata must not create a metadata-only session"
        );

        record_typed_messages(&[crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "materialize".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            },
        )])
        .unwrap();
        futures::executor::block_on(flush_session_storage()).unwrap();

        let types = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(types, vec!["content-replacement", "user"]);

        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    /// CC `saveAiGeneratedTitle` (sessionStorage.ts:2640-2673): the ai-title
    /// entry surfaces through the reader's `customTitle ?? aiTitle`
    /// preference, and a later user rename always wins.
    #[test]
    fn ai_generated_title_surfaces_until_a_user_rename_wins() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-ai-title-{}", Uuid::new_v4()));
        let _guard = set_test_projects_dir_override(root.join("projects"));
        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let project_path = "/tmp/ai-title-project";
        crate::bootstrap::state::set_original_cwd(project_path);
        let session_id = "ai-title-session";
        let session_dir = get_project_dir(project_path);
        fs::create_dir_all(&session_dir).unwrap();
        write_lines(
            &session_dir.join(format!("{session_id}.jsonl")),
            vec![value_line(json!({
                "type":"user",
                "uuid":"ai-title-message",
                "parentUuid":null,
                "timestamp":"2026-01-01T00:00:00Z",
                "sessionId":session_id,
                "cwd":project_path,
                "message":{"role":"user","content":"prompt"}
            }))],
        );

        save_ai_generated_title(session_id, "Fix login button on mobile").unwrap();
        let sessions = list_sessions(project_path);
        assert_eq!(
            sessions[0].custom_title.as_deref(),
            Some("Fix login button on mobile")
        );

        save_custom_title(session_id, "my rename", None).unwrap();
        let sessions = list_sessions(project_path);
        assert_eq!(
            sessions[0].custom_title.as_deref(),
            Some("my rename"),
            "a user rename must win over the AI title"
        );

        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(root);
    }

    /// CC `enrichLog` (sessionStorage.ts:5061-5069) drops team sessions from
    /// the /resume picker: `if (enriched.teamName) return null`.
    #[test]
    fn enrichment_filters_team_sessions_like_official() {
        let root = std::env::temp_dir().join(format!("cometix-team-filter-{}", Uuid::new_v4()));
        let _guard = set_test_projects_dir_override(root.join("projects"));
        let project_path = "/tmp/team-filter-project";
        let session_dir = get_project_dir(project_path);
        fs::create_dir_all(&session_dir).unwrap();
        for (session_id, team_name) in [("plain-session", None), ("team-session", Some("crew"))] {
            let mut record = json!({
                "type":"user",
                "uuid":format!("{session_id}-message"),
                "parentUuid":null,
                "timestamp":"2026-01-01T00:00:00Z",
                "sessionId":session_id,
                "cwd":project_path,
                "message":{"role":"user","content":format!("{session_id} prompt")}
            });
            if let Some(team_name) = team_name {
                record["teamName"] = json!(team_name);
            }
            write_lines(
                &session_dir.join(format!("{session_id}.jsonl")),
                vec![value_line(record)],
            );
        }

        let sessions = list_sessions(project_path);
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["plain-session"],
            "team sessions must not enter the resume list"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn progressive_loading_enriches_fifty_then_continues_without_duplicates() {
        let root =
            std::env::temp_dir().join(format!("cometix-progressive-sessions-{}", Uuid::new_v4()));
        let _guard = set_test_projects_dir_override(root.join("projects"));
        let project_path = "/tmp/progressive-project";
        // CC anchors the single-worktree list on getOriginalCwd(), not on
        // worktreePaths[0]: a cwd nested inside a repo (worktree root differs)
        // must still list ITS project directory. Pin cwd to the project and
        // hand the loader only the unrelated root.
        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(project_path);
        let session_dir = get_project_dir(project_path);
        fs::create_dir_all(&session_dir).unwrap();
        for index in 0..65 {
            let session_id = format!("progressive-{index:03}");
            write_lines(
                &session_dir.join(format!("{session_id}.jsonl")),
                vec![value_line(json!({
                    "type":"user",
                    "uuid":format!("message-{index:03}"),
                    "parentUuid":null,
                    "timestamp":"2026-01-01T00:00:00Z",
                    "sessionId":session_id,
                    "cwd":project_path,
                    "message":{"role":"user","content":format!("prompt {index}")}
                }))],
            );
        }

        let initial =
            load_same_repo_message_logs_progressive(&["/tmp/progressive-root".to_string()]);
        assert_eq!(initial.logs.len(), 50);
        assert_eq!(initial.all_stat_logs.len(), 65);
        assert_eq!(initial.next_index, 50);

        let more = enrich_logs(&initial.all_stat_logs, initial.next_index, 50);
        assert_eq!(more.logs.len(), 15);
        assert_eq!(more.next_index, 65);
        let initial_ids = initial
            .logs
            .iter()
            .map(|log| log.session_id.as_str())
            .collect::<HashSet<_>>();
        assert!(
            more.logs
                .iter()
                .all(|log| !initial_ids.contains(log.session_id.as_str()))
        );
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rename_metadata_appends_to_the_selected_cross_project_file() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-rename-{}", Uuid::new_v4()));
        let path = root.join("other-project/session-rename.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_lines(
            &path,
            vec![value_line(json!({
                "type":"user","uuid":"rename-root","parentUuid":null,
                "message":{"role":"user","content":"before rename"}
            }))],
        );

        save_custom_title("session-rename", "Persistent title", Some(&path)).unwrap();
        save_agent_name("session-rename", "Persistent title", Some(&path)).unwrap();
        let entries = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let renamed = &entries[entries.len() - 2];
        assert_eq!(renamed["type"], "custom-title");
        assert_eq!(renamed["customTitle"], "Persistent title");
        assert_eq!(renamed["sessionId"], "session-rename");
        let agent_name = entries.last().unwrap();
        assert_eq!(agent_name["type"], "agent-name");
        assert_eq!(agent_name["agentName"], "Persistent title");
        assert_eq!(agent_name["sessionId"], "session-rename");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn adopted_resume_records_new_messages_on_the_existing_parent_chain() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-adopt-{}", Uuid::new_v4()));
        let project_dir = root.join("cross-project");
        let session_id = "adopted-session";
        let path = project_dir.join(format!("{session_id}.jsonl"));
        fs::create_dir_all(&project_dir).unwrap();
        write_lines(
            &path,
            vec![value_line(json!({
                "type":"user",
                "uuid":"existing-root",
                "parentUuid":null,
                "timestamp":"2026-01-01T00:00:00Z",
                "sessionId":session_id,
                "cwd":"/old/project",
                "message":{"role":"user","content":"existing"}
            }))],
        );

        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session(session_id.to_string(), Some(project_dir));
        clear_session_metadata();
        restore_session_metadata(&SessionMetadataCache {
            session_id: session_id.to_string(),
            custom_title: Some("Adopted title".to_string()),
            ..SessionMetadataCache::default()
        });
        adopt_resumed_session_file().unwrap();
        record_typed_messages(&[crate::types::message::Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "continued".to_string(),
                )],
                model: Some("claude-test".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: None,
            },
        )])
        .unwrap();
        futures::executor::block_on(flush_session_storage()).unwrap();

        assert_eq!(
            current_session_file_for_test().as_deref(),
            Some(path.as_path())
        );
        let persisted = fs::read_to_string(&path).unwrap();
        let entries = persisted
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(entries.iter().any(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("custom-title")
                && entry.get("customTitle").and_then(Value::as_str) == Some("Adopted title")
        }));
        let assistant = entries
            .iter()
            .find(|entry| entry.get("type").and_then(Value::as_str) == Some("assistant"))
            .expect("continued assistant message");
        assert_eq!(assistant["parentUuid"], "existing-root");
        assert!(
            persisted
                .lines()
                .find(|line| line.contains("\"type\":\"assistant\""))
                .is_some_and(|line| line.starts_with("{\"parentUuid\":"))
        );

        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn subagent_transcript_paths_and_metadata_match_official_sidecar_shape() {
        let root =
            std::env::temp_dir().join(format!("cometix-subagent-session-{}", Uuid::new_v4()));
        let _guard = set_test_projects_dir_override(root.join("projects"));
        let project_path = "/tmp/project with spaces";
        let session_id = "session-1";
        let agent_id = "a123";

        clear_agent_transcript_subdir(agent_id);
        let transcript_path =
            get_agent_transcript_path_for_session(project_path, session_id, agent_id);
        assert!(transcript_path.ends_with("session-1/subagents/agent-a123.jsonl"));
        assert_eq!(
            get_agent_metadata_path_for_session(project_path, session_id, agent_id)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("agent-a123.meta.json")
        );

        set_agent_transcript_subdir(agent_id, "workflows/run-7");
        let grouped_path =
            get_agent_transcript_path_for_session(project_path, session_id, agent_id);
        assert!(grouped_path.ends_with("session-1/subagents/workflows/run-7/agent-a123.jsonl"));

        let metadata_path = get_agent_metadata_path_for_session(project_path, session_id, agent_id);
        fs::create_dir_all(metadata_path.parent().unwrap()).unwrap();
        fs::write(
            &metadata_path,
            r#"{"agentType":"general-purpose","worktreePath":"/tmp/wt","description":"Inspect"}"#,
        )
        .unwrap();
        let metadata = read_agent_metadata_for_session(project_path, session_id, agent_id)
            .unwrap()
            .expect("metadata");
        assert_eq!(metadata.agent_type, "general-purpose");
        assert_eq!(metadata.worktree_path.as_deref(), Some("/tmp/wt"));
        assert_eq!(metadata.description.as_deref(), Some("Inspect"));

        clear_agent_transcript_subdir(agent_id);
        let _ = fs::remove_dir_all(root);
    }

    /// The two typed sidechain messages `run_agent` records for an agent —
    /// enough for `get_agent_transcript` to rebuild a chain (user is the root,
    /// assistant is the leaf).
    fn agent_sidechain_messages() -> Vec<crate::types::message::Message> {
        vec![
            crate::types::message::Message::User(crate::types::message::UserMessage {
                uuid: "agent-prompt-uuid".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "inspect the worktree".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: "agent-reply-uuid".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "done".to_string(),
                )],
                model: Some("claude-test".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: None,
            }),
        ]
    }

    /// CC has ONE root for a subagent's artifacts: `getAgentTranscriptPath`
    /// (`sessionStorage.ts:247-257`) takes only an agentId and resolves
    /// `getSessionProjectDir() ?? getProjectDir(getOriginalCwd())`. A run cwd
    /// reaches only the entry stamp, via `getCwd()` inside `insertMessageChain`
    /// (`:1059`) — CC installs that as an AsyncLocalStorage override for the
    /// whole AgentTool run (`AgentTool.tsx:917`) and it never touches
    /// `getOriginalCwd`/`getSessionProjectDir`.
    ///
    /// The port had collapsed root and stamp into one `cwd` parameter, so an
    /// `isolation: "worktree"` agent (ungated schema; `mod.rs` sets
    /// `cwd_override` for it) wrote its transcript under
    /// `projects/<sanitized worktree>` while `write_agent_metadata` (zero-arg)
    /// and the `resume_agent.rs` reader stayed on the original project dir —
    /// `SendMessageTool` resume then failed with "No transcript found for
    /// agent ID". This exercises both halves: the writer runs with the
    /// override, the reader is the resume one.
    #[test]
    fn agent_transcript_written_under_a_cwd_override_is_found_by_the_resume_reader() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root =
            std::env::temp_dir().join(format!("cometix-agent-cwd-override-{}", Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let parent_cwd = root.join("parent-repo");
        let worktree_cwd = root.join("worktrees").join("agent-abc12345");
        fs::create_dir_all(&parent_cwd).unwrap();
        fs::create_dir_all(&worktree_cwd).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&parent_cwd);
        crate::bootstrap::state::switch_session("session-worktree-agent", None);
        clear_session_metadata();
        reset_session_file_pointer();

        let agent_id = "agent-worktree-1";
        clear_agent_transcript_subdir(agent_id);

        // Writer: what `SidechainTranscriptRecorder` calls, with the run cwd
        // of a worktree-isolated agent (CC runAgent.ts:735 / :794).
        record_typed_sidechain_transcript(
            &agent_sidechain_messages(),
            agent_id,
            None,
            &worktree_cwd,
        )
        .expect("sidechain write");
        // Sibling writer that never saw the run cwd (CC `writeAgentMetadata`,
        // called zero-arg from runAgent.ts) — the pair that used to disagree.
        write_agent_metadata(
            agent_id,
            &AgentMetadata {
                agent_type: "general-purpose".to_string(),
                worktree_path: Some(worktree_cwd.to_string_lossy().to_string()),
                description: Some("Inspect".to_string()),
            },
        )
        .expect("metadata write");
        futures::executor::block_on(flush_session_storage()).unwrap();

        // Reader: `resume_agent.rs:39` → `get_agent_transcript` →
        // `get_agent_transcript_path`.
        let transcript =
            get_agent_transcript(agent_id).expect("resume must find the transcript the run wrote");
        assert_eq!(transcript.messages.len(), 2);

        let transcript_path = get_agent_transcript_path(agent_id);
        assert!(transcript_path.is_file());
        assert!(transcript_path.starts_with(get_project_dir(&parent_cwd.to_string_lossy())));
        assert!(
            read_agent_metadata(agent_id)
                .expect("metadata read")
                .is_some()
        );
        // Nothing was routed into a worktree-derived project dir.
        assert!(
            !get_projects_dir()
                .join(sanitize_path(&worktree_cwd.to_string_lossy()))
                .exists()
        );
        // CC `insertMessageChain` materializes through zero-arg
        // `ensureCurrentSessionFile` → `getTranscriptPath()`
        // (`sessionStorage.ts:1271-1277`, `:202-205`), so the sidechain write
        // must not repoint the MAIN session file at the run cwd's project dir
        // — every later main-thread write follows that pointer.
        assert_eq!(
            current_session_file_for_test(),
            Some(
                get_project_dir(&parent_cwd.to_string_lossy()).join("session-worktree-agent.jsonl")
            )
        );

        // The stamp still carries the run cwd: CC `insertMessageChain` writes
        // `cwd: getCwd()` (:1059), which is the override `AgentTool.tsx:917`
        // installed.
        let first: Value = serde_json::from_str(
            fs::read_to_string(&transcript_path)
                .unwrap()
                .lines()
                .next()
                .expect("at least one entry"),
        )
        .unwrap();
        assert_eq!(
            first["cwd"].as_str(),
            Some(worktree_cwd.to_string_lossy().as_ref())
        );

        clear_agent_transcript_subdir(agent_id);
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(&root);
    }

    /// CC `getAgentTranscriptPath` reads `getSessionProjectDir() ??
    /// getProjectDir(getOriginalCwd())` with its own comment on the invariant
    /// (`sessionStorage.ts:248-251`): "subagent transcripts live under the
    /// session dir, so if the session transcript is at sessionProjectDir,
    /// subagent transcripts are too." `session_restore.rs:352` sets that dir on
    /// every resume (CC `bootstrap/state.ts:468-479`), so ignoring it put every
    /// resumed session's subagent artifacts in the wrong project directory.
    #[test]
    fn resumed_session_agent_transcript_lives_under_the_session_project_dir() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root =
            std::env::temp_dir().join(format!("cometix-resumed-subagent-{}", Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let original_cwd = root.join("current-project");
        // What `session_restore.rs:352` passes: the directory holding the
        // resumed session's JSONL, which is not the current project's.
        let session_project_dir = root.join("projects").join("-resumed-elsewhere");
        fs::create_dir_all(&original_cwd).unwrap();
        fs::create_dir_all(&session_project_dir).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&original_cwd);
        crate::bootstrap::state::switch_session(
            "session-resumed-elsewhere",
            Some(session_project_dir.clone()),
        );
        clear_session_metadata();
        reset_session_file_pointer();

        let agent_id = "agent-resumed-1";
        clear_agent_transcript_subdir(agent_id);

        record_typed_sidechain_transcript(
            &agent_sidechain_messages(),
            agent_id,
            None,
            &original_cwd,
        )
        .expect("sidechain write");
        futures::executor::block_on(flush_session_storage()).unwrap();

        let transcript_path = get_agent_transcript_path(agent_id);
        assert!(transcript_path.starts_with(&session_project_dir));
        assert!(transcript_path.is_file());
        assert!(get_agent_transcript(agent_id).is_some());
        assert!(
            !get_project_dir(&original_cwd.to_string_lossy())
                .join("session-resumed-elsewhere")
                .join("subagents")
                .exists()
        );

        clear_agent_transcript_subdir(agent_id);
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(&root);
    }

    /// What `message_start` puts on a per-block assistant: the real input and
    /// cache legs, and a placeholder `output_tokens` (`claude.ts:1981`
    /// `partialMessage = part.message`, `:2192-2196` `{ ...partialMessage,
    /// content }`).
    fn message_start_usage() -> crate::types::message::TokenUsage {
        crate::types::message::TokenUsage {
            input_tokens: 120,
            output_tokens: 0,
            cache_creation_input_tokens: 300,
            cache_read_input_tokens: 4_000,
            cache_deleted_input_tokens: 0,
        }
    }

    fn sidechain_user_message(uuid: &str, text: &str) -> crate::types::message::Message {
        crate::types::message::Message::User(crate::types::message::UserMessage {
            uuid: uuid.to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(text.to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })
    }

    /// A per-block assistant exactly as the stream hands it over: the
    /// `message_start` usage and no stop reason yet.
    fn streamed_sidechain_assistant(uuid: &str, text: &str) -> crate::types::message::Message {
        crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid.to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Text(
                text.to_string(),
            )],
            model: Some("claude-test".to_string()),
            stop_reason: None,
            usage: Some(message_start_usage()),
        })
    }

    fn read_jsonl(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect()
    }

    /// CC's transcript write queue holds a REFERENCE to the message object and
    /// stringifies it `FLUSH_INTERVAL_MS = 100` later (`:567`, `enqueueWrite`
    /// `:606`, `drainWriteQueue` `:658`); `cleanMessagesForLogging` `.filter`s
    /// the array (`:4454`) and `insertMessageChain`'s `{ ...msg }` (`:1046`)
    /// copies `.message` as a pointer. That is why `claude.ts:2244-2248`
    /// finalises `usage`/`stop_reason` by direct property mutation — one site
    /// for both, `ast-grep --lang ts -p '$A.message.usage = $B'` and
    /// `-p '$A.message.stop_reason = $B'` over `rebuild/src` return `:2246`
    /// and `:2247` and nothing else — and the source says so itself at
    /// `:2235-2243`: "Object replacement … would disconnect the queued
    /// reference; direct mutation ensures the transcript captures the final
    /// values."
    ///
    /// Subagents are on that exact path rather than inheriting it from the main
    /// loop: `runAgent.ts:750` and `forkedAgent.ts:546` both drive `query()`,
    /// and `query`'s only model call is `deps.callModel` — `ast-grep --lang ts
    /// -p 'deps.callModel($$$)'` returns `query.ts:659` alone — bound to
    /// `queryModelWithStreaming` (`query/deps.ts:23`, `:35`), the generator at
    /// `claude.ts:752` that owns the `message_delta` arm.
    ///
    /// Cometix serializes at enqueue, so before the parked record every
    /// assistant row in every `agent-<id>.jsonl` kept the `message_start`
    /// placeholder — `output_tokens: 0` and `stop_reason: null`, always.
    ///
    /// Driven through the production sequence the `run_agent` loop performs, in
    /// order, starting from `message_start` values only: nothing here pre-loads
    /// a finalised usage. Two turns, and turn one has two per-block assistants
    /// so the displaced-record rule is pinned too — CC mutates
    /// `newMessages.at(-1)` and nothing else, so a per-block assistant that is
    /// no longer last keeps its `message_start` usage in CC as well.
    #[test]
    fn sidechain_assistant_rows_capture_the_message_delta_finalisation() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root = std::env::temp_dir().join(format!("cometix-sidechain-delta-{}", Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let cwd = root.join("project");
        fs::create_dir_all(&cwd).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::bootstrap::state::switch_session("session-sidechain-delta", None);
        clear_session_metadata();
        reset_session_file_pointer();

        let agent_id = "asidechain-delta";
        clear_agent_transcript_subdir(agent_id);

        // CC `runAgent.ts:735` + `:745`.
        let prompt = sidechain_user_message("prompt-uuid", "inspect");
        let mut recorder =
            SidechainTranscriptRecorder::start(agent_id, std::slice::from_ref(&prompt), &cwd);
        assert_eq!(recorder.last_recorded_uuid(), Some("prompt-uuid"));

        // Turn one: two per-block assistants, then one `message_delta`.
        recorder.record(&streamed_sidechain_assistant("assistant-1a", "thinking"));
        recorder.record(&streamed_sidechain_assistant("assistant-1b", "calling"));
        let turn_one_final = crate::types::message::TokenUsage {
            output_tokens: 350,
            ..message_start_usage()
        };
        recorder.apply_assistant_delta(
            "assistant-1b",
            Some(crate::types::message::StopReason::ToolUse),
            Some(turn_one_final.clone()),
        );

        // The tool result comes back and turn two runs.
        recorder.record(&sidechain_user_message("result-uuid", "tool output"));
        recorder.record(&streamed_sidechain_assistant("assistant-2", "done"));
        let turn_two_final = crate::types::message::TokenUsage {
            output_tokens: 77,
            ..message_start_usage()
        };
        recorder.apply_assistant_delta(
            "assistant-2",
            Some(crate::types::message::StopReason::EndTurn),
            Some(turn_two_final.clone()),
        );
        // The loop exited; nothing is parked, so this writes nothing.
        recorder.flush();
        futures::executor::block_on(flush_session_storage()).unwrap();

        let transcript_path = get_agent_transcript_path(agent_id);
        let rows = read_jsonl(&transcript_path);
        assert_eq!(
            rows.iter()
                .map(|row| row["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "prompt-uuid",
                "assistant-1a",
                "assistant-1b",
                "result-uuid",
                "assistant-2"
            ],
            "deferring the assistant record must not reorder the JSONL"
        );
        assert_eq!(
            rows.iter()
                .map(|row| row["parentUuid"].as_str())
                .collect::<Vec<_>>(),
            vec![
                None,
                Some("prompt-uuid"),
                Some("assistant-1a"),
                Some("assistant-1b"),
                Some("result-uuid")
            ],
            "the parent chain must survive the deferral"
        );

        // The defect: these were `0` / `null` for every agent, always.
        assert_eq!(rows[2]["message"]["usage"]["output_tokens"], 350);
        assert_eq!(rows[2]["message"]["stop_reason"], "tool_use");
        assert_eq!(rows[4]["message"]["usage"]["output_tokens"], 77);
        assert_eq!(rows[4]["message"]["stop_reason"], "end_turn");
        // The cache/input legs `message_start` already carried are untouched.
        assert_eq!(rows[2]["message"]["usage"]["input_tokens"], 120);
        assert_eq!(
            rows[2]["message"]["usage"]["cache_read_input_tokens"],
            4_000
        );
        // CC `claude.ts:2244` mutates `newMessages.at(-1)` only, so the
        // displaced per-block assistant keeps `message_start`'s placeholder
        // there too.
        assert_eq!(rows[1]["message"]["usage"]["output_tokens"], 0);
        assert!(rows[1]["message"]["stop_reason"].is_null());

        // Resume reads this same file (`resume_agent.rs:39` →
        // `get_agent_transcript`); the corrected fields must not disturb what
        // it reconstructs.
        let transcript = get_agent_transcript(agent_id).expect("resume must find the transcript");
        assert_eq!(
            transcript
                .messages
                .iter()
                .map(crate::types::message::Message::uuid)
                .collect::<Vec<_>>(),
            vec![
                "prompt-uuid",
                "assistant-1a",
                "assistant-1b",
                "result-uuid",
                "assistant-2"
            ]
        );

        clear_agent_transcript_subdir(agent_id);
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(&root);
    }

    /// CC's sidechain cursor is a caller-side local, assigned from the messages
    /// handed IN — never from what the write did with them:
    ///
    /// - the seed is `initialMessages.at(-1)?.uuid ?? null` (`runAgent.ts:745`;
    ///   `forkedAgent.ts:536-539` is the same expression), and
    /// - the advance is `if (message.type !== 'progress') lastRecordedUuid =
    ///   message.uuid` (`:801-803`), outside the `.catch` that swallows a
    ///   failed write.
    ///
    /// `recordSidechainTranscript` returns `void` (`sessionStorage.ts:1451-1462`)
    /// and so does `insertMessageChain` (`:999-1082`), so there is nothing for
    /// a caller to adopt. The port had wired the cursor to a Rust-only return
    /// value carrying "the last row actually written", which drifts on three
    /// paths: a failed write, a batch the privacy filter emptied, and — the one
    /// this exercises — an `initialMessages` tail the filter withholds.
    ///
    /// Both audiences run this: `hook_additional_context` IS written on `ant`
    /// and withheld elsewhere (`:4357-4366`), and the cursor must be the
    /// attachment's uuid either way. On a non-`ant` build that means the first
    /// loop row names a parent no row carries — CC's own outcome, and one the
    /// leaf-to-root reader (`getAgentTranscript` `:4210-4224`) is sensitive to,
    /// so "repairing" it would make the port reconstruct a different prefix
    /// than CC from the same run.
    #[test]
    fn sidechain_cursor_seeds_from_the_last_initial_message_like_official() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        let _hook_context = EnvVarGuard::unset("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root = std::env::temp_dir().join(format!("cometix-sidechain-seed-{}", Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let cwd = root.join("project");
        fs::create_dir_all(&cwd).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::bootstrap::state::switch_session("session-sidechain-seed", None);
        clear_session_metadata();
        reset_session_file_pointer();

        let agent_id = "aseed-cursor";
        clear_agent_transcript_subdir(agent_id);

        // `initialMessages` as `runAgent` builds it when a SubagentStart hook
        // returned context: the prompt, then the attachment (`:546-554`).
        let initial = vec![
            sidechain_user_message("prompt-uuid", "investigate"),
            crate::types::message::Message::Attachment(
                crate::types::message::AttachmentMessage::new(json!({
                    "type": "hook_additional_context",
                    "content": ["from the hook"],
                    "hookName": "SubagentStart",
                    "hookEvent": "SubagentStart",
                })),
            ),
        ];
        let attachment_uuid = initial[1].uuid().to_string();

        let mut recorder = SidechainTranscriptRecorder::start(agent_id, &initial, &cwd);
        assert_eq!(
            recorder.last_recorded_uuid(),
            Some(attachment_uuid.as_str()),
            "the seed is the last INPUT message, whatever the filter did with it"
        );

        recorder.record(&streamed_sidechain_assistant("assistant-uuid", "on it"));
        recorder.flush();
        futures::executor::block_on(flush_session_storage()).unwrap();

        let rows = read_jsonl(&get_agent_transcript_path(agent_id));
        let internal = crate::utils::build_profile::build_audience().is_internal();
        let expected_uuids: Vec<&str> = if internal {
            vec!["prompt-uuid", attachment_uuid.as_str(), "assistant-uuid"]
        } else {
            vec!["prompt-uuid", "assistant-uuid"]
        };
        assert_eq!(
            rows.iter()
                .map(|row| row["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected_uuids,
            "the privacy filter is the only thing that decides what is written"
        );
        // Identical on both audiences — the cursor never consults the write.
        assert_eq!(
            rows.last().unwrap()["parentUuid"].as_str(),
            Some(attachment_uuid.as_str())
        );

        clear_agent_transcript_subdir(agent_id);
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(&root);
    }

    /// The trap that comes with owning the cursor at record time: CC advances
    /// it the instant the message is handed to `recordSidechainTranscript`
    /// (`runAgent.ts:794-803`), but this port PARKS assistant records so
    /// `message_delta` can still reach them. By the time a parked record is
    /// materialized the live cursor is already that record's OWN uuid, so a
    /// writer that re-reads the cursor writes `parentUuid === uuid` — a
    /// self-parenting row. The chain walk both readers perform
    /// (`build_conversation_chain` ← CC `buildConversationChain`) would then
    /// either stop at that row or loop on it, losing every earlier message of a
    /// resumed agent.
    ///
    /// So the park carries the parent it was made with. Also pinned here: the
    /// progress exception (`:801-803`) — progress is handed to the recorder,
    /// writes nothing (`isLoggableMessage` `:4351`), and must leave the cursor
    /// where it was so the NEXT row parents to the assistant, not to a uuid
    /// that reached no file.
    #[test]
    fn parked_assistant_keeps_its_park_time_parent_and_progress_never_advances_the_cursor() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root = std::env::temp_dir().join(format!("cometix-sidechain-park-{}", Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let cwd = root.join("project");
        fs::create_dir_all(&cwd).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::bootstrap::state::switch_session("session-sidechain-park", None);
        clear_session_metadata();
        reset_session_file_pointer();

        let agent_id = "apark-parent";
        clear_agent_transcript_subdir(agent_id);

        let prompt = sidechain_user_message("prompt-uuid", "investigate");
        let mut recorder =
            SidechainTranscriptRecorder::start(agent_id, std::slice::from_ref(&prompt), &cwd);

        // Parked, and the cursor moves past it immediately — CC's assignment
        // does not wait for the write.
        recorder.record(&streamed_sidechain_assistant("assistant-uuid", "calling"));
        assert_eq!(recorder.last_recorded_uuid(), Some("assistant-uuid"));

        // A bash tick between the assistant and its tool result.
        recorder.record(&crate::types::message::Message::Progress(
            crate::types::message::ProgressMessage {
                uuid: "progress-uuid".to_string(),
                timestamp: chrono::Utc::now(),
                tool_use_id: "toolu_1".to_string(),
                parent_tool_use_id: "toolu_1".to_string(),
                data: crate::types::message::ToolUseProgressMessage::BashProgress {
                    output: "tick".to_string(),
                    full_output: "tick".to_string(),
                    elapsed_time_seconds: 1,
                    total_lines: 1,
                    total_bytes: None,
                    task_id: None,
                    timeout_ms: None,
                },
            },
        ));
        assert_eq!(
            recorder.last_recorded_uuid(),
            Some("assistant-uuid"),
            "progress is not a chain participant, in CC or here"
        );

        // This is the flush that displaces the parked assistant, while the
        // cursor already reads `assistant-uuid`.
        recorder.record(&sidechain_user_message("result-uuid", "tool output"));
        recorder.flush();
        futures::executor::block_on(flush_session_storage()).unwrap();

        let rows = read_jsonl(&get_agent_transcript_path(agent_id));
        assert_eq!(
            rows.iter()
                .map(|row| row["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["prompt-uuid", "assistant-uuid", "result-uuid"]
        );
        // Not a tautology: the assertion above pinned the live cursor at
        // `assistant-uuid` BEFORE the flush that wrote row 1, so a writer that
        // consulted `last_recorded_uuid` at write time could only have produced
        // `parentUuid == uuid == assistant-uuid` here.
        assert_eq!(
            rows.iter()
                .map(|row| row["parentUuid"].as_str())
                .collect::<Vec<_>>(),
            vec![None, Some("prompt-uuid"), Some("assistant-uuid")]
        );
        for row in &rows {
            assert_ne!(
                row["parentUuid"].as_str(),
                row["uuid"].as_str(),
                "a row may never be its own parent: {row}"
            );
        }

        // The reader has to walk the whole chain back from the leaf.
        let transcript = get_agent_transcript(agent_id).expect("resume must find the transcript");
        assert_eq!(
            transcript
                .messages
                .iter()
                .map(crate::types::message::Message::uuid)
                .collect::<Vec<_>>(),
            vec!["prompt-uuid", "assistant-uuid", "result-uuid"]
        );

        clear_agent_transcript_subdir(agent_id);
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(&root);
    }

    /// Several agents are in flight at once — `AgentTool` spawns background
    /// runs (`mod.rs:625`, `:967`), `resume_agent.rs:183` restarts them, and
    /// `run_forked_agent` is driven from side questions and compaction. A
    /// single global park slot (the main session's
    /// the historical main-session deferred slot, which assumed exactly
    /// one conversation) would let agent B's record flush agent A's early, with
    /// the pre-delta usage this fix exists to remove, and through
    /// `record_typed_message` — i.e. into the MAIN session file rather than
    /// A's. One recorder per run makes that unrepresentable; this drives two
    /// interleaved runs to prove it.
    #[test]
    fn interleaved_agent_recorders_do_not_disturb_each_other() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_session_persistence_disabled(false);
        assert!(is_session_write_enabled());

        let root =
            std::env::temp_dir().join(format!("cometix-sidechain-interleave-{}", Uuid::new_v4()));
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let cwd = root.join("project");
        fs::create_dir_all(&cwd).unwrap();

        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::bootstrap::state::switch_session("session-sidechain-interleave", None);
        clear_session_metadata();
        reset_session_file_pointer();

        let (first_id, second_id) = ("ainterleave-1", "ainterleave-2");
        clear_agent_transcript_subdir(first_id);
        clear_agent_transcript_subdir(second_id);

        let first_prompt = sidechain_user_message("first-prompt", "first");
        let second_prompt = sidechain_user_message("second-prompt", "second");
        let mut first =
            SidechainTranscriptRecorder::start(first_id, std::slice::from_ref(&first_prompt), &cwd);
        let mut second = SidechainTranscriptRecorder::start(
            second_id,
            std::slice::from_ref(&second_prompt),
            &cwd,
        );

        // Both park, then both finalise — in the worst order, second-in
        // first-out.
        first.record(&streamed_sidechain_assistant("first-assistant", "A"));
        second.record(&streamed_sidechain_assistant("second-assistant", "B"));
        second.apply_assistant_delta(
            "second-assistant",
            Some(crate::types::message::StopReason::EndTurn),
            Some(crate::types::message::TokenUsage {
                output_tokens: 22,
                ..message_start_usage()
            }),
        );
        first.apply_assistant_delta(
            "first-assistant",
            Some(crate::types::message::StopReason::MaxTokens),
            Some(crate::types::message::TokenUsage {
                output_tokens: 11,
                ..message_start_usage()
            }),
        );
        futures::executor::block_on(flush_session_storage()).unwrap();

        let first_rows = read_jsonl(&get_agent_transcript_path(first_id));
        let second_rows = read_jsonl(&get_agent_transcript_path(second_id));
        assert_eq!(
            first_rows
                .iter()
                .map(|row| row["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["first-prompt", "first-assistant"]
        );
        assert_eq!(
            second_rows
                .iter()
                .map(|row| row["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["second-prompt", "second-assistant"]
        );
        assert_eq!(first_rows[1]["message"]["usage"]["output_tokens"], 11);
        assert_eq!(first_rows[1]["message"]["stop_reason"], "max_tokens");
        assert_eq!(second_rows[1]["message"]["usage"]["output_tokens"], 22);
        assert_eq!(second_rows[1]["message"]["stop_reason"], "end_turn");

        clear_agent_transcript_subdir(first_id);
        clear_agent_transcript_subdir(second_id);
        clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn get_agent_transcript_reconstructs_latest_sidechain_chain() {
        let session_id = "session-agent";
        let agent_id = "a0123456789abcdef";
        let path = temp_jsonl_path("agent-transcript");
        write_lines(
            &path,
            vec![
                value_line(json!({
                    "type": "user",
                    "uuid": "u1",
                    "parentUuid": null,
                    "timestamp": "2026-01-01T00:00:00Z",
                    "sessionId": session_id,
                    "isSidechain": true,
                    "agentId": agent_id,
                    "message": {"role":"user","content":[{"type":"text","text":"start"}]}
                })),
                value_line(json!({
                    "type": "assistant",
                    "uuid": "a1",
                    "parentUuid": "u1",
                    "timestamp": "2026-01-01T00:00:01Z",
                    "sessionId": session_id,
                    "isSidechain": true,
                    "agentId": agent_id,
                    "message": {
                        "role":"assistant",
                        "content":[{"type":"text","text":"working"}],
                        "model":"claude-sonnet",
                        "stop_reason":"end_turn",
                        "usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":3,"cache_read_input_tokens":4}
                    }
                })),
                value_line(json!({
                    "type": "user",
                    "uuid": "u2",
                    "parentUuid": "a1",
                    "timestamp": "2026-01-01T00:00:02Z",
                    "sessionId": session_id,
                    "isSidechain": true,
                    "agentId": agent_id,
                    "message": {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}
                })),
                value_line(json!({
                    "type": "content-replacement",
                    "sessionId": session_id,
                    "agentId": agent_id,
                    "replacements": [{"toolUseId":"toolu_1"}]
                })),
            ],
        );

        let transcript =
            get_agent_transcript_from_path(&path, agent_id).expect("agent transcript should load");
        assert_eq!(transcript.messages.len(), 3);
        assert_eq!(transcript.content_replacements.len(), 1);
        match &transcript.messages[0] {
            crate::types::message::Message::User(user) => assert!(matches!(
                &user.content[0],
                crate::types::message::UserContent::Text(text) if text == "start"
            )),
            other => panic!("unexpected first transcript message: {other:?}"),
        }
        match &transcript.messages[1] {
            crate::types::message::Message::Assistant(assistant) => {
                assert_eq!(assistant.model.as_deref(), Some("claude-sonnet"));
                assert_eq!(assistant.usage.as_ref().unwrap().output_tokens, 2);
            }
            other => panic!("unexpected assistant transcript message: {other:?}"),
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn sidechain_record_and_metadata_write_are_safe_noops_while_writes_disabled() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_disabled = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        assert!(!is_session_write_enabled());
        let stamp = SessionStamp {
            session_id: "session-sidechain".to_string(),
            cwd: "/tmp/project".to_string(),
            version: "test".to_string(),
            git_branch: None,
            user_type: "external".to_string(),
            entrypoint: "test".to_string(),
            slug: None,
            team_name: None,
            agent_name: None,
        };
        let messages = vec![json!({"type":"assistant","uuid":"a1"})];
        record_sidechain_transcript(
            "/tmp/project",
            "session-sidechain",
            &messages,
            "agent-1",
            Some("u0"),
            &stamp,
        )
        .expect("disabled sidechain seam");

        // The read-only gate is Cometix's; CC has no such branch, so it must
        // not become visible in the chain either. The cursor is the caller's
        // `lastRecordedUuid` (`runAgent.ts:745`, `:801-803`) — assigned from
        // the message, never from the write — so it advances exactly as it
        // would with writes on. It used to be read back out of the write, and
        // this seam pinned it at the seed for the whole run.
        let mut recorder = SidechainTranscriptRecorder::start(
            "agent-1",
            std::slice::from_ref(&sidechain_user_message("prompt-uuid", "go")),
            Path::new("/tmp/project"),
        );
        assert_eq!(recorder.last_recorded_uuid(), Some("prompt-uuid"));
        recorder.record(&streamed_sidechain_assistant("assistant-uuid", "hi"));
        assert_eq!(recorder.last_recorded_uuid(), Some("assistant-uuid"));

        write_agent_metadata_for_session(
            "/tmp/project",
            "session-sidechain",
            "agent-1",
            &AgentMetadata {
                agent_type: "general-purpose".to_string(),
                worktree_path: None,
                description: Some("Inspect".to_string()),
            },
        )
        .expect("disabled metadata seam");
    }

    fn transcript_line(
        parent_uuid: Option<&str>,
        message_type: &str,
        uuid: &str,
        timestamp: &str,
        session_id: &str,
        content: &str,
    ) -> String {
        let role = if message_type == "assistant" {
            "assistant"
        } else {
            "user"
        };
        format!(
            "{{\"parentUuid\":{},\"type\":\"{}\",\"uuid\":\"{}\",\"timestamp\":\"{}\",\"sessionId\":\"{}\",\"message\":{{\"role\":\"{}\",\"content\":{}}}}}",
            parent_json(parent_uuid),
            message_type,
            uuid,
            timestamp,
            session_id,
            role,
            quoted(content)
        )
    }

    #[test]
    fn lite_metadata_matches_official_tail_title_and_pr_fields() {
        let path = temp_jsonl_path("lite-metadata");
        let session_id = "lite-session";
        write_lines(
            &path,
            vec![
                value_line(json!({
                    "type": "user",
                    "uuid": "u1",
                    "parentUuid": null,
                    "sessionId": session_id,
                    "timestamp": "2026-06-13T13:39:30.000Z",
                    "cwd": "/repo",
                    "gitBranch": "main",
                    "agentSetting": "reviewer",
                    "message": {"role": "user", "content": [
                        {"type": "text", "text": "<ide_selection>noise</ide_selection>"},
                        {"type": "text", "text": "initial prompt"}
                    ]}
                })),
                value_line(json!({
                    "type": "custom-title",
                    "sessionId": session_id,
                    "customTitle": "User title"
                })),
                value_line(json!({
                    "type": "ai-title",
                    "sessionId": session_id,
                    "aiTitle": "AI title"
                })),
                value_line(json!({
                    "type": "summary",
                    "leafUuid": "u1",
                    "summary": "Tail summary"
                })),
                value_line(json!({
                    "type": "last-prompt",
                    "sessionId": session_id,
                    "lastPrompt": "recent work"
                })),
                value_line(json!({
                    "type": "tag",
                    "sessionId": session_id,
                    "tag": "bug"
                })),
                value_line(json!({
                    "type": "pr-link",
                    "sessionId": session_id,
                    "prNumber": 42,
                    "prUrl": "https://example.test/pr/42",
                    "prRepository": "owner/repo"
                })),
            ],
        );

        let meta = read_lite_metadata(&path, fs::metadata(&path).unwrap().len());
        fs::remove_file(&path).ok();

        assert_eq!(meta.first_prompt, "recent work");
        assert_eq!(meta.custom_title.as_deref(), Some("User title"));
        assert_eq!(meta.summary.as_deref(), Some("Tail summary"));
        assert_eq!(meta.tag.as_deref(), Some("bug"));
        assert_eq!(meta.git_branch.as_deref(), Some("main"));
        assert_eq!(meta.project_path.as_deref(), Some("/repo"));
        assert_eq!(meta.agent_setting.as_deref(), Some("reviewer"));
        assert_eq!(meta.pr_number, Some(42));
        assert_eq!(meta.pr_url.as_deref(), Some("https://example.test/pr/42"));
        assert_eq!(meta.pr_repository.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn leaf_uuid_walks_terminal_attachment_to_user_assistant_ancestor() {
        let session = build_session(vec![
            json!({
                "type": "user",
                "uuid": "u1",
                "parentUuid": null,
                "timestamp": "2026-06-13T13:39:30.000Z",
                "message": {"role": "user", "content": "hello"}
            }),
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": "u1",
                "timestamp": "2026-06-13T13:39:31.000Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}
            }),
            json!({
                "type": "attachment",
                "uuid": "att1",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:39:32.000Z",
                "attachment": {"type": "file", "content": "note"}
            }),
        ]);

        assert!(session.leaf_uuids.contains("a1"));
        assert!(!session.leaf_uuids.contains("att1"));
        assert_eq!(get_latest_leaf_uuid(&session).as_deref(), Some("a1"));
        assert_eq!(
            uuids(&build_conversation_chain(&session, "a1")),
            vec!["u1", "a1"]
        );
    }

    #[test]
    fn load_session_structured_keeps_only_last_large_attribution_snapshot_like_official() {
        let path = temp_jsonl_path("attr-snapshot-large");
        let session_id = "attr-large-session";
        let root = Uuid::new_v4().to_string();
        let after = Uuid::new_v4().to_string();
        let large_content = "x".repeat((SKIP_PRECOMPACT_THRESHOLD as usize) + 1024);
        write_lines(
            &path,
            vec![
                transcript_line(
                    None,
                    "user",
                    &root,
                    "2026-06-13T13:39:30.000Z",
                    session_id,
                    &large_content,
                ),
                value_line(json!({
                    "type": "attribution-snapshot",
                    "messageId": "first-message",
                    "attribution": {"value": "first"}
                })),
                transcript_line(
                    Some(&root),
                    "assistant",
                    &after,
                    "2026-06-13T13:39:31.000Z",
                    session_id,
                    "after",
                ),
                value_line(json!({
                    "type": "attribution-snapshot",
                    "messageId": "last-message",
                    "attribution": {"value": "last"}
                })),
            ],
        );

        let session = load_session_structured_from_path(&path);
        fs::remove_file(&path).ok();

        assert_eq!(session.attribution_snapshots.len(), 1);
        assert_eq!(
            session.attribution_snapshots[0]
                .get("messageId")
                .and_then(|value| value.as_str()),
            Some("last-message")
        );
    }

    #[test]
    fn load_session_structured_truncates_large_compact_boundary_and_restores_metadata() {
        let path = temp_jsonl_path("compact-large");
        let session_id = "compact-large-session";
        let before = Uuid::new_v4().to_string();
        let after = Uuid::new_v4().to_string();
        let large_content = "x".repeat((SKIP_PRECOMPACT_THRESHOLD as usize) + 1024);
        write_lines(
            &path,
            vec![
                value_line(json!({
                    "type": "custom-title",
                    "sessionId": session_id,
                    "customTitle": "Pre-boundary title"
                })),
                value_line(json!({
                    "type": "tag",
                    "sessionId": session_id,
                    "tag": "resume-large"
                })),
                transcript_line(
                    None,
                    "user",
                    &before,
                    "2026-06-13T13:39:30.000Z",
                    session_id,
                    &large_content,
                ),
                value_line(json!({
                    "type": "system",
                    "uuid": Uuid::new_v4().to_string(),
                    "parentUuid": before,
                    "sessionId": session_id,
                    "timestamp": "2026-06-13T13:39:31.000Z",
                    "subtype": SUBTYPE_COMPACT_BOUNDARY,
                    "message": {"role": "system", "content": "compact"}
                })),
                transcript_line(
                    None,
                    "user",
                    &after,
                    "2026-06-13T13:39:32.000Z",
                    session_id,
                    "after boundary",
                ),
            ],
        );

        let session = load_session_structured_from_path(&path);
        fs::remove_file(&path).ok();

        assert!(!session.messages.contains_key(&before));
        assert!(session.messages.contains_key(&after));
        assert_eq!(
            session.custom_titles.get(session_id).map(String::as_str),
            Some("Pre-boundary title")
        );
        assert_eq!(
            session.tags.get(session_id).map(String::as_str),
            Some("resume-large")
        );
    }

    #[test]
    fn read_transcript_entries_keeps_preserved_segment_large_buffer_before_chain_drop() {
        let path = temp_jsonl_path("preserved-large");
        let session_id = "preserved-large-session";
        let root = Uuid::new_v4().to_string();
        let dead = Uuid::new_v4().to_string();
        let live = Uuid::new_v4().to_string();
        let large_dead_content = "x".repeat((SKIP_PRECOMPACT_THRESHOLD as usize) + 1024);
        write_lines(
            &path,
            vec![
                transcript_line(
                    None,
                    "user",
                    &root,
                    "2026-06-13T13:39:30.000Z",
                    session_id,
                    "root",
                ),
                transcript_line(
                    Some(&root),
                    "assistant",
                    &dead,
                    "2026-06-13T13:39:31.000Z",
                    session_id,
                    &large_dead_content,
                ),
                value_line(json!({
                    "type": "system",
                    "uuid": Uuid::new_v4().to_string(),
                    "parentUuid": dead,
                    "sessionId": session_id,
                    "timestamp": "2026-06-13T13:39:32.000Z",
                    "subtype": SUBTYPE_COMPACT_BOUNDARY,
                    "compactMetadata": {
                        "preservedSegment": {
                            "headUuid": root,
                            "tailUuid": dead,
                            "anchorUuid": root
                        }
                    },
                    "message": {"role": "system", "content": "compact"}
                })),
                transcript_line(
                    Some(&root),
                    "assistant",
                    &live,
                    "2026-06-13T13:39:33.000Z",
                    session_id,
                    "live",
                ),
            ],
        );

        let entries = read_transcript_entries(&path);
        fs::remove_file(&path).ok();
        let parsed_uuids = uuids(&entries);

        assert!(parsed_uuids.contains(&root.as_str()));
        assert!(parsed_uuids.contains(&dead.as_str()));
        assert!(parsed_uuids.contains(&live.as_str()));
    }

    #[test]
    fn load_session_structured_drops_large_dead_fork_before_parse_like_official() {
        let path = temp_jsonl_path("dead-fork-large");
        let session_id = "dead-fork-large-session";
        let root = Uuid::new_v4().to_string();
        let dead = Uuid::new_v4().to_string();
        let live = Uuid::new_v4().to_string();
        let large_dead_content = "x".repeat((SKIP_PRECOMPACT_THRESHOLD as usize) + 1024);
        write_lines(
            &path,
            vec![
                transcript_line(
                    None,
                    "user",
                    &root,
                    "2026-06-13T13:39:30.000Z",
                    session_id,
                    "root",
                ),
                transcript_line(
                    Some(&root),
                    "assistant",
                    &dead,
                    "2026-06-13T13:39:31.000Z",
                    session_id,
                    &large_dead_content,
                ),
                transcript_line(
                    Some(&root),
                    "assistant",
                    &live,
                    "2026-06-13T13:39:32.000Z",
                    session_id,
                    "live",
                ),
            ],
        );

        let raw_entries = read_transcript_entries(&path);
        let session = load_session_structured_from_path(&path);
        fs::remove_file(&path).ok();
        let chain = build_conversation_chain(&session, &live);

        assert_eq!(uuids(&raw_entries), vec![root.as_str(), live.as_str()]);
        assert!(!session.messages.contains_key(&dead));
        assert_eq!(uuids(&chain), vec![root.as_str(), live.as_str()]);
    }

    #[test]
    fn load_session_structured_bridges_legacy_progress_entries_like_official() {
        let session_id = "progress-bridge-session";
        let path = std::env::temp_dir().join(format!(
            "cometix-session-progress-bridge-{}.jsonl",
            Uuid::new_v4()
        ));
        let entries = vec![
            json!({
                "type": "user",
                "uuid": "u1",
                "parentUuid": null,
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:39:30.000Z",
                "message": {"role": "user", "content": "start"}
            }),
            json!({
                "type": "progress",
                "uuid": "p1",
                "parentUuid": "u1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:39:31.000Z",
                "data": {"type": "bash_progress"}
            }),
            json!({
                "type": "progress",
                "uuid": "p2",
                "parentUuid": "p1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:39:32.000Z",
                "data": {"type": "bash_progress"}
            }),
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": "p2",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:39:33.000Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}
            }),
            json!({
                "type": "user",
                "uuid": "u2",
                "parentUuid": "a1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:39:34.000Z",
                "message": {"role": "user", "content": "next"}
            }),
        ];
        let contents = entries
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{contents}\n")).unwrap();

        let session = load_session_structured_from_path(&path);
        let _ = fs::remove_file(&path);

        assert!(!session.messages.contains_key("p1"));
        assert!(!session.messages.contains_key("p2"));
        assert_eq!(
            session
                .messages
                .get("a1")
                .and_then(|message| message.get("parentUuid"))
                .and_then(Value::as_str),
            Some("u1")
        );
        assert_eq!(
            uuids(&build_conversation_chain(&session, "u2")),
            vec!["u1", "a1", "u2"]
        );
    }

    #[test]
    fn load_session_structured_applies_snip_removals_and_relinks_survivors() {
        let session_id = "snip-removal-session";
        let path = std::env::temp_dir().join(format!(
            "cometix-session-snip-removal-{}.jsonl",
            Uuid::new_v4()
        ));
        let entries = vec![
            json!({
                "type": "user",
                "uuid": "u1",
                "parentUuid": null,
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:38:30.000Z",
                "message": {"role": "user", "content": "start"}
            }),
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": "u1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:38:31.000Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "keep"}]}
            }),
            json!({
                "type": "user",
                "uuid": "u-removed",
                "parentUuid": "a1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:38:32.000Z",
                "message": {"role": "user", "content": "remove user"}
            }),
            json!({
                "type": "assistant",
                "uuid": "a-removed",
                "parentUuid": "u-removed",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:38:33.000Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "remove assistant"}]}
            }),
            json!({
                "type": "user",
                "uuid": "u2",
                "parentUuid": "a-removed",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:38:34.000Z",
                "message": {"role": "user", "content": "survivor"}
            }),
            json!({
                "type": "system",
                "uuid": "snip-boundary",
                "parentUuid": "u2",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:38:35.000Z",
                "subtype": "snip_boundary",
                "snipMetadata": {"removedUuids": ["u-removed", "a-removed"]},
                "message": {"role": "system", "content": "snipped"}
            }),
            json!({
                "type": "user",
                "uuid": "u3",
                "parentUuid": "snip-boundary",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:38:36.000Z",
                "message": {"role": "user", "content": "next"}
            }),
        ];
        let contents = entries
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{contents}\n")).unwrap();

        let session = load_session_structured_from_path(&path);
        let _ = fs::remove_file(&path);

        assert!(!session.messages.contains_key("u-removed"));
        assert!(!session.messages.contains_key("a-removed"));
        assert_eq!(
            session
                .messages
                .get("u2")
                .and_then(|message| message.get("parentUuid"))
                .and_then(Value::as_str),
            Some("a1")
        );
        assert_eq!(
            uuids(&build_conversation_chain(&session, "u3")),
            vec!["u1", "a1", "u2", "snip-boundary", "u3"]
        );
    }

    #[test]
    fn build_conversation_chain_recovers_parallel_tool_result_sibling() {
        let session = build_session(vec![
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": null,
                "timestamp": "2026-06-13T13:40:30.000Z",
                "message": {
                    "id": "msg_parallel",
                    "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "first"}}]
                }
            }),
            json!({
                "type": "assistant",
                "uuid": "a2",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:40:31.000Z",
                "message": {
                    "id": "msg_parallel",
                    "content": [{"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {"command": "second"}}]
                }
            }),
            json!({
                "type": "user",
                "uuid": "r1",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:40:32.000Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "first output"}]},
                "toolUseResult": {"stdout": "first output", "stderr": ""}
            }),
            json!({
                "type": "user",
                "uuid": "r2",
                "parentUuid": "a2",
                "timestamp": "2026-06-13T13:40:33.000Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "second output"}]},
                "toolUseResult": {"stdout": "second output", "stderr": ""}
            }),
        ]);

        let chain = build_conversation_chain(&session, "r2");

        assert_eq!(uuids(&chain), vec!["a1", "a2", "r1", "r2"]);
    }

    #[test]
    fn build_conversation_chain_recovers_orphaned_tool_result_when_walk_follows_other_child() {
        let session = build_session(vec![
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": null,
                "timestamp": "2026-06-13T13:41:30.000Z",
                "message": {
                    "id": "msg_tool",
                    "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "echo hi"}}]
                }
            }),
            json!({
                "type": "attachment",
                "uuid": "p1",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:41:31.000Z",
                "attachment": {"type": "hook_success", "toolUseID": "toolu_1"}
            }),
            json!({
                "type": "user",
                "uuid": "u2",
                "parentUuid": "p1",
                "timestamp": "2026-06-13T13:41:33.000Z",
                "message": {"role": "user", "content": "next prompt"}
            }),
            json!({
                "type": "user",
                "uuid": "r1",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:41:32.000Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "hi"}]},
                "toolUseResult": {"stdout": "hi", "stderr": ""}
            }),
        ]);

        let chain = build_conversation_chain(&session, "u2");

        assert_eq!(uuids(&chain), vec!["a1", "r1", "p1", "u2"]);
    }

    #[test]
    fn build_conversation_chain_keeps_stable_timestamp_order_for_recovered_messages() {
        let session = build_session(vec![
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": null,
                "timestamp": "2026-06-13T13:42:30.000Z",
                "message": {
                    "id": "msg_parallel",
                    "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "one"}}]
                }
            }),
            json!({
                "type": "user",
                "uuid": "next",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:42:35.000Z",
                "message": {"role": "user", "content": "next prompt"}
            }),
            json!({
                "type": "assistant",
                "uuid": "a2",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:42:31.000Z",
                "message": {
                    "id": "msg_parallel",
                    "content": [{"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {"command": "two"}}]
                }
            }),
            json!({
                "type": "assistant",
                "uuid": "a3",
                "parentUuid": "a1",
                "timestamp": "2026-06-13T13:42:31.000Z",
                "message": {
                    "id": "msg_parallel",
                    "content": [{"type": "tool_use", "id": "toolu_3", "name": "Bash", "input": {"command": "three"}}]
                }
            }),
            json!({
                "type": "user",
                "uuid": "r2",
                "parentUuid": "a2",
                "timestamp": "2026-06-13T13:42:32.000Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "two"}]}
            }),
            json!({
                "type": "user",
                "uuid": "r3",
                "parentUuid": "a3",
                "timestamp": "2026-06-13T13:42:32.000Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_3", "content": "three"}]}
            }),
        ]);

        let chain = build_conversation_chain(&session, "next");

        assert_eq!(uuids(&chain), vec!["a1", "a2", "a3", "r2", "r3", "next"]);
    }

    /// Maps to: CC `isLoggableMessage` (sessionStorage.ts:4357-4366).
    ///
    /// Attachments are withheld from non-ant transcripts — the source's comment
    /// gives the reason: "they have sensitive info for training that we don't
    /// want exposed to the public". Hook output is the single exception, and
    /// only behind `CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT`, because it is
    /// user-configured content worth having on resume.
    ///
    /// `getUserType() !== 'ant'` is the build audience here, which is a
    /// compile-time feature — so this asserts against the CURRENT build and
    /// both sides are covered by `just test-all-audiences`.
    #[test]
    fn attachments_are_withheld_from_external_transcripts_like_official() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_env::remove("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");

        let plain = crate::types::message::Message::Attachment(
            crate::types::message::AttachmentMessage::new(
                serde_json::json!({"type": "file_read", "path": "/tmp/notes.txt"}),
            ),
        );
        let hook = crate::types::message::Message::Attachment(
            crate::types::message::AttachmentMessage::new(
                serde_json::json!({"type": "hook_additional_context", "content": "hook output"}),
            ),
        );
        let internal = crate::utils::build_profile::build_audience().is_internal();

        // Flag off: an external build logs neither.
        assert_eq!(typed_message_entry(&plain).is_some(), internal);
        assert_eq!(typed_message_entry(&hook).is_some(), internal);

        // Flag on: it opens the hook exception ONLY — not attachments at large.
        crate::utils::process_env::set("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT", "1");
        assert_eq!(
            typed_message_entry(&plain).is_some(),
            internal,
            "the env flag is not a blanket opt-in for attachments"
        );
        assert!(
            typed_message_entry(&hook).is_some(),
            "hook_additional_context is loggable on every audience once the flag is set"
        );
        crate::utils::process_env::remove("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");
    }

    /// Maps to: CC `isLoggableMessage` (sessionStorage.ts:4351-4367).
    ///
    /// A hook result at CC runtime is an attachment-typed row
    /// (`createAttachmentMessage`, utils/attachments.ts:3201-3210 — the
    /// `hook_result` union member of types/message.ts:119 has no runtime
    /// producer), and the filter keys on that WIRE type. So the
    /// `Message::HookResult` arm — which writes `"type":"attachment"` —
    /// takes the same withholding as the `Attachment` arm: an external
    /// build writes nothing, except `hook_additional_context` behind
    /// `CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT`; an internal build
    /// writes everything. Asserted against the CURRENT build audience;
    /// both sides run under `just test-all-audiences`.
    #[test]
    fn hook_results_take_the_attachment_withholding_like_official() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_env::remove("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");

        // The SessionStart projection's two payload shapes
        // (session_start.rs#hook_result_messages ↔ sessionStart.ts:164-171).
        let context = crate::types::message::Message::HookResult(
            crate::types::message::HookResultMessage::attachment(serde_json::json!({
                "type": "hook_additional_context",
                "content": ["hook output"],
                "hookName": "SessionStart",
                "toolUseID": "SessionStart",
                "hookEvent": "SessionStart"
            })),
        );
        let system = crate::types::message::Message::HookResult(
            crate::types::message::HookResultMessage::attachment(serde_json::json!({
                "type": "hook_system_message",
                "content": "a system note",
                "hookName": "SessionStart",
                "toolUseID": "SessionStart",
                "hookEvent": "SessionStart"
            })),
        );
        let internal = crate::utils::build_profile::build_audience().is_internal();

        // Flag off: an external build withholds both hook rows.
        assert_eq!(typed_message_entry(&context).is_some(), internal);
        assert_eq!(typed_message_entry(&system).is_some(), internal);

        // Flag on: only hook_additional_context comes through — the same
        // single exception the Attachment arm grants.
        crate::utils::process_env::set("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT", "1");
        assert!(
            typed_message_entry(&context).is_some(),
            "hook_additional_context is loggable on every audience once the flag is set"
        );
        assert_eq!(
            typed_message_entry(&system).is_some(),
            internal,
            "the env flag opens the hook-context exception only"
        );
        crate::utils::process_env::remove("CLAUDE_CODE_SAVE_HOOK_ADDITIONAL_CONTEXT");

        // The row that IS written keeps CC's wire shape: hook results have no
        // wire type of their own — they are attachment rows.
        if internal {
            let (entry, _) = typed_message_entry(&system).expect("internal build writes hook rows");
            assert_eq!(
                entry.get("type").and_then(serde_json::Value::as_str),
                Some("attachment")
            );
        }
    }

    #[test]
    fn typed_attachment_transcript_round_trip_preserves_open_payload() {
        let payload = serde_json::json!({
            "type": "task_status",
            "taskId": "agent-1",
            "status": "running",
            "deltaSummary": {"nested": [1, 2, 3]},
            "futureField": {"kept": true}
        });
        let messages = vec![crate::types::message::Message::Attachment(
            crate::types::message::AttachmentMessage::new(payload.clone()),
        )];

        let entries = typed_messages_as_transcript_values(&messages);
        if !crate::utils::build_profile::build_audience().is_internal() {
            // CC `isLoggableMessage` (sessionStorage.ts:4357-4366) keeps
            // attachments out of non-ant transcripts entirely, so there is
            // nothing to round-trip on this build. The payload fidelity this
            // test covers is exercised by the internal-audience run
            // (`just test-all-audiences`).
            assert!(entries.is_empty());
            return;
        }
        assert_eq!(entries[0].get("attachment"), Some(&payload));
        let restored = crate::utils::conversation::into_typed_messages(entries);
        // The malformed-for-`task_status` payload rides `Attachment::Unknown`
        // (batch D2), whose wire projection is the untouched open payload.
        assert!(matches!(
            restored.as_slice(),
            [crate::types::message::Message::Attachment(attachment)]
                if attachment.attachment.to_wire() == payload
        ));
    }

    #[test]
    fn build_conversation_chain_stops_on_parent_cycle() {
        let session = build_session(vec![
            json!({
                "type": "user",
                "uuid": "a",
                "parentUuid": "b",
                "timestamp": "2026-06-13T13:43:30.000Z",
                "message": {"role": "user", "content": "a"}
            }),
            json!({
                "type": "user",
                "uuid": "b",
                "parentUuid": "a",
                "timestamp": "2026-06-13T13:43:31.000Z",
                "message": {"role": "user", "content": "b"}
            }),
        ]);

        let chain = build_conversation_chain(&session, "a");

        assert_eq!(uuids(&chain), vec!["b", "a"]);
    }
    /// CC services/api/claude.ts:2235-2248 mutates the queued message before
    /// sessionStorage.ts:658 stringify. Delta must not create/reparent a row.
    #[test]
    fn queued_assistant_delta_matches_official_lazy_write_and_drain_race() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("queued-delta-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("main.jsonl");
        let other = root.join("other.jsonl");
        let (sender, receiver) = std::sync::mpsc::channel();
        let entry = json!({"type":"assistant", "uuid":"answer", "parentUuid":"retained", "message":{"content":[],"stop_reason":null,"usage":null}});
        for target in [&path, &other] {
            let (resolve, _written) = async_channel::bounded(1);
            sender
                .send(ProjectWriteCommand::EnqueueWrite {
                    path: target.clone(),
                    entry: entry.clone(),
                    resolve,
                })
                .unwrap();
        }
        sender
            .send(ProjectWriteCommand::PatchAssistantDelta {
                path: path.clone(),
                uuid: "answer".into(),
                stop_reason: json!("end_turn"),
                usage: json!({"input_tokens":12,"output_tokens":3}),
            })
            .unwrap();
        let (reply, _flushed) = async_channel::bounded(1);
        sender.send(ProjectWriteCommand::Flush { reply }).unwrap();
        // A delta after drain is ignored, exactly like mutating CC's object
        // after it has already been stringified; it must not append a correction.
        sender
            .send(ProjectWriteCommand::PatchAssistantDelta {
                path: path.clone(),
                uuid: "answer".into(),
                stop_reason: json!("max_tokens"),
                usage: json!({}),
            })
            .unwrap();
        drop(sender);
        Project::schedule_drain(receiver);
        let rows = read_jsonl(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["parentUuid"], "retained");
        assert_eq!(rows[0]["message"]["stop_reason"], "end_turn");
        assert_eq!(rows[0]["message"]["usage"]["output_tokens"], 3);
        assert!(read_jsonl(&other)[0]["message"]["stop_reason"].is_null());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn transcript_helpers_match_official_type_and_session_path_rules() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct Restore {
            id: String,
            dir: Option<PathBuf>,
            meta: CurrentSessionMeta,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(&self.id, self.dir.clone());
                *get_project().current_session_meta.write().unwrap() = self.meta.clone();
            }
        }
        let _restore = Restore {
            id: crate::bootstrap::state::get_session_id(),
            dir: crate::bootstrap::state::get_session_project_dir(),
            meta: get_project().current_session_meta.read().unwrap().clone(),
        };
        for kind in ["user", "assistant", "attachment", "system"] {
            assert!(is_transcript_message(&json!({"type":kind})));
        }
        for entry in [
            json!({}),
            json!({"type":"progress"}),
            json!({"type":"content-replacement"}),
            json!({"type":"custom-title"}),
        ] {
            assert!(!is_transcript_message(&entry));
        }
        let detached = PathBuf::from("/tmp/cc-detached-project");
        crate::bootstrap::state::switch_session("current", Some(detached.clone()));
        get_project()
            .current_session_meta
            .write()
            .unwrap()
            .session_file = Some(PathBuf::from("/tmp/stale-materialized.jsonl"));
        assert_eq!(
            get_transcript_path_for_session("current"),
            detached.join("current.jsonl")
        );
        let cwd = crate::bootstrap::state::get_original_cwd();
        assert_eq!(
            get_transcript_path_for_session("other"),
            get_session_file_path(&cwd.to_string_lossy(), "other")
        );
        crate::bootstrap::state::switch_session("current", None);
        assert_eq!(
            get_transcript_path_for_session("current"),
            get_session_file_path(&cwd.to_string_lossy(), "current")
        );
    }

    #[cfg(unix)]
    #[test]
    fn search_custom_titles_matches_official_all_pages_worktrees_and_limits() {
        use std::os::unix::fs::PermissionsExt;
        // getWorktreePaths runs gitExe(), whose Bun.which lookup reads the startup
        // PATH (utils/git.ts:212-216), so the fake git must be on PATH when the
        // process starts: the parent writes it and re-runs this test as a child.
        const PROBE: &str = "COMETIX_TITLE_SEARCH_ROOT";
        let cwd = crate::bootstrap::state::get_original_cwd()
            .to_string_lossy()
            .into_owned();
        let Some(root) = crate::utils::process_env::var(PROBE) else {
            let root =
                std::env::temp_dir().join(format!("cometix-title-search-{}", Uuid::new_v4()));
            struct Cleanup(PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = fs::remove_dir_all(&self.0);
                }
            }
            let _cleanup = Cleanup(root.clone());
            let sibling = root.join("sibling").to_string_lossy().into_owned();
            let bin = root.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let git = bin.join("git");
            fs::write(
                &git,
                format!("#!/bin/sh\nprintf '%s\\n' 'worktree {cwd}' '' 'worktree {sibling}' ''\n"),
            )
            .unwrap();
            fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "utils::session_storage::tests::search_custom_titles_matches_official_all_pages_worktrees_and_limits",
                    "--nocapture",
                ])
                .env(PROBE, &root)
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        bin.display(),
                        crate::utils::process_env::var("PATH").unwrap_or_default()
                    ),
                )
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{stdout} {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = PathBuf::from(root);
        let _projects = set_test_projects_dir_override(root.join("projects"));
        let sibling = root.join("sibling").to_string_lossy().into_owned();
        let current_dir = get_project_dir(&cwd);
        let sibling_dir = get_project_dir(&sibling);
        for dir in [&current_dir, &sibling_dir] {
            fs::create_dir_all(dir).unwrap();
        }
        let write = |dir: &Path, id: &str, title: &str, time: u64, extra: Value| {
            let path = dir.join(format!("{id}.jsonl"));
            let mut entry = json!({"type":"user", "sessionId":id, "message":{"content":"hello"}});
            entry
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            fs::write(
                &path,
                format!(
                    "{}\n{}\n",
                    entry,
                    json!({"type":"custom-title","sessionId":id,"customTitle":title})
                ),
            )
            .unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(
                    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(time),
                ))
                .unwrap();
        };
        for index in 0..60 {
            write(
                &current_dir,
                &format!("s{index}"),
                &format!("Topic {index}"),
                100 + index,
                json!({}),
            );
        }
        write(&current_dir, "shared", "Old title", 50, json!({}));
        write(
            &sibling_dir,
            "shared",
            "\u{feff}TOPIC (Branch)\u{00a0}",
            500,
            json!({}),
        );
        write(
            &sibling_dir,
            "sidechain",
            "Topic hidden",
            900,
            json!({"isSidechain":true}),
        );
        write(
            &sibling_dir,
            "team",
            "Topic team",
            950,
            json!({"teamName":"members"}),
        );
        write(&sibling_dir, "blank", "\u{feff} ", 999, json!({}));
        let all = search_sessions_by_custom_title(" TOPIC ", None);
        assert_eq!(all.len(), 61);
        assert_eq!(all[0].session_id, "shared");
        assert_eq!(all.last().unwrap().session_id, "s0");
        let exact = search_sessions_by_custom_title(
            " topic (branch) ",
            Some(SearchSessionsByCustomTitleOptions {
                exact: true,
                limit: None,
            }),
        );
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].file_path, sibling_dir.join("shared.jsonl"));
        assert!(search_sessions_by_custom_title("Old title", None).is_empty());
        for (limit, expected) in [(0, 61), (2, 2), (-1, 60), (-100, 0)] {
            assert_eq!(
                search_sessions_by_custom_title(
                    "topic",
                    Some(SearchSessionsByCustomTitleOptions {
                        limit: Some(limit),
                        exact: false,
                    })
                )
                .len(),
                expected
            );
        }
        assert_eq!(search_sessions_by_custom_title("", None).len(), 61);
    }

    #[test]
    fn find_latest_message_matches_official_time_offsets_ties_and_invalid_dates() {
        let entries = vec![
            json!({"uuid":"missing"}),
            json!({"uuid":"invalid","timestamp":"not-a-date"}),
            json!({"uuid":"first","timestamp":"2026-09-12T01:00:00.000Z"}),
            json!({"uuid":"offset-older","timestamp":"2026-09-12T02:30:00+02:00"}),
            json!({"uuid":"tie","timestamp":"2026-09-12T03:00:00.000+02:00"}),
            json!({"uuid":"same-js-ms","timestamp":"2026-09-12T01:00:00.0009Z"}),
        ];
        assert_eq!(
            find_latest_message(&entries, |_| true).unwrap()["uuid"],
            "first"
        );
        assert_eq!(
            find_latest_message(&entries, |entry| entry["uuid"] != "first").unwrap()["uuid"],
            "tie"
        );
        assert!(find_latest_message(&entries[..2], |_| true).is_none());
    }

    #[test]
    fn get_last_session_log_matches_official_terminal_system_and_vacant_cache_prime() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct Restore {
            id: String,
            dir: Option<PathBuf>,
            meta: CurrentSessionMeta,
            path: PathBuf,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(&self.id, self.dir.clone());
                *get_project().current_session_meta.write().unwrap() = self.meta.clone();
                let _ = fs::remove_file(&self.path);
            }
        }
        let restore = Restore {
            id: crate::bootstrap::state::get_session_id(),
            dir: crate::bootstrap::state::get_session_project_dir(),
            meta: get_project().current_session_meta.read().unwrap().clone(),
            path: {
                let dir = temp_jsonl_path("last-session-local-command-dir");
                fs::create_dir_all(&dir).unwrap();
                dir.join("last-session.jsonl")
            },
        };
        let id = "last-session";
        crate::bootstrap::state::switch_session(id, restore.path.parent().map(Path::to_path_buf));
        clear_session_messages_cache();
        reset_session_file_pointer();
        let entries = [
            json!({"type":"user","uuid":"u","parentUuid":null,"sessionId":id,
                "timestamp":"2026-09-12T01:00:00.000Z","message":{"role":"user","content":"seed"}}),
            json!({"type":"assistant","uuid":"a","parentUuid":"u","sessionId":id,
                "timestamp":"2026-09-12T01:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"ready"}]}}),
            json!({"type":"system","subtype":"local_command","uuid":"cmd","parentUuid":"a","sessionId":id,
                "timestamp":"2026-09-12T01:00:02.000Z","content":"/branch copied"}),
            json!({"type":"system","subtype":"local_command","uuid":"out","parentUuid":"cmd","sessionId":id,
                "timestamp":"2026-09-12T01:00:03.000Z","content":"Branched conversation."}),
            json!({"type":"assistant","uuid":"side","parentUuid":"a","sessionId":id,"isSidechain":true,
                "timestamp":"2026-09-12T02:00:00.000Z","message":{"role":"assistant","content":[]}}),
        ];
        fs::write(
            &restore.path,
            entries
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let (loaded, transcript) = get_last_session_log(id).unwrap();
        assert_eq!(
            transcript
                .iter()
                .map(|entry| entry["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["u", "a", "cmd", "out"]
        );
        assert!(
            transcript.iter().all(
                |entry| entry.get("parentUuid").is_none() && entry.get("isSidechain").is_none()
            )
        );
        // Source leaf policy remains separate: terminal local commands do not
        // turn into user/assistant leaves for picker/loadFullLog.
        assert!(!loaded.leaf_uuids.contains("out"));
        assert!(!loaded.leaf_uuids.contains("cmd"));
        // loadFullLog's leaf predicate does not exclude sidechains. This
        // fixture's later sidechain assistant wins, while getLastSessionLog
        // above excludes it and retains the terminal local-command chain.
        let picker_leaf = get_latest_leaf_uuid(&loaded).unwrap();
        assert_eq!(picker_leaf, "side");
        assert_eq!(
            build_conversation_chain(&loaded, &picker_leaf)
                .iter()
                .map(|entry| entry["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["u", "a", "side"]
        );
        let message_set = get_session_messages(id);
        assert_eq!(message_set.read().unwrap().len(), 5);
        message_set
            .write()
            .unwrap()
            .insert("queued-not-on-disk".into());
        get_last_session_log(id).unwrap();
        assert!(message_set.read().unwrap().contains("queued-not-on-disk"));
        clear_session_messages_cache();
    }
    #[test]
    fn session_message_cache_matches_official_per_id_priming_and_clear() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let previous_id = crate::bootstrap::state::get_session_id();
        let previous_dir = crate::bootstrap::state::get_session_project_dir();
        let dir = temp_jsonl_path("session-id-memo-dir");
        fs::create_dir_all(&dir).unwrap();
        crate::bootstrap::state::switch_session("unrelated-active", Some(dir.clone()));
        clear_session_messages_cache();
        let row = |uuid: &str, id: &str| {
            json!({"type":"user","uuid":uuid,"sessionId":id,"parentUuid":null,
            "timestamp":"2026-09-12T01:00:00.000Z","message":{"role":"user","content":"seed"}})
        };
        fs::write(
            dir.join("first.jsonl"),
            format!("{}\n", row("u-first", "first")),
        )
        .unwrap();
        fs::write(
            dir.join("second.jsonl"),
            format!("{}\n", row("u-second", "second")),
        )
        .unwrap();
        assert_eq!(load_session_file_path("first"), dir.join("first.jsonl"));
        assert!(get_last_session_log("first").is_some());
        assert!(get_last_session_log("second").is_some());
        assert!(does_message_exist_in_session("first", "u-first"));
        assert!(!does_message_exist_in_session("second", "u-first"));
        get_session_messages("first")
            .write()
            .unwrap()
            .insert("queued".into());
        get_last_session_log("first").unwrap();
        assert!(does_message_exist_in_session("first", "queued"));
        // Both non-active slots retain their loaded sets even if disk changes.
        fs::write(
            dir.join("first.jsonl"),
            format!("{}\n", row("disk-new", "first")),
        )
        .unwrap();
        assert!(!does_message_exist_in_session("first", "disk-new"));
        clear_session_messages_cache();
        assert!(does_message_exist_in_session("first", "disk-new"));
        assert!(!does_message_exist_in_session("first", "queued"));
        assert!(does_message_exist_in_session("second", "u-second"));
        clear_session_messages_cache();
        crate::bootstrap::state::switch_session(&previous_id, previous_dir);
        fs::remove_dir_all(&dir).unwrap();
    }
    /// Maps to CC conversationRecovery.ts:209-214, messages.ts:460-524,
    /// sessionStorage.ts:1039-1068 and services/api/claude.ts:588-631.
    #[test]
    fn cold_continuation_matches_official_meta_identity_and_api_projection() {
        let raw = crate::utils::messages::create_user_message_value(
            "Continue from where you left off.",
            true,
        );
        let typed = crate::utils::conversation::into_typed_messages(vec![raw.clone()]);
        let (entry, id) = typed_message_entry(&typed[0]).unwrap();
        assert_eq!(id, raw["uuid"].as_str().unwrap());
        assert_eq!(entry["isMeta"], true);
        assert_eq!(
            entry["message"]["content"],
            json!([
                {"type":"text", "text":"Continue from where you left off."}
            ])
        );
        let restored = crate::utils::conversation::into_typed_messages(vec![entry]);
        assert_eq!(restored, typed);
        let crate::types::message::Message::User(user) = &restored[0] else {
            panic!("continuation must remain a meta user message");
        };
        let api =
            crate::services::api::claude::user_message_to_message_param(user, false, false, None);
        assert_eq!(
            serde_json::to_value(api).unwrap(),
            json!({
                "role":"user", "content":[{
                    "type":"text", "text":"Continue from where you left off."
                }]
            })
        );
    }

    /// Maps to CC conversationRecovery.ts:240-244, messages.ts:355-408,
    /// sessionStorage.ts:1039-1068 insertMessageChain and services/api/claude.ts:670-673.
    /// Check the actual cold reader -> typed writer -> cold reader and API consumer;
    /// the broader optional API-envelope carrier remains a separately recorded gap.
    #[test]
    fn cold_sentinel_matches_official_identity_usage_and_api_roundtrip() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        struct Restore {
            session_id: String,
            project_dir: Option<PathBuf>,
            meta: CurrentSessionMeta,
            root: PathBuf,
            drained: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                *get_project().current_session_meta.write().unwrap() = self.meta.clone();
                crate::bootstrap::state::switch_session(&self.session_id, self.project_dir.clone());
                clear_session_messages_cache();
                // A timed-out writer may still own queued paths. Leave its
                // unique directory in place instead of racing deletion.
                if self.drained {
                    let _ = fs::remove_dir_all(&self.root);
                }
            }
        }
        let mut restore = Restore {
            session_id: crate::bootstrap::state::get_session_id(),
            project_dir: crate::bootstrap::state::get_session_project_dir(),
            meta: get_project().current_session_meta.read().unwrap().clone(),
            root: std::env::temp_dir().join(format!("cometix-cold-sentinel-{}", Uuid::new_v4())),
            drained: false,
        };
        let id = Uuid::new_v4().to_string();
        clear_session_metadata();
        clear_session_messages_cache();
        crate::bootstrap::state::switch_session(&id, Some(restore.root.clone()));
        let recovered = crate::utils::conversation::deserialize_messages(vec![json!({
            "type":"user", "uuid":"interrupted-user", "message":{"role":"user","content":"hello"},
            "timestamp":"2026-09-12T00:00:00.000Z"
        })]);
        let raw = recovered.last().unwrap();
        let typed = crate::utils::conversation::into_typed_messages(recovered.clone());
        let sentinel = typed.last().unwrap();
        let (entry, uuid) = typed_message_entry(sentinel).unwrap();
        assert_eq!(uuid, raw["uuid"].as_str().unwrap());
        assert_eq!(entry["message"]["id"], raw["message"]["id"]);
        assert_eq!(entry["message"]["model"], "<synthetic>");
        assert_eq!(entry["message"]["stop_reason"], "stop_sequence");
        for field in [
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ] {
            assert_eq!(entry["message"]["usage"][field], 0, "{field}");
        }
        record_typed_messages(&typed).unwrap();
        match futures::executor::block_on(futures::future::select(
            Box::pin(flush_session_storage()),
            Box::pin(futures_timer::Delay::new(std::time::Duration::from_secs(
                10,
            ))),
        )) {
            futures::future::Either::Left((result, _)) => {
                restore.drained = true;
                result.unwrap();
            }
            futures::future::Either::Right(_) => panic!(
                "session writer did not flush within 10s; retained {}",
                restore.root.display()
            ),
        }
        let path = restore.root.join(format!("{id}.jsonl"));
        let reloaded = load_session_structured_from_path(&path);
        let saved = reloaded.messages.get(&uuid).expect("persisted sentinel");
        assert_eq!(saved["parentUuid"], "interrupted-user");
        assert_eq!(saved["sessionId"], id);
        assert_eq!(saved["message"], entry["message"]);
        let again = crate::utils::conversation::deserialize_messages(vec![
            reloaded.messages.get("interrupted-user").unwrap().clone(),
            saved.clone(),
        ]);
        assert_eq!(
            again.len(),
            2,
            "a recovered sentinel completes the turn; do not add another"
        );
        let typed = crate::utils::conversation::into_typed_messages(again);
        let crate::types::message::Message::Assistant(assistant) = &typed[1] else {
            panic!("assistant sentinel")
        };
        assert_eq!(assistant.model.as_deref(), Some("<synthetic>"));
        assert_eq!(
            assistant.stop_reason,
            Some(crate::types::message::StopReason::StopSequence)
        );
        assert_eq!(assistant.uuid, uuid);
        assert_eq!(assistant.api_message_id(), raw["message"]["id"].as_str());
        let usage = assistant.usage.as_ref().unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_creation_input_tokens,
                usage.cache_read_input_tokens
            ),
            (0, 0, 0, 0)
        );
        let param = crate::services::api::claude::assistant_message_to_message_param(
            assistant, false, false, None,
        );
        let json = serde_json::to_value(param).unwrap();
        assert_eq!(
            json["content"],
            json!([{"type":"text","text":"No response requested."}])
        );
        assert!(json.get("usage").is_none());
        assert!(json.get("model").is_none());
    }
}

#[cfg(test)]
mod raw_image_tests {
    use super::*;
    use crate::types::message::{Message, ToolResultContentBlock, UserContent};

    #[test]
    fn raw_images_roundtrip_matches_official_url_metadata_and_meta_envelope() {
        let blocks = [
            serde_json::json!({"type":"image","source":{"type":"url","url":"https://example.test/image.png"},"cache_control":{"type":"ephemeral"},"extra":{"keep":1}}),
            serde_json::json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA","extra":"source"},"cache_control":{"type":"ephemeral"}}),
        ];
        for is_meta in [false, true] {
            for block in &blocks {
                let wire = serde_json::json!({"type":"user","uuid":"image-uuid","timestamp":"2026-09-10T00:00:00Z","isMeta":is_meta,"message":{"role":"user","content":[block]}});
                let message = crate::utils::conversation::into_typed_messages(vec![wire])
                    .pop()
                    .unwrap();
                let Message::User(user) = &message else {
                    panic!("user expected");
                };
                assert!(
                    matches!(&user.content[0], UserContent::RawImage { block: retained, is_meta: retained_meta } if retained == block && *retained_meta == is_meta)
                );
                let (stored, _) = typed_message_entry(&message).unwrap();
                assert_eq!(stored["message"]["content"][0], *block);
                assert_eq!(
                    stored
                        .get("isMeta")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    is_meta
                );
                let restored = crate::utils::conversation::into_typed_messages(vec![stored])
                    .pop()
                    .unwrap();
                assert_eq!(restored, message);
                let agent_blocks =
                    transcript_user_content_blocks(&serde_json::json!([block]), is_meta);
                assert_eq!(agent_blocks, user.content);
                let sdk = crate::query_engine::stream_message_value(&message).unwrap();
                assert_eq!(sdk["message"]["content"][0], *block);
                assert_eq!(sdk["isSynthetic"], is_meta);
            }
        }
    }

    #[test]
    fn nested_raw_images_roundtrip_matches_official_without_accepting_unknown_blocks() {
        for image in [
            serde_json::json!({"type":"image","source":{"type":"url","url":"https://example.test/image.png"},"cache_control":{"type":"ephemeral"}}),
            serde_json::json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"},"extra":{"keep":true}}),
        ] {
            let block: ToolResultContentBlock = serde_json::from_value(image.clone()).unwrap();
            assert!(matches!(&block, ToolResultContentBlock::RawImage(value) if value == &image));
            assert_eq!(serde_json::to_value(&block).unwrap(), image);
            let wire = serde_json::json!({"type":"user","uuid":"result-uuid","timestamp":"2026-09-10T00:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool","content":[image],"is_error":false}]}});
            let message = crate::utils::conversation::into_typed_messages(vec![wire])
                .pop()
                .unwrap();
            let (stored, _) = typed_message_entry(&message).unwrap();
            assert_eq!(stored["message"]["content"][0]["content"][0], image);
        }
        assert!(
            serde_json::from_value::<ToolResultContentBlock>(
                serde_json::json!({"type":"future_non_image"})
            )
            .is_err()
        );
        assert!(matches!(
            ToolResultContentBlock::from_structured_value(
                &serde_json::json!({"type":"future_non_image"})
            ),
            ToolResultContentBlock::Text { .. }
        ));
        let plain = serde_json::json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}});
        assert!(matches!(
            serde_json::from_value::<ToolResultContentBlock>(plain.clone()).unwrap(),
            ToolResultContentBlock::Image { .. }
        ));
        assert!(matches!(
            UserContent::from_image_block(plain, true),
            UserContent::MetaImage { .. }
        ));
    }
}
