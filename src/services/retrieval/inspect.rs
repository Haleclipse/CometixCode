use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use super::index::{language_for, symbols};
use super::outcome::{Branch, Candidate};

#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedCandidate {
    pub candidate: Candidate,
    pub snippet: String,
}

/// Re-read and hash a candidate before it can enter the result (retrieval
/// 4.6). Indexes carry ranges and hashes only; no indexed source text is
/// trusted here.
///
/// 1. the path is worktree-relative and stays inside the worktree;
/// 2. the recomputed raw hash matches the candidate's;
/// 3. the line range exists, and a candidate that names a symbol is checked
///    against a fresh parse of the file (languages without a grammar are
///    reported as `no_symbol_check`, never silently verified);
/// 4. the snippet comes from the bytes just read.
pub fn inspect_candidate(root: &Path, candidate: &Candidate) -> Result<VerifiedCandidate, String> {
    let relative = normalize_relative_path(&candidate.path)?;
    let absolute = root.join(&relative);
    let root = root
        .canonicalize()
        .map_err(|error| format!("root unavailable: {error}"))?;
    let canonical = absolute
        .canonicalize()
        .map_err(|error| format!("file unavailable: {error}"))?;
    if !canonical.starts_with(&root) {
        return Err("path outside worktree".to_string());
    }
    let bytes = std::fs::read(&canonical).map_err(|error| error.to_string())?;
    let hash = hex_hash(&bytes);
    if !candidate.file_hash_raw.is_empty() && candidate.file_hash_raw != hash {
        return Err("file changed since indexing".to_string());
    }
    // The index stores files decoded per tool-io T5 (a GBK source decodes
    // cleanly); the snippet must come from the same decoder or a legacy
    // encoded file would be dropped here after being indexed successfully.
    let text = super::index::decode_for_snippet(&bytes, &candidate.path)?;
    let lines = text.lines().collect::<Vec<_>>();
    if candidate.start_line == 0
        || candidate.end_line < candidate.start_line
        || candidate.end_line > lines.len()
    {
        return Err("line range no longer exists".to_string());
    }
    let mut verified = candidate.clone();
    // 4.6 step 3: any candidate that names a symbol (symbol branch, text
    // chunk or semantic payload) must still find that symbol covering the
    // range. Exact hits are fresh scans and never name one.
    if candidate.branch != Branch::Exact
        && let Some(symbol) = candidate.symbol.as_deref()
    {
        let language = language_for(&candidate.path);
        match symbols::symbol_covers(
            &text,
            &language,
            symbol,
            candidate.start_line,
            candidate.end_line,
        ) {
            Ok(true) => {}
            Ok(false) => return Err(format!("symbol {symbol} no longer at this range")),
            // No grammar (or a grammar failure): the caller renders
            // `(no symbol check)` and only hash + range were verified.
            Err(_) => verified.no_symbol_check = true,
        }
    }
    let snippet = lines[candidate.start_line - 1..candidate.end_line].join("\n");
    verified.file_hash_raw = hash;
    verified.snippet = Some(snippet.clone());
    Ok(VerifiedCandidate {
        candidate: verified,
        snippet,
    })
}

pub fn normalize_relative_path(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err("absolute candidate path".to_string());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => return Err("candidate path escapes worktree".to_string()),
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute candidate path".to_string());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("empty candidate path".to_string());
    }
    Ok(normalized)
}

pub fn hex_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_cannot_escape_and_hashes_are_raw_byte_hashes() {
        assert!(normalize_relative_path("../secret").is_err());
        assert!(normalize_relative_path("/tmp/secret").is_err());
        assert_eq!(
            hex_hash(b"nyan"),
            "f55b080c8b804ab13085ab5773040b6f5627bc09b27665c9be9410f095af20f8"
        );
    }

    #[test]
    fn symbol_candidates_are_checked_against_a_fresh_parse() {
        let root =
            std::env::temp_dir().join(format!("nyan-inspect-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let source = "fn alpha() {\n    1;\n}\n";
        std::fs::write(root.join("a.rs"), source).unwrap();
        let hash = hex_hash(source.as_bytes());
        let mut candidate = Candidate {
            branch: Branch::Symbol,
            path: "a.rs".to_string(),
            start_line: 1,
            end_line: 3,
            symbol: Some("alpha".to_string()),
            file_hash_raw: hash.clone(),
            ..Candidate::default()
        };
        assert!(inspect_candidate(&root, &candidate).is_ok());
        candidate.symbol = Some("beta".to_string());
        assert!(inspect_candidate(&root, &candidate).is_err());
        candidate.symbol = Some("alpha".to_string());
        candidate.start_line = 2;
        candidate.end_line = 3;
        // A sub-window of a long symbol still verifies.
        assert!(inspect_candidate(&root, &candidate).is_ok());
        // A stale hash is rejected before any symbol work.
        candidate.file_hash_raw = "0".repeat(64);
        assert!(
            inspect_candidate(&root, &candidate)
                .unwrap_err()
                .contains("changed since indexing")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unsupported_language_marks_no_symbol_check_instead_of_failing() {
        let root =
            std::env::temp_dir().join(format!("nyan-inspect-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let source = "some opaque config\nmore\n";
        std::fs::write(root.join("a.xyz"), source).unwrap();
        let candidate = Candidate {
            branch: Branch::Symbol,
            path: "a.xyz".to_string(),
            start_line: 1,
            end_line: 2,
            symbol: Some("some".to_string()),
            file_hash_raw: hex_hash(source.as_bytes()),
            ..Candidate::default()
        };
        let verified = inspect_candidate(&root, &candidate).unwrap();
        assert!(verified.candidate.no_symbol_check);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn non_utf8_sources_inspect_without_being_dropped() {
        let root =
            std::env::temp_dir().join(format!("nyan-inspect-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        // UTF-16LE with a BOM: valid text, invalid UTF-8. The index decodes it
        // through tool-io T5, so Inspect must decode it the same way or every
        // candidate from this file would be dropped as "invalid UTF-8".
        let source = "fn alpha() { /* 中文 */ }\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in source.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(root.join("a.rs"), &bytes).unwrap();
        let candidate = Candidate {
            branch: Branch::Text,
            path: "a.rs".to_string(),
            start_line: 1,
            end_line: 1,
            file_hash_raw: hex_hash(&bytes),
            ..Candidate::default()
        };
        let verified = inspect_candidate(&root, &candidate).unwrap();
        assert!(verified.snippet.contains("中文"));
        let _ = std::fs::remove_dir_all(root);
    }
}
