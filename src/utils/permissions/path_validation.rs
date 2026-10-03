//! Maps to: CC `utils/permissions/pathValidation.ts`.
//!
//! Path-level permission validation shared by file tools and shell parsers. This
//! port preserves the official pure helpers and the core check ordering:
//! deny rules → internal write carve-outs → safety checks → working directory →
//! internal reads → sandbox write allowlist → allow rules → ask.
//!
//! Current intentional gap: Rust `ToolPermissionContext.additionalWorkingDirectories`
//! is now represented and honored, but broader source-aware loader wiring into
//! every caller remains in-progress with the managed-only filtering slice.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::{PermissionBehavior, PermissionMode};
#[cfg(test)]
use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
use crate::utils::fs_operations::{self, get_fs_implementation};
use crate::utils::permissions::filesystem::{
    FilePermissionType, PathSafetyForAutoEdit, check_editable_internal_path,
    check_path_safety_for_auto_edit, check_readable_internal_path, matching_rule_for_input,
    path_in_allowed_working_path, path_in_working_path,
};
use crate::utils::permissions::permission_result::PermissionDecisionReason;
use regex::Regex;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

const MAX_DIRS_TO_LIST: usize = 5;
const GLOB_PATTERN_CHARS: &[char] = &['*', '?', '[', ']', '{', '}'];
static WINDOWS_DRIVE_ROOT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]:/?$").expect("valid drive root regex"));
static WINDOWS_DRIVE_CHILD_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]:/[^/]+$").expect("valid drive child regex"));
/// Maps to: CC `FileOperationType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOperationType {
    Read,
    Write,
    Create,
}

/// Maps to: CC `PathCheckResult`.
#[derive(Clone, Debug, PartialEq)]
pub struct PathCheckResult {
    pub allowed: bool,
    pub decision_reason: Option<PermissionDecisionReason>,
}

/// Maps to: CC `ResolvedPathCheckResult`.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPathCheckResult {
    pub allowed: bool,
    pub resolved_path: String,
    pub decision_reason: Option<PermissionDecisionReason>,
}

