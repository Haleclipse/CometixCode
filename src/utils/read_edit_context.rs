//! Bounded context reads for Edit permission/rejection previews.
//!
//! Maps to: CC `utils/readEditContext.ts:1-221`.
//! CC uses async `FileHandle` reads; Rust performs the same bounded chunked
//! scan synchronously because callers run in tool/session workers, never in the
//! model API actor. The byte caps, CRLF fallback, overlap, context window, and
//! line-offset semantics remain unchanged.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

pub const CHUNK_SIZE: usize = 8 * 1024;
pub const MAX_SCAN_BYTES: usize = 10 * 1024 * 1024;
const NL: u8 = b'\n';

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditContext {
    pub content: String,
    pub line_offset: usize,
    pub truncated: bool,
}

/// Maps to CC `readEditContext(...)`.
pub fn read_edit_context(
    path: &Path,
    needle: &str,
    context_lines: usize,
) -> std::io::Result<Option<EditContext>> {
    let Some(mut file) = open_for_scan(path)? else {
        return Ok(None);
    };
    scan_for_context(&mut file, needle, context_lines).map(Some)
}

/// Maps to CC `openForScan(...)`, with a security hardening deviation: Unix
/// preview reads open nonblocking and all platforms verify the opened handle is
/// regular so a FIFO or symlink-retarget race cannot stall the permission worker.
pub fn open_for_scan(path: &Path) -> std::io::Result<Option<std::fs::File>> {
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
    };
    #[cfg(windows)]
    let opened = std::fs::File::open(path);
    #[cfg(not(any(unix, windows)))]
    let opened = std::fs::File::open(path);

    match opened {
        Ok(file) => {
            if !file.metadata()?.file_type().is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Edit preview source is not a regular file",
                ));
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Maps to CC `scanForContext(...)`.
pub fn scan_for_context(
    file: &mut std::fs::File,
    needle: &str,
    context_lines: usize,
) -> std::io::Result<EditContext> {
    if needle.is_empty() {
        return Ok(EditContext {
            content: String::new(),
            line_offset: 1,
            truncated: false,
        });
    }

    let needle_lf = needle.as_bytes();
    let newline_count = needle_lf.iter().filter(|byte| **byte == NL).count();
    let needle_crlf = (newline_count > 0).then(|| needle.replace('\n', "\r\n").into_bytes());
    let overlap = needle_lf
        .len()
        .saturating_add(newline_count)
        .saturating_sub(1);
    let mut tail = Vec::<u8>::new();
    let mut chunk = vec![0u8; CHUNK_SIZE];
    let mut position = 0usize;
    let mut lines_before_buffer = 0usize;

    while position < MAX_SCAN_BYTES {
        file.seek(SeekFrom::Start(position as u64))?;
        let bytes_read = file.read(&mut chunk)?;
        if bytes_read == 0 {
            break;
        }

        let mut view = Vec::with_capacity(tail.len() + bytes_read);
        view.extend_from_slice(&tail);
        view.extend_from_slice(&chunk[..bytes_read]);

        let match_info = index_of_within(&view, needle_lf)
            .map(|index| (index, needle_lf.len()))
            .or_else(|| {
                needle_crlf.as_deref().and_then(|needle| {
                    index_of_within(&view, needle).map(|index| (index, needle.len()))
                })
            });
        if let Some((match_at, match_len)) = match_info {
            let absolute_match = position.saturating_sub(tail.len()) + match_at;
            let lines_before_match = lines_before_buffer + count_newlines(&view[..match_at]);
            return slice_context(
                file,
                absolute_match,
                match_len,
                context_lines,
                lines_before_match,
            );
        }

        position += bytes_read;
        let next_tail = overlap.min(view.len());
        let discarded = view.len().saturating_sub(next_tail);
        lines_before_buffer += count_newlines(&view[..discarded]);
        tail.clear();
        tail.extend_from_slice(&view[discarded..]);
    }

    Ok(EditContext {
        content: String::new(),
        line_offset: 1,
        truncated: position >= MAX_SCAN_BYTES,
    })
}

/// Maps to CC `readCapped(...)`.
pub fn read_capped(file: &mut std::fs::File) -> std::io::Result<Option<String>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(CHUNK_SIZE);
    let mut chunk = vec![0u8; CHUNK_SIZE];
    loop {
        let bytes_read = file.read(&mut chunk)?;
        if bytes_read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..bytes_read]);
        if bytes.len() > MAX_SCAN_BYTES {
            return Ok(None);
        }
    }
    Ok(Some(normalize_crlf(&bytes)))
}

fn index_of_within(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == NL).count()
}

fn normalize_crlf(bytes: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(bytes).into_owned();
    if decoded.contains('\r') {
        decoded.replace("\r\n", "\n")
    } else {
        decoded
    }
}

