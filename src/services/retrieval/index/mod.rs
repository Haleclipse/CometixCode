//! Local, rebuildable retrieval index. It stores metadata, symbol ranges,
//! identifier occurrences and a Tantivy text index under the config directory;
//! source text is always read from the active worktree during Inspect.
//!
//! Incremental policy (retrieval 4.5): in a git worktree the changed set comes
//! from `git status --porcelain` plus `git diff <indexed HEAD> HEAD`, so an
//! unchanged file is never re-read; outside git, size/mtime decide and only a
//! mismatch re-reads the file. A file whose content hash still matches keeps
//! its previous parse.

pub mod symbols;
pub mod text;

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::utils::permissions::filesystem::read_deny_rg_globs;
use crate::utils::text_decode::{DecodeError, decode_bytes};

use super::inspect::hex_hash;
use super::outcome::{Branch, Candidate, SearchRequest};

use self::text::{ChunkDocument, TextHit, TextIndex};

const INDEX_SCHEMA_VERSION: &str = "nyan-retrieval-index-v2";
pub const CHUNKER_VERSION: &str = "symbol-v4";
const MAX_CHUNK_LINES: usize = 60;
/// Keep embedding inputs below the deployed model's context limit even when a
/// file has unusually dense lines (for example generated JSON fixtures).
const MAX_CHUNK_BYTES: usize = 2_000;
const MAX_REFERENCES_PER_FILE: usize = 2_000;
/// Design cap: at most 3 results per file (retrieval 4.4).
const MAX_PER_FILE: usize = 3;
/// A lock held by a live pid for longer than this is treated as a leaked lock
/// (pid reuse after a crash). A legitimate first build of a very large
/// worktree is minutes, not hours.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexState {
    Ready,
    Building,
    Rebuilding,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SkipCounts {
    pub binary: usize,
    pub too_large: usize,
    pub sensitive: usize,
    pub denied: usize,
    pub unsupported: usize,
}

impl SkipCounts {
    pub fn total(&self) -> usize {
        self.binary + self.too_large + self.sensitive + self.denied + self.unsupported
    }
}

#[derive(Clone)]
pub struct LocalIndex {
    root: PathBuf,
    storage: PathBuf,
    text: Option<TextIndex>,
    files: Vec<IndexedFile>,
    file_by_path: HashMap<String, usize>,
    pub state: IndexState,
    pub skipped: SkipCounts,
    pub generation: String,
    /// HEAD recorded with the persisted index; `None` outside git.
    head: Option<String>,
    changed_files: Vec<String>,
    removed_files: Vec<String>,
    writable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedIndex {
    schema: String,
    generation: String,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    skipped: SkipCounts,
    files: Vec<IndexedFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexedFile {
    pub path: String,
    pub file_hash_raw: String,
    pub size: u64,
    pub mtime_ms: u128,
    pub language: String,
    pub symbol_checked: bool,
    pub symbols: Vec<SymbolRecord>,
    #[serde(default)]
    pub references: Vec<ReferenceRecord>,
    /// Why the symbol table is known to be incomplete (parse error, walk cap).
    #[serde(default)]
    pub symbol_note: Option<String>,
    pub chunks: Vec<ChunkRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SymbolRecord {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub parent: Option<String>,
}

/// One identifier occurrence that is not itself a definition name. `call`
/// marks callee positions; `enclosing` is the innermost function/method that
/// contains it, which is what "who calls X" reports.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReferenceRecord {
    pub name: String,
    pub line: usize,
    pub call: bool,
    pub enclosing: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChunkRecord {
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
}

/// Single-writer lock with crashed-writer takeover. The file holds our pid so
/// a second process can tell a live writer from a stale lock.
pub(crate) struct IndexLock {
    path: PathBuf,
    owned: bool,
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl IndexLock {
    pub fn acquire(storage: &Path) -> Option<Self> {
        let path = storage.join("index.lock");
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut handle) => {
                    use std::io::Write;
                    let _ = write!(handle, "{} {}", std::process::id(), unix_now().as_millis());
                    return Some(Self { path, owned: true });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !stale_lock(&path) {
                        return None;
                    }
                    // The previous owner is gone; removing the file is the
                    // takeover. A concurrent takeover can race here, and the
                    // next create_new decides the winner.
                    let _ = std::fs::remove_file(&path);
                }
                Err(_) => return None,
            }
        }
        None
    }
}

fn stale_lock(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        // Unreadable but present is suspicious; treat a zero-length file as
        // stale, anything else as held.
        return std::fs::metadata(path).map_or(true, |metadata| metadata.len() == 0);
    };
    let mut parts = content.split_whitespace();
    let pid = parts.next().and_then(|value| value.parse::<i32>().ok());
    let started = parts.next().and_then(|value| value.parse::<u128>().ok());
    // A lock older than the reclaim window is leaked even if the pid now
    // belongs to an unrelated live process.
    if let Some(started) = started
        && unix_now().as_millis().saturating_sub(started) > LOCK_STALE_AFTER.as_millis()
    {
        return true;
    }
    match pid {
        Some(pid) => !process_alive(pid),
        None => true,
    }
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) only checks for existence/permission and delivers
    // no signal. EPERM means the process exists under another user.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    true
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

