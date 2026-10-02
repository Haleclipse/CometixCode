//! Filesystem operation boundary.
//!
//! Maps to: CC `utils/fsOperations.ts` `ReadFileRangeResult`,
//! `readFileRange(...)`, and `tailFile(...)`.
//!
//! The source interface and native implementation live in private child modules.
//! Public helpers preserve the source-defined active implementation boundary.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

mod eager;
mod error;
mod handle;
pub use error::FsError;
mod encoding;
mod interface;
mod types;
pub use types::*;
pub(crate) mod native;
pub(crate) mod path;
mod reverse_lines;
#[cfg(windows)]
mod windows;
mod write_stream;
pub use interface::*;
pub use reverse_lines::read_lines_reverse;

/// Maps to: CC `utils/fsOperations.ts#NodeFsOperations.readdir:398-400`.
/// Native carrier for Node's withFileTypes result. Node's libuv scandir returns
/// filename order on macOS; Tokio exposes filesystem iteration order instead.
/// Preserve that boundary before callers filter or truncate entries.
pub fn readdir(path: &Path) -> futures::future::BoxFuture<'static, std::io::Result<Vec<FsDirent>>> {
    get_fs_implementation().readdir(path)
}

/// Maps to: CC `utils/fsOperations.ts#NodeFsOperations.rm:410-412`.
/// Native carrier for the source recursive/force options. Neither force nor
/// recursion permits swallowing errors other than a missing path. Ordinary
/// files and symlinks are unlinked; directory contents use native removal.
/// Bun-specific nonrecursive-directory diagnostics and trailing-separator
/// symlink behavior are not claimed equivalent to the native filesystem API.
pub fn rm(
    path: &Path,
    recursive: bool,
    force: bool,
) -> futures::future::BoxFuture<'static, std::io::Result<()>> {
    get_fs_implementation().rm(path, RmOptions { recursive, force })
}

/// Maps to: CC `utils/fsOperations.ts#NodeFsOperations.mkdir:414-425`.
/// The source options object has only `mode`; recursive is always true. The
/// EEXIST catch is unconditional, despite its Bun/Windows explanatory comment.
pub fn mkdir(
    path: &Path,
    mode: Option<u32>,
) -> futures::future::BoxFuture<'static, std::io::Result<()>> {
    get_fs_implementation().mkdir(path, mode)
}

/// Maps to CC `safeResolvePath(...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeResolvedPath {
    pub resolved_path: PathBuf,
    pub is_symlink: bool,
    pub is_canonical: bool,
}

/// Maps to CC `utils/fsOperations.ts#isDuplicatePath`.
/// The check and insertion remain synchronous, including failed realpath cases.
pub fn is_duplicate_path(
    fs: &dyn FsOperations,
    file_path: &Path,
    loaded_paths: &mut std::collections::HashSet<std::ffi::OsString>,
) -> bool {
    !loaded_paths.insert(
        safe_resolve_path(fs, file_path)
            .resolved_path
            .into_os_string(),
    )
}

fn is_special_file_type(file_type: &FsFileType) -> bool {
    file_type.is_fifo()
        || file_type.is_socket()
        || file_type.is_char_device()
        || file_type.is_block_device()
}

/// Maps to CC `utils/fsOperations.ts#safeResolvePath(fs, filePath)`.
///
/// UNC paths are returned without touching the filesystem. Missing, broken,
/// inaccessible, and special paths retain their logical path so callers can
/// apply the same creation/error policy as CC.
pub fn safe_resolve_path(fs: &dyn FsOperations, file_path: &Path) -> SafeResolvedPath {
    let raw = file_path.to_string_lossy();
    if raw.starts_with("//") || raw.starts_with("\\\\") {
        return SafeResolvedPath {
            resolved_path: file_path.to_path_buf(),
            is_symlink: false,
            is_canonical: false,
        };
    }

    let Ok(metadata) = fs.lstat_sync(file_path) else {
        return SafeResolvedPath {
            resolved_path: file_path.to_path_buf(),
            is_symlink: false,
            is_canonical: false,
        };
    };
    if is_special_file_type(&metadata.file_type()) {
        return SafeResolvedPath {
            resolved_path: file_path.to_path_buf(),
            is_symlink: false,
            is_canonical: false,
        };
    }
    let Ok(resolved_path) = fs.realpath_sync(file_path) else {
        return SafeResolvedPath {
            resolved_path: file_path.to_path_buf(),
            is_symlink: false,
            is_canonical: false,
        };
    };
    // CC fsOperations.ts:138 receives FsOperations; its default realpathSync
    // (:523-525) returns NFC before safeResolvePath compares literal spelling.
    SafeResolvedPath {
        // CC fsOperations.ts:166 compares string spelling, not normalized
        // path components: '/cwd/' and '/cwd/./' differ from '/cwd'.
        is_symlink: resolved_path.as_os_str() != file_path.as_os_str(),
        resolved_path,
        is_canonical: true,
    }
}