fn read_range(file: &mut std::fs::File, offset: usize, length: usize) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut output = vec![0u8; length];
    let mut total = 0usize;
    while total < length {
        let bytes_read = file.read(&mut output[total..])?;
        if bytes_read == 0 {
            break;
        }
        total += bytes_read;
    }
    output.truncate(total);
    Ok(output)
}

/// Maps to CC `sliceContext(...)`.
fn slice_context(
    file: &mut std::fs::File,
    match_start: usize,
    match_len: usize,
    context_lines: usize,
    lines_before_match: usize,
) -> std::io::Result<EditContext> {
    let back_chunk = match_start.min(CHUNK_SIZE);
    let backward = read_range(file, match_start - back_chunk, back_chunk)?;
    let mut context_start = match_start;
    let mut newlines_seen = 0usize;
    for byte in backward.iter().rev() {
        if *byte == NL {
            newlines_seen += 1;
            if newlines_seen > context_lines {
                break;
            }
        }
        context_start = context_start.saturating_sub(1);
    }
    let walked_back = match_start - context_start;
    let line_offset = lines_before_match.saturating_sub(count_newlines(
        &backward[backward.len().saturating_sub(walked_back)..],
    )) + 1;

    let match_end = match_start.saturating_add(match_len);
    let forward = read_range(file, match_end, CHUNK_SIZE)?;
    let mut context_end = match_end;
    newlines_seen = 0;
    for byte in &forward {
        context_end += 1;
        if *byte == NL {
            newlines_seen += 1;
            if newlines_seen > context_lines {
                break;
            }
        }
    }

    let bytes = read_range(file, context_start, context_end - context_start)?;
    Ok(EditContext {
        content: normalize_crlf(&bytes),
        line_offset,
        truncated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(content: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nyan-read-edit-context-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn chunked_context_finds_straddled_match_and_line_offset_like_official() {
        let prefix = format!("first\n{}", "x".repeat(CHUNK_SIZE - 10));
        let content = format!("{prefix}needle\nafter-1\nafter-2\nafter-3\n");
        let path = fixture(content.as_bytes());
        let context = read_edit_context(&path, "needle", 1)
            .unwrap()
            .expect("context");
        assert!(context.content.contains("needle\nafter-1\n"));
        assert!(context.line_offset >= 1);
        assert!(!context.truncated);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn context_matches_lf_needle_in_crlf_file_and_normalizes_result() {
        let path = fixture(b"one\r\ntwo\r\nthree\r\nfour\r\n");
        let context = read_edit_context(&path, "two\nthree", 1)
            .unwrap()
            .expect("context");
        assert_eq!(context.content, "one\ntwo\nthree\nfour\n");
        assert_eq!(context.line_offset, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn line_offset_counts_discarded_lines_exactly() {
        let content = (1..=15)
            .map(|line| {
                if line == 11 {
                    "needle".to_string()
                } else {
                    format!("line-{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let path = fixture(content.as_bytes());
        let context = read_edit_context(&path, "needle", 1)
            .unwrap()
            .expect("context");
        assert_eq!(context.line_offset, 10);
        assert!(context.content.starts_with("line-10\nneedle\nline-12\n"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn scan_reports_truncated_when_needle_is_beyond_official_cap() {
        let path = fixture(b"");
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len((MAX_SCAN_BYTES + 16) as u64).unwrap();
        file.seek(SeekFrom::Start(MAX_SCAN_BYTES as u64 + 1))
            .unwrap();
        use std::io::Write as _;
        file.write_all(b"needle").unwrap();
        drop(file);
        let context = read_edit_context(&path, "needle", 1)
            .unwrap()
            .expect("context");
        assert!(context.truncated);
        assert!(context.content.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn preview_open_follows_regular_symlinks_like_source() {
        use std::os::unix::fs::symlink;
        let target = fixture(b"old\n");
        let link = target.with_extension("link");
        symlink(&target, &link).unwrap();
        let context = read_edit_context(&link, "old", 1)
            .unwrap()
            .expect("symlink target preview");
        assert!(context.content.contains("old"));
        let _ = std::fs::remove_file(link);
        let _ = std::fs::remove_file(target);
    }

    #[cfg(unix)]
    #[test]
    fn preview_open_rejects_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::env::temp_dir().join(format!(
            "nyan-read-edit-context-fifo-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(open_for_scan(&path).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn capped_read_rejects_files_above_official_limit() {
        let path = fixture(b"small");
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len((MAX_SCAN_BYTES + 1) as u64).unwrap();
        drop(file);
        let mut file = std::fs::File::open(&path).unwrap();
        assert!(read_capped(&mut file).unwrap().is_none());
        let _ = std::fs::remove_file(path);
    }
}
