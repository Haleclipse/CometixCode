//! Filesystem permission policy shared by Read/Write/Edit tools.
//!
//! Maps to: CC `utils/permissions/filesystem.ts`.
//! Matcher, safety, internal-path, working-directory, and write-policy
//! ownership lives here; `path_validation.rs` imports these primitives just
//! like the CC source instead of maintaining a compatibility implementation.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionBehavior, PermissionMode, PermissionRule, PermissionRuleSource, PermissionUpdate,
    PermissionUpdateDestination,
};
use crate::utils::feature_flags::{FeatureFlag, feature_enabled};
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use crate::utils::process_env::JsTruthy;
use crate::utils::settings::constants::SettingSource;
use ignore::Match;
use ignore::gitignore::GitignoreBuilder;
use indexmap::IndexMap;
use regex::Regex;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

pub use super::path_validation::{
    FileOperationType as FilesystemOperationType, PathCheckResult, is_path_allowed,
};

const DANGEROUS_FILES: &[&str] = &[
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
    ".claude.json",
];
const DANGEROUS_DIRECTORIES: &[&str] = &[".git", ".vscode", ".idea", ".claude"];

static DOS_DEVICE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$").expect("valid dos device regex")
});
static THREE_DOTS_COMPONENT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|/|\\)\.{3,}(/|\\|$)").expect("valid dots regex"));
static SHORT_NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~\d").expect("valid short name regex"));
static TRAILING_DOT_SPACE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.\s]+$").expect("valid trailing regex"));

/// Maps to CC `PathSafetyForAutoEdit` in `permissions/filesystem.ts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSafetyForAutoEdit {
    Safe,
    Unsafe {
        message: String,
        classifier_approvable: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePermissionType {
    Read,
    Edit,
}

pub type FilesystemPermissionType = FilePermissionType;
pub type PathSafety = PathSafetyForAutoEdit;

fn normalize_permission_path_buf(path: impl AsRef<Path>) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.as_ref().components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn normalize_permission_path(path: &str) -> String {
    normalize_permission_path_buf(Path::new(path))
        .display()
        .to_string()
}

fn expand_permission_path(path: &str) -> String {
    let expanded = if path == "~" {
        crate::utils::node_os::homedir().display().to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        crate::utils::node_os::homedir()
            .join(rest)
            .display()
            .to_string()
    } else {
        path.to_string()
    };
    let path_buf = PathBuf::from(&expanded);
    if path_buf.is_absolute() {
        normalize_permission_path(&expanded)
    } else {
        std::env::current_dir()
            .map(|cwd| normalize_permission_path(&cwd.join(path_buf).display().to_string()))
            .unwrap_or_else(|_| normalize_permission_path(&expanded))
    }
}

fn collapse_to_forward_slashes(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut last_was_separator = false;
    for character in path.chars() {
        if matches!(character, '/' | '\\') {
            if !last_was_separator {
                result.push('/');
                last_was_separator = true;
            }
        } else {
            result.push(character);
            last_was_separator = false;
        }
    }
    result
}

fn normalize_for_permission_comparison(path: &str) -> String {
    let path = collapse_to_forward_slashes(&normalize_permission_path(path));
    // CC filesystem.ts:716-721 anchors these aliases at the path root.
    // /private/tmp also matches without a trailing slash; /private/var does not.
    let path = if let Some(suffix) = path.strip_prefix("/private/var/") {
        format!("/var/{suffix}")
    } else if path == "/private/tmp" {
        "/tmp".to_string()
    } else if let Some(suffix) = path.strip_prefix("/private/tmp/") {
        format!("/tmp/{suffix}")
    } else {
        path
    };
    path.to_lowercase()
}

fn normalize_for_rule_relative(path: &str) -> String {
    collapse_to_forward_slashes(&normalize_permission_path(path))
        .replace("/private/var/", "/var/")
        .replace("/private/tmp/", "/tmp/")
}

fn relative_path_string(root: &str, path: &str) -> String {
    let root = normalize_for_rule_relative(root);
    let path = normalize_for_rule_relative(path);
    if path == root {
        return String::new();
    }
    let prefix = if root.ends_with('/') {
        root
    } else {
        format!("{root}/")
    };
    path.strip_prefix(&prefix)
        .map(str::to_string)
        .unwrap_or_else(|| "../".to_string())
}

/// Maps to: CC `rootPathForSource(...)`.
fn root_path_for_source(source: PermissionRuleSource) -> String {
    match source {
        PermissionRuleSource::CliArg
        | PermissionRuleSource::Command
        | PermissionRuleSource::Session => crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string(),
        PermissionRuleSource::UserSettings => {
            crate::utils::settings::get_settings_root_path_for_source(SettingSource::User)
                .display()
                .to_string()
        }
        PermissionRuleSource::ProjectSettings => {
            crate::utils::settings::get_settings_root_path_for_source(SettingSource::Project)
                .display()
                .to_string()
        }
        PermissionRuleSource::LocalSettings => {
            crate::utils::settings::get_settings_root_path_for_source(SettingSource::Local)
                .display()
                .to_string()
        }
        PermissionRuleSource::FlagSettings => {
            crate::utils::settings::get_settings_root_path_for_source(SettingSource::Flag)
                .display()
                .to_string()
        }
        PermissionRuleSource::PolicySettings => {
            crate::utils::settings::get_settings_root_path_for_source(SettingSource::Policy)
                .display()
                .to_string()
        }
    }
}

fn pattern_with_root(pattern: &str, source: PermissionRuleSource) -> (String, Option<String>) {
    if pattern.starts_with("//") {
        let without_double_slash = &pattern[1..];
        #[cfg(windows)]
        if without_double_slash.as_bytes().get(2) == Some(&b'/')
            && without_double_slash
                .as_bytes()
                .get(1)
                .is_some_and(u8::is_ascii_alphabetic)
        {
            let drive = (without_double_slash.as_bytes()[1] as char).to_ascii_uppercase();
            return (
                without_double_slash[3..].to_string(),
                Some(format!("{drive}:\\")),
            );
        }
        return (without_double_slash.to_string(), Some("/".to_string()));
    }
    if let Some(rest) = pattern.strip_prefix('~') {
        if rest.starts_with('/') {
            return (
                rest.to_string(),
                Some(
                    crate::utils::node_os::homedir()
                        .to_string_lossy()
                        .nfc()
                        .collect::<String>(),
                ),
            );
        }
    }
    if pattern.starts_with('/') {
        return (pattern.to_string(), Some(root_path_for_source(source)));
    }
    (
        pattern.strip_prefix("./").unwrap_or(pattern).to_string(),
        None,
    )
}

/// Maps to: CC `getPatternsByRoot(...)`.
fn get_patterns_by_root(
    context: &ToolPermissionContext,
    tool_type: FilePermissionType,
    behavior: PermissionBehavior,
) -> IndexMap<Option<String>, IndexMap<String, PermissionRule>> {
    let tool_name = match tool_type {
        FilePermissionType::Read => crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME,
        FilePermissionType::Edit => "Edit",
    };
    let mut patterns_by_root = IndexMap::new();
    for (pattern, rule) in
        super::permissions::get_rule_by_contents_for_tool_name(context, tool_name, behavior)
    {
        let (relative_pattern, root) = pattern_with_root(&pattern, rule.source);
        patterns_by_root
            .entry(root)
            .or_insert_with(IndexMap::new)
            .insert(relative_pattern, rule);
    }
    patterns_by_root
}

/// Maps to CC `getFileReadIgnorePatterns(...)`.
pub fn get_file_read_ignore_patterns(
    context: &ToolPermissionContext,
) -> IndexMap<Option<String>, Vec<String>> {
    get_patterns_by_root(context, FilePermissionType::Read, PermissionBehavior::Deny)
        .into_iter()
        .map(|(root, patterns)| (root, patterns.into_keys().collect()))
        .collect()
}

fn posix_normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components
                    .last()
                    .is_some_and(|component| *component != "..")
                {
                    components.pop();
                } else if !absolute {
                    components.push("..");
                }
            }
            component => components.push(component),
        }
    }
    let joined = components.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

fn posix_join(left: &str, right: &str) -> String {
    posix_normalize(&format!(
        "{}/{}",
        left.trim_end_matches('/'),
        right.trim_start_matches('/')
    ))
}