/// Maps to CC `resolveDeepestExistingAncestorSync(...)`.
pub fn resolve_deepest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    resolve_deepest_existing_ancestor_sync(get_fs_implementation().as_ref(), path)
        .ok()
        .flatten()
}

/// Maps to CC `resolveDeepestExistingAncestorSync(fs, absolutePath)`.
pub fn resolve_deepest_existing_ancestor_sync(
    fs: &dyn FsOperations,
    path: &Path,
) -> std::io::Result<Option<PathBuf>> {
    let mut current = path.to_path_buf();
    let mut tail = Vec::<std::ffi::OsString>::new();
    loop {
        let parent = native::dirname(&current);
        if current.as_os_str() == parent.as_os_str() {
            return Ok(None);
        }
        match fs.lstat_sync(&current) {
            Err(_) => {
                tail.push(native::basename(&current));
                current = parent;
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let resolved = match fs.realpath_sync(&current) {
                    Ok(path) => path,
                    Err(_) => {
                        let target = fs.readlink_sync(&current)?;
                        if native::is_absolute(&target) {
                            target
                        } else {
                            native::resolve_path(&parent, &target)?
                        }
                    }
                };
                return Ok(Some(if tail.is_empty() {
                    resolved
                } else {
                    let mut segments = vec![resolved.as_path()];
                    segments.extend(tail.iter().rev().map(Path::new));
                    native::join_paths(&segments)
                }));
            }
            Ok(_) => {
                if let Ok(resolved) = fs.realpath_sync(&current) {
                    if resolved.as_os_str() != current.as_os_str() {
                        return Ok(Some(if tail.is_empty() {
                            resolved
                        } else {
                            let mut segments = vec![resolved.as_path()];
                            segments.extend(tail.iter().rev().map(Path::new));
                            native::join_paths(&segments)
                        }));
                    }
                }
                return Ok(None);
            }
        }
    }
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths
        .iter()
        .any(|existing| existing.as_os_str() == path.as_os_str())
    {
        paths.push(path);
    }
}

/// Maps to CC `utils/fsOperations.ts#getPathsForPermissionCheck`.
/// Preserves insertion order: logical path, each immediate link target, then
/// the final canonical/deepest-existing destination.
pub fn get_paths_for_permission_check(input_path: &Path) -> Vec<PathBuf> {
    let fs = get_fs_implementation();
    let mut path = input_path.to_path_buf();
    let raw = input_path.to_string_lossy();
    if raw == "~" || raw.starts_with("~/") {
        let home = PathBuf::from(
            crate::utils::node_os::homedir()
                .to_string_lossy()
                .nfc()
                .collect::<String>(),
        );
        path = if raw == "~" {
            home
        } else {
            native::join_path(&home, Path::new(&raw[2..]))
        };
    }

    let mut paths = vec![path.clone()];
    let raw = path.to_string_lossy();
    if raw.starts_with("//") || raw.starts_with("\\\\") {
        return paths;
    }

    let mut current = path.clone();
    let mut visited = Vec::<PathBuf>::new();
    for _ in 0..40 {
        if visited
            .iter()
            .any(|seen| seen.as_os_str() == current.as_os_str())
        {
            break;
        }
        visited.push(current.clone());
        if !fs.exists_sync(&current) {
            if current.as_os_str() == path.as_os_str() {
                if let Ok(Some(resolved)) =
                    resolve_deepest_existing_ancestor_sync(fs.as_ref(), &path)
                {
                    push_unique_path(&mut paths, resolved);
                }
            }
            break;
        }
        let Ok(metadata) = fs.lstat_sync(&current) else {
            break;
        };
        if is_special_file_type(&metadata.file_type()) || !metadata.file_type().is_symlink() {
            break;
        }
        let Ok(target) = fs.readlink_sync(&current) else {
            break;
        };
        let target = if native::is_absolute(&target) {
            target
        } else {
            match native::resolve_path(&native::dirname(&current), &target) {
                Ok(target) => target,
                Err(_) => break,
            }
        };
        push_unique_path(&mut paths, target.clone());
        current = target;
    }

    let resolved = safe_resolve_path(fs.as_ref(), &path);
    if resolved.is_symlink && resolved.resolved_path.as_os_str() != path.as_os_str() {
        push_unique_path(&mut paths, resolved.resolved_path);
    }
    paths
}