/// Maps to: CC `formatDirectoryList(directories)`.
pub fn format_directory_list(directories: &[String]) -> String {
    if directories.len() <= MAX_DIRS_TO_LIST {
        return directories
            .iter()
            .map(|dir| format!("'{dir}'"))
            .collect::<Vec<_>>()
            .join(", ");
    }
    let first_dirs = directories
        .iter()
        .take(MAX_DIRS_TO_LIST)
        .map(|dir| format!("'{dir}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{first_dirs}, and {} more",
        directories.len() - MAX_DIRS_TO_LIST
    )
}

/// Maps to: CC `getGlobBaseDirectory(path)`.
pub fn get_glob_base_directory(path: &str) -> String {
    let Some(glob_index) = path.find(GLOB_PATTERN_CHARS) else {
        return path.to_string();
    };
    let before_glob = &path[..glob_index];
    let last_sep_index = if cfg!(target_os = "windows") {
        before_glob.rfind(['/', '\\'])
    } else {
        before_glob.rfind('/')
    };
    match last_sep_index {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(index) => before_glob[..index].to_string(),
    }
}

/// Maps to: CC `expandTilde(path)`.
pub fn expand_tilde(path: &str) -> String {
    if path == "~"
        || path.starts_with("~/")
        || (cfg!(target_os = "windows") && path.starts_with("~\\"))
    {
        return format!(
            "{}{}",
            crate::utils::node_os::homedir().display(),
            &path[1..]
        );
    }
    path.to_string()
}

/// Maps to: CC `isPathInSandboxWriteAllowlist(resolvedPath)`.
pub fn is_path_in_sandbox_write_allowlist(resolved_path: &str) -> bool {
    if !crate::utils::sandbox::sandbox_adapter::is_sandboxing_enabled() {
        return false;
    }
    let settings = crate::utils::settings::get_initial_settings();
    let config =
        crate::utils::sandbox::sandbox_adapter::convert_to_sandbox_runtime_config(&settings);
    let permission_paths = |path: &str| {
        crate::utils::fs_operations::get_paths_for_permission_check(Path::new(path))
            .into_iter()
            .map(|path| normalize_path_buf(path).display().to_string())
            .collect::<Vec<_>>()
    };
    let paths_to_check = permission_paths(resolved_path);
    let resolved_allow = config
        .filesystem
        .allow_write
        .iter()
        .flat_map(|path| permission_paths(path))
        .collect::<Vec<_>>();
    let resolved_deny = config
        .filesystem
        .deny_write
        .iter()
        .flat_map(|path| permission_paths(path))
        .collect::<Vec<_>>();

    paths_to_check.iter().all(|path| {
        !resolved_deny
            .iter()
            .any(|deny_path| path_in_working_path(path, deny_path))
            && resolved_allow
                .iter()
                .any(|allow_path| path_in_working_path(path, allow_path))
    })
}

/// Maps to: CC `isPathAllowed(...)`.
pub fn is_path_allowed(
    resolved_path: &str,
    context: &ToolPermissionContext,
    operation_type: FileOperationType,
    precomputed_paths_to_check: Option<&[String]>,
) -> PathCheckResult {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let permission_type = if operation_type == FileOperationType::Read {
        FilePermissionType::Read
    } else {
        FilePermissionType::Edit
    };

    if let Some(rule) = matching_rule_for_input(
        resolved_path,
        context,
        permission_type,
        PermissionBehavior::Deny,
        &cwd,
    ) {
        return PathCheckResult {
            allowed: false,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
        };
    }

    if operation_type != FileOperationType::Read {
        if let Some(reason) = check_editable_internal_path(resolved_path, &cwd) {
            return PathCheckResult {
                allowed: true,
                decision_reason: Some(reason),
            };
        }
    }

    if operation_type != FileOperationType::Read {
        match check_path_safety_for_auto_edit(resolved_path, precomputed_paths_to_check) {
            PathSafetyForAutoEdit::Unsafe {
                message,
                classifier_approvable,
            } => {
                return PathCheckResult {
                    allowed: false,
                    decision_reason: Some(PermissionDecisionReason::SafetyCheck {
                        reason: message,
                        classifier_approvable,
                    }),
                };
            }
            PathSafetyForAutoEdit::Safe => {}
        }
    }

    let is_in_working_dir =
        path_in_allowed_working_path(resolved_path, context, precomputed_paths_to_check);
    if is_in_working_dir {
        if operation_type == FileOperationType::Read || context.mode == PermissionMode::AcceptEdits
        {
            return PathCheckResult {
                allowed: true,
                decision_reason: None,
            };
        }
    }

    if operation_type == FileOperationType::Read {
        if let Some(reason) = check_readable_internal_path(resolved_path, &cwd) {
            return PathCheckResult {
                allowed: true,
                decision_reason: Some(reason),
            };
        }
    }

    if operation_type != FileOperationType::Read
        && !is_in_working_dir
        && is_path_in_sandbox_write_allowlist(resolved_path)
    {
        return PathCheckResult {
            allowed: true,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Path is in sandbox write allowlist".to_string(),
            }),
        };
    }

    if let Some(rule) = matching_rule_for_input(
        resolved_path,
        context,
        permission_type,
        PermissionBehavior::Allow,
        &cwd,
    ) {
        return PathCheckResult {
            allowed: true,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
        };
    }

    PathCheckResult {
        allowed: false,
        decision_reason: None,
    }
}

