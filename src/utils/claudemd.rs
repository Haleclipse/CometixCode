//!    - .claude/CLAUDE.md (dot-claude)

use crate::utils::config;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

#[derive(Debug, Clone)]
pub struct ClaudeMdFile {
    pub path: PathBuf,
    pub source: ClaudeMdSource,
    pub kind: ClaudeMdKind,
    pub content: String,
    /// File that referenced this one through an `@path` include.
    pub parent: Option<PathBuf>,
    /// Reserved for dynamically loaded nested-directory memories.
    pub is_nested: bool,
}

/// Disk/content metadata used by nested-memory persistence. Keeping this as a
/// derived projection avoids duplicating raw bytes on every startup memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeMdDiskProjection {
    pub raw_content: String,
    pub content_differs_from_disk: bool,
    pub globs: Option<Vec<String>>,
}

/// Maps to: CC `utils/claudemd.ts:229-243` `MemoryFileInfo` — the wire shape
/// embedded as `nested_memory` attachment `content`
/// (utils/attachments.ts:490-495).
///
/// [`ClaudeMdFile`] + [`ClaudeMdDiskProjection`] remain the in-memory model;
/// this struct is the serde projection the attachment seam stores and
/// round-trips through session JSONL.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFileInfo {
    pub path: String,
    /// CC `type: MemoryType` — `'User' | 'Project' | 'Local' | 'Managed' |
    /// 'AutoMem' | 'TeamMem'` (utils/memory/types.ts:3-12). Kept as `String`
    /// per this union's scalar-string-field convention (see `plan_mode
    /// reminder_type`), which also keeps feature-gated values parseable.
    #[serde(rename = "type")]
    pub memory_type: String,
    pub content: String,
    /// Path of the file that included this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Glob patterns for file paths this rule applies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub globs: Option<Vec<String>>,
    /// True when auto-injection transformed `content` away from disk bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_differs_from_disk: Option<bool>,
    /// Unmodified disk bytes when `contentDiffersFromDisk` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_content: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeMdKind {
    Managed,
    User,
    Project,
    Local,
    AutoMem,
    TeamMem,
}

/// Maps to: CC `utils/claudemd.ts` `ExternalClaudeMdInclude`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalClaudeMdInclude {
    pub path: String,
    pub parent: String,
}

impl ExternalClaudeMdInclude {
    pub fn new(path: impl Into<String>, parent: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            parent: parent.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeMdSource {
    Managed,
    UserGlobal,
    Project,
    Local,
}

impl ClaudeMdSource {
    fn kind(self) -> ClaudeMdKind {
        match self {
            Self::Managed => ClaudeMdKind::Managed,
            Self::UserGlobal => ClaudeMdKind::User,
            Self::Project => ClaudeMdKind::Project,
            Self::Local => ClaudeMdKind::Local,
        }
    }
}

/// Maps to CC `utils/claudemd.ts` memoized `getMemoryFiles()` owner.
static MEMORY_FILES_CACHE: LazyLock<RwLock<Option<Vec<ClaudeMdFile>>>> =
    LazyLock::new(|| RwLock::new(None));
static SHOULD_FIRE_INSTRUCTIONS_LOADED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);
static NEXT_EAGER_LOAD_REASON: LazyLock<RwLock<&'static str>> =
    LazyLock::new(|| RwLock::new("session_start"));

pub fn discover_claude_md_files() -> Vec<ClaudeMdFile> {
    #[cfg(not(test))]
    if let Ok(cache) = MEMORY_FILES_CACHE.read() {
        if let Some(files) = cache.as_ref() {
            return files.clone();
        }
    }

    let additional_dirs = crate::bootstrap::state::get_additional_directories_for_claude_md();
    let include_default_discovery = !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
    );
    let files = discover_claude_md_files_with_options(include_default_discovery, &additional_dirs);

    #[cfg(not(test))]
    if let Ok(mut cache) = MEMORY_FILES_CACHE.write() {
        *cache = Some(files.clone());
    }
    files
}

/// Maps to CC `utils/claudemd.ts:1119-1122` `clearMemoryFileCaches()` — drops the
/// memoized discovery result without arming the InstructionsLoaded hook.
pub fn clear_memory_file_caches() {
    if let Ok(mut cache) = MEMORY_FILES_CACHE.write() {
        *cache = None;
    }
}

/// Maps to CC `utils/claudemd.ts` `resetGetMemoryFilesCache(reason)` for the
/// cache-invalidating portion. InstructionsLoaded hook arming remains owned by
/// the hook service and is not fabricated here.
pub fn reset_get_memory_files_cache(reason: &str) {
    clear_memory_file_caches();
    let reason = if reason == "compact" {
        "compact"
    } else {
        "session_start"
    };
    if let Ok(mut next_reason) = NEXT_EAGER_LOAD_REASON.write() {
        *next_reason = reason;
    }
    SHOULD_FIRE_INSTRUCTIONS_LOADED.store(true, std::sync::atomic::Ordering::Release);
}

#[cfg(test)]
pub(crate) fn seed_memory_files_cache_for_test() {
    MEMORY_FILES_CACHE.write().unwrap().replace(Vec::new());
}

#[cfg(test)]
pub(crate) fn memory_files_cache_is_populated_for_test() -> bool {
    MEMORY_FILES_CACHE
        .read()
        .map(|cache| cache.is_some())
        .unwrap_or(false)
}

/// Maps to CC `utils/claudemd.ts` `getMemoryFiles()` default discovery plus
/// additional `--add-dir` discovery.
///
/// `include_default_discovery=false` is the Rust equivalent of CC bare mode's
/// "skip what I didn't ask for" behavior. Explicit add-dir CLAUDE.md loading
/// remains controlled by `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD`, just
/// like upstream.
pub fn discover_claude_md_files_with_options(
    include_default_discovery: bool,
    additional_dirs: &[PathBuf],
) -> Vec<ClaudeMdFile> {
    discover_claude_md_files_with_external_policy(include_default_discovery, additional_dirs, false)
}

