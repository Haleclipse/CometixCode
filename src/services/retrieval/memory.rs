//! @nyan: memory M6 seam — RelevantMemories production (docs/nyan/memory.md
//! M6, docs/nyan/retrieval.md §5).
//!
//! MEMORY.md is injected in full while it fits (< 200 lines), which is enough
//! for small memory sets. This module only takes over when the index outgrows
//! that budget: it keeps a small Tantivy index of the memory directory keyed by
//! git root (the same root memdir uses), so retrieval is by question instead of
//! keyword scoring, and the caller injects the hits through the existing
//! `Attachment::RelevantMemories` type.
//!
//! Read-time safety: every candidate is re-read and scanned with the shared
//! secret rules before it can be injected; a hit is isolated (logged, not
//! modified) exactly as memory.md M3 requires.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::types::message::{Message, RelevantMemory};
use crate::utils::settings::nyan::MemorySettings;

use super::index::text::{ChunkDocument, TextIndex};

const MEMORY_INDEX_GENERATION: &str = "nyan-memory-index-v1";
const MEMORY_CHUNK_CHARS: usize = 4_000;
const MEMORY_MAX_RESULTS: usize = 8;
/// MEMORY.md's documented injection budget (memory.md M6 / retrieval.md §5).
const MEMORY_INDEX_BUDGET_LINES: usize = 200;

#[derive(Default)]
struct MemoryIndexMeta {
    generation: String,
    files: BTreeMap<String, (u128, u64)>,
}

/// True when the memory index has outgrown the MEMORY.md injection budget.
pub fn memory_index_over_budget(root: &Path) -> bool {
    let index_path = root.join("MEMORY.md");
    let Ok(content) = std::fs::read_to_string(&index_path) else {
        return false;
    };
    content.lines().count() >= MEMORY_INDEX_BUDGET_LINES
}

/// Search memdir for memories relevant to `query`. Returns an empty vector
/// when there is no index to search, the index is over budget but empty, or
/// every hit is isolated by the read-time guard.
pub fn search_memory_directory(
    query: &str,
    max_results: usize,
    memory_settings: &MemorySettings,
) -> Vec<RelevantMemory> {
    let root = crate::memdir::paths::get_auto_mem_path();
    if query.trim().is_empty() || !root.is_dir() || !memory_index_over_budget(&root) {
        return Vec::new();
    }
    let git_root = crate::bootstrap::state::get_original_cwd();
    let git_root = crate::utils::git::find_canonical_git_root(&git_root).unwrap_or(git_root);
    let storage = crate::utils::session_storage::get_project_dir(&git_root.to_string_lossy())
        .join("memory-index")
        .join(MEMORY_INDEX_GENERATION);
    let Ok(index) = MemoryTextIndex::open(&storage) else {
        return Vec::new();
    };
    if let Err(error) = index.refresh(&root) {
        // A failed refresh leaves the previous generation in place; log and
        // search it rather than pretending there are no memories.
        crate::utils::debug::log_for_debugging(&format!("memory index refresh failed: {error}"));
    }
    let hits = match index
        .text
        .search(query, max_results.clamp(1, MEMORY_MAX_RESULTS))
    {
        Ok(hits) => hits,
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!("memory index search failed: {error}"));
            return Vec::new();
        }
    };
    let mut memories = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for hit in hits {
        if !seen.insert(hit.path.clone()) {
            continue;
        }
        let path = root.join(&hit.path);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(rule) = blocked_rule(&path, &content, memory_settings) {
            tracing::warn!(
                target: "nyan::memory",
                memory_guard_blocked = true,
                path = %path.display(),
                rules = %rule,
                "memory_guard_blocked"
            );
            continue;
        }
        memories.push(RelevantMemory {
            path: path.to_string_lossy().to_string(),
            content,
            mtime_ms: std::fs::metadata(&path)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64),
            header: None,
            limit: None,
        });
    }
    memories
}

/// Read-time secret guard, shared verbatim with the memory module's
/// `check_memory_content` (memory.md M3). A hit returns rule identifiers only;
/// the caller logs and skips the file without modifying it.
fn blocked_rule(path: &Path, content: &str, memory_settings: &MemorySettings) -> Option<String> {
    if !memory_settings.secret_scan() || !crate::memdir::paths::is_auto_mem_path(path) {
        return None;
    }
    crate::memdir::write_guard::check_memory_content(path, content)
        .err()
        .map(|error| error.rule_ids.join(", "))
}

/// The last user text in the history, if the newest message is a user turn.
/// Post-tool attachment passes end with a tool result, so this only fires at
/// the start of a turn — memories are selected once per user input, not once
/// per request.
pub fn last_user_prompt(messages: &[Message]) -> Option<String> {
    match messages.last()? {
        Message::User(user) => crate::utils::messages::get_user_message_text(user),
        _ => None,
    }
}

struct MemoryTextIndex {
    text: TextIndex,
    directory: PathBuf,
}

impl MemoryTextIndex {
    fn open(directory: &Path) -> Result<Self, String> {
        let text = TextIndex::open(directory)?;
        Ok(Self {
            text,
            directory: directory.to_path_buf(),
        })
    }