/// Maps to: CC `validateGlobPattern(...)`.
pub fn validate_glob_pattern(
    clean_path: &str,
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    operation_type: FileOperationType,
) -> ResolvedPathCheckResult {
    if contains_path_traversal(clean_path) {
        let absolute_path = absolute_path(clean_path, cwd);
        let result = fs_operations::safe_resolve_path(
            get_fs_implementation().as_ref(),
            Path::new(&absolute_path),
        );
        let resolved_path = result.resolved_path.to_string_lossy().into_owned();
        let paths = result.is_canonical.then(|| vec![resolved_path.clone()]);
        let result = is_path_allowed(
            &resolved_path,
            tool_permission_context,
            operation_type,
            paths.as_deref(),
        );
        return ResolvedPathCheckResult {
            allowed: result.allowed,
            resolved_path,
            decision_reason: result.decision_reason,
        };
    }

    let base_path = get_glob_base_directory(clean_path);
    let absolute_base_path = absolute_path(&base_path, cwd);
    let result = fs_operations::safe_resolve_path(
        get_fs_implementation().as_ref(),
        Path::new(&absolute_base_path),
    );
    let resolved_path = result.resolved_path.to_string_lossy().into_owned();
    let paths = result.is_canonical.then(|| vec![resolved_path.clone()]);
    let result = is_path_allowed(
        &resolved_path,
        tool_permission_context,
        operation_type,
        paths.as_deref(),
    );
    ResolvedPathCheckResult {
        allowed: result.allowed,
        resolved_path,
        decision_reason: result.decision_reason,
    }
}

/// Maps to: CC `isDangerousRemovalPath(resolvedPath)`.
pub fn is_dangerous_removal_path(resolved_path: &str) -> bool {
    let forward_slashed = collapse_to_forward_slashes(resolved_path);
    if forward_slashed == "*" || forward_slashed.ends_with("/*") {
        return true;
    }

    let normalized_path = if forward_slashed == "/" {
        forward_slashed
    } else {
        forward_slashed.trim_end_matches('/').to_string()
    };

    if normalized_path == "/" || WINDOWS_DRIVE_ROOT_REGEX.is_match(&normalized_path) {
        return true;
    }

    let home = crate::utils::node_os::homedir();
    if normalized_path == collapse_to_forward_slashes(&home.to_string_lossy()) {
        return true;
    }

    if parent_dir_string(&normalized_path).as_deref() == Some("/") {
        return true;
    }

    WINDOWS_DRIVE_CHILD_REGEX.is_match(&normalized_path)
}

/// Maps to: CC `validatePath(...)`.
pub fn validate_path(
    path: &str,
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    operation_type: FileOperationType,
) -> ResolvedPathCheckResult {
    let clean_path = expand_tilde(&strip_surrounding_quotes(path));

    if contains_vulnerable_unc_path(&clean_path) {
        return ResolvedPathCheckResult {
            allowed: false,
            resolved_path: clean_path,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "UNC network paths require manual approval".to_string(),
            }),
        };
    }

    if clean_path.starts_with('~') {
        return ResolvedPathCheckResult {
            allowed: false,
            resolved_path: clean_path,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Tilde expansion variants (~user, ~+, ~-) in paths require manual approval"
                    .to_string(),
            }),
        };
    }

    if clean_path.contains('$') || clean_path.contains('%') || clean_path.starts_with('=') {
        return ResolvedPathCheckResult {
            allowed: false,
            resolved_path: clean_path,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Shell expansion syntax in paths requires manual approval".to_string(),
            }),
        };
    }

    if has_glob_pattern(&clean_path) {
        if matches!(
            operation_type,
            FileOperationType::Write | FileOperationType::Create
        ) {
            return ResolvedPathCheckResult {
                allowed: false,
                resolved_path: clean_path,
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: "Glob patterns are not allowed in write operations. Please specify an exact file path."
                        .to_string(),
                }),
            };
        }
        return validate_glob_pattern(&clean_path, cwd, tool_permission_context, operation_type);
    }

    let absolute_path = absolute_path(&clean_path, cwd);
    let result = fs_operations::safe_resolve_path(
        get_fs_implementation().as_ref(),
        Path::new(&absolute_path),
    );
    let resolved_path = result.resolved_path.to_string_lossy().into_owned();
    let paths = result.is_canonical.then(|| vec![resolved_path.clone()]);
    let result = is_path_allowed(
        &resolved_path,
        tool_permission_context,
        operation_type,
        paths.as_deref(),
    );
    ResolvedPathCheckResult {
        allowed: result.allowed,
        resolved_path,
        decision_reason: result.decision_reason,
    }
}