impl LocalIndex {
    /// Open synchronously. Used by `/retrieval status`, sync bookkeeping and
    /// tests; the search path uses [`Self::open_or_defer`] so a first build
    /// never blocks a tool call.
    pub fn open(
        root: impl Into<PathBuf>,
        max_file_bytes: u64,
        permission_context: &crate::tool::ToolPermissionContext,
    ) -> Result<Self, String> {
        let root = root
            .into()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let storage = storage_for(&root);
        std::fs::create_dir_all(&storage).map_err(|error| error.to_string())?;
        let generation = generation_for(&root, max_file_bytes);
        let text_dir = storage.join("text").join(&generation);
        let persisted = load_persisted(&storage);
        let current_head = git_head(&root);
        let stale_generation = persisted
            .as_ref()
            .is_some_and(|index| index.generation != generation);
        let previous = persisted
            .as_ref()
            .filter(|index| index.generation == generation)
            .cloned();
        let previous_files = previous
            .as_ref()
            .map(|index| index.files.as_slice())
            .unwrap_or_default();
        let previous_head = previous.as_ref().and_then(|index| index.head.clone());

        let lock = IndexLock::acquire(&storage);
        let writable = lock.is_some();
        let (files, skipped, changed_files, removed_files) = refresh_files(
            &root,
            max_file_bytes,
            permission_context,
            previous_files,
            previous_head.as_deref(),
            current_head.as_deref(),
        )?;

        let mut text = None;
        if writable {
            let existing = TextIndex::open_existing(&text_dir);
            let needs_rebuild =
                !changed_files.is_empty() || !removed_files.is_empty() || existing.is_none();
            if needs_rebuild {
                let changed_documents = changed_files
                    .iter()
                    .filter_map(|path| {
                        files
                            .iter()
                            .find(|file| &file.path == path)
                            .map(|file| chunk_documents(&root, file))
                    })
                    .collect::<Vec<_>>();
                let index = TextIndex::open(&text_dir)?;
                let fields = index.fields();
                let mut writer = index.writer()?;
                if existing.is_some() {
                    for path in &removed_files {
                        TextIndex::delete_file(&mut writer, &fields, path);
                    }
                }
                for documents in changed_documents {
                    for document in documents {
                        TextIndex::replace_file(&mut writer, &fields, &document);
                    }
                }
                writer.commit().map_err(|error| error.to_string())?;
                index.reload()?;
                text = Some(index);
            } else {
                text = existing;
            }
            remove_other_text_generations(&storage.join("text"), &generation);
        } else if !stale_generation {
            text = TextIndex::open_existing(&text_dir);
        }

        let state = if stale_generation && !writable {
            IndexState::Rebuilding
        } else if text.is_some() {
            IndexState::Ready
        } else {
            IndexState::Building
        };

        let index = Self {
            file_by_path: files
                .iter()
                .enumerate()
                .map(|(position, file)| (file.path.clone(), position))
                .collect(),
            root,
            storage,
            text,
            files,
            state,
            skipped,
            generation,
            head: current_head.clone(),
            changed_files,
            removed_files,
            writable,
        };
        if writable {
            // 4.5: skipped files are named by category, never silently dropped.
            if index.skipped.total() > 0 {
                crate::utils::debug::log_for_debugging(&format!(
                    "retrieval index skipped {} files (binary {}, too large {}, sensitive {}, denied {}, unsupported {})",
                    index.skipped.total(),
                    index.skipped.binary,
                    index.skipped.too_large,
                    index.skipped.sensitive,
                    index.skipped.denied,
                    index.skipped.unsupported
                ));
            }
            // Persist only when the on-disk state would change; a no-op
            // refresh must not rewrite a multi-megabyte index file on every
            // search.
            let dirty = previous.is_none()
                || stale_generation
                || !index.changed_files.is_empty()
                || !index.removed_files.is_empty()
                || previous
                    .as_ref()
                    .and_then(|previous| previous.head.as_deref())
                    != current_head.as_deref();
            if dirty {
                index.persist(current_head.as_deref())?;
            }
        }
        Ok(index)
    }

    /// Cheap change check for the in-process cache (retrieval 4.5):
    /// `Some(true/false)` when git can answer from `status` + `diff`,
    /// `None` when there is no git evidence and the caller must fall back to
    /// the time-based refresh.
    pub fn needs_refresh(&self) -> Option<bool> {
        let head_now = git_head(&self.root);
        if self.head.is_none() && head_now.is_none() {
            return None;
        }
        let changed = git_changed_files(
            &self.root,
            self.head.as_deref(),
            head_now.as_deref(),
            !self.files.is_empty(),
        )?;
        if !changed.is_empty() {
            return Some(true);
        }
        Some(false)
    }