/// Maps to CC `utils/fsOperations.ts:578-599#readFileBytes`.
/// With `max_bytes`, CC reads at most that many bytes from the start rather
/// than reading the whole file and throwing for a larger file.
pub fn read_file_bytes(path: &Path, max_bytes: Option<f64>) -> std::io::Result<Vec<u8>> {
    futures::executor::block_on(get_fs_implementation().read_file_bytes(path, max_bytes))
}

pub(super) fn read_file_bytes_native(
    path: &Path,
    max_bytes: Option<f64>,
) -> std::io::Result<Vec<u8>> {
    let at_path = |error, operation| error::native(error, operation, path, None);
    let mut file = std::fs::File::open(path).map_err(|error| at_path(error, "open"))?;
    let result = (|| {
        let Some(max_bytes) = max_bytes else {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| at_path(error, "read"))?;
            return Ok(bytes);
        };
        let file_size = file
            .metadata()
            .map_err(|error| at_path(error, "fstat"))?
            .len() as f64;
        let requested = if max_bytes.is_nan() {
            f64::NAN
        } else {
            file_size.min(max_bytes)
        };
        if requested.is_nan() || requested < 0.0 {
            return Err(error::argument(
                "ERR_OUT_OF_RANGE",
                format!(
                    "The value of \"size\" is out of range. It must be >= 0 && <= 9007199254740991. Received {}",
                    ryu_js::Buffer::new().format(requested)
                ),
            ));
        }
        let read_size = requested as usize;
        if requested.fract() != 0.0 {
            return Err(if read_size == 0 {
                error::argument(
                    "ERR_INVALID_ARG_VALUE",
                    "The argument 'buffer' is empty and cannot be written. Received <Buffer >"
                        .into(),
                )
            } else {
                error::argument(
                    "ERR_OUT_OF_RANGE",
                    format!(
                        "The value of \"length\" is out of range. It must be <= {read_size}. Received {}",
                        ryu_js::Buffer::new().format(requested)
                    ),
                )
            });
        }
        let mut bytes = vec![0_u8; read_size];
        let mut offset = 0usize;
        while offset < read_size {
            let count = file
                .read(&mut bytes[offset..])
                .map_err(|error| at_path(error, "read"))?;
            if count == 0 {
                break;
            }
            offset += count;
        }
        bytes.truncate(offset);
        Ok(bytes)
    })();
    handle::finish(file, result)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadFileRangeResult {
    pub content: String,
    pub bytes_read: usize,
    pub bytes_total: u64,
}

/// Rust security hardening for the source `open(path, 'r')` call sites. Bash
/// output paths are controlled by an untrusted child after spawn, so opening
/// with no-follow semantics prevents a rename-and-symlink swap from redirecting
/// host-side reads or truncation.
pub(crate) fn open_regular_file_no_follow(
    path: &Path,
    writable: bool,
) -> std::io::Result<std::fs::File> {
    let before = std::fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing unsafe file path",
        ));
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(writable);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // FILE_FLAG_OPEN_REPARSE_POINT prevents a late symlink/junction swap
        // from resolving to its target between metadata and open.
        options.custom_flags(0x0020_0000);
    }

    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing non-regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
