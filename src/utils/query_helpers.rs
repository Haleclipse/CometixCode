//! Resume-state helpers matching official `utils/queryHelpers.ts` extraction.
//! Read/Write entries come from transcript payloads; successful Edit entries
//! refresh current disk content and mtime exactly once during cold restore.

use crate::tools::bash_tool::tool_name::BASH_TOOL_NAME;
use crate::utils::file_state_cache::{DEFAULT_MAX_CACHE_SIZE_BYTES, FileStateCache};
use chrono::DateTime;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const ASK_READ_FILE_STATE_CACHE_SIZE: usize = 10;

const FILE_READ_TOOL_NAME: &str = "Read";
const FILE_WRITE_TOOL_NAME: &str = "Write";
const FILE_EDIT_TOOL_NAME: &str = "Edit";
const FILE_UNCHANGED_STUB: &str = "File unchanged since last read. The content from the earlier Read tool_result in this conversation is still current — refer to that instead of re-reading.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadFileStateSource {
    Read,
    Write,
    EditRefresh,
    NestedMemory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadFileStateEntry {
    pub path: String,
    pub content: Option<String>,
    pub timestamp_ms: Option<i64>,
    /// Raw JavaScript Read range values. `None` is `undefined`; other JSON
    /// values survive authoritative post-hook replacement without coercion.
    pub offset: Option<serde_json::Value>,
    pub limit: Option<serde_json::Value>,
    /// Maps to CC `FileState.isPartialView`; Edit/Write must require a direct
    /// Read before trusting auto-injected content that differs from disk.
    pub is_partial_view: bool,
    pub source: ReadFileStateSource,
}

impl ReadFileStateEntry {
    fn new(
        path: String,
        content: Option<String>,
        timestamp_ms: Option<i64>,
        source: ReadFileStateSource,
    ) -> Self {
        Self {
            path,
            content,
            timestamp_ms,
            offset: None,
            limit: None,
            is_partial_view: false,
            source,
        }
    }
}

