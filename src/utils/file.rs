use crate::utils::fs_operations::{get_fs_implementation, safe_resolve_path};

pub use crate::utils::file_read::{FileEncoding, LineEndingType};

/// Maps to: CC `utils/file.ts` `MAX_OUTPUT_SIZE` — 0.25 MiB default text-read gate.
pub const MAX_OUTPUT_SIZE: u64 = 262_144;

/// Maps to CC `getFileModificationTime(filePath)` floor-to-milliseconds rule.
pub fn get_file_modification_time_result(path: &std::path::Path) -> std::io::Result<i64> {
    Ok(get_fs_implementation().stat_sync(path)?.mtime_ms.floor() as i64)
}

pub fn get_file_modification_time(path: &std::path::Path) -> Option<i64> {
    get_file_modification_time_result(path).ok()
}

/// Maps to CC `detectFileEncoding(filePath)`.
pub fn detect_file_encoding(path: &std::path::Path) -> FileEncoding {
    let fs = get_fs_implementation();
    let resolved = safe_resolve_path(fs.as_ref(), path);
    match crate::utils::file_read::detect_encoding_for_resolved_path(&resolved.resolved_path) {
        Ok(encoding) => encoding,
        Err(error) => {
            if crate::utils::errors::is_fs_inaccessible(&error) {
                crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "detectFileEncoding failed for expected reason: {}",
                        crate::utils::errors::io_errno_code(&error).unwrap_or("undefined")
                    ),
                    crate::utils::debug::DebugLogLevel::Debug,
                );
            } else {
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            }
            FileEncoding::Utf8
        }
    }
}

/// Maps to CC `detectLineEndings(filePath, encoding)` (first 4096 bytes;
/// CRLF wins only when its count is strictly greater than bare LF).
pub fn detect_line_endings(path: &std::path::Path, encoding: FileEncoding) -> LineEndingType {
    let fs = get_fs_implementation();
    let resolved = safe_resolve_path(fs.as_ref(), path);
    let sample = fs.read_sync(&resolved.resolved_path, 4096);
    match sample {
        Ok(sample) => {
            let bytes = &sample.buffer[..sample.bytes_read];
            let content = match encoding {
                FileEncoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
                FileEncoding::Utf16Le => String::from_utf16_lossy(
                    &bytes
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect::<Vec<_>>(),
                ),
            };
            crate::utils::file_read::detect_line_endings_for_string(&content)
        }
        Err(error) => {
            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            LineEndingType::Lf
        }
    }
}

fn encode_text_content(content: &str, encoding: FileEncoding) -> Vec<u8> {
    match encoding {
        FileEncoding::Utf8 => content.as_bytes().to_vec(),
        FileEncoding::Utf16Le => content
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    }
}

fn write_and_flush(mut file: std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_write_new(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    write_and_flush(file, bytes)
}

fn sync_write_no_follow(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "refusing to write to a non-regular file",
        ));
    }
    write_and_flush(file, bytes)
}

/// Maps to CC `writeFileSyncAndFlushDeprecated(...)`.
///
/// The temporary file is placed beside the destination and atomically renamed.
/// If that replacement fails, CC falls back to a flushed direct write. A direct
/// symlink target is resolved first so rename does not replace the link itself.
pub fn write_file_sync_and_flush_deprecated(
    file_path: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    let fs = get_fs_implementation();
    let target_path = fs
        .readlink_sync(file_path)
        .ok()
        .and_then(|target| {
            if crate::utils::fs_operations::native::is_absolute(&target) {
                Some(target)
            } else {
                crate::utils::fs_operations::native::resolve_path(
                    &crate::utils::fs_operations::native::dirname(file_path),
                    &target,
                )
                .ok()
            }
        })
        .inspect(|target| {
            crate::utils::debug::log_for_debugging(&format!(
                "Writing through symlink: {} -> {}",
                file_path.display(),
                target.display()
            ));
        })
        .unwrap_or_else(|| file_path.to_path_buf());
    write_file_sync_and_flush_to_target(&target_path, bytes)
}

