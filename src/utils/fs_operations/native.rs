//! Native dependency carrier for CC `utils/fsOperations.ts#NodeFsOperations`.
use super::path::SugarPath;
use super::{
    BufferEncoding, FsDirent, FsStats, FsText, FsWriteStream, ReadSyncResult, RmOptions,
    SymlinkType,
};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

pub(super) fn cwd() -> io::Result<PathBuf> {
    std::env::current_dir()
}
pub(super) fn exists_sync(path: &Path) -> bool {
    path.exists()
}
pub(super) async fn stat(path: &Path) -> io::Result<FsStats> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || stat_sync(&path))
        .await
        .map_err(io::Error::other)?
}
pub(super) async fn readdir(path: &Path) -> io::Result<Vec<FsDirent>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || readdir_sync(&path))
        .await
        .map_err(io::Error::other)?
}
pub(super) async fn unlink(path: &Path) -> io::Result<()> {
    tokio::fs::remove_file(path).await
}
pub(super) async fn rmdir(path: &Path) -> io::Result<()> {
    tokio::fs::remove_dir(path).await
}
pub(crate) async fn rm(path: &Path, options: RmOptions) -> io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || rm_sync(&path, options))
        .await
        .map_err(io::Error::other)?
}
pub(super) async fn mkdir(path: &Path, mode: Option<u32>) -> io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || mkdir_sync(&path, mode))
        .await
        .map_err(io::Error::other)?
}
pub(super) async fn read_file(path: &Path, encoding: BufferEncoding) -> io::Result<FsText> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || read_file_sync(&path, encoding))
        .await
        .map_err(io::Error::other)?
}