    /// Search-path entry: open incrementally when a current-generation index
    /// exists; otherwise start one background build and report `Building`.
    /// `Err` is a hard failure (worktree unreadable); `Ok(None)` means the
    /// first build is running elsewhere in this process.
    pub fn open_or_defer(
        root: &Path,
        max_file_bytes: u64,
        permission_context: &crate::tool::ToolPermissionContext,
    ) -> Result<Option<Self>, String> {
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let storage = storage_for(&root);
        let generation = generation_for(&root, max_file_bytes);
        let current = load_persisted(&storage).is_some_and(|index| index.generation == generation);
        if current {
            return Self::open(root, max_file_bytes, permission_context).map(Some);
        }
        static BUILDING: std::sync::Mutex<Option<std::collections::BTreeSet<PathBuf>>> =
            std::sync::Mutex::new(None);
        {
            let mut guard = BUILDING
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let building = guard.get_or_insert_with(Default::default);
            if !building.insert(root.clone()) {
                return Ok(None);
            }
        }
        let build_root = root.clone();
        let permission = permission_context.clone();
        let future = async move {
            let result = tokio::task::spawn_blocking(move || {
                let result = LocalIndex::open(&build_root, max_file_bytes, &permission);
                let mut guard = BUILDING
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(building) = guard.as_mut() {
                    building.remove(&build_root);
                }
                result
            })
            .await;
            if let Ok(Err(error)) = result {
                crate::utils::debug::log_for_debugging(&format!(
                    "retrieval index build failed: {error}"
                ));
            }
        };
        if let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
            handle.spawn(future);
        } else {
            tokio::spawn(future);
        }
        Ok(None)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn storage(&self) -> &Path {
        &self.storage
    }

    pub fn files(&self) -> &[IndexedFile] {
        &self.files
    }

    /// Files added or modified by this open, for the semantic pending queue.
    pub fn changed_files(&self) -> &[String] {
        &self.changed_files
    }