/// Maps to: CC `utils/queryHelpers.ts#extractReadFilesFromMessages`.
pub fn extract_read_files_from_messages(
    messages: &[Value],
    cwd: &str,
    max_size: usize,
) -> Vec<ReadFileStateEntry> {
    let mut file_read_tool_use_ids: HashMap<String, String> = HashMap::new();
    let mut file_write_tool_use_ids: HashMap<String, (String, String)> = HashMap::new();
    let mut file_edit_tool_use_ids: HashMap<String, String> = HashMap::new();

    for message in messages {
        if message.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        for content in message_content_blocks(message) {
            if content.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(tool_use_id) = content.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(name) = content.get("name").and_then(Value::as_str) else {
                continue;
            };
            let input = content.get("input").unwrap_or(&Value::Null);
            match name {
                FILE_READ_TOOL_NAME => {
                    let Some(file_path) = non_empty_input_string(input, "file_path") else {
                        continue;
                    };
                    if has_non_null_key(input, "offset") || has_non_null_key(input, "limit") {
                        continue;
                    }
                    file_read_tool_use_ids
                        .insert(tool_use_id.to_string(), expand_path(file_path, cwd));
                }
                FILE_WRITE_TOOL_NAME => {
                    let Some(file_path) = non_empty_input_string(input, "file_path") else {
                        continue;
                    };
                    let Some(content) = non_empty_input_string(input, "content") else {
                        continue;
                    };
                    file_write_tool_use_ids.insert(
                        tool_use_id.to_string(),
                        (expand_path(file_path, cwd), content.to_string()),
                    );
                }
                FILE_EDIT_TOOL_NAME => {
                    let Some(file_path) = non_empty_input_string(input, "file_path") else {
                        continue;
                    };
                    file_edit_tool_use_ids
                        .insert(tool_use_id.to_string(), expand_path(file_path, cwd));
                }
                _ => {}
            }
        }
    }

    let mut cache = FileStateCache::with_size_limit(max_size, DEFAULT_MAX_CACHE_SIZE_BYTES);
    for message in messages {
        if message.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        for content in message_content_blocks(message) {
            if content.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(tool_use_id) = content.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };

            if let Some(read_file_path) = file_read_tool_use_ids.get(tool_use_id) {
                if let Some(result_content) = content.get("content").and_then(Value::as_str) {
                    if !result_content.starts_with(FILE_UNCHANGED_STUB) {
                        if let Some(timestamp_ms) = timestamp_ms(message) {
                            let processed = strip_system_reminders(result_content);
                            let file_content = processed
                                .split('\n')
                                .map(strip_line_number_prefix)
                                .collect::<Vec<_>>()
                                .join("\n")
                                .trim()
                                .to_string();
                            let entry = ReadFileStateEntry::new(
                                read_file_path.clone(),
                                Some(file_content),
                                Some(timestamp_ms),
                                ReadFileStateSource::Read,
                            );
                            cache.set(Path::new(read_file_path), entry);
                        }
                    }
                }
            }

            if let Some((write_file_path, write_content)) = file_write_tool_use_ids.get(tool_use_id)
            {
                if let Some(timestamp_ms) = timestamp_ms(message) {
                    let entry = ReadFileStateEntry::new(
                        write_file_path.clone(),
                        Some(write_content.clone()),
                        Some(timestamp_ms),
                        ReadFileStateSource::Write,
                    );
                    cache.set(Path::new(write_file_path), entry);
                }
            }

            if let Some(edit_file_path) = file_edit_tool_use_ids.get(tool_use_id) {
                if content.get("is_error").and_then(Value::as_bool) != Some(true) {
                    let path = Path::new(edit_file_path);
                    match (
                        crate::utils::file_read::read_file_sync_with_metadata(path),
                        crate::utils::file::get_file_modification_time_result(path),
                    ) {
                        (Ok(metadata), Ok(timestamp_ms)) => {
                            let entry = ReadFileStateEntry::new(
                                edit_file_path.clone(),
                                Some(metadata.content),
                                Some(timestamp_ms),
                                ReadFileStateSource::EditRefresh,
                            );
                            cache.set(Path::new(edit_file_path), entry);
                        }
                        (Err(error), _) | (_, Err(error)) => {
                            // CC skips files deleted/inaccessible since Edit.
                            // The Vec-returning Rust compatibility API cannot
                            // throw unexpected I/O errors, so log those while
                            // retaining the same safe skip behavior.
                            if !matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound
                                    | std::io::ErrorKind::PermissionDenied
                                    | std::io::ErrorKind::NotADirectory
                            ) {
                                crate::utils::debug::log_for_debugging(&format!(
                                    "Unable to restore Edit read-file state for {edit_file_path}: {error}"
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    cache.entries_lru_to_mru()
}

/// Maps to: CC `utils/queryHelpers.ts#extractBashToolsFromMessages`.
pub fn extract_bash_tools_from_messages(messages: &[Value]) -> Vec<String> {
    let mut tools = Vec::new();
    let mut seen = HashSet::new();

    for message in messages {
        if message.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        for content in message_content_blocks(message) {
            if content.get("type").and_then(Value::as_str) != Some("tool_use")
                || content.get("name").and_then(Value::as_str) != Some(BASH_TOOL_NAME)
            {
                continue;
            }
            let command = content
                .get("input")
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str);
            if let Some(tool) = extract_cli_name(command) {
                if seen.insert(tool.clone()) {
                    tools.push(tool);
                }
            }
        }
    }

    tools
}

fn message_content_blocks(message: &Value) -> &[Value] {
    message
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn non_empty_input_string<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn has_non_null_key(input: &Value, key: &str) -> bool {
    input.get(key).is_some_and(|value| !value.is_null())
}

fn timestamp_ms(message: &Value) -> Option<i64> {
    let timestamp = message.get("timestamp").and_then(Value::as_str)?;
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|value| value.timestamp_millis())
}

fn strip_system_reminders(content: &str) -> String {
    let mut output = String::new();
    let mut rest = content;
    loop {
        let Some(start) = rest.find("<system-reminder>") else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..start]);
        let after_start = &rest[start + "<system-reminder>".len()..];
        let Some(end) = after_start.find("</system-reminder>") else {
            output.push_str(&rest[start..]);
            break;
        };
        rest = &after_start[end + "</system-reminder>".len()..];
    }
    output
}

fn strip_line_number_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    let digit_end = trimmed
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()
        .unwrap_or(0);
    if digit_end == 0 {
        return line;
    }
    let after_digits = &trimmed[digit_end..];
    let Some(separator) = after_digits.chars().next() else {
        return line;
    };
    if separator == '\t' || separator == '\u{2192}' {
        &after_digits[separator.len_utf8()..]
    } else {
        line
    }
}

fn extract_cli_name(command: Option<&str>) -> Option<String> {
    let tokens = command?.trim().split_whitespace();
    for token in tokens {
        if is_env_assignment(token) || token == "sudo" {
            continue;
        }
        return Some(token.to_string());
    }
    None
}

fn is_env_assignment(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    for ch in chars {
        if ch == '=' {
            return true;
        }
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            return false;
        }
    }
    false
}

fn expand_path(path: &str, cwd: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return normalize_path_string(PathBuf::from(cwd));
    }
    if trimmed == "~" {
        return crate::utils::node_os::homedir()
            .to_string_lossy()
            .into_owned();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return normalize_path_string(crate::utils::node_os::homedir().join(rest));
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        normalize_path_string(candidate.to_path_buf())
    } else {
        normalize_path_string(Path::new(cwd).join(candidate))
    }
}

fn normalize_path_string(path: PathBuf) -> String {
    path.components().collect::<PathBuf>().display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_read_files_caches_whole_read_results_like_official() {
        let messages = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "read-1", "name": "Read", "input": {"file_path": "src/lib.rs"}}]}
            }),
            json!({
                "type": "user",
                "timestamp": "2026-06-13T13:40:30.000Z",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "read-1", "content": "     1→fn main() {}\n<system-reminder>ignore</system-reminder>     2\tprintln!();"}]}
            }),
        ];

        let files = extract_read_files_from_messages(
            &messages,
            "/repo",
            crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE,
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/repo/src/lib.rs");
        assert_eq!(
            files[0].content.as_deref(),
            Some("fn main() {}\nprintln!();")
        );
        assert_eq!(files[0].timestamp_ms, Some(1_781_358_030_000));
        assert_eq!(files[0].source, ReadFileStateSource::Read);
    }

    #[test]
    fn extract_read_files_skips_ranged_reads_and_unchanged_stub() {
        let messages = vec![
            json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "range", "name": "Read", "input": {"file_path": "a.rs", "offset": 1}},
                    {"type": "tool_use", "id": "stub", "name": "Read", "input": {"file_path": "b.rs"}}
                ]}
            }),
            json!({
                "type": "user",
                "timestamp": "2026-06-13T13:40:30.000Z",
                "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "range", "content": "1→ignored"},
                    {"type": "tool_result", "tool_use_id": "stub", "content": FILE_UNCHANGED_STUB}
                ]}
            }),
        ];

        assert!(
            extract_read_files_from_messages(
                &messages,
                "/repo",
                crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE,
            )
            .is_empty()
        );
    }

    #[test]
    fn extract_read_files_records_write_content_and_refreshes_edit_from_disk() {
        let root = std::env::temp_dir().join(format!(
            "cometix-resume-edit-state-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("b.rs"), "post edit\n").unwrap();
        let messages = vec![
            json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "write-1", "name": "Write", "input": {"file_path": "a.rs", "content": "new content"}},
                    {"type": "tool_use", "id": "edit-1", "name": "Edit", "input": {"file_path": "b.rs"}},
                    {"type": "tool_use", "id": "edit-err", "name": "Edit", "input": {"file_path": "c.rs"}}
                ]}
            }),
            json!({
                "type": "user",
                "timestamp": "2026-06-13T13:40:30.000Z",
                "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "write-1", "content": "ok"},
                    {"type": "tool_result", "tool_use_id": "edit-1", "content": "ok", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "edit-err", "content": "bad", "is_error": true}
                ]}
            }),
        ];

        let files = extract_read_files_from_messages(
            &messages,
            &root.display().to_string(),
            crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE,
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, root.join("a.rs").display().to_string());
        assert_eq!(files[0].content.as_deref(), Some("new content"));
        assert_eq!(files[0].source, ReadFileStateSource::Write);
        assert_eq!(files[1].path, root.join("b.rs").display().to_string());
        assert_eq!(files[1].content.as_deref(), Some("post edit\n"));
        assert_eq!(
            files[1].timestamp_ms,
            crate::utils::file::get_file_modification_time(&root.join("b.rs"))
        );
        assert_eq!(files[1].source, ReadFileStateSource::EditRefresh);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn extract_read_files_uses_canonical_lru_last_write_matches_official() {
        let messages = vec![
            json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "write-1", "name": "Write", "input": {"file_path": "a.rs", "content": "one"}},
                    {"type": "tool_use", "id": "write-2", "name": "Write", "input": {"file_path": "b.rs", "content": "two"}},
                    {"type": "tool_use", "id": "write-3", "name": "Write", "input": {"file_path": "a.rs", "content": "three"}}
                ]}
            }),
            json!({
                "type": "user",
                "timestamp": "2026-06-13T13:40:30.000Z",
                "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "write-1", "content": "ok"},
                    {"type": "tool_result", "tool_use_id": "write-2", "content": "ok"},
                    {"type": "tool_result", "tool_use_id": "write-3", "content": "ok"}
                ]}
            }),
        ];

        let files = extract_read_files_from_messages(&messages, "/repo", 1);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/repo/a.rs");
        assert_eq!(files[0].content.as_deref(), Some("three"));
    }

    #[test]
    fn extract_bash_tools_skips_env_assignments_and_sudo_like_official() {
        let messages = vec![json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "id": "bash-1", "name": "Bash", "input": {"command": "FOO=bar sudo git status"}},
                {"type": "tool_use", "id": "bash-2", "name": "Bash", "input": {"command": "AWS_PROFILE=dev aws s3 ls"}},
                {"type": "tool_use", "id": "bash-3", "name": "Bash", "input": {"command": "git diff"}}
            ]}
        })];

        assert_eq!(
            extract_bash_tools_from_messages(&messages),
            vec!["git".to_string(), "aws".to_string()]
        );
    }
}