    /// Bring the index up to date with memdir: changed files are re-chunked,
    /// deleted files are removed, unchanged files are left alone.
    fn refresh(&self, root: &Path) -> Result<(), String> {
        let meta_path = self.directory.join("memory-index.json");
        let mut meta = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| {
                let generation = value.get("generation")?.as_str()?.to_string();
                let files = value
                    .get("files")?
                    .as_object()?
                    .iter()
                    .filter_map(|(path, stat)| {
                        let stat = stat.as_array()?;
                        Some((
                            path.clone(),
                            (stat.first()?.as_u64()? as u128, stat.get(1)?.as_u64()?),
                        ))
                    })
                    .collect::<BTreeMap<_, _>>();
                Some(MemoryIndexMeta { generation, files })
            })
            .filter(|meta| meta.generation == MEMORY_INDEX_GENERATION)
            .unwrap_or_default();
        let mut seen = std::collections::BTreeSet::new();
        let mut changed = false;
        let fields = self.text.fields();
        let mut writer = self.text.writer()?;
        for path in walk_memory_files(root) {
            let Some(relative) = path.strip_prefix(root).ok() else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if relative == "MEMORY.md" {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let mtime_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_millis());
            seen.insert(relative.clone());
            if meta.files.get(&relative) == Some(&(mtime_ms, metadata.len())) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            TextIndex::delete_file(&mut writer, &fields, &relative);
            for (chunk_index, chunk) in chunk_memory(&content).into_iter().enumerate() {
                TextIndex::replace_file(
                    &mut writer,
                    &fields,
                    &ChunkDocument {
                        path: relative.clone(),
                        start_line: chunk_index * MEMORY_CHUNK_CHARS + 1,
                        end_line: (chunk_index + 1) * MEMORY_CHUNK_CHARS,
                        symbol: None,
                        file_hash_raw: format!("{mtime_ms}:{}", metadata.len()),
                        body: chunk,
                    },
                );
            }
            meta.files.insert(relative, (mtime_ms, metadata.len()));
            changed = true;
        }
        for removed in meta.files.keys().cloned().collect::<Vec<_>>() {
            if !seen.contains(&removed) {
                TextIndex::delete_file(&mut writer, &fields, &removed);
                meta.files.remove(&removed);
                changed = true;
            }
        }
        if changed {
            writer.commit().map_err(|error| error.to_string())?;
            self.text.reload()?;
        }
        meta.generation = MEMORY_INDEX_GENERATION.to_string();
        let value = serde_json::json!({
            "generation": meta.generation,
            "files": meta.files.iter().map(|(path, (mtime, size))| {
                (path.clone(), serde_json::json!([*mtime as u64, size]))
            }).collect::<serde_json::Map<_, _>>(),
        });
        let temporary = meta_path.with_extension("json.tmp");
        std::fs::write(
            &temporary,
            serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        std::fs::rename(temporary, meta_path).map_err(|error| error.to_string())
    }
}

/// Memory files are whole documents; very long ones are split at line
/// boundaries so a single hit never injects an unbounded body.
fn chunk_memory(content: &str) -> Vec<String> {
    if content.chars().count() <= MEMORY_CHUNK_CHARS {
        return vec![content.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in content.lines() {
        if current.chars().count() + line.chars().count() > MEMORY_CHUNK_CHARS
            && !current.is_empty()
        {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn walk_memory_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
    {
        if entry.path().is_file() {
            files.push(entry.into_path());
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn empty_queries_do_not_read_memory() {
        assert!(search_memory_directory("", 10, &MemorySettings::default()).is_empty());
    }

    #[test]
    fn retrieval_only_turns_on_above_the_index_budget() {
        let root = std::env::temp_dir().join(format!("nyan-mem-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("MEMORY.md"), "- short index\n").unwrap();
        assert!(!memory_index_over_budget(&root));
        let long = (0..200)
            .map(|line| format!("- line {line}\n"))
            .collect::<String>();
        std::fs::write(root.join("MEMORY.md"), long).unwrap();
        assert!(memory_index_over_budget(&root));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn memory_search_returns_matching_files_with_guard_applied() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("nyan-mem-{}", uuid::Uuid::new_v4().simple()));
        let memory = root.join("memory");
        let config = root.join("config");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(
            memory.join("MEMORY.md"),
            (0..200)
                .map(|line| format!("- line {line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        std::fs::write(
            memory.join("cache_ttl.md"),
            "The prompt cache lives for five minutes and compaction keeps the prefix stable.\n",
        )
        .unwrap();
        std::fs::write(memory.join("unrelated.md"), "bananas and oranges\n").unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config);
        let _override = EnvVarGuard::set(
            "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
            memory.to_str().unwrap(),
        );
        let _cwd = EnvVarGuard::set("PWD", root.to_str().unwrap());
        let hits =
            search_memory_directory("prompt cache compaction", 5, &MemorySettings::default());
        assert!(
            !hits.is_empty(),
            "expected a memory hit; memory path {}",
            memory.display()
        );
        assert!(hits.iter().all(|hit| hit.path.ends_with("cache_ttl.md")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn last_user_prompt_only_fires_on_a_user_turn() {
        let user = crate::utils::messages::create_user_message("hello".to_string());
        assert_eq!(
            last_user_prompt(&[Message::User(user.clone())]).as_deref(),
            Some("hello")
        );
        assert!(last_user_prompt(&[]).is_none());
    }
}