    pub fn removed_files(&self) -> &[String] {
        &self.removed_files
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn file(&self, path: &str) -> Option<&IndexedFile> {
        self.file_by_path
            .get(path)
            .and_then(|position| self.files.get(*position))
    }

    /// Definition lookup, file outline, references and callers (retrieval
    /// 4.3/4.5). All matching is token based so snake/camel queries meet their
    /// definition; ordering is exact match, then path bytes, then line.
    pub fn search_symbols(
        &self,
        request: &SearchRequest,
        relation_query: bool,
        reference_query: bool,
        max_results: usize,
    ) -> Vec<Candidate> {
        let query = request.query.as_str();
        let outline_path = outline_target(query, &self.files);
        let mut query_tokens = natural_tokens(query);
        let stripped = query_tokens
            .iter()
            .filter(|token| !RELATION_WORDS.contains(&token.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !stripped.is_empty() {
            query_tokens = stripped;
        }
        let mut scored: Vec<(u8, Candidate)> = Vec::new();

        if let Some(path) = outline_path {
            if let Some(file) = self.file(&path) {
                for symbol in &file.symbols {
                    scored.push((
                        0,
                        Candidate {
                            branch: Branch::Symbol,
                            path: file.path.clone(),
                            start_line: symbol.start_line,
                            end_line: symbol.end_line,
                            symbol: Some(symbol.name.clone()),
                            file_hash_raw: file.file_hash_raw.clone(),
                            no_symbol_check: false,
                            ..Candidate::default()
                        },
                    ));
                }
            }
        } else if !query_tokens.is_empty() {
            let mut targets = BTreeMap::<String, u8>::new();
            for file in &self.files {
                if !matches_request(&file.path, request) {
                    continue;
                }
                for symbol in &file.symbols {
                    let name_tokens = identifier_tokens(&symbol.name);
                    let exact = symbol.name.eq_ignore_ascii_case(query)
                        || name_tokens.join("") == query_tokens.join("");
                    let all_tokens = query_tokens
                        .iter()
                        .all(|token| name_tokens.iter().any(|name| name == token));
                    if !exact && !all_tokens {
                        continue;
                    }
                    *targets.entry(symbol.name.clone()).or_insert(0) += 1;
                    scored.push((
                        if exact { 0 } else { 2 },
                        Candidate {
                            branch: Branch::Symbol,
                            path: file.path.clone(),
                            start_line: symbol.start_line,
                            end_line: symbol.end_line,
                            symbol: Some(symbol.name.clone()),
                            file_hash_raw: file.file_hash_raw.clone(),
                            no_symbol_check: false,
                            ..Candidate::default()
                        },
                    ));
                }
            }

            if relation_query || reference_query {
                let target_names = targets.keys().cloned().collect::<Vec<_>>();
                for file in &self.files {
                    if !matches_request(&file.path, request) {
                        continue;
                    }
                    for reference in &file.references {
                        let matches_target = target_names.contains(&reference.name)
                            || query_tokens.iter().any(|token| {
                                identifier_tokens(&reference.name)
                                    .iter()
                                    .any(|name_token| name_token == token)
                            });
                        if !matches_target {
                            continue;
                        }
                        if relation_query && !reference.call {
                            continue;
                        }
                        scored.push((
                            1,
                            Candidate {
                                branch: Branch::Symbol,
                                path: file.path.clone(),
                                start_line: reference.line,
                                end_line: reference.line,
                                symbol: reference.enclosing.clone(),
                                file_hash_raw: file.file_hash_raw.clone(),
                                no_symbol_check: false,
                                ..Candidate::default()
                            },
                        ));
                    }
                }
            }
        }

        scored.sort_by(|(left_score, left), (right_score, right)| {
            left_score
                .cmp(right_score)
                .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
                .then_with(|| left.start_line.cmp(&right.start_line))
                .then_with(|| left.end_line.cmp(&right.end_line))
        });
        let mut seen = BTreeSet::new();
        let mut results = Vec::new();
        for (_, mut candidate) in scored {
            if !seen.insert(candidate.key()) {
                // Keep the strongest source marker when a definition also
                // shows up as a reference position.
                continue;
            }
            candidate.rank = results.len() + 1;
            results.push(candidate);
            if results.len() >= max_results {
                break;
            }
        }
        results
    }

    /// BM25 text search over the Tantivy index. When the index is not usable
    /// (`building`/`rebuilding`) this returns `None` so the caller can report
    /// the branch as unavailable instead of "zero matches".
    pub fn search_text(
        &self,
        request: &SearchRequest,
        max_results: usize,
    ) -> Option<Vec<Candidate>> {
        let text = self.text.as_ref()?;
        let hits = text.search(&request.query, text::SEARCH_LIMIT).ok()?;
        let mut per_file = HashMap::<String, usize>::new();
        let mut seen = BTreeSet::new();
        let mut results = Vec::new();
        for TextHit {
            path,
            start_line,
            end_line,
            symbol,
            file_hash_raw: hit_hash,
            ..
        } in hits
        {
            if !matches_request(&path, request) {
                continue;
            }
            let count = per_file.entry(path.clone()).or_default();
            if *count >= MAX_PER_FILE {
                continue;
            }
            let Some(file) = self.file(&path) else {
                // A hit from a stale segment that vanished between commit and
                // search; Inspect would drop it anyway.
                continue;
            };
            // Keep the hash the text document was built with. Relabeling it
            // with the current index hash would let a stale hit pass Inspect
            // and show a range that no longer contains the query.
            if !hit_hash.is_empty() && hit_hash != file.file_hash_raw {
                continue;
            }
            if !seen.insert((path.clone(), start_line, end_line)) {
                continue;
            }
            *count += 1;
            results.push(Candidate {
                branch: Branch::Text,
                path,
                start_line,
                end_line,
                symbol,
                file_hash_raw: hit_hash,
                rank: results.len() + 1,
                no_symbol_check: !file.symbol_checked,
                ..Candidate::default()
            });
            if results.len() >= max_results {
                break;
            }
        }
        Some(results)
    }

    /// Chunk text for embedding, read from the current worktree. Files are
    /// re-hashed against the indexed record so a changed file never syncs
    /// stale text. Each text is paired with the chunk record it was cut from:
    /// a chunk that yields no text must not shift the line ranges of the
    /// chunks after it.
    pub fn file_chunks(&self, path: &str) -> Option<(IndexedFile, Vec<(ChunkRecord, String)>)> {
        let file = self.file(path)?.clone();
        let bytes = std::fs::read(self.root.join(path)).ok()?;
        if hex_hash(&bytes) != file.file_hash_raw {
            return None;
        }
        let text = decode_bytes(&bytes, None, path).ok()?.text;
        let lines = text.lines().collect::<Vec<_>>();
        let mut contents = Vec::with_capacity(file.chunks.len());
        for chunk in &file.chunks {
            let start = chunk.start_line.saturating_sub(1);
            let end = chunk.end_line.min(lines.len());
            if start < end {
                let mut content = lines[start..end].join("\n");
                if content.len() > MAX_CHUNK_BYTES {
                    let mut limit = MAX_CHUNK_BYTES;
                    while !content.is_char_boundary(limit) {
                        limit -= 1;
                    }
                    content.truncate(limit);
                }
                // A zero-content file (e.g. a 1-byte `__init__.py`) still has
                // a chunk record, but the embedding service rejects the whole
                // batch with 400 "The decoder prompt cannot be empty".
                // Dropping it here keeps points, texts and refs aligned.
                if content.trim().is_empty() {
                    continue;
                }
                contents.push((chunk.clone(), content));
            }
        }
        Some((file, contents))
    }

    fn persist(&self, head: Option<&str>) -> Result<(), String> {
        let value = PersistedIndex {
            schema: INDEX_SCHEMA_VERSION.to_string(),
            generation: self.generation.clone(),
            head: head.map(str::to_string),
            skipped: self.skipped.clone(),
            files: self.files.clone(),
        };
        let temporary = self.storage.join("index.json.tmp");
        let final_path = self.storage.join("index.json");
        std::fs::write(
            &temporary,
            serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        std::fs::rename(temporary, final_path).map_err(|error| error.to_string())
    }
}

fn chunk_documents(root: &Path, file: &IndexedFile) -> Vec<ChunkDocument> {
    let Ok(bytes) = std::fs::read(root.join(&file.path)) else {
        return Vec::new();
    };
    let Ok(decoded) = decode_bytes(&bytes, None, &file.path) else {
        return Vec::new();
    };
    let text = decoded.text;
    let lines = text.lines().collect::<Vec<_>>();
    file.chunks
        .iter()
        .filter_map(|chunk| {
            let start = chunk.start_line.saturating_sub(1);
            let end = chunk.end_line.min(lines.len());
            (start < end).then(|| ChunkDocument {
                path: file.path.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                symbol: chunk.symbol.clone(),
                file_hash_raw: file.file_hash_raw.clone(),
                body: lines[start..end].join("\n"),
            })
        })
        .collect()
}

/// Index storage lives under the config directory, never the worktree
/// (retrieval 4.5 / legacy Decision #1).
pub fn storage_for(root: &Path) -> PathBuf {
    crate::utils::session_storage::get_project_dir(&root.to_string_lossy()).join("retrieval")
}

/// Decode for Inspect with the same rules the index used (tool-io T5).
pub(crate) fn decode_for_snippet(bytes: &[u8], path: &str) -> Result<String, String> {
    decode_bytes(bytes, None, path)
        .map(|decoded| decoded.text)
        .map_err(|error| error.message(path))
}

fn load_persisted(storage: &Path) -> Option<PersistedIndex> {
    let bytes = std::fs::read(storage.join("index.json")).ok()?;
    let index: PersistedIndex = serde_json::from_slice(&bytes).ok()?;
    (index.schema == INDEX_SCHEMA_VERSION).then_some(index)
}

fn remove_other_text_generations(text_root: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(text_root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() != keep {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Refresh the file table. Returns the files, skip counts, changed paths and
/// removed paths. `previous` entries are reused unless git (or, without git,
/// size/mtime) says the file changed.
fn refresh_files(
    root: &Path,
    max_file_bytes: u64,
    permission_context: &crate::tool::ToolPermissionContext,
    previous: &[IndexedFile],
    previous_head: Option<&str>,
    current_head: Option<&str>,
) -> Result<(Vec<IndexedFile>, SkipCounts, Vec<String>, Vec<String>), String> {
    let deny_patterns = read_deny_rg_globs(permission_context, root, root);
    let previous_by_path = previous
        .iter()
        .map(|file| (file.path.clone(), file))
        .collect::<HashMap<_, _>>();
    let changed_by_git = git_changed_files(root, previous_head, current_head, !previous.is_empty());
    let mut files = Vec::new();
    let mut skipped = SkipCounts::default();
    let mut changed_files = Vec::new();
    let mut seen = HashSet::new();

    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false);
    let mut entries = walker
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.path().cmp(right.path()));

    for entry in entries {
        let path = entry.path();
        let Some(relative) = path.strip_prefix(root).ok() else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative.starts_with(".git/") {
            continue;
        }
        if sensitive_path(&relative) {
            skipped.sensitive += 1;
            continue;
        }
        if denied_path(&relative, path, &deny_patterns) {
            skipped.denied += 1;
            continue;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if metadata.len() > max_file_bytes {
            skipped.too_large += 1;
            continue;
        }
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_millis());
        seen.insert(relative.clone());

        let previous_entry = previous_by_path.get(&relative).copied();
        let must_check = match (previous_entry, changed_by_git.as_ref()) {
            (None, _) => true,
            (Some(existing), Some(changed)) => {
                changed.contains(&relative)
                    || existing.size != metadata.len()
                    || existing.mtime_ms != mtime_ms
            }
            (Some(existing), None) => {
                existing.size != metadata.len() || existing.mtime_ms != mtime_ms
            }
        };
        if !must_check && let Some(existing) = previous_entry {
            files.push(existing.clone());
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let hash = hex_hash(&bytes);
        if let Some(existing) = previous_entry
            && existing.file_hash_raw == hash
            && existing.size == metadata.len()
        {
            files.push(existing.clone());
            continue;
        }
        let decoded = match decode_bytes(&bytes, None, &relative) {
            Ok(decoded) => decoded,
            Err(DecodeError::Binary { .. }) => {
                skipped.binary += 1;
                continue;
            }
            Err(_) => {
                skipped.unsupported += 1;
                continue;
            }
        };
        let text = decoded.text;
        let language = language_for(&relative);
        let (symbol_checked, symbols, references, symbol_note) = if supports_symbols(&language) {
            match symbols::parse(&text, &language) {
                Ok(mut parsed) => {
                    let mut note = parsed.partial.take();
                    if parsed.references.len() > MAX_REFERENCES_PER_FILE {
                        parsed.references.truncate(MAX_REFERENCES_PER_FILE);
                        note.get_or_insert_with(|| {
                            format!("references truncated at {MAX_REFERENCES_PER_FILE}")
                        });
                    }
                    (true, parsed.symbols, parsed.references, note)
                }
                // A grammar failure is named, never an empty-but-checked table.
                Err(error) => (false, Vec::new(), Vec::new(), Some(error)),
            }
        } else {
            (false, Vec::new(), Vec::new(), None)
        };
        let chunks = make_chunks(&text, &symbols, symbol_checked);
        if previous_entry.is_some() {
            changed_files.push(relative.clone());
        }
        files.push(IndexedFile {
            path: relative,
            file_hash_raw: hash,
            size: metadata.len(),
            mtime_ms,
            language,
            symbol_checked,
            symbols,
            references,
            symbol_note,
            chunks,
        });
    }
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));

    // First build: everything counts as new for the semantic queue.
    if previous.is_empty() {
        changed_files = files.iter().map(|file| file.path.clone()).collect();
    }
    let removed_files = previous
        .iter()
        .filter(|file| !seen.contains(&file.path))
        .map(|file| file.path.clone())
        .collect();
    Ok((files, skipped, changed_files, removed_files))
}

/// `Some(set)` = "only these tracked files changed" (tracked and untracked
/// alike). `None` = no git evidence, fall back to size/mtime.
fn git_changed_files(
    root: &Path,
    previous_head: Option<&str>,
    current_head: Option<&str>,
    have_previous: bool,
) -> Option<HashSet<String>> {
    if !have_previous {
        return Some(HashSet::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut changed = HashSet::new();
    let mut fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        // "XY path" for normal entries; renames emit the old path as an
        // extra NUL-separated field (the new path is in the status record).
        if field.len() < 4 {
            continue;
        }
        let status = &field[..2];
        let path = String::from_utf8_lossy(&field[3..]).into_owned();
        changed.insert(path);
        if status.contains(&b'R') {
            fields.next();
        }
    }
    if let (Some(previous), Some(current)) = (previous_head, current_head)
        && previous != current
    {
        let diff = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--name-only", "-z", previous, current, "--"])
            .output()
            .ok()?;
        if diff.status.success() {
            for path in diff.stdout.split(|byte| *byte == 0) {
                if !path.is_empty() {
                    changed.insert(String::from_utf8_lossy(path).into_owned());
                }
            }
        }
    }
    Some(changed)
}

fn git_head(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|head| !head.is_empty())
}

fn denied_path(relative: &str, absolute: &Path, patterns: &[String]) -> bool {
    let absolute = absolute.to_string_lossy();
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim_start_matches('!');
        let matches_glob = globset::Glob::new(pattern)
            .ok()
            .and_then(|glob| glob.compile_matcher().is_match(relative).then_some(()))
            .is_some()
            || globset::Glob::new(pattern)
                .ok()
                .and_then(|glob| {
                    glob.compile_matcher()
                        .is_match(absolute.as_ref())
                        .then_some(())
                })
                .is_some();
        matches_glob
            || relative == pattern.trim_start_matches("**/")
            || relative.ends_with(&format!("/{}", pattern.trim_start_matches("**/")))
    })
}

fn sensitive_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let basename = lower.rsplit('/').next().unwrap_or(&lower);
    basename == ".env"
        || basename.starts_with(".env.")
        || basename.ends_with(".pem")
        || basename.ends_with(".key")
        || basename.ends_with(".p12")
        || basename.ends_with(".pfx")
        || basename.starts_with("id_rsa")
        || basename.starts_with("id_ed25519")
        || basename == ".netrc"
        || basename == "credentials"
        || basename == "credentials.json"
        || lower.contains("private_key")
        || lower.contains("private-key")
        || lower.contains("/secrets/")
}

pub(crate) fn language_for(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".rs") {
        "rust"
    } else if lower.ends_with(".ts") || lower.ends_with(".tsx") {
        "typescript"
    } else if lower.ends_with(".js") || lower.ends_with(".jsx") || lower.ends_with(".mjs") {
        "javascript"
    } else if lower.ends_with(".py") {
        "python"
    } else if lower.ends_with(".go") {
        "go"
    } else if lower.ends_with(".java") {
        "java"
    } else if lower.ends_with(".c")
        || lower.ends_with(".h")
        || lower.ends_with(".cc")
        || lower.ends_with(".cpp")
        || lower.ends_with(".cxx")
        || lower.ends_with(".hpp")
        || lower.ends_with(".hh")
    {
        "cpp"
    } else if lower.ends_with(".sh") || lower.ends_with(".bash") {
        "bash"
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") {
        "markdown"
    } else {
        "unknown"
    }
    .to_string()
}

