//! Maps to: CC `tools/PowerShellTool/gitSafety.ts`.
//!
//! Git can be weaponized for sandbox escape via two vectors:
//! 1. Bare-repo attack: if cwd contains HEAD + objects/ + refs/ but no valid
//!    .git/HEAD, Git treats cwd as a bare repository and runs hooks from cwd.
//! 2. Git-internal write + git: a compound command creates HEAD/objects/refs/
//!    hooks/ then runs git — the git subcommand executes the freshly-created
//!    malicious hooks.

use regex::Regex;
use std::sync::LazyLock;

pub use crate::utils::powershell::parser::PS_TOKENIZER_DASH_CHARS;

const GIT_INTERNAL_PREFIXES: [&str; 4] = ["head", "objects", "refs", "hooks"];

static FILESYSTEM_PROVIDER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:[A-Za-z0-9_.]+\\){0,3}FileSystem::").expect("valid provider prefix regex")
});
static DRIVE_RELATIVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]:($|[^/\\])").expect("valid drive-relative regex"));
static SHORT_NAME_GIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^git~\d+($|/)").expect("valid 8.3 short name regex"));
static DRIVE_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z]:").expect("valid drive prefix regex"));

fn cwd() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

/// Maps to: CC `gitSafety.ts:23-38#resolveCwdReentry`.
fn resolve_cwd_reentry(normalized: &str) -> String {
    if !normalized.starts_with("../") {
        return normalized.to_string();
    }
    let cwd = cwd();
    let cwd_base = std::path::Path::new(&cwd)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if cwd_base.is_empty() {
        return normalized.to_string();
    }
    let prefix = format!("../{cwd_base}/");
    let mut value = normalized.to_string();
    while let Some(rest) = value.strip_prefix(&prefix) {
        value = rest.to_string();
    }
    if value == format!("../{cwd_base}") {
        return ".".to_string();
    }
    value
}

/// Maps to: CC `path.posix.normalize(path)` as used by `normalizeGitPathArg`.
fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let is_absolute = path.starts_with('/');
    let has_trailing_slash = path.ends_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => match segments.last() {
                Some(last) if *last != ".." => {
                    segments.pop();
                }
                _ => {
                    if !is_absolute {
                        segments.push("..");
                    }
                }
            },
            other => segments.push(other),
        }
    }
    let mut joined = segments.join("/");
    if joined.is_empty() {
        return if is_absolute {
            "/".to_string()
        } else if has_trailing_slash {
            "./".to_string()
        } else {
            ".".to_string()
        };
    }
    if has_trailing_slash {
        joined.push('/');
    }
    if is_absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Maps to: CC `gitSafety.ts:48-87#normalizeGitPathArg`.
