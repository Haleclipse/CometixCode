//! Native path expansion helpers.
//!
//! Maps to CC `utils/path.ts`. Permission-specific canonical/symlink checks
//! remain in `utils/permissions/path_validation.rs`.

use crate::utils::fs_operations::path::SugarPath;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

fn normalize_path(path: &Path) -> PathBuf {
    let normalized = path.normalize();
    PathBuf::from(normalized.to_string_lossy().nfc().collect::<String>())
}

#[cfg(windows)]
fn native_path(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/' {
        return format!(
            "{}:\\{}",
            (bytes[1] as char).to_ascii_uppercase(),
            &path[3..]
        );
    }
    path.to_string()
}

#[cfg(not(windows))]
fn native_path(path: &str) -> String {
    path.to_string()
}

/// Maps to the native node:path.relative dependency used by CC FileWriteTool
/// UI.tsx:59,307, fileSuggestions.ts:162,503,700 and pluginLoader.ts:338-340.
/// Both inputs resolve against process cwd, not against `from`. The existing
/// synchronous String carrier uses SugarPath's throwing/panicking form when
/// native cwd resolution fails; filesystem effect callers must remain fallible.
pub fn node_path_relative(from: &Path, to: &Path) -> String {
    to.relative(from).to_string_lossy().into_owned()
}

/// Maps to CC `utils/path.ts:32-83` `expandPath(...)`.
pub fn expand_path(path: &str, base_dir: Option<&Path>) -> Result<PathBuf, String> {
    // CC getCwd's async override is carried explicitly by base_dir (tool
    // callers pass cwd_override); its default is the canonical session cwd.
    // A virtual FsOperations.cwd must not override this source priority.
    let base = base_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::bootstrap::state::get_original_cwd);
    if path.contains('\0') || base.as_os_str().to_string_lossy().contains('\0') {
        return Err("Path contains null bytes".to_string());
    }

    // CC utils/path.ts:54-55 uses ECMAScript trim: FEFF is whitespace,
    // whereas U+0085 is a literal path character (unlike Rust str::trim).
    let trimmed = path.trim_matches(|character| {
        matches!(character,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    });
    if trimmed.is_empty() {
        return Ok(normalize_path(&base));
    }
    let home = crate::utils::process_env::var_os("HOME")
        .or_else(|| crate::utils::process_env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if trimmed == "~" {
        return Ok(normalize_path(home.as_deref().unwrap_or(Path::new("~"))));
    }
    if let Some(suffix) = trimmed.strip_prefix("~/") {
        if let Some(home) = home {
            return Ok(normalize_path(
                &crate::utils::fs_operations::native::join_path(&home, Path::new(suffix)),
            ));
        }
    }

    let processed = PathBuf::from(native_path(trimmed));
    if crate::utils::fs_operations::native::is_absolute(&processed) {
        Ok(normalize_path(&processed))
    } else {
        let resolved = crate::utils::fs_operations::native::resolve_path(&base, &processed)
            .map_err(|error| error.to_string())?;
        Ok(PathBuf::from(
            resolved.to_string_lossy().nfc().collect::<String>(),
        ))
    }
}

/// Maps to CC `getDirectoryForPath(path)`.
pub fn get_directory_for_path(path: &str) -> String {
    let absolute = expand_path(path, None).unwrap_or_else(|_| PathBuf::from(path));
    let display = absolute.display().to_string();
    if !(display.starts_with("\\\\") || display.starts_with("//")) {
        if crate::utils::fs_operations::get_fs_implementation()
            .stat_sync(&absolute)
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return display;
        }
    }
    crate::utils::fs_operations::native::dirname(&absolute)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_path_resolves_relative_segments_without_filesystem_canonicalization() {
        assert_eq!(
            expand_path("./src/../Cargo.toml", Some(Path::new("/repo"))).unwrap(),
            PathBuf::from("/repo/Cargo.toml")
        );
    }

    #[test]
    fn directory_for_path_keeps_existing_directories_and_uses_file_parent() {
        let directory = std::env::temp_dir().join(format!(
            "cometix-path-directory-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        assert_eq!(
            get_directory_for_path(&directory.display().to_string()),
            directory.display().to_string()
        );
        assert_eq!(
            get_directory_for_path(&directory.join("missing.txt").display().to_string()),
            directory.display().to_string()
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// CC utils/path.ts:54-81 uses ECMAScript trim, then NFC normalization.
    #[test]
    fn expand_path_matches_official_ecmascript_trim_and_nfc() {
        assert_eq!(
            expand_path("\u{feff}cafe\u{301}\u{feff}", Some(Path::new("/repo"))).unwrap(),
            PathBuf::from("/repo/caf\u{e9}")
        );
        assert_eq!(
            expand_path("\u{85}", Some(Path::new("/repo"))).unwrap(),
            PathBuf::from("/repo/\u{85}")
        );
        assert_eq!(
            expand_path(" \t\u{feff}", Some(Path::new("/repo"))).unwrap(),
            PathBuf::from("/repo")
        );
    }

    #[cfg(unix)]
    #[test]
    fn expand_path_matches_official_normalize_and_resolve_branches() {
        // CC utils/path.ts:55,77 preserve normalize's tail; :81 uses resolve.
        assert_eq!(
            expand_path("", Some(Path::new("/repo/")))
                .unwrap()
                .as_os_str(),
            "/repo/"
        );
        assert_eq!(
            expand_path("/repo/a/../", None).unwrap().as_os_str(),
            "/repo/"
        );
        assert_eq!(
            expand_path("a/", Some(Path::new("/repo")))
                .unwrap()
                .as_os_str(),
            "/repo/a"
        );
        assert_eq!(
            expand_path("", Some(Path::new("../../a")))
                .unwrap()
                .as_os_str(),
            "../../a"
        );
    }

    #[test]
    fn expand_path_rejects_null_bytes() {
        assert_eq!(
            expand_path("bad\0path", Some(Path::new("/repo"))).unwrap_err(),
            "Path contains null bytes"
        );
    }

    /// Maps to: Node `path.relative` oracles — inside cwd, outside cwd
    /// (`..` climb), equal paths (`""`), and a relative `to` resolved
    /// against process cwd (CC native path dependency).
    #[test]
    fn node_path_relative_matches_node_oracles() {
        let from = Path::new("/repo/project");
        assert_eq!(
            node_path_relative(from, Path::new("/repo/project/src/a.rs")),
            "src/a.rs"
        );
        assert_eq!(
            node_path_relative(from, Path::new("/repo/elsewhere/out.txt")),
            "../elsewhere/out.txt"
        );
        assert_eq!(node_path_relative(from, Path::new("/repo/project")), "");
        // Bun resolves relative `to` against process cwd, independently of from.
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(node_path_relative(&cwd, Path::new("a.txt")), "a.txt");
        assert_eq!(node_path_relative(Path::new("a"), Path::new("a")), "");
        assert_eq!(
            node_path_relative(Path::new("a"), Path::new("../a")),
            "../../a"
        );
    }
}

/// Maps to: CC `utils/path.ts:133-135#containsPathTraversal`.
pub fn contains_path_traversal(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| segment == "..")
}