/// @nyan: tree-sitter grammar for a first-batch language (retrieval 4.5).
/// `None` means the language has no symbol table and is reported as
/// `(no symbol check)` rather than silently treated as parsed.
pub(crate) fn grammar_for(language: &str) -> Option<tree_sitter::Language> {
    Some(match language {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "javascript" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "markdown" => tree_sitter_md::LANGUAGE.into(),
        _ => return None,
    })
}

fn supports_symbols(language: &str) -> bool {
    grammar_for(language).is_some()
}

fn make_chunks(text: &str, symbols: &[SymbolRecord], symbol_checked: bool) -> Vec<ChunkRecord> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    if symbol_checked {
        // Symbol ranges nest (`impl Foo` contains its methods). A container
        // chunk would duplicate every child's text in the text index and in
        // each embedding batch, so only leaf definitions become chunks; a
        // type with no methods still chunks as itself.
        let leaves = symbols
            .iter()
            .filter(|symbol| {
                !symbols.iter().any(|other| {
                    other.start_line >= symbol.start_line
                        && other.end_line <= symbol.end_line
                        && (other.start_line, other.end_line, &other.name)
                            != (symbol.start_line, symbol.end_line, &symbol.name)
                })
            })
            .collect::<Vec<_>>();
        for symbol in leaves {
            ranges.push((
                symbol.start_line,
                symbol.end_line,
                Some(symbol.name.clone()),
            ));
        }
    }
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    ranges.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
    if ranges.is_empty() {
        let mut start = 1;
        while start <= lines.len() {
            let end = (start + MAX_CHUNK_LINES - 1).min(lines.len());
            ranges.push((start, end, None));
            start = end + 1;
        }
    }
    let mut chunks = Vec::new();
    for (start, end, symbol) in ranges {
        if end < start {
            continue;
        }
        let mut cursor = start;
        while cursor <= end {
            let mut sub_end = (cursor + MAX_CHUNK_LINES - 1).min(end);
            while sub_end > cursor && lines[cursor - 1..sub_end].join("\n").len() > MAX_CHUNK_BYTES
            {
                sub_end -= 1;
            }
            chunks.push(ChunkRecord {
                start_line: cursor,
                end_line: sub_end,
                symbol: symbol.clone(),
            });
            cursor = sub_end + 1;
        }
    }
    chunks
}