fn posix_relative(from: &str, to: &str) -> String {
    let from = posix_normalize(from);
    let to = posix_normalize(to);
    let from_components = from
        .trim_start_matches('/')
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    let to_components = to
        .trim_start_matches('/')
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    let shared = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();
    std::iter::repeat_n("..", from_components.len().saturating_sub(shared))
        .chain(to_components[shared..].iter().copied())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_pattern_to_path(pattern_root: &str, pattern: &str, root_path: &str) -> Option<String> {
    let full_pattern = posix_join(pattern_root, pattern);
    let normalized = if pattern_root == root_path {
        pattern.to_string()
    } else if full_pattern.starts_with(&format!("{root_path}/")) {
        full_pattern[root_path.len()..].to_string()
    } else {
        let relative_path = posix_relative(root_path, pattern_root);
        if relative_path.is_empty() || relative_path == ".." || relative_path.starts_with("../") {
            return None;
        }
        posix_join(&relative_path, pattern)
    };
    Some(posix_join("/", &normalized))
}

/// Maps to CC `normalizePatternsToPath(...)`.
pub fn normalize_patterns_to_path(
    patterns_by_root: &IndexMap<Option<String>, Vec<String>>,
    root: &str,
) -> Vec<String> {
    let mut result = indexmap::IndexSet::new();
    if let Some(patterns) = patterns_by_root.get(&None) {
        result.extend(patterns.iter().cloned());
    }
    for (pattern_root, patterns) in patterns_by_root {
        let Some(pattern_root) = pattern_root else {
            continue;
        };
        for pattern in patterns {
            if let Some(pattern) = normalize_pattern_to_path(pattern_root, pattern, root) {
                result.insert(pattern);
            }
        }
    }
    result.into_iter().collect()
}

/// Maps to CC `matchingRuleForInput(...)`, including npm `ignore` semantics.
pub(crate) fn matching_rule_for_input(
    path: &str,
    context: &ToolPermissionContext,
    tool_type: FilePermissionType,
    behavior: PermissionBehavior,
    cwd: &Path,
) -> Option<PermissionRule> {
    let absolute_path = expand_permission_path(path);
    for (root, pattern_map) in get_patterns_by_root(context, tool_type, behavior) {
        let mut builder = GitignoreBuilder::new(".");
        for pattern in pattern_map.keys() {
            let adjusted = pattern.strip_suffix("/**").unwrap_or(pattern);
            if builder.add_line(None, adjusted).is_err() {
                continue;
            }
        }
        let Ok(matcher) = builder.build() else {
            continue;
        };
        let match_root = root.unwrap_or_else(|| cwd.display().to_string());
        let relative = relative_path_string(&match_root, &absolute_path);
        if relative.is_empty() || relative == ".." || relative.starts_with("../") {
            continue;
        }
        let Match::Ignore(glob) = matcher.matched_path_or_any_parents(&relative, false) else {
            continue;
        };
        let original = glob.original();
        if let Some(rule) = pattern_map.get(&format!("{original}/**")) {
            return Some(rule.clone());
        }
        if let Some(rule) = pattern_map.get(original) {
            return Some(rule.clone());
        }
    }
    None
}

/// Maps to CC `pathInWorkingPath(path, workingPath)`.
pub fn path_in_working_path(path: &str, working_path: &str) -> bool {
    let normalized_path = normalize_for_permission_comparison(&expand_permission_path(path));
    let normalized_working_path =
        normalize_for_permission_comparison(&expand_permission_path(working_path));
    normalized_path == normalized_working_path
        // CC filesystem.ts:731-742 relativePath treats filesystem roots as
        // ancestors too. Avoid adding a second slash to '/' or a drive root.
        || normalized_path.starts_with(&format!("{}/", normalized_working_path.trim_end_matches('/')))
}

/// Maps to CC `pathInAllowedWorkingPath(...)`.
pub fn path_in_allowed_working_path(
    path: &str,
    context: &ToolPermissionContext,
    precomputed_paths_to_check: Option<&[String]>,
) -> bool {
    let owned_paths;
    let paths = if let Some(paths) = precomputed_paths_to_check {
        paths
    } else {
        owned_paths = paths_to_check(path, &host_cwd());
        &owned_paths
    };
    let working_paths = all_working_directories(context)
        .into_iter()
        .flat_map(|working_directory| paths_to_check(&working_directory, &host_cwd()))
        .collect::<Vec<_>>();
    paths.iter().all(|candidate| {
        working_paths
            .iter()
            .any(|working_path| path_in_working_path(candidate, working_path))
    })
}

fn contains_vulnerable_unc_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

fn has_suspicious_windows_path_pattern(path: &str) -> bool {
    use crate::utils::platform::{Platform, get_platform};
    if matches!(get_platform(), Platform::Windows | Platform::Wsl)
        && path.get(2..).is_some_and(|tail| tail.contains(':'))
    {
        return true;
    }
    SHORT_NAME_REGEX.is_match(path)
        || path.starts_with("\\\\?\\")
        || path.starts_with("\\\\.\\")
        || path.starts_with("//?/")
        || path.starts_with("//./")
        || TRAILING_DOT_SPACE_REGEX.is_match(path)
        || DOS_DEVICE_REGEX.is_match(path)
        || THREE_DOTS_COMPONENT_REGEX.is_match(path)
        || contains_vulnerable_unc_path(path)
}

/// Maps to CC `isClaudeSettingsPath(filePath)`.
pub fn is_claude_settings_path(file_path: &str, cwd: &Path) -> bool {
    let expanded = crate::utils::path::expand_path(file_path, Some(cwd))
        .unwrap_or_else(|_| PathBuf::from(file_path));
    let normalized = normalize_for_permission_comparison(&expanded.display().to_string());
    if normalized.ends_with("/.claude/settings.json")
        || normalized.ends_with("/.claude/settings.local.json")
    {
        return true;
    }
    crate::utils::settings::constants::get_enabled_setting_sources()
        .into_iter()
        .filter_map(crate::utils::settings::get_settings_file_path_for_source)
        .map(|path| normalize_for_permission_comparison(&path.display().to_string()))
        .any(|path| path == normalized)
}

fn is_claude_config_file_path(file_path: &str) -> bool {
    let cwd = crate::bootstrap::state::get_original_cwd();
    if is_claude_settings_path(file_path, &cwd) {
        return true;
    }
    ["commands", "agents", "skills"].iter().any(|directory| {
        path_in_working_path(
            file_path,
            &cwd.join(".claude").join(directory).display().to_string(),
        )
    })
}

fn is_dangerous_file_path_to_auto_edit(path: &str) -> bool {
    // Maps to: CC filesystem.ts:435-489. Worktree storage is structural;
    // nested sensitive directories remain subject to the same check.
    if path.starts_with("\\\\") || path.starts_with("//") {
        return true;
    }
    let expanded = normalize_for_permission_comparison(&expand_permission_path(path));
    let segments: Vec<_> = expanded.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        if DANGEROUS_DIRECTORIES.contains(segment) {
            if *segment == ".claude" && segments.get(index + 1) == Some(&"worktrees") {
                continue;
            }
            return true;
        }
    }
    segments
        .last()
        .is_some_and(|name| DANGEROUS_FILES.contains(name))
}

/// Maps to CC `checkPathSafetyForAutoEdit(...)`.
pub fn check_path_safety_for_auto_edit(
    path: &str,
    precomputed_paths_to_check: Option<&[String]>,
) -> PathSafetyForAutoEdit {
    let owned_paths;
    let paths = if let Some(paths) = precomputed_paths_to_check {
        paths
    } else {
        owned_paths = paths_to_check(path, &host_cwd());
        &owned_paths
    };
    if paths
        .iter()
        .any(|candidate| has_suspicious_windows_path_pattern(candidate))
    {
        return PathSafetyForAutoEdit::Unsafe {
            message: format!(
                "Claude requested permissions to write to {path}, which contains a suspicious Windows path pattern that requires manual approval."
            ),
            classifier_approvable: false,
        };
    }
    if paths
        .iter()
        .any(|candidate| is_claude_config_file_path(candidate))
    {
        return PathSafetyForAutoEdit::Unsafe {
            message: format!(
                "Claude requested permissions to write to {path}, but you haven't granted it yet."
            ),
            classifier_approvable: true,
        };
    }
    if paths
        .iter()
        .any(|candidate| is_dangerous_file_path_to_auto_edit(candidate))
    {
        return PathSafetyForAutoEdit::Unsafe {
            message: format!(
                "Claude requested permissions to edit {path} which is a sensitive file."
            ),
            classifier_approvable: true,
        };
    }
    PathSafetyForAutoEdit::Safe
}