pub(super) async fn rename(source: &Path, target: &Path) -> io::Result<()> {
    tokio::fs::rename(source, target).await
}
pub(super) fn stat_sync(path: &Path) -> io::Result<FsStats> {
    #[cfg(windows)]
    {
        super::windows::stat(path, true)
    }
    #[cfg(not(windows))]
    {
        fs::metadata(path).map(Into::into)
    }
}
pub(super) fn lstat_sync(path: &Path) -> io::Result<FsStats> {
    #[cfg(windows)]
    {
        super::windows::stat(path, false)
    }
    #[cfg(not(windows))]
    {
        fs::symlink_metadata(path).map(Into::into)
    }
}
pub(super) fn read_file_sync(path: &Path, encoding: BufferEncoding) -> io::Result<FsText> {
    Ok(encoding.decode_text(&read_file_bytes_sync(path)?))
}
pub(super) fn read_file_bytes_sync(path: &Path) -> io::Result<Vec<u8>> {
    let mut file =
        fs::File::open(path).map_err(|error| super::error::native(error, "open", path, None))?;
    let mut bytes = Vec::new();
    let result = file
        .read_to_end(&mut bytes)
        .map(|_| bytes)
        .map_err(|error| super::error::native(error, "read", path, None));
    super::handle::finish(file, result)
}
pub(super) fn read_sync(path: &Path, length: usize) -> io::Result<ReadSyncResult> {
    let mut file =
        fs::File::open(path).map_err(|error| super::error::native(error, "open", path, None))?;
    let mut buffer = vec![0; length];
    let result = file
        .read(&mut buffer)
        .map(|bytes_read| ReadSyncResult { buffer, bytes_read })
        .map_err(|error| super::error::native(error, "read", path, None));
    super::handle::finish(file, result)
}
pub(super) fn append_file_sync(path: &Path, data: &FsText, mode: Option<u32>) -> io::Result<()> {
    let append = |mut file: fs::File| {
        let result = file
            .write_all(data.to_string_lossy().as_bytes())
            .map_err(|error| super::error::native(error, "write", path, None));
        super::handle::finish(file, result)
    };
    if let Some(mode) = mode {
        let mut options = fs::OpenOptions::new();
        options.append(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let result = options
            .open(path)
            .map_err(|error| super::error::native(error, "open", path, None))
            .and_then(append);
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|error| super::error::native(error, "open", path, None))?;
    append(file)
}
pub(super) fn copy_file_sync(source: &Path, target: &Path) -> io::Result<()> {
    // Node copyFile(src, src) succeeds without truncating the inode.
    if let (Ok(a), Ok(b)) = (fs::metadata(source), fs::metadata(target)) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if a.is_file() && b.is_file() && a.dev() == b.dev() && a.ino() == b.ino() {
                return Ok(());
            }
        }
        #[cfg(not(unix))]
        {
            if a.is_file() && b.is_file() && fs::canonicalize(source)? == fs::canonicalize(target)?
            {
                return Ok(());
            }
        }
    }
    fs::copy(source, target).map(|_| ())
}
pub(super) fn unlink_sync(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}
pub(super) fn rename_sync(source: &Path, target: &Path) -> io::Result<()> {
    super::replace_file_atomic(source, target)
}
pub(super) fn link_sync(target: &Path, path: &Path) -> io::Result<()> {
    fs::hard_link(target, path)
}
pub(super) fn symlink_sync(
    target: &Path,
    path: &Path,
    kind: Option<SymlinkType>,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = kind;
        std::os::unix::fs::symlink(target, path)
    }
    #[cfg(windows)]
    {
        // Maps to Node 24.14 lib/fs.js#symlinkSync: omitted type probes the
        // target relative to the new link's parent, independently of activeFs.
        let kind = match kind {
            Some(kind) => kind,
            None => {
                let absolute =
                    super::path::windows_lexical::resolve(&[path, Path::new(".."), target])?;
                match stat_sync(&absolute) {
                    Ok(stats) if stats.is_dir() => SymlinkType::Dir,
                    Ok(_) => SymlinkType::File,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                        ) =>
                    {
                        SymlinkType::File
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        // Node getValidatedPath rejects NUL before handing either string to
        // libuv. Junction targets are encoded manually and need this check too.
        for (name, value) in [("target", target), ("path", path)] {
            if value.as_os_str().as_encoded_bytes().contains(&0) {
                return Err(super::error::argument(
                    "ERR_INVALID_ARG_VALUE",
                    format!(
                        "The argument '{name}' must be a string, Uint8Array, or URL without null bytes. Received {:?}",
                        value.as_os_str()
                    ),
                ));
            }
        }
        match kind {
            SymlinkType::File => std::os::windows::fs::symlink_file(
                super::windows::symlink_destination(target)?,
                path,
            ),
            SymlinkType::Dir => std::os::windows::fs::symlink_dir(
                super::windows::symlink_destination(target)?,
                path,
            ),
            SymlinkType::Junction => super::windows::junction(target, path),
        }
    }
}
pub(super) fn readlink_sync(path: &Path) -> io::Result<PathBuf> {
    fs::read_link(path)
}
pub(super) fn realpath_sync(path: &Path) -> io::Result<PathBuf> {
    // Node fs.realpathSync first applies path.resolve (including lexical ..).
    let absolute = resolve_path(Path::new("."), path)?;
    let resolved = fs::canonicalize(absolute)?;
    let text = resolved.to_string_lossy();
    #[cfg(windows)]
    let text = node_windows_realpath(&text);
    Ok(PathBuf::from(text.nfc().collect::<String>()))
}
/// Node realpath uses Win32 spelling, whereas Rust canonicalize uses an extended-length prefix.
#[cfg(any(windows, test))]
pub(super) fn node_windows_realpath(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.to_owned()
    }
}
pub(super) fn mkdir_sync(path: &Path, mode: Option<u32>) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no such file or directory",
        ));
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    match builder.create(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}
pub(super) fn readdir_sync(path: &Path) -> io::Result<Vec<FsDirent>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| FsDirent::native(entry?, path))
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}
pub(super) fn readdir_string_sync(path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    Ok(readdir_sync(path)?
        .into_iter()
        .map(|entry| entry.file_name())
        .collect())
}
pub(super) fn rmdir_sync(path: &Path) -> io::Result<()> {
    fs::remove_dir(path)
}
pub(super) fn rm_sync(path: &Path, options: RmOptions) -> io::Result<()> {
    let result = (|| {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            if options.recursive {
                fs::remove_dir_all(path)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::IsADirectory,
                    super::FsError {
                        code: "ERR_FS_EISDIR",
                        syscall: Some("rm"),
                        path: Some(path.to_owned()),
                        dest: None,
                        message: format!(
                            "Path is a directory: rm returned EISDIR (is a directory) {}",
                            path.display()
                        ),
                        native: None,
                    },
                ))
            }
        } else {
            fs::remove_file(path)
        }
    })();
    match result {
        Err(error) if options.force && error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
pub(super) fn create_write_stream(path: &Path) -> FsWriteStream {
    Box::pin(super::write_stream::NativeWriteStream::new(path))
}
pub(super) async fn read_file_bytes(path: &Path, max_bytes: Option<f64>) -> io::Result<Vec<u8>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || super::read_file_bytes_native(&path, max_bytes))
        .await
        .map_err(io::Error::other)?
}
// Maps to the native node:path.normalize dependency. Preserve lexical spelling,
// including one non-root trailing separator; this does not resolve symlinks.
fn normalize(path: &Path) -> PathBuf {
    path.normalize().into_owned()
}
pub(crate) fn resolve_path(base: &Path, target: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        super::path::windows_lexical::resolve(&[base, target])
    }
    #[cfg(not(windows))]
    {
        let joined = if target.is_absolute() {
            target.to_owned()
        } else {
            base.join(target)
        };
        joined.try_absolutize().map(|path| path.into_owned())
    }
}

