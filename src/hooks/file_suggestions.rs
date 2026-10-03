//! Maps to: CC `hooks/fileSuggestions.ts`.
//!
//! The source keeps a cwd-scoped file index and exposes it to `useTypeahead`
//! for `@` mentions.  Rust keeps the same owner and result shape, while the
//! index is built asynchronously from the current git file list (with a
//! filesystem fallback) so typing never blocks the iocraft render loop.

use crate::components::prompt_input::prompt_input_footer_suggestions::SuggestionItem;
use crate::utils::settings::get_initial_settings;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use unicode_normalization::UnicodeNormalization;

const MAX_SUGGESTIONS: usize = 15;
const CACHE_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default)]
struct FileCache {
    cwd: PathBuf,
    paths: Vec<String>,
    loaded_at: Option<Instant>,
}

static FILE_CACHE: LazyLock<Mutex<FileCache>> = LazyLock::new(|| Mutex::new(FileCache::default()));
static CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);
static REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAST_REFRESH_MS: AtomicU64 = AtomicU64::new(0);
const REFRESH_THROTTLE_MS: u64 = 5_000;

/// Maps to CC `ignorePatternsCache` / `ignorePatternsCacheKey`
/// (`fileSuggestions.ts:62-63`) — one slot keyed by `repoRoot:cwd`.
type IgnorePatternsCache = Mutex<Option<(String, Option<Arc<Gitignore>>)>>;
static IGNORE_PATTERNS_CACHE: LazyLock<IgnorePatternsCache> = LazyLock::new(|| Mutex::new(None));

/// Maps to CC `loadRipgrepIgnorePatterns` (`fileSuggestions.ts:202-238`).
///
/// `git ls-files --exclude-standard` honours `.gitignore` and the repository's
/// exclude files, but knows nothing about ripgrep's `.ignore`/`.rgignore`. The
/// source loads those separately and applies them to the git rows, which is
/// also why they survive `respectGitignore: false` — `config.ts:459` states
/// plainly that "`.ignore` files are always respected".
///
/// Returns `None` when no pattern file exists, matching the source's
/// `hasPatterns ? ig : null`, so the caller can skip the filter entirely.
fn load_ripgrep_ignore_patterns(repo_root: &Path, cwd: &Path) -> Option<Arc<Gitignore>> {
    let cache_key = format!("{}:{}", repo_root.display(), cwd.display());
    if let Ok(cache) = IGNORE_PATTERNS_CACHE.lock() {
        if let Some((key, patterns)) = cache.as_ref() {
            if key == &cache_key {
                return patterns.clone();
            }
        }
    }

    // CC `[...new Set([repoRoot, cwd])]` — one pass when the cwd is the root.
    let mut directories = vec![repo_root];
    if cwd != repo_root {
        directories.push(cwd);
    }
    // Patterns are written relative to the file that declares them, but the
    // rows being filtered are cwd-relative (and may carry `../`), so the
    // matcher is anchored at the cwd.
    let mut builder = GitignoreBuilder::new(cwd);
    let mut has_patterns = false;
    for directory in directories {
        for file_name in [".ignore", ".rgignore"] {
            let path = directory.join(file_name);
            if builder.add(&path).is_none() {
                has_patterns = true;
                crate::utils::debug::log_for_debugging(&format!(
                    "[FileIndex] loaded ignore patterns from {}",
                    path.display()
                ));
            }
        }
    }

    let result = has_patterns
        .then(|| builder.build().ok().map(Arc::new))
        .flatten();
    if let Ok(mut cache) = IGNORE_PATTERNS_CACHE.lock() {
        *cache = Some((cache_key, result.clone()));
    }
    result
}

/// Maps to CC `ignorePatterns.filter(paths)` — keep the rows the patterns do
/// not claim.
///
/// The npm `ignore` package walks each path segment, so a `vendor/` pattern
/// claims `vendor/bundle.js` and not just the directory entry itself. The Rust
/// equivalent of that walk is `matched_path_or_any_parents`; plain `matched`
/// only tests the final component and would let every file under a claimed
/// directory through. That method asserts its argument is rooted under the
/// matcher, so absolute rows (the Claude config files) are passed through
/// untouched — the source never feeds them to the filter either.
fn apply_ignore_patterns(patterns: &Gitignore, paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| {
            let candidate = Path::new(path.trim_end_matches('/'));
            if candidate.is_absolute() {
                return true;
            }
            !patterns
                .matched_path_or_any_parents(candidate, path.ends_with('/'))
                .is_ignore()
        })
        .collect()
}