/// Maps to CC `checkEditableInternalPath(...)`.
pub(crate) fn check_editable_internal_path(
    path: &str,
    cwd: &Path,
) -> Option<PermissionDecisionReason> {
    let normalized = normalize_permission_path_buf(Path::new(path));
    if is_session_plan_file(&normalized) {
        return Some(PermissionDecisionReason::Other {
            reason: "Plan files for current session are allowed for writing".to_string(),
        });
    }
    if is_scratchpad_path(&normalized) {
        return Some(PermissionDecisionReason::Other {
            reason: "Scratchpad files for current session are allowed for writing".to_string(),
        });
    }
    // DEVIATION(BUILD): CC gates template-job carve-outs with its TEMPLATES
    // bundle feature. Cometix projects internal bundle features onto the
    // compile-time `anthropic_internal` Cargo feature so the env key and
    // policy are absent from external binaries.
    #[cfg(feature = "anthropic_internal")]
    if let Some(job_dir) = crate::utils::process_env::var_os("CLAUDE_JOB_DIR")
        .as_deref()
        .truthy()
        .map(PathBuf::from)
    {
        let jobs_root = crate::utils::env_utils::get_claude_config_home_dir().join("jobs");
        let job_forms = paths_to_check(&job_dir.display().to_string(), cwd);
        let jobs_root_forms = paths_to_check(&jobs_root.display().to_string(), cwd);
        let job_dir_is_trusted = job_forms.iter().all(|job_form| {
            jobs_root_forms.iter().any(|root_form| {
                normalize_for_permission_comparison(job_form)
                    != normalize_for_permission_comparison(root_form)
                    && path_in_working_path(job_form, root_form)
            })
        });
        if job_dir_is_trusted {
            let target_forms = paths_to_check(&normalized.display().to_string(), cwd);
            if target_forms.iter().all(|target| {
                job_forms
                    .iter()
                    .any(|job| path_in_working_path(target, job))
            }) {
                return Some(PermissionDecisionReason::Other {
                    reason: "Job directory files for current job are allowed for writing"
                        .to_string(),
                });
            }
        }
    }
    if crate::tools::agent_tool::agent_memory::is_agent_memory_path(&normalized, cwd) {
        return Some(PermissionDecisionReason::Other {
            reason: "Agent memory files are allowed for writing".to_string(),
        });
    }
    if !crate::memdir::paths::has_auto_mem_path_override()
        && crate::memdir::paths::is_auto_mem_path(&normalized)
    {
        return Some(PermissionDecisionReason::Other {
            reason: "auto memory files are allowed for writing".to_string(),
        });
    }
    if normalize_for_permission_comparison(&normalized.display().to_string())
        == normalize_for_permission_comparison(
            &crate::bootstrap::state::get_original_cwd()
                .join(".claude")
                .join("launch.json")
                .display()
                .to_string(),
        )
    {
        return Some(PermissionDecisionReason::Other {
            reason: "Preview launch config is allowed for writing".to_string(),
        });
    }
    None
}

/// Maps to CC `checkReadableInternalPath(...)` (:1611-1777).
/// Callers pass the async-context cwd; this does not consult the process cwd.
pub(crate) fn check_readable_internal_path(
    path: &str,
    cwd: &Path,
) -> Option<PermissionDecisionReason> {
    let normalized = normalize_permission_path_buf(Path::new(path));
    let session_memory_dir = normalize_permission_path_buf(
        crate::utils::session_storage::get_project_dir(&cwd.display().to_string())
            .join(crate::bootstrap::state::get_session_id())
            .join("session-memory"),
    );
    if normalized == session_memory_dir || normalized.starts_with(&session_memory_dir) {
        return Some(PermissionDecisionReason::Other {
            reason: "Session memory files are allowed for reading".to_string(),
        });
    }
    let project_dir = normalize_permission_path_buf(
        crate::utils::session_storage::get_project_dir(&cwd.display().to_string()),
    );
    if normalized == project_dir || normalized.starts_with(&project_dir) {
        return Some(PermissionDecisionReason::Other {
            reason: "Project directory files are allowed for reading".to_string(),
        });
    }
    if is_session_plan_file(&normalized) {
        return Some(PermissionDecisionReason::Other {
            reason: "Plan files for current session are allowed for reading".to_string(),
        });
    }
    let tool_results_dir =
        normalize_permission_path_buf(crate::utils::tool_result_storage::get_tool_results_dir());
    if normalized == tool_results_dir || normalized.starts_with(&tool_results_dir) {
        return Some(PermissionDecisionReason::Other {
            reason: "Tool result files are allowed for reading".to_string(),
        });
    }
    if is_scratchpad_path(&normalized) {
        return Some(PermissionDecisionReason::Other {
            reason: "Scratchpad files for current session are allowed for reading".to_string(),
        });
    }
    if normalized.starts_with(normalize_permission_path_buf(get_project_temp_dir())) {
        return Some(PermissionDecisionReason::Other {
            reason: "Project temp directory files are allowed for reading".to_string(),
        });
    }
    if crate::tools::agent_tool::agent_memory::is_agent_memory_path(&normalized, cwd) {
        return Some(PermissionDecisionReason::Other {
            reason: "Agent memory files are allowed for reading".to_string(),
        });
    }
    if crate::memdir::paths::is_auto_mem_path(&normalized) {
        return Some(PermissionDecisionReason::Other {
            reason: "auto memory files are allowed for reading".to_string(),
        });
    }

    let config_home = crate::utils::env_utils::get_claude_config_home_dir();
    let tasks_dir = normalize_permission_path_buf(config_home.join("tasks"));
    if normalized == tasks_dir || normalized.starts_with(&tasks_dir) {
        return Some(PermissionDecisionReason::Other {
            reason: "Task files are allowed for reading".to_string(),
        });
    }
    let teams_dir = normalize_permission_path_buf(config_home.join("teams"));
    if normalized == teams_dir || normalized.starts_with(&teams_dir) {
        return Some(PermissionDecisionReason::Other {
            reason: "Team files are allowed for reading".to_string(),
        });
    }
    let bundled_skills_root = normalize_permission_path_buf(get_bundled_skills_root());
    if normalized.starts_with(&bundled_skills_root) && normalized != bundled_skills_root {
        return Some(PermissionDecisionReason::Other {
            reason: "Bundled skill reference files are allowed for reading".to_string(),
        });
    }
    None
}

/// Maps to CC `isScratchpadEnabled()`.
pub fn is_scratchpad_enabled() -> bool {
    feature_enabled(FeatureFlag::Scratchpad)
}

/// Maps to CC `getClaudeTempDirName()` (`filesystem.ts:307-315`).
pub fn get_claude_temp_dir_name() -> String {
    use crate::utils::platform::{Platform, get_platform};
    if get_platform() == Platform::Windows {
        return "claude".to_string();
    }
    // Use UID to create per-user directories, preventing permission conflicts
    // when multiple users share the same /tmp directory
    format!("claude-{}", current_uid())
}