thread_local! {
    static FORCE_REPLACE_FILE_ATOMIC_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn force_next_replace_file_atomic_failure_for_test() {
    FORCE_REPLACE_FILE_ATOMIC_FAILURE.set(true);
}

fn take_forced_replace_failure() -> bool {
    #[cfg(test)]
    {
        return FORCE_REPLACE_FILE_ATOMIC_FAILURE.replace(false);
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Maps to CC `FsOperations.renameSync(...)`, with Windows replace-existing
/// parity required by atomic settings and retained Bash-output commits.
pub(crate) fn replace_file_atomic(source: &Path, target: &Path) -> std::io::Result<()> {
    if take_forced_replace_failure() {
        return Err(std::io::Error::other("forced atomic replace failure"));
    }
    std::fs::rename(source, target)
}

/// Maps to CC `utils/fsOperations.ts#readFileRange`; raw I/O promise, independent of activeFs.
pub fn read_file_range(
    path: &Path,
    offset: u64,
    max_bytes: usize,
) -> futures::future::BoxFuture<'static, std::io::Result<Option<ReadFileRangeResult>>> {
    let path = path.to_owned();
    eager::start(async move {
        tokio::task::spawn_blocking(move || read_file_range_native(&path, offset, max_bytes))
            .await
            .map_err(std::io::Error::other)?
    })
}
/// Maps to CC `utils/fsOperations.ts#tailFile`; raw I/O promise, independent of activeFs.
pub fn tail_file(
    path: &Path,
    max_bytes: usize,
) -> futures::future::BoxFuture<'static, std::io::Result<ReadFileRangeResult>> {
    let path = path.to_owned();
    eager::start(async move {
        tokio::task::spawn_blocking(move || tail_file_native(&path, max_bytes))
            .await
            .map_err(std::io::Error::other)?
    })
}

/// Maps to CC `utils/fsOperations.ts#readFileRange`.
fn read_file_range_native(
    path: &Path,
    offset: u64,
    max_bytes: usize,
) -> std::io::Result<Option<ReadFileRangeResult>> {
    let mut file =
        std::fs::File::open(path).map_err(|error| error::native(error, "open", path, None))?;
    let result = (|| {
        let bytes_total = file
            .metadata()
            .map_err(|error| error::native(error, "fstat", path, None))?
            .len();
        if bytes_total <= offset {
            return Ok(None);
        }

        file.seek(SeekFrom::Start(offset))?;
        let bytes_to_read = ((bytes_total - offset) as usize).min(max_bytes);
        let mut bytes = vec![0; bytes_to_read];
        let mut bytes_read = 0usize;
        while bytes_read < bytes_to_read {
            let count = file
                .read(&mut bytes[bytes_read..])
                .map_err(|error| error::native(error, "read", path, None))?;
            if count == 0 {
                break;
            }
            bytes_read += count;
        }
        bytes.truncate(bytes_read);
        Ok(Some(ReadFileRangeResult {
            content: String::from_utf8_lossy(&bytes).into_owned(),
            bytes_read,
            bytes_total,
        }))
    })();
    handle::finish(file, result)
}

/// Maps to CC `utils/fsOperations.ts#tailFile`.
fn tail_file_native(path: &Path, max_bytes: usize) -> std::io::Result<ReadFileRangeResult> {
    let mut file =
        std::fs::File::open(path).map_err(|error| error::native(error, "open", path, None))?;
    let result = (|| {
        let bytes_total = file
            .metadata()
            .map_err(|error| error::native(error, "fstat", path, None))?
            .len();
        if bytes_total == 0 {
            return Ok(ReadFileRangeResult::default());
        }

        let offset = bytes_total.saturating_sub(max_bytes as u64);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::with_capacity((bytes_total - offset) as usize);
        (&mut file)
            .take(bytes_total - offset)
            .read_to_end(&mut bytes)
            .map_err(|error| error::native(error, "read", path, None))?;
        let bytes_read = bytes.len();
        Ok(ReadFileRangeResult {
            content: String::from_utf8_lossy(&bytes).into_owned(),
            bytes_read,
            bytes_total,
        })
    })();
    handle::finish(file, result)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod parity_tests;

#[cfg(test)]
mod injection_tests;

#[cfg(all(test, windows))]
mod windows_link_tests;