/// Maps to CC `getMemoryFiles(forceIncludeExternal)`.
///
/// When `force_include_external` is true, `@include` targets outside the
/// original CWD are loaded even if the project has not approved them — used
/// only by the external-includes approval check, not for context building.
pub fn discover_claude_md_files_with_external_policy(
    include_default_discovery: bool,
    additional_dirs: &[PathBuf],
    force_include_external: bool,
) -> Vec<ClaudeMdFile> {
    let mut files = Vec::new();
    let mut processed = HashSet::new();
    let cwd = std::env::current_dir().unwrap_or_default();
    let config_home = crate::utils::env_utils::get_claude_config_home_dir();
    let global_config = config::load_global_config();
    let project_key = config::normalize_project_path(&cwd.to_string_lossy());
    let include_project_external = force_include_external
        || global_config
            .projects
            .get(&project_key)
            .and_then(|project| project.has_claude_md_external_includes_approved)
            .unwrap_or(false);

    if include_default_discovery {
        // Managed policy is always enabled, but its external includes use the
        // same approval-derived gate as upstream project policy.
        process_memory_file(
            &config::get_memory_path("Managed"),
            ClaudeMdSource::Managed,
            include_project_external,
            0,
            None,
            &cwd,
            &mut processed,
            &mut files,
        );
        load_rules_recursively(
            &config::get_managed_claude_rules_dir(),
            ClaudeMdSource::Managed,
            include_project_external,
            &cwd,
            &mut processed,
            &mut files,
        );

        if crate::utils::settings::constants::is_setting_source_enabled(
            crate::utils::settings::constants::SettingSource::User,
        ) {
            process_memory_file(
                &config_home.join("CLAUDE.md"),
                ClaudeMdSource::UserGlobal,
                true,
                0,
                None,
                &cwd,
                &mut processed,
                &mut files,
            );
            load_rules_recursively(
                &config_home.join("rules"),
                ClaudeMdSource::UserGlobal,
                true,
                &cwd,
                &mut processed,
                &mut files,
            );
        }

        // Official discovery walks from filesystem root down to the original
        // CWD so parent memories precede more specific child memories.
        let mut directories = cwd.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
        directories.reverse();
        let git_root = crate::utils::git::find_git_root(&cwd);
        let canonical_root = crate::utils::git::find_canonical_git_root(&cwd);
        let is_nested_worktree = git_root.as_ref().is_some_and(|git_root| {
            canonical_root.as_ref().is_some_and(|canonical_root| {
                normalize_for_dedup(git_root) != normalize_for_dedup(canonical_root)
                    && is_path_inside(git_root, canonical_root)
            })
        });
        for directory in directories {
            let skip_project = is_nested_worktree
                && canonical_root.as_ref().is_some_and(|canonical_root| {
                    is_path_inside(&directory, canonical_root)
                        && !git_root
                            .as_ref()
                            .is_some_and(|git_root| is_path_inside(&directory, git_root))
                });
            if crate::utils::settings::constants::is_setting_source_enabled(
                crate::utils::settings::constants::SettingSource::Project,
            ) && !skip_project
            {
                load_project_claude_md_files(
                    &directory,
                    include_project_external,
                    &cwd,
                    &mut processed,
                    &mut files,
                );
            }
            if crate::utils::settings::constants::is_setting_source_enabled(
                crate::utils::settings::constants::SettingSource::Local,
            ) {
                process_memory_file(
                    &directory.join("CLAUDE.local.md"),
                    ClaudeMdSource::Local,
                    include_project_external,
                    0,
                    None,
                    &cwd,
                    &mut processed,
                    &mut files,
                );
            }
        }

        if crate::memdir::paths::is_auto_memory_enabled() {
            process_memory_file_with_kind(
                &crate::memdir::paths::get_auto_mem_entrypoint(),
                ClaudeMdSource::UserGlobal,
                ClaudeMdKind::AutoMem,
                true,
                0,
                None,
                &cwd,
                &mut processed,
                &mut files,
            );
        }
    }

    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD")
            .as_deref(),
    ) {
        for dir in additional_dirs {
            load_project_claude_md_files(
                dir,
                include_project_external,
                &cwd,
                &mut processed,
                &mut files,
            );
        }
    }

    if !force_include_external
        && SHOULD_FIRE_INSTRUCTIONS_LOADED.swap(false, std::sync::atomic::Ordering::AcqRel)
    {
        let eager_reason = NEXT_EAGER_LOAD_REASON
            .read()
            .map(|reason| *reason)
            .unwrap_or("session_start");
        if let Ok(mut next_reason) = NEXT_EAGER_LOAD_REASON.write() {
            *next_reason = "session_start";
        }
        for file in &files {
            let memory_type = match file.kind {
                ClaudeMdKind::Managed => Some("Managed"),
                ClaudeMdKind::User => Some("User"),
                ClaudeMdKind::Project => Some("Project"),
                ClaudeMdKind::Local => Some("Local"),
                ClaudeMdKind::AutoMem | ClaudeMdKind::TeamMem => None,
            };
            let Some(memory_type) = memory_type else {
                continue;
            };
            let projection = claude_md_disk_projection(file);
            crate::services::hooks::instructions_loaded::dispatch_instructions_loaded_hooks(
                crate::services::hooks::instructions_loaded::InstructionsLoadedInput {
                    file_path: file.path.display().to_string(),
                    memory_type,
                    load_reason: if file.parent.is_some() {
                        "include"
                    } else {
                        eager_reason
                    },
                    globs: projection.globs,
                    trigger_file_path: None,
                    parent_file_path: file.parent.as_ref().map(|path| path.display().to_string()),
                },
            );
        }
    }

    files
}

/// Maps to: CC `utils/claudemd.ts` `pathInOriginalCwd`.
fn path_in_original_cwd(path: &Path) -> bool {
    let cwd = crate::bootstrap::state::get_original_cwd();
    is_path_inside(path, &cwd)
}

/// Maps to: CC `utils/claudemd.ts` `getExternalClaudeMdIncludes`.
pub fn get_external_claude_md_includes(files: &[ClaudeMdFile]) -> Vec<ExternalClaudeMdInclude> {
    let mut externals = Vec::new();
    for file in files {
        // User-global memories may always include outside CWD; they are not
        // part of the project trust warning surface.
        if file.kind == ClaudeMdKind::User {
            continue;
        }
        let Some(parent) = file.parent.as_ref() else {
            continue;
        };
        if path_in_original_cwd(&file.path) {
            continue;
        }
        externals.push(ExternalClaudeMdInclude::new(
            file.path.display().to_string(),
            parent.display().to_string(),
        ));
    }
    externals
}

/// Maps to: CC `utils/claudemd.ts` `hasExternalClaudeMdIncludes`.
pub fn has_external_claude_md_includes(files: &[ClaudeMdFile]) -> bool {
    !get_external_claude_md_includes(files).is_empty()
}

/// Maps to: CC `utils/claudemd.ts` `shouldShowClaudeMdExternalIncludesWarning`.
pub fn should_show_claude_md_external_includes_warning() -> bool {
    let project = config::get_current_project_config();
    if project
        .has_claude_md_external_includes_approved
        .unwrap_or(false)
        || project
            .has_claude_md_external_includes_warning_shown
            .unwrap_or(false)
    {
        return false;
    }

    let additional_dirs = crate::bootstrap::state::get_additional_directories_for_claude_md();
    let include_default_discovery = !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
    );
    let files = discover_claude_md_files_with_external_policy(
        include_default_discovery,
        &additional_dirs,
        true,
    );
    has_external_claude_md_includes(&files)
}