/// Cometix forward-port of CC 2.1.285's check of its per-uid temp root
/// (user, 10-03), used by `/copy` and the sandbox temp directory. Opens the
/// directory without following a symlink and requires the user's own uid,
/// fixing a mode other than 0700. A directory owned by someone else is
/// accepted, with a warning, when running as root in a container
/// (`CLAUDE_CODE_CONTAINER_ID`, trimmed as 2.1.285's environment reads are).
#[cfg(unix)]
pub(crate) fn verify_owned_temp_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    const HINT: &str =
        "Set CLAUDE_CODE_TMPDIR to a directory you control, or ask an administrator to remove it.";
    let uid = current_uid();
    let shown = dir.display();
    // `e.replace(/[\/]+$/, "") || e`
    let bytes = dir.as_os_str().as_bytes();
    let path = match bytes.iter().rposition(|byte| *byte != b'/') {
        Some(last) => Path::new(std::ffi::OsStr::from_bytes(&bytes[..=last])),
        None => dir,
    };
    let directory = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(directory) => directory,
        Err(error) => {
            return Err(match error.raw_os_error() {
                Some(libc::ELOOP | libc::ENOTDIR) => std::io::Error::other(format!(
                    "Temp directory {shown} is not a directory (may be an attacker-planted symlink). Refusing to use it. {HINT}"
                )),
                Some(libc::EACCES) => match std::fs::symlink_metadata(path)
                    .ok()
                    .map(|metadata| metadata.uid())
                    .filter(|owner| *owner != uid)
                {
                    Some(owner) => std::io::Error::other(format!(
                        "Temp directory {shown} is owned by uid {owner}, expected {uid}. Refusing to use it \u{2014} another user may have pre-created it. {HINT}"
                    )),
                    None => std::io::Error::other(format!(
                        "Temp directory {shown} is not readable (its mode may have been altered, or a path component denies search). Refusing to use it \u{2014} restore its permissions (chmod 0700) or remove it. {HINT}"
                    )),
                },
                _ => error,
            });
        }
    };
    let metadata = directory.metadata()?;
    if metadata.uid() != uid {
        if uid == 0
            && crate::utils::process_env::var("CLAUDE_CODE_CONTAINER_ID")
                .map(|value| value.trim().to_owned())
                .truthy()
                .is_some()
        {
            // `logForDiagnosticsNoPII('warn', 'tempdir_owner_mismatch', …)`;
            // `diagLogs.ts` is not ported, so this goes to the debug log.
            crate::utils::debug::log_for_debugging(&format!(
                "tempdir_owner_mismatch: observed uid {}",
                metadata.uid()
            ));
            return Ok(());
        }
        return Err(std::io::Error::other(format!(
            "Temp directory {shown} is owned by uid {}, expected {uid}. Refusing to use it \u{2014} another user may have pre-created it. {HINT}",
            metadata.uid()
        )));
    }
    if metadata.mode() & 0o777 != 0o700 {
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Maps to CC `getClaudeTempDir()` (`filesystem.ts:331-346`), memoized: the
/// per-user temp directory under `CLAUDE_CODE_TMPDIR || (Windows ? tmpdir() :
/// '/tmp')` with the base's symlinks resolved (`/tmp` is `/private/tmp` on
/// macOS), with a trailing separator.
///
/// Test builds compute it on every call: under `cargo test` a directory fixed
/// by the first test would take every later test's writes, into the real
/// per-user temp directory.
pub fn get_claude_temp_dir() -> PathBuf {
    #[cfg(not(test))]
    {
        static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        DIR.get_or_init(claude_temp_dir).clone()
    }
    #[cfg(test)]
    claude_temp_dir()
}

/// The body of [`get_claude_temp_dir`].
fn claude_temp_dir() -> PathBuf {
    use crate::utils::platform::{Platform, get_platform};
    let base_tmp_dir = crate::utils::process_env::var_os("CLAUDE_CODE_TMPDIR")
        .as_deref()
        .truthy()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if get_platform() == Platform::Windows {
                crate::utils::node_os::tmpdir()
            } else {
                PathBuf::from("/tmp")
            }
        });
    // Resolve symlinks in the base temp directory (e.g., /tmp -> /private/tmp
    // on macOS) so the path matches resolved paths in permission checks.
    let resolved = crate::utils::fs_operations::get_fs_implementation()
        .realpath_sync(&base_tmp_dir)
        .unwrap_or(base_tmp_dir);
    let mut dir = crate::utils::fs_operations::native::join_path(
        &resolved,
        Path::new(&get_claude_temp_dir_name()),
    )
    .into_os_string();
    dir.push(std::path::MAIN_SEPARATOR_STR);
    PathBuf::from(dir)
}

/// Maps to CC `getBundledSkillsRoot()` (:365-369). The process-random nonce
/// is the load-bearing defense against pre-created shared-temp symlink trees.
pub fn get_bundled_skills_root() -> PathBuf {
    static ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
        get_claude_temp_dir()
            .join("bundled-skills")
            .join(env!("CARGO_PKG_VERSION"))
            .join(uuid::Uuid::new_v4().simple().to_string())
    });
    ROOT.clone()
}

/// Maps to CC `getProjectTempDir()` (`filesystem.ts:376-378`):
/// `join(getClaudeTempDir(), sanitizePath(getOriginalCwd())) + sep`.
pub fn get_project_temp_dir() -> PathBuf {
    let mut dir = crate::utils::fs_operations::native::join_path(
        &get_claude_temp_dir(),
        Path::new(&crate::utils::session_storage::sanitize_path(
            &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
        )),
    )
    .into_os_string();
    dir.push(std::path::MAIN_SEPARATOR_STR);
    PathBuf::from(dir)
}

/// Maps to CC `getScratchpadDir()`.
pub fn get_scratchpad_dir() -> PathBuf {
    get_project_temp_dir()
        .join(crate::bootstrap::state::get_session_id())
        .join("scratchpad")
}

/// Maps to CC `ensureScratchpadDir()`.
pub fn ensure_scratchpad_dir() -> anyhow::Result<PathBuf> {
    if !is_scratchpad_enabled() {
        anyhow::bail!("Scratchpad directory feature is not enabled");
    }
    let scratchpad_dir = get_scratchpad_dir();
    futures::executor::block_on(
        crate::utils::fs_operations::get_fs_implementation().mkdir(&scratchpad_dir, Some(0o700)),
    )?;
    Ok(scratchpad_dir)
}

/// Maps to CC `isScratchpadPath(...)`.
pub fn is_scratchpad_path(path: impl AsRef<Path>) -> bool {
    if !is_scratchpad_enabled() {
        return false;
    }
    let scratchpad_dir = normalize_internal_path(get_scratchpad_dir());
    let normalized_path = normalize_internal_path(path.as_ref());
    normalized_path == scratchpad_dir || normalized_path.starts_with(&scratchpad_dir)
}

/// Maps to CC `allWorkingDirectories(context)`.
pub fn all_working_directories(context: &ToolPermissionContext) -> Vec<String> {
    let mut directories = vec![
        crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string(),
    ];
    for directory in context.additional_working_directories.keys() {
        if !directories.contains(directory) {
            directories.push(directory.clone());
        }
    }
    directories
}

/// Maps to CC `getSessionMemoryDir()`.
pub fn get_session_memory_dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::utils::session_storage::get_project_dir(&cwd.display().to_string())
        .join(crate::bootstrap::state::get_session_id())
        .join("session-memory")
}

/// Maps to CC `getSessionMemoryPath()`.
pub fn get_session_memory_path() -> PathBuf {
    get_session_memory_dir().join("summary.md")
}

/// Maps to CC `isSessionMemoryPath(...)`.
pub fn is_session_memory_path(path: impl AsRef<Path>) -> bool {
    let directory = normalize_internal_path(get_session_memory_dir());
    let path = normalize_internal_path(path.as_ref());
    path == directory || path.starts_with(directory)
}

/// Maps to CC `isProjectDirPath(...)`.
pub fn is_project_dir_path(path: impl AsRef<Path>) -> bool {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let directory = normalize_internal_path(crate::utils::session_storage::get_project_dir(
        &cwd.display().to_string(),
    ));
    let path = normalize_internal_path(path.as_ref());
    path == directory || path.starts_with(directory)
}

fn normalize_internal_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().canonicalize().unwrap_or_else(|_| {
        let mut normalized = PathBuf::new();
        for component in path.as_ref().components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        normalized
    })
}

/// `process.getuid?.() ?? 0`.
#[cfg(unix)]
pub(crate) fn current_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: `getuid` has no preconditions.
    unsafe { getuid() }
}