pub fn natural_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for raw in text.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
    {
        if raw.is_empty() {
            continue;
        }
        let split = split_identifier(raw);
        if split.is_empty() {
            tokens.push(raw.to_ascii_lowercase());
        } else {
            tokens.extend(split);
        }
    }
    tokens
}

pub fn identifier_tokens(text: &str) -> Vec<String> {
    split_identifier(text)
}

fn split_identifier(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character == '_' || character == '-' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else if character.is_ascii_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
            current.push(character.to_ascii_lowercase());
        } else if character.is_ascii_alphanumeric() {
            current.push(character.to_ascii_lowercase());
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Shared worktree-relative path + glob filter for every local branch.
pub fn matches_request(path: &str, request: &SearchRequest) -> bool {
    if let Some(scope) = request.path.as_deref().filter(|value| !value.is_empty()) {
        let scope = scope.trim_matches('/');
        if !(path == scope || path.starts_with(&format!("{scope}/"))) {
            return false;
        }
    }
    if let Some(glob) = request.glob.as_deref().filter(|value| !value.is_empty()) {
        match globset::Glob::new(glob) {
            Ok(compiled) => {
                if !compiled.compile_matcher().is_match(path) {
                    return false;
                }
            }
            // An invalid glob is the caller's error; the tool validates it
            // before searching. Being permissive here never hides files.
            Err(_) => {}
        }
    }
    true
}

/// English relation phrasing. Stripped before symbol-name matching so
/// "who calls parse_config" matches `parse_config`, but only when something
/// remains: a bare `call` or `File` query must still find that symbol.
const RELATION_WORDS: &[&str] = &[
    "who",
    "what",
    "which",
    "where",
    "calls",
    "caller",
    "callers",
    "called",
    "reference",
    "references",
    "referenced",
    "usage",
    "usages",
    "used",
    "uses",
    "using",
    "impact",
    "impacts",
    "depends",
    "dependency",
    "dependencies",
    "dependents",
    "definition",
    "definitions",
    "defined",
    "outline",
];

/// `outline src/foo.rs` / `大纲 services/retrieval` returns the symbol table
/// of matching files instead of a name lookup.
fn outline_target(query: &str, files: &[IndexedFile]) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if !(lower.contains("outline") || query.contains("大纲")) {
        return None;
    }
    files
        .iter()
        .find(|file| lower.contains(&file.path.to_ascii_lowercase()))
        .map(|file| file.path.clone())
}