fn normalize_git_path_arg(arg: &str) -> String {
    let mut value = arg.to_string();

    if value
        .chars()
        .next()
        .is_some_and(|first| PS_TOKENIZER_DASH_CHARS.contains(&first) || first == '/')
        && let Some(colon) = value
            .char_indices()
            .skip(1)
            .find_map(|(index, character)| (character == ':').then_some(index))
    {
        value = value[colon + 1..].to_string();
    }

    value = value
        .strip_prefix(['\'', '"'])
        .unwrap_or(&value)
        .to_string();
    value = value
        .strip_suffix(['\'', '"'])
        .unwrap_or(&value)
        .to_string();
    value = value.replace('`', "");
    value = FILESYSTEM_PROVIDER_RE.replace(&value, "").to_string();
    if DRIVE_RELATIVE_RE.is_match(&value) {
        value = value[2..].to_string();
    }
    value = value.replace('\\', "/");

    // Win32 CreateFileW per-component: iteratively strip trailing spaces, then
    // trailing dots, stopping if the result is `.` or `..`.
    value = value
        .split('/')
        .map(|component| {
            if component.is_empty() {
                return String::new();
            }
            let mut current = component.to_string();
            loop {
                let previous = current.clone();
                current = current.trim_end_matches(' ').to_string();
                if current == "." || current == ".." {
                    return current;
                }
                current = current.trim_end_matches('.').to_string();
                if current == previous {
                    break;
                }
            }
            if current.is_empty() {
                ".".to_string()
            } else {
                current
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    value = posix_normalize(&value);
    if let Some(rest) = value.strip_prefix("./") {
        value = rest.to_string();
    }
    value.to_lowercase()
}

/// Maps to: CC `gitSafety.ts:106-122#resolveEscapingPathToCwdRelative`.
fn resolve_escaping_path_to_cwd_relative(normalized: &str) -> Option<String> {
    let cwd = cwd();
    if cwd.is_empty() {
        return None;
    }
    let cwd_forward = cwd.replace('\\', "/");
    let absolute = if normalized.starts_with('/') || DRIVE_PREFIX_RE.is_match(normalized) {
        posix_normalize(normalized)
    } else {
        posix_normalize(&format!(
            "{}/{normalized}",
            cwd_forward.trim_end_matches('/')
        ))
    };
    let cwd_with_separator = format!("{}/", cwd_forward.trim_end_matches('/'));
    let absolute_lower = absolute.to_lowercase();
    let cwd_lower = cwd_forward.trim_end_matches('/').to_lowercase();
    let cwd_with_separator_lower = cwd_with_separator.to_lowercase();
    if absolute_lower == cwd_lower {
        return Some(".".to_string());
    }
    absolute_lower
        .strip_prefix(&cwd_with_separator_lower)
        .map(|rest| rest.replace('\\', "/"))
}

/// Maps to: CC `gitSafety.ts:124-132#matchesGitInternalPrefix`.
fn matches_git_internal_prefix(normalized: &str) -> bool {
    if normalized == "head" || normalized == ".git" {
        return true;
    }
    if normalized.starts_with(".git/") || SHORT_NAME_GIT_RE.is_match(normalized) {
        return true;
    }
    GIT_INTERNAL_PREFIXES.iter().any(|prefix| {
        *prefix != "head"
            && (normalized == *prefix || normalized.starts_with(&format!("{prefix}/")))
    })
}

/// Maps to: CC `gitSafety.ts:170-176#matchesDotGitPrefix`.
fn matches_dot_git_prefix(normalized: &str) -> bool {
    normalized == ".git"
        || normalized.starts_with(".git/")
        || SHORT_NAME_GIT_RE.is_match(normalized)
}

fn escapes_cwd(normalized: &str) -> bool {
    normalized.starts_with("../")
        || normalized.starts_with('/')
        || DRIVE_PREFIX_RE.is_match(normalized)
}

/// Maps to: CC `gitSafety.ts:139-151#isGitInternalPathPS`.
pub fn is_git_internal_path_ps(arg: &str) -> bool {
    let normalized = resolve_cwd_reentry(&normalize_git_path_arg(arg));
    if matches_git_internal_prefix(&normalized) {
        return true;
    }
    if escapes_cwd(&normalized)
        && let Some(relative) = resolve_escaping_path_to_cwd_relative(&normalized)
    {
        return matches_git_internal_prefix(&relative);
    }
    false
}

/// Maps to: CC `gitSafety.ts:158-168#isDotGitPathPS`.
pub fn is_dot_git_path_ps(arg: &str) -> bool {
    let normalized = resolve_cwd_reentry(&normalize_git_path_arg(arg));
    if matches_dot_git_prefix(&normalized) {
        return true;
    }
    if escapes_cwd(&normalized)
        && let Some(relative) = resolve_escaping_path_to_cwd_relative(&normalized)
    {
        return matches_dot_git_prefix(&relative);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_internal_paths_match_official_prefix_set() {
        for arg in [
            "HEAD",
            "hooks/pre-commit",
            "refs",
            "objects/ab",
            ".git",
            ".git/config",
            "GIT~1/hooks",
            "./hooks/pre-commit",
            ".\\hooks\\pre-commit",
            "'hooks/pre-commit'",
            "-Path:hooks/pre-commit",
            "FileSystem::hooks/pre-commit",
            "hoo`ks/pre-commit",
            "hooks /pre-commit",
        ] {
            assert!(is_git_internal_path_ps(arg), "arg={arg:?}");
        }

        for arg in ["src/main.rs", "headers/x.h", "../../etc/passwd"] {
            assert!(!is_git_internal_path_ps(arg), "arg={arg:?}");
        }
    }

    #[test]
    fn dot_git_paths_exclude_bare_repo_shaped_names() {
        assert!(is_dot_git_path_ps(".git/hooks/pre-commit"));
        assert!(is_dot_git_path_ps("git~1/config"));
        assert!(!is_dot_git_path_ps("hooks/pre-commit"));
        assert!(!is_dot_git_path_ps("HEAD"));
    }

    #[test]
    fn cwd_reentry_resolves_back_into_the_working_directory() {
        let cwd = cwd();
        let base = std::path::Path::new(&cwd)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if base.is_empty() {
            return;
        }
        assert!(is_git_internal_path_ps(&format!("../{base}/hooks")));
        assert!(is_dot_git_path_ps(&format!("../{base}/.git/config")));
    }
}