#[cfg(not(unix))]
pub(crate) fn current_uid() -> u32 {
    0
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn host_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn paths_to_check(path: &str, cwd: &Path) -> Vec<String> {
    let expanded = crate::utils::path::expand_path(path, Some(cwd))
        .unwrap_or_else(|_| std::path::PathBuf::from(path));
    crate::utils::fs_operations::get_paths_for_permission_check(&expanded)
        .into_iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn claude_folder_session_allow_rule(
    path: &str,
    context: &ToolPermissionContext,
    cwd: &Path,
) -> Option<PermissionRule> {
    let mut session_context = context.clone();
    session_context
        .always_allow_rules
        .retain(|source, _| *source == PermissionRuleSource::Session);
    let rule = matching_rule_for_input(
        path,
        &session_context,
        FilePermissionType::Edit,
        PermissionBehavior::Allow,
        cwd,
    )?;
    let content = rule.rule_value.rule_content.as_deref()?;
    let project_prefix = "/.claude/**".trim_end_matches("**");
    let global_prefix = "~/.claude/**".trim_end_matches("**");
    ((content.starts_with(project_prefix) || content.starts_with(global_prefix))
        && !content.contains("..")
        && content.ends_with("/**"))
    .then_some(rule)
}

/// Maps to CC `getClaudeSkillScope(filePath)`.
pub fn get_claude_skill_scope(file_path: &str) -> Option<(String, String)> {
    let absolute = crate::utils::path::expand_path(file_path, None).ok()?;
    let absolute = absolute.display().to_string().replace('\\', "/");
    let bases = [
        (
            crate::bootstrap::state::get_original_cwd()
                .join(".claude")
                .join("skills"),
            "/.claude/skills/",
        ),
        (
            crate::utils::node_os::homedir()
                .join(".claude")
                .join("skills"),
            "~/.claude/skills/",
        ),
    ];
    for (base, prefix) in bases {
        let base = crate::utils::path::expand_path(&base.display().to_string(), None)
            .ok()?
            .display()
            .to_string()
            .replace('\\', "/");
        let expected_prefix = format!("{}/", base.trim_end_matches('/'));
        if !absolute
            .to_lowercase()
            .starts_with(&expected_prefix.to_lowercase())
        {
            continue;
        }
        let rest = &absolute[expected_prefix.len()..];
        let (skill_name, _) = rest.split_once('/')?;
        if skill_name.is_empty()
            || skill_name == "."
            || skill_name.contains("..")
            || skill_name
                .chars()
                .any(|character| matches!(character, '*' | '?' | '[' | ']'))
        {
            return None;
        }
        return Some((skill_name.to_string(), format!("{prefix}{skill_name}/**")));
    }
    None
}

/// Maps to: CC `utils/permissions/filesystem.ts:1414-1474#generateSuggestions`.
pub fn generate_suggestions(
    file_path: &str,
    operation_type: FilesystemOperationType,
    context: &ToolPermissionContext,
    precomputed_paths_to_check: Option<&[String]>,
) -> Vec<PermissionUpdate> {
    let is_outside_working_dir =
        !path_in_allowed_working_path(file_path, context, precomputed_paths_to_check);

    if operation_type == FilesystemOperationType::Read && is_outside_working_dir {
        let directory = crate::utils::path::get_directory_for_path(file_path);
        return paths_to_check(&directory, &host_cwd())
            .into_iter()
            .filter_map(|directory| {
                crate::utils::permissions::permission_update::create_read_rule_suggestion(
                    &directory,
                    PermissionUpdateDestination::Session,
                )
            })
            .collect();
    }

    let should_suggest_accept_edits =
        matches!(context.mode, PermissionMode::Default | PermissionMode::Plan);
    if matches!(
        operation_type,
        FilesystemOperationType::Write | FilesystemOperationType::Create
    ) {
        let mut updates = if should_suggest_accept_edits {
            vec![PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            }]
        } else {
            Vec::new()
        };
        if is_outside_working_dir {
            let directory = crate::utils::path::get_directory_for_path(file_path);
            updates.push(PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: paths_to_check(&directory, &host_cwd()),
            });
        }
        return updates;
    }

    if should_suggest_accept_edits {
        vec![PermissionUpdate::SetMode {
            destination: PermissionUpdateDestination::Session,
            mode: PermissionMode::AcceptEdits,
        }]
    } else {
        Vec::new()
    }
}

