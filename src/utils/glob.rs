//! Ripgrep-backed file globbing.
//!
//! Maps to: CC `utils/glob.ts:1-130`.

use crate::tool::AbortController;
use crate::tool::ToolPermissionContext;
use crate::utils::process_env::JsTruthy;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedGlobBaseDirectory {
    pub base_dir: String,
    pub relative_pattern: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobResult {
    pub files: Vec<PathBuf>,
    pub truncated: bool,
}

#[cfg(not(windows))]
fn literal_directory_and_basename(pattern: &str) -> (String, String) {
    // Node `path.posix.dirname` preserves duplicate separators immediately
    // before the basename (for example `foo//bar` -> `foo/`).
    let bytes = pattern.as_bytes();
    let directory = if bytes.is_empty() {
        ".".to_string()
    } else {
        let has_root = bytes[0] == b'/';
        let mut end = None;
        let mut matched_separator = true;
        for index in (1..bytes.len()).rev() {
            if bytes[index] == b'/' {
                if !matched_separator {
                    end = Some(index);
                    break;
                }
            } else {
                matched_separator = false;
            }
        }
        match end {
            None if has_root => "/".to_string(),
            None => ".".to_string(),
            Some(1) if has_root => "//".to_string(),
            Some(end) => pattern[..end].to_string(),
        }
    };

    let trimmed = pattern.trim_end_matches('/');
    let basename = if trimmed.is_empty() {
        String::new()
    } else {
        trimmed
            .rsplit_once('/')
            .map(|(_, basename)| basename)
            .unwrap_or(trimmed)
            .to_string()
    };
    (directory, basename)
}

#[cfg(windows)]
fn literal_directory_and_basename(pattern: &str) -> (String, String) {
    let path = Path::new(pattern);
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let basename = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            pattern
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
                .to_string()
        });
    (directory, basename)
}

fn is_absolute_pattern(pattern: &str) -> bool {
    #[cfg(windows)]
    {
        let bytes = pattern.as_bytes();
        matches!(bytes.first(), Some(b'/' | b'\\'))
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\'))
    }
    #[cfg(not(windows))]
    {
        Path::new(pattern).is_absolute()
    }
}

/// Maps to CC `utils/glob.ts:17-64` `extractGlobBaseDirectory(...)`.
pub fn extract_glob_base_directory(pattern: &str) -> ExtractedGlobBaseDirectory {
    let first_glob = pattern
        .char_indices()
        .find_map(|(index, character)| matches!(character, '*' | '?' | '[' | '{').then_some(index));

    let Some(first_glob) = first_glob else {
        let (base_dir, relative_pattern) = literal_directory_and_basename(pattern);
        return ExtractedGlobBaseDirectory {
            base_dir,
            relative_pattern,
        };
    };

    let static_prefix = &pattern[..first_glob];
    let last_separator = static_prefix
        .char_indices()
        .filter_map(|(index, character)| {
            (character == '/' || (cfg!(windows) && character == '\\')).then_some(index)
        })
        .last();
    let Some(last_separator) = last_separator else {
        return ExtractedGlobBaseDirectory {
            base_dir: String::new(),
            relative_pattern: pattern.to_string(),
        };
    };

    let mut base_dir = static_prefix[..last_separator].to_string();
    let relative_pattern = pattern[last_separator + 1..].to_string();
    if base_dir.is_empty() && last_separator == 0 {
        base_dir = "/".to_string();
    }
    if cfg!(windows)
        && base_dir.len() == 2
        && base_dir.as_bytes()[0].is_ascii_alphabetic()
        && base_dir.as_bytes()[1] == b':'
    {
        base_dir.push(std::path::MAIN_SEPARATOR);
    }

    ExtractedGlobBaseDirectory {
        base_dir,
        relative_pattern,
    }
}

/// Returns the directory ripgrep will actually traverse. For an absolute glob
/// pattern this can differ from the tool's optional `path` argument.
pub fn effective_glob_search_root(file_pattern: &str, cwd: &Path) -> PathBuf {
    if is_absolute_pattern(file_pattern) {
        let extracted = extract_glob_base_directory(file_pattern);
        if !extracted.base_dir.is_empty() {
            return PathBuf::from(extracted.base_dir);
        }
    }
    cwd.to_path_buf()
}

/// Maps to: CC `utils/glob.ts:98-99` `isEnvTruthy(process.env.K || 'true')`.
fn env_default_true(key: &str) -> bool {
    crate::utils::env_utils::is_env_truthy(Some(
        crate::utils::process_env::var(key)
            .truthy()
            .as_deref()
            .unwrap_or("true"),
    ))
}