/// Maps to CC `normalizeGitPaths` (`fileSuggestions.ts:152-164`).
///
/// `git ls-files` is run from the repository root, so its rows are repo-root
/// relative. The index contract is cwd-relative, which inside a subdirectory
/// legitimately means rows carrying `../` prefixes — that is precisely what
/// lets `@../` and `@../../` resolve against the repository rather than
/// against whatever happens to sit on disk below the cwd.
fn normalize_git_paths(files: Vec<String>, repo_root: &Path, cwd: &Path) -> Vec<String> {
    if cwd == repo_root {
        return files;
    }
    files
        .into_iter()
        .map(|file| crate::utils::path::node_path_relative(cwd, &repo_root.join(file)))
        .collect()
}

/// Maps to CC `getFilesUsingGit` (`fileSuggestions.ts:248-383`).
///
/// `None` is reserved for "not a git repository, fall back to ripgrep": the
/// source returns null only when `findGitRoot` misses or the `git` invocation
/// itself fails (:277 tests the exit code, never the row count). An empty
/// listing is a legitimate answer — a fully gitignored working directory has
/// no candidates — and reporting it as a git failure hands the index to the
/// filesystem fallback, which then offers every ignored build artifact below
/// the cwd as an `@` completion.
///
/// Deliberate deviation: the source splits this into a synchronous tracked
/// pass (`--recurse-submodules`) plus a background untracked merge. One
/// combined invocation yields the same final set (submodule contents aside)
/// without the generation/merge machinery; `--exclude-standard` applies to
/// `--others` only, exactly as it does in the source's untracked args.
async fn get_files_using_git(cwd: &Path, respect_gitignore: bool) -> Option<Vec<String>> {
    let repo_root = crate::utils::git::find_git_root(cwd)?;
    // `find_git_root` returns an NFC-normalized ancestor (`git.rs:151`), while
    // macOS hands back NFD from `current_dir()`. Rebasing one form onto the
    // other compares components that never match, which would report every
    // repository row as living outside the cwd — normalize this side too.
    let cwd = &PathBuf::from(cwd.to_string_lossy().nfc().collect::<String>());
    let mut args = vec![
        "-c".to_string(),
        "core.quotepath=false".to_string(),
        "ls-files".to_string(),
        "--cached".to_string(),
        "--others".to_string(),
    ];
    if respect_gitignore {
        args.push("--exclude-standard".to_string());
    }
    let mut command = tokio::process::Command::new(crate::utils::git::git_exe());
    // CC inherits process.env (execFileNoThrowWithCwd); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env(&mut command);
    let output = command
        .args(args)
        // CC `execFileNoThrowWithCwd(..., { cwd: repoRoot })` — rows must be
        // repo-root relative before `normalizeGitPaths` rebases them onto cwd.
        .current_dir(&repo_root)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let files = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            (!line.is_empty()).then(|| String::from_utf8_lossy(line).replace('\\', "/"))
        })
        .collect::<Vec<_>>();
    // CC :287-297 — rebase first, then apply the ripgrep ignore files to the
    // normalized rows. Order matters: the patterns are matched against the
    // same cwd-relative spelling the index will hold.
    let normalized = normalize_git_paths(files, &repo_root, cwd);
    let Some(patterns) = load_ripgrep_ignore_patterns(&repo_root, cwd) else {
        return Some(normalized);
    };
    let before = normalized.len();
    let filtered = apply_ignore_patterns(&patterns, normalized);
    crate::utils::debug::log_for_debugging(&format!(
        "[FileIndex] applied ignore patterns: {before} -> {} files",
        filtered.len()
    ));
    Some(filtered)
}