/// Maps to: CC `utils/permissions/filesystem.ts#checkReadPermissionForTool`.
///
/// Rust adaptation: callers pass the path already derived from the Tool's
/// `getPath(input)` together with the exact async-context cwd.
pub fn check_read_permission_for_tool(
    path: &str,
    input: &serde_json::Value,
    context: &ToolPermissionContext,
    cwd: &Path,
) -> PermissionResult {
    // CC filesystem.ts:667-674: only original cwd and explicit additional
    // working directories authorize reads. Async-context cwd resolves paths;
    // carrying that value does not grant access to an additional root.
    let expanded =
        crate::utils::path::expand_path(path, Some(cwd)).unwrap_or_else(|_| PathBuf::from(path));
    let expanded_path = expanded.display().to_string();
    let paths_to_check = paths_to_check(&expanded_path, cwd);

    for candidate in &paths_to_check {
        if contains_vulnerable_unc_path(candidate) {
            return PermissionResult::Ask {
                message: format!(
                    "Claude requested permissions to read from {expanded_path}, which appears to be a UNC path that could access network resources."
                ),
                updated_input: None,
                suggestions: Vec::new(),
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: "UNC path detected (defense-in-depth check)".to_string(),
                }),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }
    for candidate in &paths_to_check {
        if has_suspicious_windows_path_pattern(candidate) {
            return PermissionResult::Ask {
                message: format!(
                    "Claude requested permissions to read from {expanded_path}, which contains a suspicious Windows path pattern that requires manual approval."
                ),
                updated_input: None,
                suggestions: Vec::new(),
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: "Path contains suspicious Windows-specific patterns (alternate data streams, short names, long path prefixes, or three or more consecutive dots) that require manual verification".to_string(),
                }),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }

    for candidate in &paths_to_check {
        if let Some(rule) = matching_rule_for_input(
            candidate,
            context,
            FilePermissionType::Read,
            PermissionBehavior::Deny,
            cwd,
        ) {
            return PermissionResult::Deny {
                message: format!("Permission to read {expanded_path} has been denied."),
                decision_reason: PermissionDecisionReason::Rule { rule },
                tool_use_id: None,
            };
        }
    }
    for candidate in &paths_to_check {
        if let Some(rule) = matching_rule_for_input(
            candidate,
            context,
            FilePermissionType::Read,
            PermissionBehavior::Ask,
            cwd,
        ) {
            return PermissionResult::Ask {
                message: format!(
                    "Claude requested permissions to read from {expanded_path}, but you haven't granted it yet."
                ),
                updated_input: None,
                suggestions: Vec::new(),
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }

    let edit_result = check_write_permission_for_tool(&expanded_path, input, context, cwd);
    if matches!(edit_result, PermissionResult::Allow { .. }) {
        return edit_result;
    }

    if path_in_allowed_working_path(&expanded_path, context, Some(&paths_to_check)) {
        return PermissionResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Mode {
                mode: PermissionMode::Default,
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    if let Some(decision_reason) = check_readable_internal_path(&expanded_path, cwd) {
        return PermissionResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(decision_reason),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    if let Some(rule) = matching_rule_for_input(
        &expanded_path,
        context,
        FilePermissionType::Read,
        PermissionBehavior::Allow,
        cwd,
    ) {
        return PermissionResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    PermissionResult::Ask {
        message: format!(
            "Claude requested permissions to read from {expanded_path}, but you haven't granted it yet."
        ),
        updated_input: None,
        suggestions: generate_suggestions(
            &expanded_path,
            FilesystemOperationType::Read,
            context,
            Some(&paths_to_check),
        ),
        decision_reason: Some(PermissionDecisionReason::WorkingDir {
            reason: "Path is outside allowed working directories".to_string(),
        }),
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

/// Maps to CC `checkWritePermissionForTool(...)`.
///
/// Rust adaptation: the source receives the generic Tool solely to call
/// `getPath(input)`. Rust callers pass that already-derived path and structured
/// input explicitly; policy ordering and the returned decision remain here.
pub fn check_write_permission_for_tool(
    path: &str,
    input: &serde_json::Value,
    context: &ToolPermissionContext,
    cwd: &Path,
) -> PermissionResult {
    let paths_to_check = paths_to_check(path, cwd);

    for path_to_check in &paths_to_check {
        if let Some(rule) = matching_rule_for_input(
            path_to_check,
            context,
            FilePermissionType::Edit,
            PermissionBehavior::Deny,
            cwd,
        ) {
            return PermissionResult::Deny {
                message: format!("Permission to edit {path} has been denied."),
                decision_reason: PermissionDecisionReason::Rule { rule },
                tool_use_id: None,
            };
        }
    }

    if let Some(decision_reason) = check_editable_internal_path(
        &crate::utils::path::expand_path(path, Some(cwd))
            .unwrap_or_else(|_| PathBuf::from(path))
            .display()
            .to_string(),
        cwd,
    ) {
        return PermissionResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(decision_reason),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    if let Some(rule) = claude_folder_session_allow_rule(path, context, cwd) {
        return PermissionResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    match check_path_safety_for_auto_edit(path, Some(&paths_to_check)) {
        PathSafetyForAutoEdit::Unsafe {
            message,
            classifier_approvable,
        } => {
            let suggestions = get_claude_skill_scope(path)
                .map(|(_, pattern)| {
                    vec![PermissionUpdate::AddRules {
                        destination: PermissionUpdateDestination::Session,
                        behavior: PermissionBehavior::Allow,
                        rules: vec![crate::types::permissions::PermissionRuleValue::new(
                            "Edit",
                            Some(pattern),
                        )],
                    }]
                })
                .unwrap_or_else(|| {
                    generate_suggestions(
                        path,
                        FilesystemOperationType::Write,
                        context,
                        Some(&paths_to_check),
                    )
                });
            return PermissionResult::Ask {
                suggestions,
                updated_input: None,
                decision_reason: Some(PermissionDecisionReason::SafetyCheck {
                    reason: message.clone(),
                    classifier_approvable,
                }),
                message,
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
        PathSafetyForAutoEdit::Safe => {}
    }

    for path_to_check in &paths_to_check {
        if let Some(rule) = matching_rule_for_input(
            path_to_check,
            context,
            FilePermissionType::Edit,
            PermissionBehavior::Ask,
            cwd,
        ) {
            return PermissionResult::Ask {
                message: format!(
                    "Claude requested permissions to write to {path}, but you haven't granted it yet."
                ),
                updated_input: None,
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }

    let is_in_working_dir = path_in_allowed_working_path(path, context, Some(&paths_to_check));
    if context.mode == PermissionMode::AcceptEdits && is_in_working_dir {
        return PermissionResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Mode { mode: context.mode }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    if let Some(rule) = matching_rule_for_input(
        path,
        context,
        FilePermissionType::Edit,
        PermissionBehavior::Allow,
        cwd,
    ) {
        return PermissionResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    PermissionResult::Ask {
        message: format!(
            "Claude requested permissions to write to {path}, but you haven't granted it yet."
        ),
        updated_input: None,
        suggestions: generate_suggestions(
            path,
            FilesystemOperationType::Write,
            context,
            Some(&paths_to_check),
        ),
        decision_reason: (!is_in_working_dir).then(|| PermissionDecisionReason::WorkingDir {
            reason: "Path is outside allowed working directories".to_string(),
        }),
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

/// Maps to CC `utils/permissions/filesystem.ts#isSessionPlanFile:245-255`.
pub fn is_session_plan_file(path: impl AsRef<Path>) -> bool {
    is_session_plan_file_for_session_with_settings(
        None,
        &crate::bootstrap::state::get_session_id(),
        path,
    )
}

/// Existing Rust settings/session injection adapter for the same source
/// predicate. Production uses the memoized global resolver; tests can supply
/// their settings without replacing process-wide initial settings.
fn is_session_plan_file_for_session_with_settings(
    settings: Option<&crate::utils::settings::types::SettingsJson>,
    session_id: &str,
    path: impl AsRef<Path>,
) -> bool {
    let plans_dir = match settings {
        Some(settings) => crate::utils::plans::get_plans_directory_with_settings(settings),
        None => crate::utils::plans::get_plans_directory(),
    };
    let expected_prefix = plans_dir.join(crate::utils::plans::get_plan_slug(Some(session_id)));
    let normalized_path = normalize_permission_path_buf(path);
    let normalized_text = normalized_path.to_string_lossy();
    normalized_text.starts_with(expected_prefix.to_string_lossy().as_ref())
        && normalized_text.ends_with(".md")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::PermissionRuleValue;
    use crate::utils::test_env::{EnvVarGuard, PinnedProjectDir, TEST_ENV_LOCK};

    /// CC `filesystem.ts:331-346` and `:376-378`: the per-user directory
    /// under the resolved `CLAUDE_CODE_TMPDIR`, and the project directory
    /// under it named by `sanitizePath`, each with a trailing separator.
    #[cfg(unix)]
    #[test]
    fn claude_temp_dirs_match_official_shape() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-temp-shape-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let resolved = root.canonicalize().unwrap();
        let _tmp = EnvVarGuard::set("CLAUDE_CODE_TMPDIR", &root);
        let expected = format!("{}/claude-{}/", resolved.display(), current_uid());
        assert_eq!(get_claude_temp_dir().to_string_lossy(), expected);
        let cwd = crate::bootstrap::state::get_original_cwd();
        assert_eq!(
            get_project_temp_dir().to_string_lossy(),
            format!(
                "{expected}{}/",
                crate::utils::session_storage::sanitize_path(&cwd.to_string_lossy())
            )
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn root_directory_containment_matches_official_relative_path() {
        // CC filesystem.ts:731-742: relative('/', '/nested') is 'nested'.
        assert!(path_in_working_path("/nested/child", "/"));
        assert!(path_in_working_path("/", "/"));
        assert!(!path_in_working_path("/nested-other", "/nested"));
    }

    #[test]
    fn private_directory_aliases_matches_official_anchored_boundaries() {
        // CC filesystem.ts:716-721: only root aliases are normalized.
        assert!(path_in_working_path("/private/tmp", "/tmp"));
        assert!(path_in_working_path("/tmp/child", "/private/tmp"));
        assert!(path_in_working_path("/private/var/child", "/var"));
        assert!(!path_in_working_path("/private/var", "/var"));
        assert!(!path_in_working_path(
            "/repo/private/tmp/child",
            "/repo/tmp"
        ));
        assert!(!path_in_working_path(
            "/repo/private/var/child",
            "/repo/var"
        ));
        assert!(!path_in_working_path("/private/tmp-other/child", "/tmp"));
    }

    /// CC `filesystem.ts:546`: the NTFS alternate-data-stream check applies
    /// on Windows and WSL. WSL is what `/proc/version` says, which a test
    /// cannot set, so this checks the current platform's answer.
    #[test]
    fn ntfs_alternate_data_stream_check_follows_the_platform() {
        use crate::utils::platform::{Platform, get_platform};
        assert_eq!(
            has_suspicious_windows_path_pattern("/mnt/c/project/file.txt:stream"),
            matches!(get_platform(), Platform::Windows | Platform::Wsl)
        );
        assert!(!has_suspicious_windows_path_pattern(
            "/mnt/c/project/file.txt"
        ));
    }

    #[test]
    fn file_read_ignore_pattern_normalization_matches_official_roots() {
        let patterns = IndexMap::from([
            (None, vec!["*.pem".to_string()]),
            (Some("/repo".to_string()), vec!["private/**".to_string()]),
            (Some("/repo/sub".to_string()), vec!["secret/**".to_string()]),
            (Some("/other".to_string()), vec!["skip/**".to_string()]),
            (Some("/".to_string()), vec!["/repo/root/**".to_string()]),
        ]);
        assert_eq!(
            normalize_patterns_to_path(&patterns, "/repo"),
            vec!["*.pem", "/private/**", "/sub/secret/**", "/root/**"]
        );
    }

    #[test]
    fn read_permission_matches_official_working_dir_rule_and_suggestion_order() {
        let root = std::env::temp_dir().join(format!(
            "cometix-read-permission-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let inside = root.join("inside");
        let blocked = root.join("blocked");
        let outside = root.with_file_name(format!(
            "cometix-read-permission-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let mut context = ToolPermissionContext::default();
        context.additional_working_directories.insert(
            root.display().to_string(),
            crate::types::permissions::AdditionalWorkingDirectory {
                path: root.display().to_string(),
                source: PermissionRuleSource::Session,
            },
        );
        let input = serde_json::json!({"pattern": "*.rs", "path": inside});
        assert!(matches!(
            check_read_permission_for_tool(
                input["path"].as_str().unwrap(),
                &input,
                &context,
                &root,
            ),
            PermissionResult::Allow { .. }
        ));

        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Read",
                Some("blocked/**".to_string()),
            )],
        );
        let blocked_input = serde_json::json!({"pattern": "*", "path": blocked});
        assert!(matches!(
            check_read_permission_for_tool(
                blocked_input["path"].as_str().unwrap(),
                &blocked_input,
                &context,
                &root,
            ),
            PermissionResult::Deny {
                decision_reason: PermissionDecisionReason::Rule { .. },
                ..
            }
        ));

        context.always_deny_rules.clear();
        let outside_input = serde_json::json!({"pattern": "*", "path": outside});
        let result = check_read_permission_for_tool(
            outside_input["path"].as_str().unwrap(),
            &outside_input,
            &context,
            &root,
        );
        assert!(matches!(
            result,
            PermissionResult::Ask {
                ref suggestions,
                decision_reason: Some(PermissionDecisionReason::WorkingDir { .. }),
                ..
            } if !suggestions.is_empty() && suggestions.iter().all(|suggestion| matches!(
                suggestion,
                PermissionUpdate::AddRules { rules, .. }
                    if rules.first().is_some_and(|rule| rule.tool_name == "Read"
                        && rule.rule_content.as_deref().is_some_and(|pattern| pattern.ends_with("/**")))
            ))
        ));

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn readable_internal_task_carve_out_uses_requested_path_like_official() {
        use std::os::unix::fs::symlink;
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "cometix-read-internal-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config = root.join("config");
        let outside = root.join("outside");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(config.join("tasks")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(&outside, config.join("tasks/escape")).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let escaped = config.join("tasks/escape/secret.txt");
        assert!(matches!(
            check_read_permission_for_tool(
                &escaped.display().to_string(),
                &serde_json::json!({"file_path": escaped}),
                &ToolPermissionContext::default(),
                &workspace,
            ),
            PermissionResult::Allow { .. }
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn read_permission_checks_resolved_working_path_then_requested_rules_like_official() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "cometix-read-working-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let workspace = root.join("workspace");
        let outside = root.with_file_name(format!(
            "cometix-read-working-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(&outside, workspace.join("escape")).unwrap();
        let escaped = workspace.join("escape/secret.txt");

        assert!(matches!(
            check_read_permission_for_tool(
                &escaped.display().to_string(),
                &serde_json::json!({"file_path": escaped}),
                &ToolPermissionContext::default(),
                &workspace,
            ),
            PermissionResult::Ask { .. }
        ));

        // Like CC, explicit allow rules are matched against the requested
        // path after the resolved working-directory check.
        let mut lexical_only = ToolPermissionContext::default();
        lexical_only.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Read",
                Some("workspace/escape/secret.txt".to_string()),
            )],
        );
        assert!(matches!(
            check_read_permission_for_tool(
                &escaped.display().to_string(),
                &serde_json::json!({"file_path": escaped}),
                &lexical_only,
                &root,
            ),
            PermissionResult::Allow { .. }
        ));

        let mut edit_lexical_only = ToolPermissionContext::default();
        edit_lexical_only.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Edit",
                Some("workspace/escape/secret.txt".to_string()),
            )],
        );
        assert!(matches!(
            check_read_permission_for_tool(
                &escaped.display().to_string(),
                &serde_json::json!({"file_path": escaped}),
                &edit_lexical_only,
                &root,
            ),
            PermissionResult::Allow { .. }
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn claude_settings_path_detection_normalizes_structure_and_case() {
        let cwd = std::env::temp_dir().join("cometix-settings-path-detection");
        assert!(is_claude_settings_path(
            &cwd.join(".claude/./settings.json").display().to_string(),
            &cwd,
        ));
        assert!(is_claude_settings_path(
            &cwd.join(".cLaUdE/settings.local.json")
                .display()
                .to_string(),
            &cwd,
        ));
        assert!(!is_claude_settings_path(
            &cwd.join("settings.json").display().to_string(),
            &cwd,
        ));
    }

    fn context_with_rule(
        behavior: PermissionBehavior,
        source: PermissionRuleSource,
        content: String,
    ) -> ToolPermissionContext {
        let mut context = ToolPermissionContext::default();
        let map = match behavior {
            PermissionBehavior::Allow => &mut context.always_allow_rules,
            PermissionBehavior::Deny => &mut context.always_deny_rules,
            PermissionBehavior::Ask => &mut context.always_ask_rules,
        };
        map.insert(
            source,
            vec![PermissionRuleValue::new("Edit", Some(content))],
        );
        context
    }

    #[test]
    fn explicit_deny_wins_over_accept_edits() {
        let root = std::env::current_dir().unwrap();
        let path = root.join("blocked.txt");
        let mut context = context_with_rule(
            PermissionBehavior::Deny,
            PermissionRuleSource::Session,
            "blocked.txt".to_string(),
        );
        context.mode = PermissionMode::AcceptEdits;
        assert!(matches!(
            check_write_permission_for_tool(
                &path.display().to_string(),
                &serde_json::json!({"file_path": path}),
                &context,
                &root,
            ),
            PermissionResult::Deny { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resolved_parent_symlink_destination_deny_rule_blocks_new_write() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "cometix-write-permission-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let target = root.join("outside");
        let logical = root.join("logical");
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, &logical).unwrap();
        let resolved_target = target.canonicalize().unwrap();
        let normalized_target = resolved_target
            .display()
            .to_string()
            .replace("/private/var/", "/var/")
            .replace("/private/tmp/", "/tmp/");
        let absolute_rule = format!("//{}/**", normalized_target.trim_start_matches('/'));
        let context = context_with_rule(
            PermissionBehavior::Deny,
            PermissionRuleSource::Session,
            absolute_rule.clone(),
        );
        let candidate = logical.join("new.txt").display().to_string();
        let result = check_write_permission_for_tool(
            &candidate,
            &serde_json::json!({"file_path": &candidate}),
            &context,
            &std::env::current_dir().unwrap(),
        );
        assert!(
            matches!(result, PermissionResult::Deny { .. }),
            "rule={absolute_rule:?} paths={:?} result={result:?}",
            crate::utils::fs_operations::get_paths_for_permission_check(Path::new(&candidate))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(all(feature = "anthropic_internal", unix))]
    #[test]
    fn internal_template_job_carve_out_rejects_hijack_and_symlink_escape() {
        use std::os::unix::fs::symlink;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-template-job-permission-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config = root.join("config");
        let job = config.join("jobs/current");
        let outside = root.join("outside");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _job = EnvVarGuard::set("CLAUDE_JOB_DIR", &job);
        let context = ToolPermissionContext::default();
        let inside = job.join("result.txt");
        assert!(matches!(
            check_write_permission_for_tool(
                &inside.display().to_string(),
                &serde_json::json!({"file_path": inside}),
                &context,
                &root,
            ),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Other { ref reason }),
                ..
            } if reason == "Job directory files for current job are allowed for writing"
        ));

        {
            let _hijacked_job = EnvVarGuard::set("CLAUDE_JOB_DIR", &outside);
            let hijacked = outside.join("result.txt");
            assert!(!matches!(
                check_write_permission_for_tool(
                    &hijacked.display().to_string(),
                    &serde_json::json!({"file_path": hijacked}),
                    &context,
                    &root,
                ),
                PermissionResult::Allow {
                    decision_reason: Some(PermissionDecisionReason::Other { ref reason }),
                    ..
                } if reason == "Job directory files for current job are allowed for writing"
            ));
        }

        symlink(&outside, job.join("escape")).unwrap();
        let escaped = job.join("escape/result.txt");
        assert!(!matches!(
            check_write_permission_for_tool(
                &escaped.display().to_string(),
                &serde_json::json!({"file_path": escaped}),
                &context,
                &root,
            ),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Other { ref reason }),
                ..
            } if reason == "Job directory files for current job are allowed for writing"
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn agent_memory_path_is_an_internal_write_carve_out_before_claude_safety() {
        let cwd = std::env::temp_dir().join(format!(
            "cometix-agent-memory-permission-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let path = cwd.join(".claude/agent-memory/reviewer/MEMORY.md");
        assert!(matches!(
            check_write_permission_for_tool(
                &path.display().to_string(),
                &serde_json::json!({"file_path": path}),
                &ToolPermissionContext::default(),
                &cwd,
            ),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Other { ref reason }),
                ..
            } if reason == "Agent memory files are allowed for writing"
        ));
    }

    #[test]
    fn matcher_uses_ignore_negation_double_star_and_source_precedence() {
        // Matcher roots resolve against the project dir.
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let cwd = crate::bootstrap::state::get_original_cwd();
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new(
                "Edit",
                Some("src/**/secret?.txt".to_string()),
            )],
        );
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![
                PermissionRuleValue::new("Edit", Some("src/**".to_string())),
                PermissionRuleValue::new("Edit", Some("!src/generated/**".to_string())),
                PermissionRuleValue::new("Edit", Some("src/**/secret?.txt".to_string())),
            ],
        );
        let nested_secret = cwd.join("src/deep/nested/secret1.txt");
        let matched = matching_rule_for_input(
            &nested_secret.display().to_string(),
            &context,
            FilePermissionType::Edit,
            PermissionBehavior::Deny,
            &cwd,
        )
        .expect("double-star pattern matches nested path");
        assert_eq!(matched.source, PermissionRuleSource::Session);
        assert!(
            matching_rule_for_input(
                &cwd.join("src/generated/output.rs").display().to_string(),
                &context,
                FilePermissionType::Edit,
                PermissionBehavior::Deny,
                &cwd,
            )
            .is_none()
        );
    }

    #[test]
    fn root_anchored_session_pattern_does_not_match_nested_basename() {
        let cwd = crate::bootstrap::state::get_original_cwd();
        let context = context_with_rule(
            PermissionBehavior::Deny,
            PermissionRuleSource::Session,
            "/.env".to_string(),
        );
        assert!(
            matching_rule_for_input(
                &cwd.join(".env").display().to_string(),
                &context,
                FilePermissionType::Edit,
                PermissionBehavior::Deny,
                &cwd,
            )
            .is_some()
        );
        assert!(
            matching_rule_for_input(
                &cwd.join("nested/.env").display().to_string(),
                &context,
                FilePermissionType::Edit,
                PermissionBehavior::Deny,
                &cwd,
            )
            .is_none()
        );
    }

    #[test]
    fn claude_skill_scope_is_narrow_and_rejects_glob_names() {
        let root = crate::bootstrap::state::get_original_cwd();
        assert_eq!(
            get_claude_skill_scope(
                &root
                    .join(".claude/skills/reviewer/SKILL.md")
                    .display()
                    .to_string()
            ),
            Some((
                "reviewer".to_string(),
                "/.claude/skills/reviewer/**".to_string()
            ))
        );
        assert!(
            get_claude_skill_scope(&root.join(".claude/skills/*/SKILL.md").display().to_string())
                .is_none()
        );
        assert!(
            get_claude_skill_scope(&root.join(".claude/skills/direct.md").display().to_string())
                .is_none()
        );
    }

    #[test]
    fn read_rule_roots_cover_all_sources_and_duplicate_content_uses_later_metadata() {
        struct RootRestore {
            original_cwd: PathBuf,
            flag_path: Option<PathBuf>,
        }
        impl Drop for RootRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(&self.original_cwd);
                crate::utils::settings::set_flag_settings_path(self.flag_path.clone());
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-permission-root-matrix-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_home = root.join("config");
        let original_cwd = root.join("workspace");
        let flag_dir = root.join("flags");
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::create_dir_all(&original_cwd).unwrap();
        std::fs::create_dir_all(&flag_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _restore = RootRestore {
            original_cwd: crate::bootstrap::state::get_original_cwd(),
            flag_path: crate::utils::settings::get_flag_settings_path(),
        };
        crate::bootstrap::state::set_original_cwd(&original_cwd);
        crate::utils::settings::set_flag_settings_path(Some(flag_dir.join("settings.json")));

        for (source, expected_root) in [
            (PermissionRuleSource::UserSettings, config_home.clone()),
            (PermissionRuleSource::ProjectSettings, original_cwd.clone()),
            (PermissionRuleSource::LocalSettings, original_cwd.clone()),
            (PermissionRuleSource::FlagSettings, flag_dir.clone()),
            (PermissionRuleSource::PolicySettings, original_cwd.clone()),
            (PermissionRuleSource::CliArg, original_cwd.clone()),
            (PermissionRuleSource::Command, original_cwd.clone()),
            (PermissionRuleSource::Session, original_cwd.clone()),
        ] {
            let mut context = ToolPermissionContext::default();
            context.always_allow_rules.insert(
                source,
                vec![PermissionRuleValue::new(
                    "Read",
                    Some("/secret/**".to_string()),
                )],
            );
            let rooted = get_patterns_by_root(
                &context,
                FilePermissionType::Read,
                PermissionBehavior::Allow,
            );
            let rules = rooted
                .get(&Some(expected_root.display().to_string()))
                .unwrap_or_else(|| panic!("missing root for {source:?}: {rooted:?}"));
            assert_eq!(rules["/secret/**"].source, source);
        }

        let mut duplicates = ToolPermissionContext::default();
        for source in crate::utils::permissions::permissions::PERMISSION_RULE_SOURCES {
            duplicates.always_allow_rules.insert(
                source,
                vec![PermissionRuleValue::new(
                    "Read",
                    Some("/secret/**".to_string()),
                )],
            );
        }
        let rooted = get_patterns_by_root(
            &duplicates,
            FilePermissionType::Read,
            PermissionBehavior::Allow,
        );
        assert_eq!(rooted.len(), 1);
        assert_eq!(
            rooted[&Some(original_cwd.display().to_string())]["/secret/**"].source,
            PermissionRuleSource::Session
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn default_write_asks_with_accept_edits_suggestion() {
        let cwd = std::env::current_dir().unwrap();
        let path = cwd.join("new-write.txt");
        let result = check_write_permission_for_tool(
            &path.display().to_string(),
            &serde_json::json!({"file_path": path}),
            &ToolPermissionContext::default(),
            &cwd,
        );
        let PermissionResult::Ask { suggestions, .. } = result else {
            panic!("default write should ask")
        };
        assert!(suggestions.iter().any(|update| matches!(
            update,
            PermissionUpdate::SetMode {
                mode: PermissionMode::AcceptEdits,
                destination: PermissionUpdateDestination::Session,
            }
        )));
    }

    /// CC filesystem.ts:667-674 authorizes original cwd plus explicit grants;
    /// async cwd only determines how the relative path resolves.
    #[test]
    fn read_permission_matches_official_async_cwd_without_implicit_grant() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct Restore {
            original: PathBuf,
            root: PathBuf,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(&self.original);
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
        let root = std::env::temp_dir().join(format!("cc-read-cwd-{}", uuid::Uuid::new_v4()));
        let original = root.join("repo");
        let outside = root.join("outside");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("a.txt"), "outside").unwrap();
        let _restore = Restore {
            original: crate::bootstrap::state::get_original_cwd(),
            root,
        };
        crate::bootstrap::state::set_original_cwd(&original);
        let mut context = ToolPermissionContext::default();
        let input = serde_json::json!({"file_path":"a.txt"});
        let result = check_read_permission_for_tool("a.txt", &input, &context, &outside);
        assert!(matches!(
            result,
            PermissionResult::Ask {
                decision_reason: Some(PermissionDecisionReason::WorkingDir { .. }),
                ..
            }
        ));
        assert!(context.additional_working_directories.is_empty());
        context.additional_working_directories.insert(
            outside.display().to_string(),
            crate::types::permissions::AdditionalWorkingDirectory {
                path: outside.display().to_string(),
                source: PermissionRuleSource::Session,
            },
        );
        assert!(
            matches!(check_read_permission_for_tool("a.txt",&input,&context,&outside),
            PermissionResult::Allow { updated_input:Some(value), .. } if value == input)
        );
        context.additional_working_directories.clear();
        assert!(matches!(
            check_read_permission_for_tool("a.txt", &input, &context, &original),
            PermissionResult::Allow { .. }
        ));
    }

    /// CC filesystem.ts:456-483 exempts only structural worktree segments and
    /// compares dangerous filenames only at the basename.
    #[test]
    fn auto_edit_safety_matches_official_worktree_and_basename_boundaries() {
        for path in [
            "/repo/.claude/worktrees/topic/src/lib.rs",
            "/repo/.ClAuDe/WORKTREES/topic/src/lib.rs",
            "/repo/.claude/worktrees/topic/.claude/worktrees/nested/file.rs",
            "/repo/.bashrc/ordinary.txt",
        ] {
            assert!(!is_dangerous_file_path_to_auto_edit(path), "{path}");
            assert_eq!(
                check_path_safety_for_auto_edit(path, Some(&[path.to_string()])),
                PathSafetyForAutoEdit::Safe,
                "{path}"
            );
        }
        for path in [
            "/repo/.claude/worktrees/topic/.claude/secrets.txt",
            "/repo/.claude/worktrees/topic/.git/config",
            "/repo/.claude/other/file.rs",
            "/repo/.claude/worktrees/topic/.BaShRc",
            "/repo/.vscode/settings.json",
            "//server/share/file.txt",
        ] {
            assert!(is_dangerous_file_path_to_auto_edit(path), "{path}");
            assert!(
                matches!(
                    check_path_safety_for_auto_edit(path, Some(&[path.to_string()])),
                    PathSafetyForAutoEdit::Unsafe { .. }
                ),
                "{path}"
            );
        }
    }
    #[test]
    fn session_plan_file_matches_main_and_agent_plan_names() {
        let _guard = crate::utils::plans::test_plan_state_lock();
        let session_id = format!("plan-match-{}", uuid::Uuid::new_v4());
        let previous_session = crate::bootstrap::state::get_session_id();
        crate::bootstrap::state::set_session_id(&session_id);
        crate::utils::plans::set_plan_slug(&session_id, "bright-maple");
        let settings = crate::utils::settings::types::SettingsJson {
            plans_directory: Some(".claude/plans".to_string()),
            ..crate::utils::settings::types::SettingsJson::default()
        };
        let plans_dir = crate::utils::plans::get_plans_directory_with_settings(&settings);

        assert!(is_session_plan_file_for_session_with_settings(
            Some(&settings),
            &session_id,
            plans_dir.join("bright-maple.md")
        ));
        assert!(is_session_plan_file_for_session_with_settings(
            Some(&settings),
            &session_id,
            plans_dir.join("bright-maple-agent-agent-1.md")
        ));
        assert!(!is_session_plan_file_for_session_with_settings(
            Some(&settings),
            &session_id,
            plans_dir.join("other.md")
        ));

        crate::bootstrap::state::set_session_id(previous_session);
        crate::utils::plans::clear_plan_slug(Some(&session_id));
    }
}