/// Security-preserving variant for a destination pinned by the caller after
/// permission evaluation. Unlike the source-shaped wrapper above, this does
/// not re-resolve a mutable symlink immediately before replacement.
pub fn write_file_sync_and_flush_to_target(
    target_path: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    let fs = get_fs_implementation();
    // SECURITY DEVIATION: CC uses a predictable pid/timestamp name and opens
    // it with truncation. A pre-created symlink could redirect that write.
    // Use an unguessable adjacent name plus `create_new` so no existing path
    // (including a symlink) is ever followed.
    let temporary_path = std::path::PathBuf::from(format!(
        "{}.tmp.{}.{}",
        target_path.display(),
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));

    let existing_permissions = match fs.stat_sync(target_path) {
        Ok(metadata) => Some(metadata.mode),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let atomic_result = (|| {
        sync_write_new(&temporary_path, bytes)?;
        if let Some(permissions) = existing_permissions.clone() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &temporary_path,
                    std::fs::Permissions::from_mode(permissions),
                )?;
            }
            #[cfg(windows)]
            {
                let mut attributes = std::fs::metadata(&temporary_path)?.permissions();
                attributes.set_readonly(permissions & 0o200 == 0);
                std::fs::set_permissions(&temporary_path, attributes)?;
            }
        }
        fs.rename_sync(&temporary_path, target_path)
    })();

    if let Err(error) = atomic_result {
        crate::utils::debug::log_for_debugging(&format!(
            "Atomic file write failed, falling back to direct write: {error}"
        ));
        let _ = fs.unlink_sync(&temporary_path);
        return sync_write_no_follow(target_path, bytes);
    }
    Ok(())
}

/// Maps to CC `writeTextContent(filePath, content, encoding, endings)`.
pub fn write_text_content(
    path: &std::path::Path,
    content: &str,
    encoding: FileEncoding,
    endings: LineEndingType,
) -> std::io::Result<()> {
    let text = text_with_line_endings(content, endings);
    write_file_sync_and_flush_deprecated(path, &encode_text_content(&text, encoding))
}

pub fn write_text_content_to_target(
    target_path: &std::path::Path,
    content: &str,
    encoding: FileEncoding,
    endings: LineEndingType,
) -> std::io::Result<()> {
    let text = text_with_line_endings(content, endings);
    write_file_sync_and_flush_to_target(target_path, &encode_text_content(&text, encoding))
}

fn text_with_line_endings(content: &str, endings: LineEndingType) -> String {
    if endings == LineEndingType::CrLf {
        content.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        content.to_string()
    }
}

/// Maps to CC `utils/file.ts:335-343#isDirEmpty`.
pub fn is_dir_empty(path: &std::path::Path) -> bool {
    match get_fs_implementation().is_dir_empty_sync(path) {
        Ok(empty) => empty,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Maps to CC `utils/file.ts#findSimilarFile`.
pub fn find_similar_file(file_path: &std::path::Path) -> Option<String> {
    use crate::utils::fs_operations::native;
    let fs = get_fs_implementation();
    let directory = native::dirname(file_path);
    let requested_stem = filename_without_extension(file_path);
    let entries = match fs.readdir_sync(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            }
            return None;
        }
    };
    entries.into_iter().find_map(|entry| {
        let name = entry.file_name();
        let candidate = native::join_path(&directory, std::path::Path::new(&name));
        (filename_without_extension(std::path::Path::new(&name)) == requested_stem
            && candidate.as_os_str() != file_path.as_os_str())
        .then(|| name.to_string_lossy().into_owned())
    })
}

fn filename_without_extension(path: &std::path::Path) -> std::ffi::OsString {
    use crate::utils::fs_operations::native;
    let name = native::basename(path);
    let extension = native::extname(path);
    let bytes = name.as_encoded_bytes();
    // Both boundaries come from ASCII Node path delimiters in the same native
    // string. No UTF-8/WTF-8 code point is split by removing the extension.
    unsafe {
        std::ffi::OsString::from_encoded_bytes_unchecked(
            bytes[..bytes.len() - extension.as_encoded_bytes().len()].to_vec(),
        )
    }
}