pub fn build_claude_md_context() -> String {
    let files = discover_claude_md_files();
    if files.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    for file in &files {
        let source_label = match file.source {
            ClaudeMdSource::Managed => "managed",
            ClaudeMdSource::UserGlobal => "user global",
            ClaudeMdSource::Project => "project",
            ClaudeMdSource::Local => "local",
        };
        parts.push(format!(
            "# Source: {} ({})\n\n{}",
            file.path.display(),
            source_label,
            file.content
        ));
    }

    parts.join("\n\n---\n\n")
}

/// Maps to CC `utils/claudemd.ts:getMemoryFilesForNestedDirectory`
/// (:1249-1320). This source-shaped owner loads project/local instructions for
/// one directory below cwd; attachment ordering/dedup remains in
/// `utils/attachments.rs`.
pub fn get_memory_files_for_nested_directory(
    dir: &Path,
    target_path: &Path,
    original_cwd: &Path,
    processed: &mut HashSet<PathBuf>,
) -> Vec<ClaudeMdFile> {
    let mut files = Vec::new();
    let project_enabled = crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::Project,
    );
    if project_enabled {
        process_memory_file(
            &dir.join("CLAUDE.md"),
            ClaudeMdSource::Project,
            false,
            0,
            None,
            original_cwd,
            processed,
            &mut files,
        );
        process_memory_file(
            &dir.join(".claude").join("CLAUDE.md"),
            ClaudeMdSource::Project,
            false,
            0,
            None,
            original_cwd,
            processed,
            &mut files,
        );
    }
    if crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::Local,
    ) {
        process_memory_file(
            &dir.join("CLAUDE.local.md"),
            ClaudeMdSource::Local,
            false,
            0,
            None,
            original_cwd,
            processed,
            &mut files,
        );
    }
    for file in &mut files {
        file.is_nested = true;
    }

    if project_enabled {
        let rules_dir = dir.join(".claude").join("rules");
        // CC uses a clone for the unconditional pass so conditional files remain
        // eligible for the target-specific pass, then merges both processed sets.
        let mut unconditional_processed = processed.clone();
        load_rules_recursively(
            &rules_dir,
            ClaudeMdSource::Project,
            false,
            original_cwd,
            &mut unconditional_processed,
            &mut files,
        );
        let mut conditional = Vec::new();
        load_rules_recursively_inner(
            &rules_dir,
            ClaudeMdSource::Project,
            false,
            original_cwd,
            processed,
            &mut conditional,
            &mut HashSet::new(),
            Some((dir, target_path)),
        );
        files.extend(conditional);
        processed.extend(unconditional_processed);
    }
    files
}

/// Maps to CC `utils/claudemd.ts#getManagedAndUserConditionalRules`.
pub fn get_managed_and_user_conditional_rules(
    target_path: &Path,
    original_cwd: &Path,
    processed: &mut HashSet<PathBuf>,
) -> Vec<ClaudeMdFile> {
    let mut files = Vec::new();
    load_rules_recursively_inner(
        &crate::utils::config::get_managed_claude_rules_dir(),
        ClaudeMdSource::Managed,
        false,
        original_cwd,
        processed,
        &mut files,
        &mut HashSet::new(),
        Some((original_cwd, target_path)),
    );
    if crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::User,
    ) {
        load_rules_recursively_inner(
            &crate::utils::config::get_user_claude_rules_dir(),
            ClaudeMdSource::UserGlobal,
            true,
            original_cwd,
            processed,
            &mut files,
            &mut HashSet::new(),
            Some((original_cwd, target_path)),
        );
    }
    files
}

/// Maps to CC `utils/claudemd.ts#getConditionalRulesForCwdLevelDirectory`.
pub fn get_conditional_rules_for_cwd_level_directory(
    dir: &Path,
    target_path: &Path,
    original_cwd: &Path,
    processed: &mut HashSet<PathBuf>,
) -> Vec<ClaudeMdFile> {
    if !crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::Project,
    ) {
        return Vec::new();
    }
    let mut files = Vec::new();
    load_rules_recursively_inner(
        &dir.join(".claude").join("rules"),
        ClaudeMdSource::Project,
        false,
        original_cwd,
        processed,
        &mut files,
        &mut HashSet::new(),
        Some((dir, target_path)),
    );
    files
}

fn load_project_claude_md_files(
    dir: &Path,
    include_external: bool,
    cwd: &Path,
    processed: &mut HashSet<PathBuf>,
    files: &mut Vec<ClaudeMdFile>,
) {
    for path in [dir.join("CLAUDE.md"), dir.join(".claude").join("CLAUDE.md")] {
        process_memory_file(
            &path,
            ClaudeMdSource::Project,
            include_external,
            0,
            None,
            cwd,
            processed,
            files,
        );
    }
    load_rules_recursively(
        &dir.join(".claude").join("rules"),
        ClaudeMdSource::Project,
        include_external,
        cwd,
        processed,
        files,
    );
}