fn escape_ripgrep_glob_literal(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut escaped = String::with_capacity(normalized.len());
    for character in normalized.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']' | '{' | '}') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub(crate) fn ripgrep_read_deny_pattern(pattern: &str, search_dir: &Path) -> String {
    if !search_dir.is_absolute() || !pattern.contains('/') || pattern.starts_with("**/") {
        return pattern.to_string();
    }
    let root =
        escape_ripgrep_glob_literal(search_dir.display().to_string().trim_matches(['/', '\\']));
    let relative = pattern.trim_start_matches('/');
    if root.is_empty() {
        format!("**/{relative}")
    } else {
        format!("**/{root}/{relative}")
    }
}

fn resolve_deepest_symlink_spelling(path: &Path) -> Option<PathBuf> {
    for prefix in path.ancestors() {
        if !std::fs::symlink_metadata(prefix)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
        {
            continue;
        }
        let target = std::fs::read_link(prefix).ok()?;
        let target = if target.is_absolute() {
            target
        } else {
            prefix
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(target)
        };
        let remainder = path.strip_prefix(prefix).ok()?;
        return Some(if remainder.as_os_str().is_empty() {
            target
        } else {
            target.join(remainder)
        });
    }
    None
}

/// Maps to CC `utils/glob.ts:66-130` `glob(...)`.
pub fn glob(
    file_pattern: &str,
    cwd: &Path,
    limit: usize,
    offset: usize,
    abort_controller: &AbortController,
    tool_permission_context: &ToolPermissionContext,
) -> Result<GlobResult, String> {
    let mut search_dir = cwd.to_path_buf();
    let mut search_pattern = file_pattern.to_string();

    if is_absolute_pattern(file_pattern) {
        let extracted = extract_glob_base_directory(file_pattern);
        if !extracted.base_dir.is_empty() {
            search_dir = PathBuf::from(extracted.base_dir);
            search_pattern = extracted.relative_pattern;
        }
    }

    let patterns_by_root = crate::utils::permissions::filesystem::get_file_read_ignore_patterns(
        tool_permission_context,
    );
    let mut ignore_patterns = indexmap::IndexSet::new();
    ignore_patterns.extend(
        crate::utils::permissions::filesystem::normalize_patterns_to_path(
            &patterns_by_root,
            &search_dir.display().to_string(),
        ),
    );
    // DEVIATION(SECURITY): CC normalizes against the search dir only
    // (`utils/glob.ts:110`). An explicitly targeted directory symlink is
    // traversed by rg even without `--follow`, so rooted deny rules are also
    // normalized against its physical destination and applied to the logical rg
    // target — approval of the symlink must not reveal denied destination files.
    let mut physical_search_dirs = indexmap::IndexSet::new();
    let mut symlink_spelling = search_dir.clone();
    for _ in 0..32 {
        let Some(target) = resolve_deepest_symlink_spelling(&symlink_spelling) else {
            break;
        };
        if target == symlink_spelling || !physical_search_dirs.insert(target.clone()) {
            break;
        }
        symlink_spelling = target;
    }
    if let Ok(target) = std::fs::canonicalize(&search_dir) {
        physical_search_dirs.insert(target);
    }
    for physical_search_dir in physical_search_dirs {
        if physical_search_dir != search_dir {
            ignore_patterns.extend(
                crate::utils::permissions::filesystem::normalize_patterns_to_path(
                    &patterns_by_root,
                    &physical_search_dir.display().to_string(),
                ),
            );
        }
    }

    let mut arguments = vec![
        "--files".to_string(),
        "--glob".to_string(),
        search_pattern,
        "--sort=modified".to_string(),
    ];
    if env_default_true("CLAUDE_CODE_GLOB_NO_IGNORE") {
        arguments.push("--no-ignore".to_string());
    }
    if env_default_true("CLAUDE_CODE_GLOB_HIDDEN") {
        arguments.push("--hidden".to_string());
    }
    for pattern in ignore_patterns {
        // Keep CC's normalized root-relative exclusion. Ripgrep evaluates this
        // spelling correctly when the target is the process cwd.
        arguments.push("--glob".to_string());
        arguments.push(format!("!{pattern}"));

        // DEVIATION(SAFETY): for an absolute target outside process cwd,
        // ripgrep instead matches against a target-prefixed spelling. Add the
        // anchored equivalent as a second exclusion so neither invocation
        // context can expose a Read-denied file.
        let anchored = ripgrep_read_deny_pattern(&pattern, &search_dir);
        if anchored != pattern {
            arguments.push("--glob".to_string());
            arguments.push(format!("!{anchored}"));
        }
    }
    for exclusion in
        crate::utils::plugins::orphaned_plugin_filter::get_glob_exclusions_for_plugin_cache(Some(
            &search_dir,
        ))
    {
        arguments.push("--glob".to_string());
        arguments.push(exclusion);
    }

    let paths = crate::utils::ripgrep::rip_grep(&arguments, &search_dir, abort_controller)?;
    let absolute_paths = paths
        .into_iter()
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                search_dir.join(path)
            }
        })
        .collect::<Vec<_>>();
    let truncated = absolute_paths.len() > offset.saturating_add(limit);
    let files = absolute_paths
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();

    Ok(GlobResult { files, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn extract_glob_base_directory_matches_official_matrix() {
        let cases = [
            ("*.ts", "", "*.ts"),
            ("src/**/*.ts", "src", "**/*.ts"),
            ("/tmp/root/**/*.rs", "/tmp/root", "**/*.rs"),
            ("/foo.txt", "/", "foo.txt"),
            ("/*.{ts,tsx}", "/", "*.{ts,tsx}"),
            ("foo", ".", "foo"),
            ("foo/bar.txt", "foo", "bar.txt"),
            ("./src/*.rs", "./src", "*.rs"),
            ("../other/*.md", "../other", "*.md"),
            ("a[b].txt", "", "a[b].txt"),
            ("{a,b}.txt", "", "{a,b}.txt"),
        ];
        for (pattern, base_dir, relative_pattern) in cases {
            assert_eq!(
                extract_glob_base_directory(pattern),
                ExtractedGlobBaseDirectory {
                    base_dir: base_dir.to_string(),
                    relative_pattern: relative_pattern.to_string(),
                },
                "pattern={pattern:?}"
            );
        }

        #[cfg(not(windows))]
        for (pattern, base_dir, relative_pattern) in [
            ("", ".", ""),
            (".", ".", "."),
            ("..", ".", ".."),
            ("/", "/", ""),
            ("//", "/", ""),
            ("/tmp/", "/", "tmp"),
            ("foo/", ".", "foo"),
            ("foo//bar", "foo/", "bar"),
            ("foo///bar", "foo//", "bar"),
            ("//foo", "//", "foo"),
            ("///foo", "//", "foo"),
        ] {
            assert_eq!(
                extract_glob_base_directory(pattern),
                ExtractedGlobBaseDirectory {
                    base_dir: base_dir.to_string(),
                    relative_pattern: relative_pattern.to_string(),
                },
                "literal pattern={pattern:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn effective_glob_search_root_uses_absolute_pattern_base() {
        assert_eq!(
            effective_glob_search_root("/outside/src/**/*.rs", Path::new("/repo")),
            PathBuf::from("/outside/src")
        );
        assert_eq!(
            effective_glob_search_root("src/**/*.rs", Path::new("/repo")),
            PathBuf::from("/repo")
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_deny_globs_are_anchored_to_absolute_ripgrep_targets() {
        assert_eq!(
            ripgrep_read_deny_pattern("secret/**", Path::new("/repo")),
            "**/repo/secret/**"
        );
        assert_eq!(
            ripgrep_read_deny_pattern("/private/**", Path::new("/repo/[literal]")),
            "**/repo/\\[literal\\]/private/**"
        );
        assert_eq!(
            ripgrep_read_deny_pattern("*.pem", Path::new("/repo")),
            "*.pem"
        );
        assert_eq!(
            ripgrep_read_deny_pattern("**/secret/**", Path::new("/repo")),
            "**/secret/**"
        );
    }

    #[test]
    fn read_deny_spellings_cover_cwd_and_outside_absolute_targets() {
        let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cwd_pattern = "/src/utils/**";
        let cwd_args = vec![
            "--files".to_string(),
            "--hidden".to_string(),
            "--no-ignore".to_string(),
            "--glob".to_string(),
            "src/utils/**".to_string(),
            "--glob".to_string(),
            format!("!{cwd_pattern}"),
            "--glob".to_string(),
            format!("!{}", ripgrep_read_deny_pattern(cwd_pattern, &cwd)),
        ];
        assert!(
            crate::utils::ripgrep::rip_grep(&cwd_args, &cwd, &AbortController::default(),)
                .unwrap()
                .is_empty()
        );

        let outside = std::env::temp_dir().join(format!(
            "cometix-deny-target-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(outside.join("blocked")).unwrap();
        std::fs::write(outside.join("blocked/secret.txt"), "secret").unwrap();
        let outside_pattern = "/blocked/**";
        let outside_args = vec![
            "--files".to_string(),
            "--hidden".to_string(),
            "--no-ignore".to_string(),
            "--glob".to_string(),
            "blocked/**".to_string(),
            "--glob".to_string(),
            format!("!{outside_pattern}"),
            "--glob".to_string(),
            format!("!{}", ripgrep_read_deny_pattern(outside_pattern, &outside)),
        ];
        assert!(
            crate::utils::ripgrep::rip_grep(&outside_args, &outside, &AbortController::default(),)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn glob_absolute_patterns_and_offset_limits_match_official_slicing() {
        let root = std::env::temp_dir().join(format!(
            "cometix-utils-glob-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        for name in ["a.txt", "b.txt", "nested/c.txt"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        let pattern = format!("{}/**/*.txt", root.display());
        let result = glob(
            &pattern,
            Path::new("/directory-that-is-not-used"),
            1,
            1,
            &AbortController::default(),
            &ToolPermissionContext::default(),
        )
        .expect("absolute glob succeeds");
        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].starts_with(&root));
        assert!(result.truncated);

        let tail = glob(
            &pattern,
            Path::new("/directory-that-is-not-used"),
            10,
            3,
            &AbortController::default(),
            &ToolPermissionContext::default(),
        )
        .expect("offset glob succeeds");
        assert!(tail.files.is_empty());
        assert!(!tail.truncated);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn glob_sorts_by_modification_time_oldest_first() {
        let root = std::env::temp_dir().join(format!(
            "cometix-utils-glob-sort-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let old = root.join("old.txt");
        let new = root.join("new.txt");
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        let now = std::time::SystemTime::now();
        std::fs::File::open(&old)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(now - std::time::Duration::from_secs(120)),
            )
            .unwrap();
        std::fs::File::open(&new)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(now))
            .unwrap();
        let result = glob(
            "*.txt",
            &root,
            100,
            0,
            &AbortController::default(),
            &ToolPermissionContext::default(),
        )
        .expect("sorted glob succeeds");
        assert_eq!(result.files, vec![old, new]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn glob_supports_official_ripgrep_wildcards_braces_and_literals() {
        let root = std::env::temp_dir().join(format!(
            "cometix-utils-glob-patterns-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        for name in [
            "a.rs",
            "b.ts",
            "a.md",
            "b.md",
            "file1.txt",
            "file22.txt",
            "nested/deep.rs",
        ] {
            std::fs::write(root.join(name), name).unwrap();
        }
        for (pattern, expected) in [
            ("*.{rs,ts}", vec!["a.rs", "b.ts", "nested/deep.rs"]),
            ("[ab].md", vec!["a.md", "b.md"]),
            ("file?.txt", vec!["file1.txt"]),
            ("a.rs", vec!["a.rs"]),
            ("**/nested/**", vec!["nested/deep.rs"]),
        ] {
            let result = glob(
                pattern,
                &root,
                100,
                0,
                &AbortController::default(),
                &ToolPermissionContext::default(),
            )
            .expect("pattern glob succeeds");
            let actual = result
                .files
                .iter()
                .filter_map(|path| path.strip_prefix(&root).ok())
                .map(|path| path.display().to_string())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                actual,
                expected.into_iter().map(str::to_string).collect(),
                "pattern={pattern:?}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn glob_hidden_and_no_ignore_environment_switches_default_true() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let keys = ["CLAUDE_CODE_GLOB_NO_IGNORE", "CLAUDE_CODE_GLOB_HIDDEN"];
        let _restore = keys.map(EnvVarGuard::preserve);
        let root = std::env::temp_dir().join(format!(
            "cometix-utils-glob-env-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("visible.txt"), "visible").unwrap();
        std::fs::write(root.join("ignored/ignored.txt"), "ignored").unwrap();
        std::fs::write(root.join(".hidden/hidden.txt"), "hidden").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();

        for key in keys {
            crate::utils::process_env::remove(key);
        }
        let defaults = glob(
            "**/*.txt",
            &root,
            100,
            0,
            &AbortController::default(),
            &ToolPermissionContext::default(),
        )
        .expect("default glob succeeds");
        let default_names = defaults
            .files
            .iter()
            .filter_map(|path| path.strip_prefix(&root).ok())
            .map(|path| path.display().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            default_names,
            [".hidden/hidden.txt", "ignored/ignored.txt", "visible.txt"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );

        for key in keys {
            crate::utils::process_env::set(key, "false");
        }
        let filtered = glob(
            "**/*.txt",
            &root,
            100,
            0,
            &AbortController::default(),
            &ToolPermissionContext::default(),
        )
        .expect("filtered glob succeeds");
        let filtered_names = filtered
            .files
            .iter()
            .filter_map(|path| path.strip_prefix(&root).ok())
            .map(|path| path.display().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            filtered_names,
            ["visible.txt".to_string()].into_iter().collect()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