/// Maps to CC `utils/file.ts#suggestPathUnderCwd`.
///
/// CC awaits `realpath`/`stat`; this synchronous projection runs during the
/// existing synchronous Rust validation boundary and preserves the same path
/// predicates and return value.
pub fn suggest_path_under_cwd(
    requested_path: &std::path::Path,
    cwd: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let cwd_parent = cwd.parent()?;
    let resolved_path = requested_path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .and_then(|parent| requested_path.file_name().map(|name| parent.join(name)))
        .unwrap_or_else(|| requested_path.to_path_buf());

    if resolved_path == cwd_parent
        || !resolved_path.starts_with(cwd_parent)
        || resolved_path.starts_with(cwd)
        || resolved_path == cwd
    {
        return None;
    }
    let relative = resolved_path.strip_prefix(cwd_parent).ok()?;
    let corrected = cwd.join(relative);
    std::fs::metadata(&corrected).ok().map(|_| corrected)
}

/// Maps to: CC `utils/file.ts` `FILE_NOT_FOUND_CWD_NOTE`.
pub const FILE_NOT_FOUND_CWD_NOTE: &str = "Note: your current working directory is";

/// Maps to CC `utils/file.ts:278-285` `isCompactLinePrefixEnabled()`.
pub fn is_compact_line_prefix_enabled() -> bool {
    // 3P default: killswitch off = compact format enabled. Cometix resolves the
    // killswitch from the source-controlled switch table instead of GrowthBook.
    !crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::CompactLinePrefixKillswitch,
    )
}

// Mechanical serde_json carriers for the JavaScript `index + startLine`
// operator written inline in CC `utils/file.ts:290-319` `addLineNumbers`.
// ECMAScript Number stringification delegates directly to `ryu-js`.
fn javascript_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value
            .as_f64()
            .map(|value| ryu_js::Buffer::new().format(value).to_string())
            .unwrap_or_else(|| value.to_string()),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| match value {
                serde_json::Value::Null => String::new(),
                value => javascript_value_to_string(value),
            })
            .collect::<Vec<_>>()
            .join(","),
        serde_json::Value::Object(_) => "[object Object]".to_string(),
    }
}