fn load_rules_recursively(
    directory: &Path,
    source: ClaudeMdSource,
    include_external: bool,
    cwd: &Path,
    processed: &mut HashSet<PathBuf>,
    files: &mut Vec<ClaudeMdFile>,
) {
    load_rules_recursively_inner(
        directory,
        source,
        include_external,
        cwd,
        processed,
        files,
        &mut HashSet::new(),
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn load_rules_recursively_inner(
    directory: &Path,
    source: ClaudeMdSource,
    include_external: bool,
    cwd: &Path,
    processed: &mut HashSet<PathBuf>,
    files: &mut Vec<ClaudeMdFile>,
    visited_directories: &mut HashSet<PathBuf>,
    conditional_target: Option<(&Path, &Path)>,
) {
    let normalized_directory = normalize_for_dedup(directory);
    if !visited_directories.insert(normalized_directory) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let entry_is_markdown = path.extension().is_some_and(|extension| extension == "md");
        let resolved_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let metadata = std::fs::metadata(&resolved_path).ok();
        if metadata.as_ref().is_some_and(std::fs::Metadata::is_dir) {
            load_rules_recursively_inner(
                &resolved_path,
                source,
                include_external,
                cwd,
                processed,
                files,
                visited_directories,
                conditional_target,
            );
        } else if entry_is_markdown && metadata.as_ref().is_some_and(std::fs::Metadata::is_file) {
            let mut candidates = Vec::new();
            process_memory_file(
                &resolved_path,
                source,
                include_external,
                0,
                None,
                cwd,
                processed,
                &mut candidates,
            );
            for mut candidate in candidates {
                let projection = claude_md_disk_projection(&candidate);
                let include = match (projection.globs.as_deref(), conditional_target) {
                    (None, None) => true,
                    (Some(globs), Some((base_dir, target_path))) => {
                        memory_rule_globs_match(globs, base_dir, target_path)
                    }
                    _ => false,
                };
                if include {
                    candidate.is_nested = conditional_target.is_some();
                    files.push(candidate);
                }
            }
        }
    }
}

fn memory_rule_globs_match(globs: &[String], base_dir: &Path, target_path: &Path) -> bool {
    let Ok(relative) = target_path.strip_prefix(base_dir) else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(base_dir);
    for glob in globs {
        if builder.add_line(None, glob).is_err() {
            return false;
        }
    }
    builder.build().is_ok_and(|matcher| {
        matcher
            .matched_path_or_any_parents(relative, false)
            .is_ignore()
    })
}

fn normalize_for_dedup(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// Maps to CC `TEXT_FILE_EXTENSIONS` in `utils/claudemd.ts`. Extensionless
// include targets remain allowed; known binary/unknown extensions are skipped.
const TEXT_FILE_EXTENSIONS: &[&str] = &[
    "md",
    "txt",
    "text",
    "json",
    "yaml",
    "yml",
    "toml",
    "xml",
    "csv",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "js",
    "ts",
    "tsx",
    "jsx",
    "mjs",
    "cjs",
    "mts",
    "cts",
    "py",
    "pyi",
    "pyw",
    "rb",
    "erb",
    "rake",
    "go",
    "rs",
    "java",
    "kt",
    "kts",
    "scala",
    "c",
    "cpp",
    "cc",
    "cxx",
    "h",
    "hpp",
    "hxx",
    "cs",
    "swift",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "env",
    "ini",
    "cfg",
    "conf",
    "config",
    "properties",
    "sql",
    "graphql",
    "gql",
    "proto",
    "vue",
    "svelte",
    "astro",
    "ejs",
    "hbs",
    "pug",
    "jade",
    "php",
    "pl",
    "pm",
    "lua",
    "r",
    "dart",
    "ex",
    "exs",
    "erl",
    "hrl",
    "clj",
    "cljs",
    "cljc",
    "edn",
    "hs",
    "lhs",
    "elm",
    "ml",
    "mli",
    "f",
    "f90",
    "f95",
    "for",
    "cmake",
    "make",
    "makefile",
    "gradle",
    "sbt",
    "rst",
    "adoc",
    "asciidoc",
    "org",
    "tex",
    "latex",
    "lock",
    "log",
    "diff",
    "patch",
];

fn memory_path_has_allowed_text_extension(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(std::ffi::OsStr::to_str) else {
        return true;
    };
    let extension = extension.to_ascii_lowercase();
    TEXT_FILE_EXTENSIONS.contains(&extension.as_str())
}

fn normalized_slashes(text: &str) -> String {
    text.replace('\\', "/")
}

fn resolved_exclude_pattern(pattern: &str) -> Option<String> {
    let normalized = normalized_slashes(pattern);
    let path = Path::new(&normalized);
    if !path.is_absolute() {
        return None;
    }
    let glob_start = normalized.find(['*', '?', '{', '[']);
    let static_prefix = glob_start
        .map(|index| &normalized[..index])
        .unwrap_or(normalized.as_str());
    let directory = Path::new(static_prefix).parent()?;
    let resolved = std::fs::canonicalize(directory).ok()?;
    let directory_text = normalized_slashes(&directory.display().to_string());
    let resolved_text = normalized_slashes(&resolved.display().to_string());
    (resolved_text != directory_text).then(|| {
        format!(
            "{resolved_text}{}",
            normalized
                .strip_prefix(&directory_text)
                .unwrap_or(normalized.as_str())
        )
    })
}

fn claude_md_path_matches_excludes(path: &Path, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let mut candidates = vec![normalized_slashes(&path.display().to_string())];
    if let Ok(resolved) = std::fs::canonicalize(path) {
        let resolved = normalized_slashes(&resolved.display().to_string());
        if !candidates.contains(&resolved) {
            candidates.push(resolved);
        }
    }
    let mut expanded = patterns
        .iter()
        .map(|pattern| normalized_slashes(pattern))
        .filter(|pattern| !pattern.is_empty())
        .collect::<Vec<_>>();
    for resolved in patterns
        .iter()
        .filter_map(|pattern| resolved_exclude_pattern(pattern))
    {
        if !expanded.contains(&resolved) {
            expanded.push(resolved);
        }
    }
    expanded.into_iter().any(|pattern| {
        let Ok(glob) = globset::GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
        else {
            return false;
        };
        let matcher = glob.compile_matcher();
        candidates
            .iter()
            .any(|candidate| matcher.is_match(candidate))
    })
}

fn is_claude_md_excluded(path: &Path, kind: ClaudeMdKind) -> bool {
    if !matches!(
        kind,
        ClaudeMdKind::User | ClaudeMdKind::Project | ClaudeMdKind::Local
    ) {
        return false;
    }
    let patterns = crate::utils::settings::get_initial_settings()
        .claude_md_excludes
        .unwrap_or_default();
    claude_md_path_matches_excludes(path, &patterns)
}

fn is_path_inside(path: &Path, root: &Path) -> bool {
    normalize_for_dedup(path).starts_with(normalize_for_dedup(root))
}

fn extract_include_paths(content: &str, file_path: &Path) -> Vec<PathBuf> {
    fn extract_from_text(text: &str, file_path: &Path, paths: &mut Vec<PathBuf>) {
        static INCLUDE: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"(?:^|\s)@((?:[^\s\\]|\\ )+)").expect("include regex")
        });
        for captures in INCLUDE.captures_iter(text) {
            let Some(mut raw) = captures.get(1).map(|capture| capture.as_str().to_string()) else {
                continue;
            };
            if let Some(hash) = raw.find('#') {
                raw.truncate(hash);
            }
            raw = raw.replace("\\ ", " ");
            if raw.is_empty() {
                continue;
            }
            let valid = raw.starts_with("./")
                || raw.starts_with("~/")
                || (raw.starts_with('/') && raw != "/")
                || (!raw.starts_with('@')
                    && !raw
                        .chars()
                        .next()
                        .is_some_and(|character| "#%^&*()".contains(character))
                    && raw.chars().next().is_some_and(|character| {
                        character.is_ascii_alphanumeric() || "._-".contains(character)
                    }));
            if !valid {
                continue;
            }
            let path = crate::utils::path::expand_path(
                &raw,
                Some(file_path.parent().unwrap_or(Path::new("."))),
            )
            .unwrap_or_else(|_| PathBuf::from(&raw));
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }

    fn visit(tokens: &[marked_rs::Token], file_path: &Path, paths: &mut Vec<PathBuf>) {
        static COMMENT_SPAN: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?s)<!--.*?-->").unwrap());
        for token in tokens {
            match token {
                marked_rs::Token::Code { .. } | marked_rs::Token::Codespan { .. } => {}
                marked_rs::Token::Html { raw, .. } => {
                    let trimmed = raw.trim_start();
                    if trimmed.starts_with("<!--") && raw.contains("-->") {
                        let residue = COMMENT_SPAN.replace_all(raw, "");
                        if !residue.trim().is_empty() {
                            extract_from_text(&residue, file_path, paths);
                        }
                    }
                }
                marked_rs::Token::Text { text, tokens, .. } => {
                    extract_from_text(text, file_path, paths);
                    if let Some(tokens) = tokens {
                        visit(tokens, file_path, paths);
                    }
                }
                marked_rs::Token::Heading { tokens, .. }
                | marked_rs::Token::Blockquote { tokens, .. }
                | marked_rs::Token::ListItem { tokens, .. }
                | marked_rs::Token::Paragraph { tokens, .. }
                | marked_rs::Token::Strong { tokens, .. }
                | marked_rs::Token::Em { tokens, .. }
                | marked_rs::Token::Del { tokens, .. }
                | marked_rs::Token::Link { tokens, .. }
                | marked_rs::Token::Image { tokens, .. } => {
                    visit(tokens, file_path, paths);
                }
                marked_rs::Token::List { items, .. } => visit(items, file_path, paths),
                marked_rs::Token::Table { header, rows, .. } => {
                    for cell in header.iter().chain(rows.iter().flatten()) {
                        visit(&cell.tokens, file_path, paths);
                    }
                }
                marked_rs::Token::Space { .. }
                | marked_rs::Token::Hr { .. }
                | marked_rs::Token::Def { .. }
                | marked_rs::Token::Escape { .. }
                | marked_rs::Token::Br { .. } => {}
            }
        }
    }

    let mut options = marked_rs::MarkedOptions::default();
    options.gfm = false;
    let tokens = marked_rs::lexer_with_options(content, options);
    let mut paths = Vec::new();
    visit(&tokens.tokens, file_path, &mut paths);
    paths
}