/// Maps to the ripgrep fallback inside CC `getProjectFiles`
/// (`fileSuggestions.ts:476-515`).
///
/// ripgrep honours `.gitignore` unless `--no-ignore-vcs` is passed, so the
/// fallback inherits the same ignore policy as the git path instead of walking
/// the tree blind.
async fn ripgrep_paths(cwd: &Path, respect_gitignore: bool) -> Vec<String> {
    let mut args = vec![
        "--files".to_string(),
        "--follow".to_string(),
        "--hidden".to_string(),
    ];
    for vcs_dir in ["!.git/", "!.svn/", "!.hg/", "!.bzr/", "!.jj/", "!.sl/"] {
        args.push("--glob".to_string());
        args.push(vcs_dir.to_string());
    }
    if !respect_gitignore {
        args.push("--no-ignore-vcs".to_string());
    }
    // `rip_grep` blocks on the child process; the render loop must not.
    let files = tokio::task::spawn_blocking(move || {
        crate::utils::ripgrep::rip_grep(
            &args,
            Path::new("."),
            &crate::tool::AbortController::default(),
        )
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    files
        .iter()
        .map(|file| crate::utils::path::node_path_relative(cwd, Path::new(file)))
        .filter(|path| !path.is_empty())
        .collect()
}

/// Maps to CC `getProjectFiles` (`fileSuggestions.ts:459-516`).
async fn get_project_files(cwd: &Path, respect_gitignore: bool) -> Vec<String> {
    match get_files_using_git(cwd, respect_gitignore).await {
        Some(files) => files,
        None => ripgrep_paths(cwd, respect_gitignore).await,
    }
}

/// Maps to CC `getDirectoryNames`. The `Set` in the source preserves first
/// discovery order; the paired vector does the same without introducing a
/// second path-index implementation.
pub fn get_directory_names(files: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut directories = Vec::new();
    for file in files {
        let mut current = Path::new(file).parent();
        while let Some(directory) = current {
            if directory.as_os_str().is_empty() || directory == Path::new(".") {
                break;
            }
            // CC's loop sentinel is `dirname(currentDir) === currentDir`
            // (`fileSuggestions.ts:434-436`), true only at the filesystem root,
            // and it breaks *before* `add` so the root is never a completion.
            // Rust reports that same fixed point as `parent() == None` rather
            // than as self, so the comparison has to be against `None` —
            // otherwise the root escapes as a `//` row. Reachable since the
            // Claude config files enter this walk as absolute paths.
            let Some(parent) = directory.parent() else {
                break;
            };
            let value = directory.to_string_lossy().replace('\\', "/");
            if !seen.insert(value.clone()) {
                break;
            }
            directories.push(format!("{value}/"));
            current = Some(parent);
        }
    }
    directories
}

/// Async owner used by the source cold-start index build. Directory walking
/// itself is synchronous over already collected path names, so this adapter
/// yields once and keeps the API awaitable for the producer worker.
pub async fn get_directory_names_async(files: &[String]) -> Vec<String> {
    let directories = get_directory_names(files);
    futures_timer::Delay::new(Duration::from_millis(0)).await;
    directories
}

/// Maps to CC `getPathsForSuggestions`. Rust's path index is represented by
/// its normalized path list; callers still receive the same files and parent
/// directories that the source `FileIndex` exposes to `search`.
pub async fn get_paths_for_suggestions() -> Vec<String> {
    indexed_paths().await
}

/// Maps to CC `startBackgroundCacheRefresh`. A single in-flight refresh is
/// shared by all typeahead instances, matching the source module singleton.
pub fn start_background_cache_refresh() {
    if REFRESH_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    let has_cache = FILE_CACHE
        .lock()
        .ok()
        .is_some_and(|cache| cache.loaded_at.is_some());
    let last = LAST_REFRESH_MS.load(Ordering::Acquire);
    if has_cache && now.saturating_sub(last) < REFRESH_THROTTLE_MS {
        REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        return;
    }
    LAST_REFRESH_MS.store(now, Ordering::Release);
    let Ok(_handle) = tokio::runtime::Handle::try_current() else {
        REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        return;
    };
    tokio::spawn(async {
        let _ = indexed_paths().await;
        REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    });
}

async fn indexed_paths() -> Vec<String> {
    let generation = CACHE_GENERATION.load(Ordering::Acquire);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(cache) = FILE_CACHE.lock() {
        if cache.cwd == cwd && cache.loaded_at.is_some_and(|at| at.elapsed() < CACHE_TTL) {
            return cache.paths.clone();
        }
    }

    let settings = get_initial_settings();
    let global = crate::utils::config::load_global_config();
    let respect_gitignore = settings
        .respect_gitignore
        .or(global.respect_gitignore)
        .unwrap_or(true);
    let mut all_files = get_project_files(&cwd, respect_gitignore).await;
    // Maps to CC `getClaudeConfigFiles` in `getPathsForSuggestions`. These
    // markdown files are outside the project git index but are still valid
    // `@` targets. The canonical loader remains the owner of source/policy
    // and worktree precedence.
    for subdir in [
        "commands",
        "agents",
        "output-styles",
        "skills",
        "workflows",
        "templates",
    ] {
        all_files.extend(
            crate::utils::markdown_config_loader::load_markdown_files_for_subdir(subdir, &cwd)
                .into_iter()
                .map(|file| file.file_path.to_string_lossy().replace('\\', "/")),
        );
    }
    // CC `getPathsForSuggestions:543-546` — parent directories are derived
    // from the file list and prepended, so a directory stays completable even
    // though git only ever reports the files inside it.
    let mut paths = get_directory_names_async(&all_files).await;
    paths.extend(all_files);
    paths.sort();
    paths.dedup();
    if CACHE_GENERATION.load(Ordering::Acquire) == generation {
        if let Ok(mut cache) = FILE_CACHE.lock() {
            cache.cwd = cwd;
            cache.paths = paths.clone();
            cache.loaded_at = Some(Instant::now());
        }
    }
    paths
}

async fn top_level_paths() -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Ok(entries) = crate::utils::fs_operations::readdir(&cwd).await else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_directory = entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false);
        paths.push(if is_directory {
            format!("{name}/")
        } else {
            name
        });
    }
    paths.truncate(MAX_SUGGESTIONS);
    paths
}