/// Format file content with line numbers for the model-visible Read result.
/// Maps to: CC `utils/file.ts:290-319` `addLineNumbers` — including raw
/// JavaScript `index + startLine` coercion after authoritative hook updates.
pub fn add_line_numbers(content: &str, start_line: &serde_json::Value) -> String {
    if content.is_empty() {
        return String::new();
    }
    let lines = content
        .split(['\n'])
        .map(|line| line.strip_suffix('\r').unwrap_or(line));
    if is_compact_line_prefix_enabled() {
        return lines
            .enumerate()
            .map(|(index, line)| {
                let number = match start_line {
                    serde_json::Value::Number(number) => ryu_js::Buffer::new()
                        .format(index as f64 + number.as_f64().unwrap_or(f64::NAN))
                        .to_string(),
                    serde_json::Value::Bool(value) => ryu_js::Buffer::new()
                        .format(index as f64 + f64::from(u8::from(*value)))
                        .to_string(),
                    serde_json::Value::Null => {
                        ryu_js::Buffer::new().format(index as f64).to_string()
                    }
                    serde_json::Value::String(value) => format!("{index}{value}"),
                    serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                        format!("{index}{}", javascript_value_to_string(start_line))
                    }
                };
                format!("{number}\t{line}")
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    lines
        .enumerate()
        .map(|(index, line)| {
            let num_str = match start_line {
                serde_json::Value::Number(number) => ryu_js::Buffer::new()
                    .format(index as f64 + number.as_f64().unwrap_or(f64::NAN))
                    .to_string(),
                serde_json::Value::Bool(value) => ryu_js::Buffer::new()
                    .format(index as f64 + f64::from(u8::from(*value)))
                    .to_string(),
                serde_json::Value::Null => ryu_js::Buffer::new().format(index as f64).to_string(),
                serde_json::Value::String(value) => format!("{index}{value}"),
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                    format!("{index}{}", javascript_value_to_string(start_line))
                }
            };
            if num_str.len() >= 6 {
                format!("{num_str}→{line}")
            } else {
                format!("{num_str:>6}→{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Maps to: CC `utils/file.ts` `getDisplayPath` — prefer cwd-relative, else `~`.
pub fn get_display_path(path: &str) -> String {
    let cwd = crate::bootstrap::state::get_original_cwd();
    let absolute = expand_path_for_display(path, &cwd);
    let absolute_path = std::path::Path::new(&absolute);
    if let Ok(relative) = absolute_path.strip_prefix(&cwd) {
        let relative = relative.to_string_lossy().replace('\\', "/");
        // Node `path.relative` yields `""` for cwd itself; keep that shape.
        if !relative.starts_with("..") {
            return relative;
        }
    }
    if let (Ok(canon_path), Ok(canon_cwd)) = (absolute_path.canonicalize(), cwd.canonicalize()) {
        if let Ok(relative) = canon_path.strip_prefix(&canon_cwd) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !relative.starts_with("..") {
                return relative;
            }
        }
    }

    let home = crate::utils::node_os::homedir()
        .to_string_lossy()
        .into_owned();
    let home_prefix = format!("{home}{}", std::path::MAIN_SEPARATOR);
    if absolute.starts_with(&home_prefix) || path.starts_with(&home_prefix) {
        let source = if absolute.starts_with(&home_prefix) {
            absolute.as_str()
        } else {
            path
        };
        return format!("~{}", &source[home.len()..].replace('\\', "/"));
    }
    if absolute == home || path == home {
        return "~".to_string();
    }
    path.to_string()
}

/// Maps to: CC `utils/path.ts` `expandPath` (display subset: `~`, absolute, cwd-join).
fn expand_path_for_display(path: &str, cwd: &std::path::Path) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return cwd.to_string_lossy().replace('\\', "/");
    }
    let home = || {
        crate::utils::node_os::homedir()
            .to_string_lossy()
            .into_owned()
    };
    if trimmed == "~" {
        return home();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return format!("{}/{rest}", home()).replace('\\', "/");
    }
    if cfg!(windows) {
        if let Some(rest) = trimmed.strip_prefix("~\\") {
            return format!("{}\\{rest}", home());
        }
    }
    let candidate = std::path::Path::new(trimmed);
    if candidate.is_absolute() {
        return trimmed.replace('\\', "/");
    }
    cwd.join(candidate).to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    #[test]
    fn filename_without_extension_matches_official_node_literal_names() {
        for (input, expected) in [
            ("a.ts", "a"),
            ("a.", "a"),
            (".env", ".env"),
            ("..foo", "."),
            ("...", ".."),
            ("..", ".."),
            (".", "."),
            ("dir/a.ts/", "a"),
            ("dir/.env.local", ".env"),
        ] {
            assert_eq!(
                super::filename_without_extension(std::path::Path::new(input)),
                std::ffi::OsString::from(expected),
                "{input}"
            );
        }
    }

    use super::*;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};

    #[cfg(unix)]
    #[test]
    fn flushed_atomic_write_follows_symlink_without_replacing_link() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let root = std::env::temp_dir().join(format!(
            "cometix-file-atomic-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        let link = root.join("link.txt");
        std::fs::write(&target, "old").unwrap();
        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        permissions.set_mode(0o640);
        std::fs::set_permissions(&target, permissions).unwrap();
        symlink("target.txt", &link).unwrap();

        write_text_content(&link, "new\n", FileEncoding::Utf8, LineEndingType::Lf).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_follows_dangling_direct_symlink_and_creates_target() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "cometix-file-atomic-dangling-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("created-target.txt");
        let link = root.join("link.txt");
        symlink("created-target.txt", &link).unwrap();

        write_text_content(&link, "created", FileEncoding::Utf8, LineEndingType::Lf).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "created");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exclusive_temp_and_no_follow_fallback_reject_precreated_symlinks() {
        use std::os::unix::fs::symlink;
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-file-atomic-hostile-links-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("victim.txt");
        let hostile_temp = root.join("hostile.tmp");
        std::fs::write(&victim, "safe").unwrap();
        symlink(&victim, &hostile_temp).unwrap();
        assert_eq!(
            sync_write_new(&hostile_temp, b"attacker")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "safe");

        let hostile_target = root.join("target-link.txt");
        symlink(&victim, &hostile_target).unwrap();
        crate::utils::fs_operations::force_next_replace_file_atomic_failure_for_test();
        assert!(write_file_sync_and_flush_to_target(&hostile_target, b"attacker").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "safe");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_replace_failure_falls_back_to_flushed_direct_write() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-file-atomic-fallback-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        std::fs::write(&target, "old").unwrap();
        crate::utils::fs_operations::force_next_replace_file_atomic_failure_for_test();

        write_text_content(&target, "new", FileEncoding::Utf8, LineEndingType::Lf).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_reader_observes_only_complete_atomic_versions() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        let root = std::env::temp_dir().join(format!(
            "cometix-file-atomic-reader-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        let first = "a".repeat(8192);
        let second = "b".repeat(16384);
        std::fs::write(&target, &first).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let invalid = Arc::new(Mutex::new(None::<usize>));
        let reader_target = target.clone();
        let reader_stopped = Arc::clone(&stopped);
        let reader_invalid = Arc::clone(&invalid);
        let first_len = first.len();
        let second_len = second.len();
        let reader = std::thread::spawn(move || {
            while !reader_stopped.load(Ordering::Acquire) {
                if let Ok(bytes) = std::fs::read(&reader_target) {
                    if !((bytes.len() == first_len && bytes.iter().all(|byte| *byte == b'a'))
                        || (bytes.len() == second_len && bytes.iter().all(|byte| *byte == b'b')))
                    {
                        *reader_invalid.lock().unwrap() = Some(bytes.len());
                        break;
                    }
                }
            }
        });
        for index in 0..40 {
            let content = if index % 2 == 0 { &second } else { &first };
            write_text_content(&target, content, FileEncoding::Utf8, LineEndingType::Lf).unwrap();
        }
        stopped.store(true, Ordering::Release);
        reader.join().unwrap();
        assert_eq!(*invalid.lock().unwrap(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_atomic_and_direct_write_cleans_temporary_file() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-file-atomic-cleanup-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let target = root.join("target-dir");
        std::fs::create_dir_all(&target).unwrap();
        crate::utils::fs_operations::force_next_replace_file_atomic_failure_for_test();

        assert!(
            write_text_content(&target, "new", FileEncoding::Utf8, LineEndingType::Lf).is_err()
        );
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn edit_path_suggestions_match_similar_extension_and_dropped_repo_patterns() {
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-path-suggestion-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let cwd = root.join("repository");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        std::fs::write(cwd.join("config.rs"), "").unwrap();
        assert_eq!(
            find_similar_file(&cwd.join("config.ts")).as_deref(),
            Some("config.rs")
        );
        assert_eq!(
            suggest_path_under_cwd(&root.join("config.rs"), &cwd),
            Some(cwd.join("config.rs"))
        );
        assert_eq!(suggest_path_under_cwd(&cwd.join("missing.rs"), &cwd), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn get_display_path_prefers_cwd_relative_like_official() {
        struct OriginalCwdRestore(std::path::PathBuf);
        impl Drop for OriginalCwdRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(&self.0);
            }
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = OriginalCwdRestore(crate::bootstrap::state::get_original_cwd());
        let cwd = std::env::temp_dir().join("cometix-display-path-cwd");
        let _ = std::fs::create_dir_all(cwd.join("src"));
        crate::bootstrap::state::set_original_cwd(&cwd);

        assert_eq!(get_display_path("src"), "src");
        assert_eq!(get_display_path(&cwd.join("src").to_string_lossy()), "src");
        let _home = EnvVarGuard::set(HOME_VAR, "/home/someone");
        assert_eq!(
            get_display_path("/home/someone/outside-project/file.rs"),
            "~/outside-project/file.rs"
        );

        let _ = std::fs::remove_dir_all(&cwd);
    }
}