fn generation_for(root: &Path, max_file_bytes: u64) -> String {
    let canonical = serde_json::json!({
        "schema": INDEX_SCHEMA_VERSION,
        "chunkerVersion": CHUNKER_VERSION,
        "textSchema": text::SCHEMA_VERSION,
        "analyzer": text::ANALYZER_NAME,
        "maxFileBytes": max_file_bytes,
        "worktree": root.to_string_lossy(),
    });
    Sha256::digest(canonical.to_string().as_bytes())
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn temp_project(name: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("nyan-{name}-{}", uuid::Uuid::new_v4().simple()));
        let config = root.join("config");
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        (config, project)
    }

    fn open(project: &Path) -> LocalIndex {
        LocalIndex::open(
            project,
            1_048_576,
            &crate::tool::ToolPermissionContext::default(),
        )
        .unwrap()
    }

    #[test]
    fn tokenization_handles_snake_camel_and_natural_language() {
        assert_eq!(
            identifier_tokens("autoCompactIfNeeded"),
            vec!["auto", "compact", "if", "needed"]
        );
        assert!(natural_tokens("auto compact if needed").contains(&"compact".to_string()));
    }

    #[test]
    fn sensitive_paths_are_skipped() {
        assert!(sensitive_path(".env"));
        assert!(sensitive_path("keys/id_rsa"));
        assert!(sensitive_path("config/credentials.json"));
        assert!(!sensitive_path("src/main.rs"));
    }

    #[test]
    fn dense_line_chunks_stay_within_embedding_input_limit() {
        let text = (0..60)
            .map(|_| "x".repeat(200))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = text.lines().collect::<Vec<_>>();
        let chunks = make_chunks(&text, &[], false);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| {
            lines[chunk.start_line - 1..chunk.end_line.min(lines.len())]
                .join("\n")
                .len()
                <= MAX_CHUNK_BYTES
        }));
    }

    #[test]
    fn blank_chunks_are_not_handed_to_the_embedding_batch() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_project("blank-chunks");
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        // A 1-byte Python package marker, exactly like the real pending files.
        std::fs::write(project.join("empty.py"), "\n").unwrap();
        std::fs::write(project.join("lib.rs"), "fn hello() {}\n").unwrap();
        let index = open(&project);
        let empty = index.file("empty.py").expect("indexed");
        assert_eq!(empty.chunks.len(), 1, "the chunker still records it");
        let (_, texts) = index.file_chunks("empty.py").expect("readable");
        assert!(
            texts.is_empty(),
            "blank chunk must not be embedded: {texts:?}"
        );
        let (_, texts) = index.file_chunks("lib.rs").expect("readable");
        assert_eq!(texts.len(), 1);
        assert!(!texts[0].1.trim().is_empty());
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn index_lives_under_config_projects_not_worktree() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_project("index");
        std::fs::write(project.join("lib.rs"), "fn hello() {}\n").unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        let index = open(&project);
        assert!(index.storage().starts_with(&config));
        assert!(!index.storage().starts_with(&project));
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn incremental_refresh_reuses_unchanged_files_and_updates_changed_ones() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_project("incremental");
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        std::fs::write(project.join("a.rs"), "fn alpha() {}\n").unwrap();
        let first = open(&project);
        assert_eq!(first.files().len(), 1);
        assert_eq!(first.changed_files(), &["a.rs".to_string()]);
        assert!(first.state == IndexState::Ready);

        // Rewrite with the same size but new mtime: the mtime prefilter notices
        // and the hash decides.
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(project.join("a.rs"), "fn beta_() {}\n").unwrap();
        std::fs::write(project.join("b.rs"), "fn gamma() {}\n").unwrap();
        let second = open(&project);
        assert_eq!(second.files().len(), 2);
        assert!(second.changed_files().contains(&"a.rs".to_string()));
        let alpha = second.search_symbols(&SearchRequest::new("alpha"), false, false, 10);
        assert!(alpha.is_empty());
        assert!(
            !second
                .search_symbols(&SearchRequest::new("beta"), false, false, 10)
                .is_empty()
        );
        assert!(
            !second
                .search_symbols(&SearchRequest::new("gamma"), false, false, 10)
                .is_empty()
        );

        std::fs::remove_file(project.join("b.rs")).unwrap();
        let third = open(&project);
        assert_eq!(third.removed_files(), &["b.rs".to_string()]);
        assert!(
            third
                .search_text(&SearchRequest::new("gamma"), 10)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn second_process_is_read_only_and_serves_the_persisted_generation() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_project("lock");
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        std::fs::write(project.join("a.rs"), "fn alpha() {}\n").unwrap();
        let writer = open(&project);
        assert!(writer.is_writable());
        let storage = writer.storage().to_path_buf();
        drop(writer);
        // A live foreign writer (this process's own pid stands in for it) owns
        // the lock; the second opener is read-only and still serves the
        // persisted index.
        std::fs::write(
            storage.join("index.lock"),
            format!("{} {}", std::process::id(), unix_now().as_millis()),
        )
        .unwrap();
        let reader = open(&project);
        assert!(!reader.is_writable());
        assert_eq!(reader.state, IndexState::Ready);
        assert!(
            !reader
                .search_symbols(&SearchRequest::new("alpha"), false, false, 10)
                .is_empty()
        );
        std::fs::remove_file(storage.join("index.lock")).unwrap();
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn text_and_symbol_branches_respect_path_and_glob_filters() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_project("filters");
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(project.join("docs")).unwrap();
        std::fs::write(project.join("src/alpha.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(project.join("docs/alpha.md"), "# alpha\n").unwrap();
        let index = open(&project);
        let mut request = SearchRequest::new("alpha");
        request.path = Some("docs".to_string());
        let text = index.search_text(&request, 10).unwrap();
        assert!(
            text.iter()
                .all(|candidate| candidate.path.starts_with("docs/"))
        );
        let symbols = index.search_symbols(&request, false, false, 10);
        assert!(
            symbols
                .iter()
                .all(|candidate| candidate.path.starts_with("docs/"))
        );
        request.path = None;
        request.glob = Some("**/*.rs".to_string());
        let symbols = index.search_symbols(&request, false, false, 10);
        assert!(
            symbols
                .iter()
                .all(|candidate| candidate.path.ends_with(".rs"))
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn callers_and_references_come_from_the_symbol_table() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_project("callers");
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        std::fs::write(
            project.join("a.rs"),
            "fn target() {}\nfn caller() {\n    target();\n}\n",
        )
        .unwrap();
        let index = open(&project);
        let callers =
            index.search_symbols(&SearchRequest::new("who calls target"), true, false, 10);
        assert!(callers.iter().any(|candidate| {
            candidate.symbol.as_deref() == Some("caller") && candidate.start_line == 3
        }));
        let references =
            index.search_symbols(&SearchRequest::new("references target"), false, true, 10);
        assert!(references.iter().any(|candidate| candidate.start_line == 3));
        let outline = index.search_symbols(&SearchRequest::new("outline a.rs"), false, false, 10);
        assert_eq!(outline.len(), 2);
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn stale_lock_from_a_dead_process_is_taken_over() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_project("stale-lock");
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        std::fs::write(project.join("a.rs"), "fn alpha() {}\n").unwrap();
        let first = open(&project);
        let lock_path = first.storage().join("index.lock");
        drop(first);
        // Simulate a crashed writer: the lock file remains, pid is gone, and
        // the start time is older than the stale threshold.
        std::fs::write(&lock_path, "999999 1").unwrap();
        let index = open(&project);
        assert!(index.is_writable());
        // Our own lock is released when the open finishes.
        assert!(!lock_path.exists());
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }
}