/// @cometix: CC `findMatchingFiles` (`fileSuggestions.ts:618-625`) only calls
/// `fileIndex.search`. Scoring lives in `native-ts/file-index/index.ts#search`
/// (:173-288). This is a nucleo stand-in for that search, not a
/// `fileSuggestions.ts` helper.
///
/// The source has no substring short-circuit. A contiguous match simply earns
/// `BONUS_CONSECUTIVE` on every step and wins on its own merit, which keeps
/// the boundary and camelCase bonuses (`scoreBonusAt`, :297-305) meaningful
/// *between* rows that all contain the query. Collapsing those rows to one
/// ceiling value discards that ordering and leaves them to be broken by path
/// length alone.
///
/// Case handling is the source's smart case (:183-184, :211 — sensitive only
/// when the query itself carries uppercase), which is precisely
/// `CaseMatching::Smart`. That requires the haystack in its original case:
/// lowercasing it here would also destroy the `BONUS_CAMEL` signal, which is
/// defined by a lower→upper transition.
fn score_path(
    path: &str,
    matcher: &mut Matcher,
    pattern: &Pattern,
    buffer: &mut Vec<char>,
) -> Option<u32> {
    pattern.score(Utf32Str::new(path, buffer), matcher)
}

/// Maps to: CC `createFileSuggestionItem` (`fileSuggestions.ts:603-612`).
///
/// The score rides in metadata because that is the only channel
/// `unifiedSuggestions` reads it from (:133). It is CC's 0..1 rank, lower is
/// better — see the rank pass in `generate_file_suggestions`.
fn create_file_suggestion_item(path: String, score: Option<f64>) -> SuggestionItem {
    SuggestionItem {
        id: format!("file-{path}"),
        display_text: path.clone(),
        tag: None,
        command_text: path,
        description: String::new(),
        metadata: score.map(|score| serde_json::json!({"score": score})),
        color: None,
    }
}