pub(crate) fn join_path(base: &Path, target: &Path) -> PathBuf {
    join_paths(&[base, target])
}

/// Maps to node:path.join(...segments): normalize once after concatenating all arguments.
pub(crate) fn join_paths(paths: &[&Path]) -> PathBuf {
    #[cfg(windows)]
    {
        super::path::windows_normalize::join_many(paths)
    }
    #[cfg(not(windows))]
    {
        let mut value = std::ffi::OsString::new();
        for path in paths.iter().filter(|p| !p.as_os_str().is_empty()) {
            if !value.is_empty() {
                value.push(std::path::MAIN_SEPARATOR_STR);
            }
            value.push(path);
        }
        normalize(Path::new(&value))
    }
}

// Node dirname/basename preserve literal '.' and '..' components. Rust's
// Path::parent/file_name discard them before the source lstat fallback loop.
pub(crate) fn dirname(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        return super::path::windows_syntax::dirname(path);
    }
    #[cfg(not(windows))]
    {
        let value = path.to_string_lossy();
        let bytes = value.as_bytes();
        let sep = |b| b == b'/' || (cfg!(windows) && b == b'\\');
        if !bytes.is_empty() && bytes.iter().all(|byte| sep(*byte)) {
            return PathBuf::from(&value[..1]);
        }
        let root = if cfg!(windows) && bytes.get(1) == Some(&b':') {
            if bytes.get(2).is_some_and(|b| sep(*b)) {
                3
            } else {
                2
            }
        } else if bytes.first().is_some_and(|b| sep(*b)) {
            1
        } else {
            0
        };
        let mut end = bytes.len();
        while end > root && sep(bytes[end - 1]) {
            end -= 1;
        }
        while end > root && !sep(bytes[end - 1]) {
            end -= 1;
        }
        if end == 0 {
            return PathBuf::from(".");
        }
        if end > root {
            end -= 1;
        }
        if root == 1 && end == 1 && bytes.get(1) == Some(&b'/') {
            end = 2;
        }
        PathBuf::from(&value[..end.max(root)])
    }
}
pub(crate) fn basename(path: &Path) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        return super::path::windows_syntax::basename(path);
    }
    #[cfg(not(windows))]
    {
        let value = path.to_string_lossy();
        let trimmed = value.trim_end_matches(|c| c == '/' || (cfg!(windows) && c == '\\'));
        let name = trimmed
            .rsplit(|c| c == '/' || (cfg!(windows) && c == '\\'))
            .next()
            .unwrap_or("");
        name.into()
    }
}

/// Maps to node:path.isAbsolute, whose Windows root-relative rule differs from Rust.
pub(crate) fn is_absolute(path: &Path) -> bool {
    #[cfg(windows)]
    {
        super::path::windows_syntax::is_absolute(path)
    }
    #[cfg(not(windows))]
    {
        path.is_absolute()
    }
}

/// Maps to node:path.extname: the last dot of the literal basename, except
/// a sole leading dot and the parent component. No filesystem access occurs.
pub(crate) fn extname(path: &Path) -> std::ffi::OsString {
    let name = basename(path);
    let bytes = name.as_encoded_bytes();
    match bytes.iter().rposition(|byte| *byte == b'.') {
        Some(index) if index > 0 && bytes != b".." => {
            // SAFETY: splitting at an ASCII dot preserves native encoding boundaries.
            unsafe { std::ffi::OsString::from_encoded_bytes_unchecked(bytes[index..].to_vec()) }
        }
        _ => std::ffi::OsString::new(),
    }
}
