//! Team-memory path helpers and write-containment validation.
//!
//! Maps to: CC `memdir/teamMemPaths.ts`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// Maps to CC `PathTraversalError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathTraversalError(pub String);

impl fmt::Display for PathTraversalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PathTraversalError {}

fn percent_decode_uri_component(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(());
        }
        let hex = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        };
        let Some(high) = hex(bytes[index + 1]) else {
            return Err(());
        };
        let Some(low) = hex(bytes[index + 2]) else {
            return Err(());
        };
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| ())
}

/// Maps to CC `sanitizePathKey(...)`.
fn sanitize_path_key(key: &str) -> Result<&str, PathTraversalError> {
    if key.contains('\0') {
        return Err(PathTraversalError(format!(
            "Null byte in path key: \"{key}\""
        )));
    }
    let decoded = percent_decode_uri_component(key).unwrap_or_else(|_| key.to_string());
    if decoded != key && (decoded.contains("..") || decoded.contains('/')) {
        return Err(PathTraversalError(format!(
            "URL-encoded traversal in path key: \"{key}\""
        )));
    }
    let normalized = key.nfkc().collect::<String>();
    if normalized != key
        && (normalized.contains("..")
            || normalized.contains('/')
            || normalized.contains('\\')
            || normalized.contains('\0'))
    {
        return Err(PathTraversalError(format!(
            "Unicode-normalized traversal in path key: \"{key}\""
        )));
    }
    if key.contains('\\') {
        return Err(PathTraversalError(format!(
            "Backslash in path key: \"{key}\""
        )));
    }
    if key.starts_with('/') {
        return Err(PathTraversalError(format!("Absolute path key: \"{key}\"")));
    }
    Ok(key)
}

/// Maps to CC `isTeamMemoryEnabled()`.
///
/// Cometix resolves the `tengu_herring_clock` cohort from the source-controlled
/// switch table instead of GrowthBook.
pub fn is_team_memory_enabled() -> bool {
    if !crate::memdir::paths::is_auto_memory_enabled() {
        return false;
    }
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::TeamMemory,
    )
}

/// Maps to CC `getTeamMemPath()`.
pub fn get_team_mem_path() -> PathBuf {
    let path = crate::memdir::paths::get_auto_mem_path().join("team");
    PathBuf::from(path.to_string_lossy().nfc().collect::<String>())
}

/// Maps to CC `getTeamMemEntrypoint()`.
pub fn get_team_mem_entrypoint() -> PathBuf {
    get_team_mem_path().join("MEMORY.md")
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn resolve_lexically(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_lexically(path)
    } else {
        normalize_lexically(
            &std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path),
        )
    }
}

/// Maps to CC `isTeamMemPath(filePath)` (lexical containment only).
pub fn is_team_mem_path(file_path: &Path) -> bool {
    let resolved = resolve_lexically(file_path);
    let team_dir = get_team_mem_path();
    resolved != team_dir && resolved.starts_with(team_dir)
}

fn errno_code(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::NotADirectory => "ENOTDIR",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        _ => {
            #[cfg(unix)]
            {
                match error.raw_os_error() {
                    Some(36) | Some(63) => "ENAMETOOLONG",
                    Some(40) | Some(62) => "ELOOP",
                    Some(5) => "EIO",
                    _ => "UNKNOWN",
                }
            }
            #[cfg(not(unix))]
            {
                "UNKNOWN"
            }
        }
    }
}

/// Maps to CC `realpathDeepestExisting(...)`.
fn realpath_deepest_existing(absolute_path: &Path) -> Result<PathBuf, PathTraversalError> {
    let mut tail: Vec<OsString> = Vec::new();
    let mut current = absolute_path.to_path_buf();
    loop {
        match std::fs::canonicalize(&current) {
            Ok(real_current) => {
                return Ok(tail
                    .iter()
                    .rev()
                    .fold(real_current, |path, segment| path.join(segment)));
            }
            Err(error) => {
                let code = errno_code(&error);
                if code == "ENOENT" {
                    if let Ok(metadata) = std::fs::symlink_metadata(&current) {
                        if metadata.file_type().is_symlink() {
                            return Err(PathTraversalError(format!(
                                "Dangling symlink detected (target does not exist): \"{}\"",
                                current.display()
                            )));
                        }
                    }
                } else if code == "ELOOP" {
                    return Err(PathTraversalError(format!(
                        "Symlink loop detected in path: \"{}\"",
                        current.display()
                    )));
                } else if code != "ENOTDIR" && code != "ENAMETOOLONG" {
                    return Err(PathTraversalError(format!(
                        "Cannot verify path containment ({code}): \"{}\"",
                        current.display()
                    )));
                }
            }
        }
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            break;
        };
        if parent == current {
            break;
        }
        if let Some(segment) = current.file_name() {
            tail.push(segment.to_os_string());
        }
        current = parent;
    }
    Ok(absolute_path.to_path_buf())
}