const MAX_INCLUDE_DEPTH: usize = 5;

/// Maps to CC `frontmatterParser.ts#splitPathInFrontmatter` and
/// `claudemd.ts#parseFrontmatterPaths`.
fn split_frontmatter_path_string(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut brace_depth = 0usize;
    for character in input.chars() {
        match character {
            '{' => {
                brace_depth = brace_depth.saturating_add(1);
                current.push(character);
            }
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                current.push(character);
            }
            ',' if brace_depth == 0 => {
                let part = current.trim();
                if !part.is_empty() {
                    parts.push(part.to_string());
                }
                current.clear();
            }
            _ => current.push(character),
        }
    }
    let part = current.trim();
    if !part.is_empty() {
        parts.push(part.to_string());
    }
    parts
}

fn expand_frontmatter_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(relative_close) = pattern[open + 1..].find('}') else {
        return vec![pattern.to_string()];
    };
    let close = open + 1 + relative_close;
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    pattern[open + 1..close]
        .split(',')
        .flat_map(|alternative| {
            expand_frontmatter_braces(&format!("{prefix}{}{suffix}", alternative.trim()))
        })
        .collect()
}

fn frontmatter_paths(
    frontmatter: &crate::utils::frontmatter_parser::FrontmatterData,
) -> Option<Vec<String>> {
    let values = match frontmatter.get("paths")? {
        serde_json::Value::String(value) => vec![value.clone()],
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let patterns = values
        .iter()
        .flat_map(|value| split_frontmatter_path_string(value))
        .flat_map(|pattern| expand_frontmatter_braces(&pattern))
        .map(|pattern| pattern.strip_suffix("/**").unwrap_or(&pattern).to_string())
        .filter(|pattern| !pattern.is_empty())
        .collect::<Vec<_>>();
    if patterns.is_empty() || patterns.iter().all(|pattern| pattern == "**") {
        None
    } else {
        Some(patterns)
    }
}

fn marked_token_raw(token: &marked_rs::Token) -> &str {
    match token {
        marked_rs::Token::Space { raw }
        | marked_rs::Token::Code { raw, .. }
        | marked_rs::Token::Heading { raw, .. }
        | marked_rs::Token::Hr { raw }
        | marked_rs::Token::Blockquote { raw, .. }
        | marked_rs::Token::List { raw, .. }
        | marked_rs::Token::ListItem { raw, .. }
        | marked_rs::Token::Paragraph { raw, .. }
        | marked_rs::Token::Text { raw, .. }
        | marked_rs::Token::Table { raw, .. }
        | marked_rs::Token::Html { raw, .. }
        | marked_rs::Token::Def { raw, .. }
        | marked_rs::Token::Escape { raw, .. }
        | marked_rs::Token::Strong { raw, .. }
        | marked_rs::Token::Em { raw, .. }
        | marked_rs::Token::Codespan { raw, .. }
        | marked_rs::Token::Br { raw }
        | marked_rs::Token::Del { raw, .. }
        | marked_rs::Token::Link { raw, .. }
        | marked_rs::Token::Image { raw, .. } => raw,
    }
}

/// Maps to CC `claudemd.ts#stripHtmlCommentsFromTokens`: strip only
/// block-level comment tokens, preserving fenced/code-span and inline HTML.
fn strip_block_html_comments(content: &str) -> String {
    if !content.contains("<!--") {
        return content.to_string();
    }
    static COMMENT_SPAN: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?s)<!--.*?-->").unwrap());
    let mut options = marked_rs::MarkedOptions::default();
    options.gfm = false;
    let tokens = marked_rs::lexer_with_options(content, options);
    let mut output = String::new();
    for token in &tokens.tokens {
        let raw = marked_token_raw(token);
        if matches!(token, marked_rs::Token::Html { .. })
            && raw.trim_start().starts_with("<!--")
            && raw.contains("-->")
        {
            let residue = COMMENT_SPAN.replace_all(raw, "");
            if !residue.trim().is_empty() {
                output.push_str(&residue);
            }
        } else {
            output.push_str(raw);
        }
    }
    output
}

fn parsed_memory_content(raw_content: &str) -> (String, Option<Vec<String>>) {
    let parsed = crate::utils::frontmatter_parser::parse_frontmatter(raw_content);
    let globs = frontmatter_paths(&parsed.frontmatter);
    (strip_block_html_comments(&parsed.content), globs)
}

