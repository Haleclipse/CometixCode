//! Node path.win32 lexical boundaries used before std::path component parsing.
//! Maps to Node lib/path.js win32 dirname/basename/isAbsolute/join root scans.
//! Byte scans split only at ASCII separators; native non-Unicode spelling is retained.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'\\')
}

fn drive(path: &[u8]) -> Option<u8> {
    (path.len() >= 2 && path[0].is_ascii_alphabetic() && path[1] == b':').then(|| path[0])
}

// A UNC server and share are separated by one OR MORE separators in Node.
fn unc_parts(path: &[u8]) -> Option<(usize, usize, usize)> {
    if path.len() < 2 || !separator(path[0]) || !separator(path[1]) {
        return None;
    }
    let server_end = (2..path.len()).find(|&i| separator(path[i]))?;
    if server_end == 2 {
        return None;
    }
    let share_start = (server_end..path.len()).find(|&i| !separator(path[i]))?;
    let share_end = (share_start..path.len())
        .find(|&i| separator(path[i]))
        .unwrap_or(path.len());
    Some((server_end, share_start, share_end))
}

fn native(bytes: Vec<u8>) -> OsString {
    // SAFETY: callers retain complete native-encoded substrings, cut at ASCII
    // separators/drive delimiters, and insert only ASCII. No encoded code unit
    // is split and no Unicode replacement is performed.
    unsafe { OsString::from_encoded_bytes_unchecked(bytes) }
}

pub(crate) fn is_absolute(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes.first().is_some_and(|b| separator(*b))
        || (drive(bytes).is_some() && bytes.get(2).is_some_and(|b| separator(*b)))
}

pub(crate) fn dirname(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        return PathBuf::from(".");
    }
    if bytes.len() == 1 {
        return if separator(bytes[0]) {
            path.to_owned()
        } else {
            PathBuf::from(".")
        };
    }
    let mut root_end = None;
    let mut offset = 0;
    if separator(bytes[0]) {
        root_end = Some(1);
        offset = 1;
        if let Some((_, _, share_end)) = unc_parts(bytes) {
            if share_end == bytes.len() {
                return path.to_owned();
            }
            offset = share_end + 1;
            root_end = Some(offset);
        }
    } else if drive(bytes).is_some() {
        offset = if bytes.get(2).is_some_and(|b| separator(*b)) {
            3
        } else {
            2
        };
        root_end = Some(offset);
    }
    let mut matched_slash = true;
    for i in (offset..bytes.len()).rev() {
        if separator(bytes[i]) {
            if !matched_slash {
                return native(bytes[..i].to_vec()).into();
            }
        } else {
            matched_slash = false;
        }
    }
    match root_end {
        Some(end) => native(bytes[..end].to_vec()).into(),
        None => PathBuf::from("."),
    }
}

pub(crate) fn basename(path: &Path) -> OsString {
    let bytes = path.as_os_str().as_encoded_bytes();
    let start = if drive(bytes).is_some() { 2 } else { 0 };
    let mut end = bytes.len();
    while end > start && separator(bytes[end - 1]) {
        end -= 1;
    }
    let begin = (start..end)
        .rev()
        .find(|&i| separator(bytes[i]))
        .map_or(start, |i| i + 1);
    native(bytes[begin..end].to_vec())
}

/// Canonicalize only ordinary UNC root spelling before Rust parses components.
/// Namespace prefixes are handled by windows_lexical and windows_normalize.
pub(crate) fn canonical_unc(path: &Path) -> Option<PathBuf> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let (server_end, share_start, share_end) = unc_parts(bytes)?;
    let server = &bytes[2..server_end];
    if server == b"?" || server == b"." {
        return None;
    }
    let mut out = b"\\\\".to_vec();
    out.extend_from_slice(server);
    out.push(b'\\');
    out.extend_from_slice(&bytes[share_start..share_end]);
    out.extend_from_slice(&bytes[share_end..]);
    if out == bytes {
        None
    } else {
        Some(native(out).into())
    }
}

/// Node join collapses accidental leading double separators, except when the
/// first nonempty argument explicitly starts with a UNC server name.
pub(crate) fn join_input(base: &Path, target: &Path) -> PathBuf {
    join_inputs(&[base, target])
}

pub(crate) fn join_inputs(paths: &[&Path]) -> PathBuf {
    let mut parts = paths
        .iter()
        .map(|p| p.as_os_str().as_encoded_bytes())
        .filter(|p| !p.is_empty());
    let first = parts.next().unwrap_or(&[]);
    let mut out = first.to_vec();
    for part in parts {
        out.push(b'\\');
        out.extend_from_slice(part);
    }
    let intentional_unc =
        first.len() > 2 && separator(first[0]) && separator(first[1]) && !separator(first[2]);
    if !intentional_unc {
        let count = out.iter().take_while(|b| separator(**b)).count();
        if count > 1 {
            out.splice(..count, *b"\\");
        }
    }
    native(out).into()
}