/// Maps to CC `isRealPathWithinTeamDir(...)`.
fn is_real_path_within_team_dir(real_candidate: &Path) -> bool {
    let real_team_dir = match std::fs::canonicalize(get_team_mem_path()) {
        Ok(path) => path,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return true;
        }
        Err(_) => return false,
    };
    real_candidate == real_team_dir || real_candidate.starts_with(real_team_dir)
}

/// Maps to CC `validateTeamMemWritePath(...)`.
pub async fn validate_team_mem_write_path(file_path: &Path) -> Result<PathBuf, PathTraversalError> {
    if file_path.as_os_str().to_string_lossy().contains('\0') {
        return Err(PathTraversalError(format!(
            "Null byte in path: \"{}\"",
            file_path.display()
        )));
    }
    let resolved = resolve_lexically(file_path);
    let team_dir = get_team_mem_path();
    if resolved == team_dir || !resolved.starts_with(&team_dir) {
        return Err(PathTraversalError(format!(
            "Path escapes team memory directory: \"{}\"",
            file_path.display()
        )));
    }
    let real_path = realpath_deepest_existing(&resolved)?;
    if !is_real_path_within_team_dir(&real_path) {
        return Err(PathTraversalError(format!(
            "Path escapes team memory directory via symlink: \"{}\"",
            file_path.display()
        )));
    }
    Ok(resolved)
}

/// Maps to CC `validateTeamMemKey(...)`.
pub async fn validate_team_mem_key(relative_key: &str) -> Result<PathBuf, PathTraversalError> {
    sanitize_path_key(relative_key)?;
    let team_dir = get_team_mem_path();
    let resolved = resolve_lexically(&team_dir.join(relative_key));
    if resolved == team_dir || !resolved.starts_with(&team_dir) {
        return Err(PathTraversalError(format!(
            "Key escapes team memory directory: \"{relative_key}\""
        )));
    }
    let real_path = realpath_deepest_existing(&resolved)?;
    if !is_real_path_within_team_dir(&real_path) {
        return Err(PathTraversalError(format!(
            "Key escapes team memory directory via symlink: \"{relative_key}\""
        )));
    }
    Ok(resolved)
}

/// Maps to CC `isTeamMemFile(filePath)`.
pub fn is_team_mem_file(file_path: &Path) -> bool {
    is_team_memory_enabled() && is_team_mem_path(file_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn team_memory_prefix_requires_a_path_component_boundary_and_uses_override() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-team-mem-path-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _override = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &root);
        let team = get_team_mem_path();
        assert_eq!(team, root.join("team"));
        assert!(is_team_mem_path(&team.join("MEMORY.md")));
        assert!(!is_team_mem_path(&team));
        assert!(!is_team_mem_path(&root.join("team-evil/MEMORY.md")));
    }

    #[tokio::test]
    async fn team_memory_key_rejects_encoded_unicode_and_lexical_traversal() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-team-mem-key-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _override = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &root);
        for key in ["%2e%2e%2fsecret", "．．／secret", "..\\secret", "/secret"] {
            assert!(
                validate_team_mem_key(key).await.is_err(),
                "accepted {key:?}"
            );
        }
        assert!(validate_team_mem_key("../secret").await.is_err());
        assert_eq!(
            validate_team_mem_key("notes/topic.md").await.unwrap(),
            root.join("team/notes/topic.md")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn team_memory_write_validation_rejects_symlink_escapes_and_dangling_links() {
        use std::os::unix::fs::symlink;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-team-mem-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-team-mem-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _override = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &root);
        std::fs::create_dir_all(root.join("team")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("team/escape")).unwrap();
        assert!(
            validate_team_mem_write_path(&root.join("team/escape/file.md"))
                .await
                .is_err()
        );
        symlink(outside.join("missing"), root.join("team/dangling")).unwrap();
        assert!(
            validate_team_mem_write_path(&root.join("team/dangling"))
                .await
                .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }
}