/// Maps to CC `memdir/memdir.ts#truncateEntrypointContent`.
fn truncate_memory_entrypoint(content: &str) -> String {
    const MAX_LINES: usize = 200;
    const MAX_UTF16_UNITS: usize = 25_000;
    let trimmed = content.trim();
    let lines = trimmed.split('\n').collect::<Vec<_>>();
    let line_count = lines.len();
    let unit_count = trimmed.encode_utf16().count();
    let line_truncated = line_count > MAX_LINES;
    let byte_truncated = unit_count > MAX_UTF16_UNITS;
    if !line_truncated && !byte_truncated {
        return trimmed.to_string();
    }
    let mut truncated = if line_truncated {
        lines[..MAX_LINES].join("\n")
    } else {
        trimmed.to_string()
    };
    if truncated.encode_utf16().count() > MAX_UTF16_UNITS {
        let mut units = 0usize;
        let mut last_newline = None;
        let mut boundary = 0usize;
        for (index, character) in truncated.char_indices() {
            let next = units.saturating_add(character.len_utf16());
            if next > MAX_UTF16_UNITS {
                break;
            }
            units = next;
            boundary = index + character.len_utf8();
            if character == '\n' {
                last_newline = Some(index);
            }
        }
        truncated.truncate(last_newline.filter(|index| *index > 0).unwrap_or(boundary));
    }
    let reason = if byte_truncated && !line_truncated {
        format!(
            "{} (limit: {}) — index entries are too long",
            crate::utils::format::format_file_size(unit_count as u64),
            crate::utils::format::format_file_size(MAX_UTF16_UNITS as u64)
        )
    } else if line_truncated && !byte_truncated {
        format!("{line_count} lines (limit: {MAX_LINES})")
    } else {
        format!(
            "{line_count} lines and {}",
            crate::utils::format::format_file_size(unit_count as u64)
        )
    };
    format!(
        "{truncated}\n\n> WARNING: MEMORY.md is {reason}. Only part of it was loaded. Keep index entries to one line under ~200 chars; move detail into topic files."
    )
}

pub fn claude_md_disk_projection(file: &ClaudeMdFile) -> ClaudeMdDiskProjection {
    let raw_content = std::fs::read_to_string(&file.path).unwrap_or_else(|_| file.content.clone());
    let (_, globs) = parsed_memory_content(&raw_content);
    ClaudeMdDiskProjection {
        content_differs_from_disk: file.content != raw_content,
        raw_content,
        globs,
    }
}

fn process_memory_file(
    path: &Path,
    source: ClaudeMdSource,
    include_external: bool,
    depth: usize,
    parent: Option<PathBuf>,
    cwd: &Path,
    processed: &mut HashSet<PathBuf>,
    files: &mut Vec<ClaudeMdFile>,
) {
    process_memory_file_with_kind(
        path,
        source,
        source.kind(),
        include_external,
        depth,
        parent,
        cwd,
        processed,
        files,
    );
}