/// Maps to CC `findLongestCommonPrefix` in `fileSuggestions.ts`.
///
/// The prefix is used by `useTypeahead` for the first Tab press: when every
/// candidate shares more path text than the user has typed, only that shared
/// prefix is inserted and the list remains open.
pub fn find_longest_common_prefix(suggestions: &[SuggestionItem]) -> String {
    let Some(first) = suggestions.first() else {
        return String::new();
    };
    let mut prefix = first.display_text.clone();
    for item in suggestions.iter().skip(1) {
        let length = prefix
            .chars()
            .zip(item.display_text.chars())
            .take_while(|(left, right)| left == right)
            .count();
        prefix = prefix.chars().take(length).collect();
        if prefix.is_empty() {
            break;
        }
    }
    prefix
}

/// Maps to `generateFileSuggestions(partialPath, showOnEmpty)`. The custom
/// `fileSuggestion` command remains an existing service seam; the built-in
/// path index is the default path and is what `@` uses when no override is set.
pub async fn generate_file_suggestions(query: &str, show_on_empty: bool) -> Vec<SuggestionItem> {
    if query.is_empty() && !show_on_empty {
        return Vec::new();
    }
    let settings = get_initial_settings();
    if settings
        .file_suggestion
        .as_ref()
        .is_some_and(|config| config.kind == "command")
    {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let session_id = crate::bootstrap::state::get_session_id();
        let transcript_path =
            crate::utils::session_storage::get_transcript_path_for_session(&session_id);
        let context = crate::services::hooks::HookContext {
            session_id,
            transcript_path: transcript_path.display().to_string(),
            cwd: cwd.display().to_string(),
            ..Default::default()
        };
        let mut payload = match crate::services::hooks::create_base_hook_input(&context) {
            serde_json::Value::Object(object) => object,
            _ => serde_json::Map::new(),
        };
        payload.insert("query".to_string(), serde_json::json!(query));
        let input_json = serde_json::Value::Object(payload).to_string();
        if let Some(paths) =
            crate::services::hooks::file_suggestion::execute_file_suggestion_command(
                &settings
                    .file_suggestion
                    .as_ref()
                    .expect("checked above")
                    .command,
                &input_json,
                Vec::new(),
            )
            .await
        {
            return paths
                .into_iter()
                .take(MAX_SUGGESTIONS)
                .map(|path| create_file_suggestion_item(path, None))
                .collect();
        }
        return Vec::new();
    }

    if query.is_empty() || query == "." || query == "./" {
        let top_level = top_level_paths().await;
        // Match CC's immediate result plus background index warm-up. The
        // latter is intentionally fire-and-forget so an empty `@` never waits
        // on git/ripgrep discovery.
        let _ = tokio::spawn(indexed_paths());
        return top_level
            .into_iter()
            .map(|path| create_file_suggestion_item(path, None))
            .collect();
    }
    let mut paths = indexed_paths().await;
    let normalized_query = query.strip_prefix("./").unwrap_or(query).replace('\\', "/");
    let pattern = Pattern::new(
        &normalized_query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buffer = Vec::new();
    let mut scored = paths
        .drain(..)
        .filter_map(|path| {
            score_path(&path, &mut matcher, &pattern, &mut buffer).map(|score| (path, score))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.len().cmp(&right.0.len()))
            .then_with(|| left.0.cmp(&right.0))
    });
    scored.truncate(MAX_SUGGESTIONS);

    // CC `index.ts:274-288` — the raw fuzzy score only *orders* the rows; the
    // score handed back is the rank itself, `i / matchCount`: 0 for the best
    // row, approaching 1 for the worst. That rank is the cross-source currency
    // `unifiedSuggestions` compares against Fuse.js output, where lower wins,
    // so it has to be a 0..1 rank and not a raw match score. Rows containing
    // `test` are pushed back by 1.05x (:283), capped at 1.
    let denominator = scored.len().max(1) as f64;
    scored
        .into_iter()
        .enumerate()
        .map(|(rank, (path, _))| {
            let position_score = rank as f64 / denominator;
            let final_score = if path.contains("test") {
                (position_score * 1.05).min(1.0)
            } else {
                position_score
            };
            create_file_suggestion_item(path, Some(final_score))
        })
        .collect()
}