fn contains_vulnerable_unc_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

fn contains_path_traversal(path: &str) -> bool {
    path.split(['/', '\\']).any(|part| part == "..")
}

fn strip_surrounding_quotes(path: &str) -> String {
    let path = path.strip_prefix(['\'', '"']).unwrap_or(path);
    path.strip_suffix(['\'', '"']).unwrap_or(path).to_string()
}

fn has_glob_pattern(path: &str) -> bool {
    path.contains(GLOB_PATTERN_CHARS)
}

fn absolute_path(path: &str, cwd: &str) -> String {
    let path_buf = PathBuf::from(path);
    if crate::utils::fs_operations::native::is_absolute(&path_buf) {
        path.to_string()
    } else {
        crate::utils::fs_operations::native::resolve_path(Path::new(cwd), &path_buf)
            .expect("node:path.resolve could not obtain the current directory")
            .to_string_lossy()
            .into_owned()
    }
}

fn normalize_path_buf(path: impl AsRef<Path>) -> PathBuf {
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

fn collapse_to_forward_slashes(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut last_was_sep = false;
    for ch in path.chars() {
        if ch == '/' || ch == '\\' {
            if !last_was_sep {
                result.push('/');
                last_was_sep = true;
            }
        } else {
            result.push(ch);
            last_was_sep = false;
        }
    }
    result
}

fn parent_dir_string(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .map(|parent| collapse_to_forward_slashes(&parent.display().to_string()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn absolute_path_matches_official_preserved_absolute_and_resolve() {
        assert_eq!(super::absolute_path("/a/../b", "/cwd"), "/a/../b");
        #[cfg(not(windows))]
        assert_eq!(super::absolute_path("a/../b", "/cwd"), "/cwd/b");
    }

    #[cfg(windows)]
    #[test]
    fn absolute_path_matches_official_windows_root_and_drive_relative() {
        assert_eq!(super::absolute_path(r"\child", r"C:\base"), r"\child");
        assert_eq!(super::absolute_path("C:foo", r"C:\base"), r"C:\base\foo");
        assert_eq!(super::absolute_path(r"C:\a\..\b", r"C:\base"), r"C:\a\..\b");
    }

    use super::*;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, PinnedProjectDir, TEST_ENV_LOCK};

    fn ctx() -> ToolPermissionContext {
        ToolPermissionContext::default()
    }

    #[test]
    fn format_directory_list_matches_official_limit() {
        let dirs = (0..6).map(|i| format!("dir{i}")).collect::<Vec<_>>();
        assert_eq!(format_directory_list(&dirs[..2]), "'dir0', 'dir1'");
        assert_eq!(
            format_directory_list(&dirs),
            "'dir0', 'dir1', 'dir2', 'dir3', 'dir4', and 1 more"
        );
    }

    #[test]
    fn glob_base_directory_and_tilde_expansion_match_official_shapes() {
        assert_eq!(get_glob_base_directory("/tmp/*.txt"), "/tmp");
        assert_eq!(get_glob_base_directory("*.txt"), ".");
        assert_eq!(get_glob_base_directory("/tmp/no-glob"), "/tmp/no-glob");

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _home = EnvVarGuard::set(HOME_VAR, "/home/someone");
        assert_eq!(expand_tilde("~/file"), "/home/someone/file");
        assert_eq!(expand_tilde("~root/file"), "~root/file");
    }

    #[test]
    fn dangerous_removal_path_blocks_roots_home_and_wildcards() {
        assert!(is_dangerous_removal_path("*"));
        assert!(is_dangerous_removal_path("/"));
        assert!(is_dangerous_removal_path("/tmp"));
        assert!(is_dangerous_removal_path("C:\\Windows"));
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _home = EnvVarGuard::set(HOME_VAR, "/home/someone");
        assert!(is_dangerous_removal_path("/home/someone"));
        assert!(!is_dangerous_removal_path("/tmp/project/file.txt"));
    }

    #[test]
    fn validate_path_blocks_shell_expansion_tilde_variants_unc_and_write_globs() {
        let cwd = std::env::current_dir().unwrap();
        for (raw, reason) in [
            (
                "//server/share",
                "UNC network paths require manual approval",
            ),
            (
                "~root/.ssh/id_rsa",
                "Tilde expansion variants (~user, ~+, ~-) in paths require manual approval",
            ),
            (
                "$HOME/.ssh/id_rsa",
                "Shell expansion syntax in paths requires manual approval",
            ),
            (
                "src/*.rs",
                "Glob patterns are not allowed in write operations. Please specify an exact file path.",
            ),
        ] {
            let result = validate_path(
                raw,
                &cwd.display().to_string(),
                &ctx(),
                FileOperationType::Write,
            );
            assert!(!result.allowed);
            assert!(matches!(
                result.decision_reason,
                Some(PermissionDecisionReason::Other { reason: ref r }) if r == reason
            ));
        }
    }

    #[test]
    fn path_allowed_honors_deny_rules_before_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Read",
                Some("Cargo.toml".to_string()),
            )],
        );
        let result = validate_path(
            "Cargo.toml",
            &cwd.display().to_string(),
            &context,
            FileOperationType::Read,
        );
        assert!(!result.allowed);
        assert!(matches!(
            result.decision_reason,
            Some(PermissionDecisionReason::Rule { ref rule })
                if rule.rule_value.rule_content.as_deref() == Some("Cargo.toml")
        ));
    }

    #[test]
    fn accept_edits_allows_safe_working_directory_writes_but_not_claude_config() {
        // "Working directory" is judged against the project dir.
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let cwd = std::env::current_dir().unwrap();
        let mut context = ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        };
        let result = validate_path(
            "src/new_file.rs",
            &cwd.display().to_string(),
            &context,
            FileOperationType::Write,
        );
        assert!(result.allowed);

        context.mode = PermissionMode::AcceptEdits;
        let unsafe_result = validate_path(
            ".claude/settings.json",
            &cwd.display().to_string(),
            &context,
            FileOperationType::Write,
        );
        assert!(!unsafe_result.allowed);
        assert!(matches!(
            unsafe_result.decision_reason,
            Some(PermissionDecisionReason::SafetyCheck {
                classifier_approvable: true,
                ..
            })
        ));
    }

    #[test]
    fn matching_rule_for_input_supports_directory_wildcard_patterns() {
        let mut context = ToolPermissionContext::default();
        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Edit", Some("src/**".to_string()))],
        );
        let cwd = std::env::current_dir().unwrap();
        let path = cwd.join("src/main.rs").display().to_string();
        let rule = matching_rule_for_input(
            &path,
            &context,
            FilePermissionType::Edit,
            PermissionBehavior::Allow,
            &cwd,
        )
        .unwrap();
        assert_eq!(rule.source, PermissionRuleSource::Session);
        assert_eq!(rule.rule_value.tool_name, "Edit");
    }

    #[test]
    fn additional_working_directories_are_honored_for_read_validation() {
        let mut context = ToolPermissionContext::default();
        context.additional_working_directories.insert(
            "/tmp/cometix-extra".to_string(),
            crate::types::permissions::AdditionalWorkingDirectory {
                path: "/tmp/cometix-extra".to_string(),
                source: PermissionRuleSource::Session,
            },
        );
        let result = is_path_allowed(
            "/tmp/cometix-extra/file.txt",
            &context,
            FileOperationType::Read,
            None,
        );
        assert!(result.allowed);
    }
}