#[allow(clippy::too_many_arguments)]
fn process_memory_file_with_kind(
    path: &Path,
    source: ClaudeMdSource,
    kind: ClaudeMdKind,
    include_external: bool,
    depth: usize,
    parent: Option<PathBuf>,
    cwd: &Path,
    processed: &mut HashSet<PathBuf>,
    files: &mut Vec<ClaudeMdFile>,
) {
    if depth >= MAX_INCLUDE_DEPTH
        || is_claude_md_excluded(path, kind)
        || !memory_path_has_allowed_text_extension(path)
    {
        return;
    }
    let resolved_path = normalize_for_dedup(path);
    if !processed.insert(resolved_path.clone()) {
        return;
    }
    let Ok(raw_content) = std::fs::read_to_string(path) else {
        return;
    };
    let (mut content, _) = parsed_memory_content(&raw_content);
    if matches!(kind, ClaudeMdKind::AutoMem | ClaudeMdKind::TeamMem) {
        content = truncate_memory_entrypoint(&content);
    }
    if content.trim().is_empty() {
        return;
    }
    files.push(ClaudeMdFile {
        path: path.to_path_buf(),
        source,
        kind,
        content: content.clone(),
        parent: parent.clone(),
        is_nested: false,
    });
    // Resolve includes relative to the symlink target, matching CC's early
    // `safeResolvePath(...).resolvedPath` policy.
    for include in extract_include_paths(&content, &resolved_path) {
        if include_external || is_path_inside(&include, cwd) {
            process_memory_file_with_kind(
                &include,
                source,
                kind,
                include_external,
                depth + 1,
                Some(path.to_path_buf()),
                cwd,
                processed,
                files,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct CurrentDirGuard(PathBuf);

    impl CurrentDirGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(path).expect("set current dir");
            Self(previous)
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    struct OriginalCwdGuard(PathBuf);

    impl OriginalCwdGuard {
        fn set(path: &Path) -> Self {
            let previous = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(path);
            Self(previous)
        }
    }

    impl Drop for OriginalCwdGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(&self.0);
        }
    }

    struct AllowedSettingSourcesGuard(Vec<String>);

    impl AllowedSettingSourcesGuard {
        fn capture() -> Self {
            Self(crate::bootstrap::state::get_allowed_setting_sources())
        }
    }

    impl Drop for AllowedSettingSourcesGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_setting_sources(self.0.clone());
        }
    }

    struct GlobalConfigGuard(Option<crate::utils::config::GlobalConfig>);

    impl GlobalConfigGuard {
        fn set(config: crate::utils::config::GlobalConfig) -> Self {
            Self(crate::utils::config::replace_test_global_config(Some(
                config,
            )))
        }
    }

    impl Drop for GlobalConfigGuard {
        fn drop(&mut self) {
            crate::utils::config::replace_test_global_config(self.0.take());
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cometix-claudemd-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn imported_memory_files_preserve_parent_depth_and_ignore_code_or_comments() {
        let root = temp_dir("imports");
        let parent = root.join("CLAUDE.md");
        let child = root.join("child.md");
        let ignored = root.join("ignored.md");
        std::fs::write(
            &parent,
            "@./child.md\n`@./ignored.md`\n``@./ignored.md``\n``@./ignored.md\ncontinued``\n<!-- @./ignored.md -->\n```\n@./ignored.md\n```\n",
        )
        .unwrap();
        std::fs::write(&child, "child memory").unwrap();
        std::fs::write(&ignored, "ignored memory").unwrap();
        let mut processed = HashSet::new();
        let mut files = Vec::new();
        process_memory_file(
            &parent,
            ClaudeMdSource::Project,
            true,
            0,
            None,
            &root,
            &mut processed,
            &mut files,
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].path, normalize_for_dedup(&child));
        assert_eq!(files[1].parent.as_deref(), Some(parent.as_path()));
        assert!(!files.iter().any(|file| file.path == ignored));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nested_rules_split_unconditional_and_matching_frontmatter_paths() {
        let root = temp_dir("nested-rules");
        let rules = root.join(".claude/rules");
        let target = root.join("src/main.rs");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "fn main() {}\n").unwrap();
        std::fs::write(rules.join("always.md"), "always").unwrap();
        std::fs::write(
            rules.join("rust.md"),
            "---\npaths: src/**/*.rs\n---\nrust only",
        )
        .unwrap();
        std::fs::write(rules.join("docs.md"), "---\npaths: docs/**\n---\ndocs only").unwrap();
        let mut processed = HashSet::new();

        let files = get_memory_files_for_nested_directory(&root, &target, &root, &mut processed);
        assert_eq!(
            files
                .iter()
                .map(|file| file.path.file_name().unwrap().to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            vec!["always.md", "rust.md"]
        );
        let rust = files
            .iter()
            .find(|file| file.path.ends_with("rust.md"))
            .unwrap();
        assert_eq!(rust.content, "rust only");
        let projection = claude_md_disk_projection(rust);
        assert!(projection.content_differs_from_disk);
        assert_eq!(projection.globs, Some(vec!["src/**/*.rs".to_string()]));
        assert!(projection.raw_content.starts_with("---\npaths:"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nested_instructions_follow_project_and_local_setting_source_gates() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _sources = AllowedSettingSourcesGuard::capture();
        let root = temp_dir("nested-source-gates");
        let target = root.join("src/lib.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.join(".claude/rules")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "project root").unwrap();
        std::fs::write(root.join(".claude/CLAUDE.md"), "project dot").unwrap();
        std::fs::write(root.join("CLAUDE.local.md"), "local").unwrap();
        std::fs::write(root.join(".claude/rules/project.md"), "project rule").unwrap();

        crate::bootstrap::state::set_allowed_setting_sources(Vec::new());
        assert!(
            get_memory_files_for_nested_directory(&root, &target, &root, &mut HashSet::new(),)
                .is_empty()
        );

        crate::bootstrap::state::set_allowed_setting_sources(vec!["projectSettings".to_string()]);
        let project =
            get_memory_files_for_nested_directory(&root, &target, &root, &mut HashSet::new());
        assert!(project.iter().any(|file| file.path.ends_with("CLAUDE.md")));
        assert!(project.iter().any(|file| file.path.ends_with("project.md")));
        assert!(
            !project
                .iter()
                .any(|file| file.path.ends_with("CLAUDE.local.md"))
        );

        crate::bootstrap::state::set_allowed_setting_sources(vec!["localSettings".to_string()]);
        let local =
            get_memory_files_for_nested_directory(&root, &target, &root, &mut HashSet::new());
        assert_eq!(local.len(), 1);
        assert!(local[0].path.ends_with("CLAUDE.local.md"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_rules_remain_enabled_while_user_rules_follow_source_gate() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _sources = AllowedSettingSourcesGuard::capture();
        let root = temp_dir("managed-user-rules-gates");
        let managed = root.join("managed");
        let config_home = root.join("config");
        let managed_rules = managed.join(".claude/rules");
        let user_rules = config_home.join("rules");
        std::fs::create_dir_all(&managed_rules).unwrap();
        std::fs::create_dir_all(&user_rules).unwrap();
        std::fs::write(
            managed_rules.join("managed.md"),
            "---\npaths: src/**\n---\nmanaged rule",
        )
        .unwrap();
        std::fs::write(
            user_rules.join("user.md"),
            "---\npaths: src/**\n---\nuser rule",
        )
        .unwrap();
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let target = root.join("src/lib.rs");

        crate::bootstrap::state::set_allowed_setting_sources(Vec::new());
        let managed_only =
            get_managed_and_user_conditional_rules(&target, &root, &mut HashSet::new());
        assert_eq!(managed_only.len(), 1);
        assert!(managed_only[0].path.ends_with("managed.md"));

        crate::bootstrap::state::set_allowed_setting_sources(vec!["userSettings".to_string()]);
        let with_user = get_managed_and_user_conditional_rules(&target, &root, &mut HashSet::new());
        assert!(
            with_user
                .iter()
                .any(|file| file.path.ends_with("managed.md"))
        );
        assert!(with_user.iter().any(|file| file.path.ends_with("user.md")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_external_includes_follow_the_shared_approval_gate() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _sources = AllowedSettingSourcesGuard::capture();
        let root = temp_dir("managed-external-include");
        let cwd = root.join("project");
        let managed = root.join("managed");
        let config_home = root.join("config");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        let external = root.join("external.md");
        std::fs::write(&external, "external managed include").unwrap();
        std::fs::write(managed.join("CLAUDE.md"), "@../external.md\nmanaged root").unwrap();

        {
            let _current_dir = CurrentDirGuard::set(&cwd);
            let _original_cwd = OriginalCwdGuard::set(&cwd);
            let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
            let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
            let _global_config =
                GlobalConfigGuard::set(crate::utils::config::GlobalConfig::default());
            crate::bootstrap::state::set_allowed_setting_sources(Vec::new());

            let unapproved = discover_claude_md_files_with_external_policy(true, &[], false);
            assert!(
                unapproved.iter().any(|file| {
                    normalize_for_dedup(&file.path)
                        == normalize_for_dedup(&managed.join("CLAUDE.md"))
                }),
                "managed={} files={unapproved:#?}",
                managed.join("CLAUDE.md").display(),
            );
            assert!(
                !unapproved.iter().any(|file| {
                    normalize_for_dedup(&file.path) == normalize_for_dedup(&external)
                })
            );

            let approval_probe = discover_claude_md_files_with_external_policy(true, &[], true);
            assert!(
                approval_probe.iter().any(|file| {
                    normalize_for_dedup(&file.path) == normalize_for_dedup(&external)
                })
            );
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nested_worktree_skips_parent_project_instructions() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _sources = AllowedSettingSourcesGuard::capture();
        let root = temp_dir("nested-worktree-parent-filter");
        let run_git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("run git")
        };
        assert!(run_git(&["init", "--quiet"]).success());
        assert!(run_git(&["config", "user.email", "tests@example.com"]).success());
        assert!(run_git(&["config", "user.name", "Cometix Tests"]).success());
        std::fs::write(root.join("seed.txt"), "seed").unwrap();
        assert!(run_git(&["add", "seed.txt"]).success());
        assert!(run_git(&["commit", "--quiet", "-m", "seed"]).success());
        let nested = root.join("nested-worktree");
        assert!(
            run_git(&[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                nested.to_str().unwrap(),
                "HEAD",
            ])
            .success()
        );
        std::fs::write(root.join("CLAUDE.md"), "parent instructions").unwrap();
        std::fs::write(nested.join("CLAUDE.md"), "worktree instructions").unwrap();
        let managed = root.join("managed");
        let config_home = root.join("config");
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();

        {
            let _current_dir = CurrentDirGuard::set(&nested);
            let _original_cwd = OriginalCwdGuard::set(&nested);
            let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
            let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
            let _global_config =
                GlobalConfigGuard::set(crate::utils::config::GlobalConfig::default());
            crate::bootstrap::state::set_allowed_setting_sources(vec![
                "projectSettings".to_string(),
            ]);
            let files = discover_claude_md_files_with_options(true, &[]);
            assert!(files.iter().any(|file| {
                normalize_for_dedup(&file.path) == normalize_for_dedup(&nested.join("CLAUDE.md"))
            }));
            assert!(!files.iter().any(|file| {
                normalize_for_dedup(&file.path) == normalize_for_dedup(&root.join("CLAUDE.md"))
            }));
        }

        let _ = run_git(&["worktree", "remove", "--force", nested.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn additional_directories_are_loaded_only_when_official_env_gate_is_enabled() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _unset = EnvVarGuard::unset("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD");
        let extra = temp_dir("additional-gate");
        std::fs::write(extra.join("CLAUDE.md"), "explicit memory").unwrap();

        let disabled = discover_claude_md_files_with_options(false, &[extra.clone()]);
        assert!(disabled.is_empty());

        let enabled = {
            let _enabled = EnvVarGuard::set("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD", "1");
            discover_claude_md_files_with_options(false, &[extra.clone()])
        };

        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].path, extra.join("CLAUDE.md"));
        assert_eq!(enabled[0].source, ClaudeMdSource::Project);
        assert_eq!(enabled[0].content, "explicit memory");
        let _ = std::fs::remove_dir_all(extra);
    }

    #[test]
    fn bare_add_dir_discovery_reads_project_dot_claude_and_rules_without_default_walk() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _enabled = EnvVarGuard::set("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD", "1");
        let extra = temp_dir("additional-shape");
        std::fs::write(extra.join("CLAUDE.md"), "root memory").unwrap();
        std::fs::create_dir_all(extra.join(".claude/rules")).unwrap();
        std::fs::write(extra.join(".claude/CLAUDE.md"), "dot memory").unwrap();
        std::fs::write(extra.join(".claude/rules/a.md"), "rule a").unwrap();
        std::fs::write(extra.join(".claude/rules/b.md"), "rule b").unwrap();

        let files = discover_claude_md_files_with_options(false, &[extra.clone()]);

        let normalized_extra = normalize_for_dedup(&extra);
        let relative_paths = files
            .iter()
            .map(|file| {
                normalize_for_dedup(&file.path)
                    .strip_prefix(&normalized_extra)
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            relative_paths,
            vec![
                "CLAUDE.md",
                ".claude/CLAUDE.md",
                ".claude/rules/a.md",
                ".claude/rules/b.md",
            ]
        );
        assert!(
            files
                .iter()
                .all(|file| file.source == ClaudeMdSource::Project)
        );
        let _ = std::fs::remove_dir_all(extra);
    }

    #[test]
    fn get_external_claude_md_includes_skips_user_and_in_cwd_paths() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        struct OriginalCwdRestore(PathBuf);
        impl Drop for OriginalCwdRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(&self.0);
            }
        }
        let _restore = OriginalCwdRestore(crate::bootstrap::state::get_original_cwd());
        let root = temp_dir("external-filter");
        crate::bootstrap::state::set_original_cwd(&root);

        let inside = root.join("inside.md");
        let outside = root.parent().unwrap_or(Path::new("/tmp")).join(format!(
            "cometix-claudemd-outside-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&inside, "inside").unwrap();
        std::fs::write(&outside, "outside").unwrap();
        let parent = root.join("CLAUDE.md");

        let files = vec![
            ClaudeMdFile {
                path: outside.clone(),
                source: ClaudeMdSource::Project,
                kind: ClaudeMdKind::Project,
                content: "outside".into(),
                parent: Some(parent.clone()),
                is_nested: false,
            },
            ClaudeMdFile {
                path: inside.clone(),
                source: ClaudeMdSource::Project,
                kind: ClaudeMdKind::Project,
                content: "inside".into(),
                parent: Some(parent.clone()),
                is_nested: false,
            },
            ClaudeMdFile {
                path: outside.clone(),
                source: ClaudeMdSource::UserGlobal,
                kind: ClaudeMdKind::User,
                content: "user outside".into(),
                parent: Some(parent),
                is_nested: false,
            },
        ];

        let externals = get_external_claude_md_includes(&files);
        assert_eq!(externals.len(), 1);
        assert_eq!(externals[0].path, outside.display().to_string());

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_md_excludes_match_absolute_globs_and_real_paths() {
        let root = temp_dir("excludes");
        let excluded = root.join("nested/.claude/rules/private.md");
        std::fs::create_dir_all(excluded.parent().unwrap()).unwrap();
        std::fs::write(&excluded, "private").unwrap();
        let patterns = vec![format!("{}/**/private.md", root.display())];
        assert!(claude_md_path_matches_excludes(&excluded, &patterns));
        assert!(!claude_md_path_matches_excludes(
            &root.join("nested/.claude/rules/public.md"),
            &patterns
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn memory_includes_reject_unknown_binary_extensions() {
        let root = temp_dir("include-types");
        let parent = root.join("CLAUDE.md");
        let allowed = root.join("rules.rs");
        let rejected = root.join("payload.bin");
        std::fs::write(&parent, "@./rules.rs\n@./payload.bin\n").unwrap();
        std::fs::write(&allowed, "allowed").unwrap();
        std::fs::write(&rejected, "valid UTF-8 but not an allowed include type").unwrap();
        let mut processed = HashSet::new();
        let mut files = Vec::new();
        process_memory_file(
            &parent,
            ClaudeMdSource::Project,
            true,
            0,
            None,
            &root,
            &mut processed,
            &mut files,
        );
        assert!(
            files
                .iter()
                .any(|file| file.path == normalize_for_dedup(&allowed))
        );
        assert!(!files.iter().any(|file| file.path == rejected));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_rule_entry_uses_resolved_target_path() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("symlink-rule-entry");
        let rules = root.join(".claude/rules");
        let real = root.join("real-rule.md");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(&real, "real rule").unwrap();
        symlink(&real, rules.join("alias.md")).unwrap();
        let mut processed = HashSet::new();
        let mut files = Vec::new();
        load_rules_recursively(
            &rules,
            ClaudeMdSource::Project,
            true,
            &root,
            &mut processed,
            &mut files,
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, normalize_for_dedup(&real));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_memory_resolves_includes_from_real_parent() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("symlink-include");
        let real = root.join("real");
        let linked = root.join("linked");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("CLAUDE.md"), "@./child.md\n").unwrap();
        std::fs::write(real.join("child.md"), "child").unwrap();
        symlink(real.join("CLAUDE.md"), &linked).unwrap();
        let mut processed = HashSet::new();
        let mut files = Vec::new();
        process_memory_file(
            &linked,
            ClaudeMdSource::Project,
            true,
            0,
            None,
            &root,
            &mut processed,
            &mut files,
        );
        assert!(
            files
                .iter()
                .any(|file| file.path == normalize_for_dedup(&real.join("child.md")))
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