/// Maps to CC `fileSuggestions.ts#applyFileSuggestion`.
///
/// Formatting (`@`, quoting and completion suffixes) belongs to the source
/// `useTypeahead.tsx#formatReplacementValue` owner.  This owner only replaces
/// the supplied partial token, preserving the surrounding input and returning
/// the new UTF-8 cursor position.  Keeping the two steps separate is required
/// for the source's common-prefix Tab path, which applies a non-complete
/// replacement without clearing the rows.
pub fn apply_file_suggestion(
    replacement: &str,
    input: &str,
    partial_path: &str,
    start_pos: usize,
) -> Option<(String, usize)> {
    if start_pos > input.len()
        || !input.is_char_boundary(start_pos)
        || partial_path.len() > input.len().saturating_sub(start_pos)
    {
        return None;
    }
    let end = start_pos + partial_path.len();
    if !input.is_char_boundary(end) {
        return None;
    }
    let mut next = String::with_capacity(input.len() - partial_path.len() + replacement.len());
    next.push_str(&input[..start_pos]);
    next.push_str(replacement);
    next.push_str(&input[end..]);
    Some((next, start_pos + replacement.len()))
}

/// Maps to CC `clearFileSuggestionCaches` (plural). The singular wrapper is
/// retained only for existing callers while the public owner uses the source
/// name and clears every generation/throttle field.
pub fn clear_file_suggestion_caches() {
    CACHE_GENERATION.fetch_add(1, Ordering::AcqRel);
    LAST_REFRESH_MS.store(0, Ordering::Release);
    REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    if let Ok(mut cache) = FILE_CACHE.lock() {
        *cache = FileCache::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::prompt_input::prompt_input_footer_suggestions::SuggestionItem;

    #[test]
    fn apply_file_suggestion_preserves_surrounding_text() {
        let input = "read @src/ma.rs now";
        // Token discovery is owned by `use_typeahead::extract_completion_token`;
        // this file only owns the source `fileSuggestions.ts` range rewrite.
        let token_start = 5;
        let token_end = 15;
        let item = SuggestionItem {
            id: "file-src/main.rs".into(),
            display_text: "src/main.rs".into(),
            tag: None,
            command_text: "src/main.rs".into(),
            description: String::new(),
            metadata: Some(serde_json::json!({"type":"file"})),
            color: None,
        };
        let replacement = crate::hooks::use_typeahead::format_replacement_value(
            &item.display_text,
            "prompt",
            true,
            item.display_text.contains(' '),
            false,
            true,
        );
        let (next, cursor) = apply_file_suggestion(
            &replacement,
            input,
            &input[token_start..token_end],
            token_start,
        )
        .expect("valid UTF-8 token range");
        assert_eq!(next, "read @src/main.rs  now");
        assert_eq!(&next[..cursor], "read @src/main.rs ");
    }

    #[test]
    fn directory_name_helpers_keep_parent_order_and_skip_workspace_root() {
        let files = vec![
            "src/index.rs".to_string(),
            "src/utils/helpers.rs".to_string(),
            "README.md".to_string(),
        ];
        assert_eq!(get_directory_names(&files), vec!["src/", "src/utils/"]);
    }

    /// CC `normalizeGitPaths` (`fileSuggestions.ts:152-164`) rebases repo-root
    /// rows onto the cwd, which from a subdirectory means `../` prefixes. The
    /// identity shortcut for `cwd === repoRoot` is part of the contract.
    #[test]
    fn git_rows_rebase_onto_cwd_like_official() {
        let repo = Path::new("/workspace/repo");
        let files = vec!["src/main.rs".to_string(), "README.md".to_string()];
        assert_eq!(
            normalize_git_paths(files.clone(), repo, repo),
            files,
            "cwd === repoRoot returns the rows untouched"
        );
        assert_eq!(
            normalize_git_paths(files, repo, &repo.join(".test/workdir")),
            vec!["../../src/main.rs", "../../README.md"],
        );
    }

    /// The parent-directory derivation has to survive rebased rows too, or a
    /// subdirectory session can complete `../../src/main.rs` but never
    /// `../../src/`. Node's `dirname` walk stops at `..`; `Path::parent` has
    /// the same fixed point, reached through an empty component.
    #[test]
    fn directory_names_cover_rebased_parent_rows() {
        let files = vec!["../../src/main.rs".to_string()];
        assert_eq!(
            get_directory_names(&files),
            vec!["../../src/", "../../", "../"]
        );
    }

    /// CC `index.ts:274-288` — the number handed to `unifiedSuggestions` is
    /// the RANK (`i / matchCount`), not the raw match quality. A nucleo score
    /// here would be on a scale no other suggestion source shares, and the
    /// old substring short-circuit made it worse by flattening every
    /// containing row onto one ceiling value.
    #[tokio::test]
    async fn file_scores_are_ranks_not_raw_match_quality() {
        let root = std::env::temp_dir().join(format!("cometix-rank-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        for name in ["alpha_one.rs", "alpha_two.rs", "alpha_three.rs"] {
            std::fs::write(root.join("src").join(name), "").unwrap();
        }
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        // The producer reads the process cwd; nextest gives each test its own
        // process, so this cannot leak into a sibling.
        std::env::set_current_dir(&root).unwrap();

        let items = generate_file_suggestions("alpha", false).await;
        let scores: Vec<f64> = items
            .iter()
            .map(|item| {
                item.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("score"))
                    .and_then(serde_json::Value::as_f64)
                    .expect("every row carries a rank")
            })
            .collect();

        assert!(scores.len() >= 3, "all three rows match: {items:?}");
        assert_eq!(scores[0], 0.0, "the best row is rank 0: {scores:?}");
        assert!(
            scores.iter().all(|score| (0.0..=1.0).contains(score)),
            "ranks stay on the shared 0..1 scale: {scores:?}"
        );
        assert!(
            scores.iter().any(|score| *score > 0.0),
            "ranks are distinct, not one flattened ceiling: {scores:?}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// `.ignore`/`.rgignore` are ripgrep's files, not git's —
    /// `--exclude-standard` never consults them, so a row git happily reports
    /// has to be filtered afterwards (`fileSuggestions.ts:290-297`). They also
    /// outrank the `respectGitignore` switch: `config.ts:459` documents them
    /// as "always respected".
    #[tokio::test]
    async fn ripgrep_ignore_files_filter_git_rows_regardless_of_the_switch() {
        let root = std::env::temp_dir().join(format!("cometix-rgignore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::write(root.join("vendor/bundle.js"), "").unwrap();
        // Only the ripgrep ignore file claims `vendor/`; `.gitignore` is absent,
        // so `git ls-files --others --exclude-standard` still reports it.
        std::fs::write(root.join(".ignore"), "vendor/\n").unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );

        for respect_gitignore in [true, false] {
            let rows = get_files_using_git(&root, respect_gitignore)
                .await
                .expect("git repo");
            assert!(
                rows.iter().any(|row| row == "src/main.rs"),
                "unclaimed rows survive (respect_gitignore={respect_gitignore}): {rows:?}"
            );
            assert!(
                !rows.iter().any(|row| row.starts_with("vendor/")),
                ".ignore claims vendor/ (respect_gitignore={respect_gitignore}): {rows:?}"
            );
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The whole defect in one fixture: a cwd that is itself gitignored.
    ///
    /// Running `ls-files` from the cwd returns zero rows there, and treating
    /// that as "not a git repo" used to drop the index onto a filesystem walk
    /// that indexed every ignored artifact below the cwd. CC only falls back
    /// when git itself fails (`fileSuggestions.ts:277`), so the repository's
    /// own files must still arrive — rebased with `../` prefixes.
    #[tokio::test]
    async fn gitignored_cwd_still_indexes_the_repository_not_its_artifacts() {
        let root =
            std::env::temp_dir().join(format!("cometix-file-index-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".test/workdir")).unwrap();
        // macOS hands out `/var/...` temp dirs that are symlinks into
        // `/private/var`; the relative walk compares components literally.
        let root = std::fs::canonicalize(&root).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::write(root.join(".gitignore"), "/.test/\n").unwrap();
        std::fs::write(root.join(".test/workdir/artifact.log"), "").unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );

        let workdir = root.join(".test/workdir");
        let rows = get_files_using_git(&workdir, true)
            .await
            .expect("an ignored cwd is still inside a git repository");

        assert!(
            rows.iter().any(|row| row == "../../src/main.rs"),
            "repository rows arrive rebased onto the cwd: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("artifact.log")),
            "ignored artifacts below the cwd stay out of the index: {rows:?}"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}
