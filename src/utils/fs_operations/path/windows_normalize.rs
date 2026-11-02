//! Maps to Node v24.14.0 lib/path.js win32.normalize and join.
//! Kept separate from resolve: reserved names and colon guards are normalize rules.
use super::windows_lexical::{bytes, native, normalized_tail, root, sep};
use std::path::{Path, PathBuf};

fn reserved(input: &[u8], colon: Option<usize>) -> bool {
    // Node slice(0, -1) removes one UTF-16 unit, not one UTF-8 byte/scalar.
    let path = native(input[..colon.unwrap_or(input.len())].to_vec());
    let mut units = super::windows_relative::wide(&path);
    if colon.is_none() {
        units.pop();
    }
    let Ok(name) = String::from_utf16(&units) else {
        return false;
    };
    let name = name.to_uppercase();
    matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            name.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}

pub(crate) fn normalize(path: &Path) -> PathBuf {
    let input = bytes(path);
    if input.is_empty() {
        return PathBuf::from(".");
    }
    if input.len() == 1 {
        return if input[0] == b'/' {
            PathBuf::from("\\")
        } else {
            path.to_owned()
        };
    }
    let mut r = root(input);
    let colon = input.iter().position(|b| *b == b':');
    if r.namespace {
        let end = colon.map_or(0, |i| i + 1);
        let possible = if end >= 4 { &input[4..end] } else { &[] };
        if reserved(possible, possible.len().checked_sub(1)) {
            r.device = br"\\?\".to_vec();
            r.device.extend_from_slice(possible);
            r.end = 4 + possible.len();
        }
    } else if !sep(input[0])
        && r.device.is_empty()
        && let Some(colon) = colon.filter(|i| *i > 0)
        && reserved(input, Some(colon))
    {
        r.device = input[..=colon].to_vec();
        r.end = colon + 1;
    }
    let mut tail = normalized_tail(&input[r.end..], !r.absolute);
    if tail.is_empty() && !r.absolute {
        tail.push(b'.');
    }
    if !tail.is_empty() && input.last().is_some_and(|b| sep(*b)) {
        tail.push(b'\\');
    }
    if !r.absolute && r.device.is_empty() && colon.is_some() {
        let drive_tail = tail.len() >= 2 && tail[0].is_ascii_alphabetic() && tail[1] == b':';
        let colon_boundary = input
            .iter()
            .enumerate()
            .any(|(i, b)| *b == b':' && (i + 1 == input.len() || sep(input[i + 1])));
        if drive_tail || colon_boundary {
            let mut result = b".\\".to_vec();
            result.extend_from_slice(&tail);
            return native(result);
        }
    }
    let mut result = Vec::new();
    if reserved(input, colon) {
        result.extend_from_slice(b".\\");
    }
    result.extend_from_slice(&r.device);
    if r.absolute {
        result.push(b'\\');
    }
    result.extend_from_slice(&tail);
    native(result)
}

pub(crate) fn join(base: &Path, target: &Path) -> PathBuf {
    join_many(&[base, target])
}

pub(crate) fn join_many(paths: &[&Path]) -> PathBuf {
    let joined = super::windows_syntax::join_inputs(paths);
    let input = bytes(&joined);
    // Node deliberately checks backslash-delimited parts before slash replacement.
    if input.split(|b| *b == b'\\').any(|part| {
        part.iter()
            .position(|b| *b == b':')
            .is_some_and(|colon| reserved(part, Some(colon)))
    }) {
        return native(
            input
                .iter()
                .map(|b| if *b == b'/' { b'\\' } else { *b })
                .collect(),
        );
    }
    normalize(&joined)
}
