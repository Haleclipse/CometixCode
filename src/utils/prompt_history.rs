//! Maps to: CC `history.ts` (prompt history + pasted-ref helpers).
//! This module reads `${CLAUDE_CONFIG_DIR}/history.jsonl` newest-first for
//! Up/Down navigation and Ctrl-R reverse search. Writes preserve the official
//! file shape but stay behind Cometix's explicit write opt-in so the default
//! main-screen path remains UI-only/read-only.
//!
//! Also owns `parseReferences` / `formatImageRef` from the same official file.

use crate::components::prompt_input::input_paste::PastedContent;
use crate::utils::{config, env_utils};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use uuid::Uuid;

const MAX_HISTORY_ITEMS: usize = 100;

/// Maps to: CC `history.ts` parseReferences match object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextReference {
    pub id: usize,
    pub match_text: String,
    /// Byte offset into the Rust `str` (official JS `index` is UTF-16; Cometix
    /// cursor/editing uses UTF-8 byte offsets).
    pub index: usize,
}

/// Maps to: CC `history.ts#formatImageRef`.
pub fn format_image_ref(id: usize) -> String {
    format!("[Image #{id}]")
}

/// Maps to: CC `history.ts#parseReferences`.
pub fn parse_references(input: &str) -> Vec<TextReference> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\[(Pasted text|Image|\.\.\.Truncated text) #(\d+)(?: \+\d+ lines)?(\.)*\]")
            .expect("valid history reference pattern")
    });
    re.captures_iter(input)
        .filter_map(|caps| {
            let full = caps.get(0)?;
            let id: usize = caps.get(2)?.as_str().parse().ok()?;
            (id > 0).then(|| TextReference {
                id,
                match_text: full.as_str().to_string(),
                index: full.start(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub display: String,
    pub timestamp_ms: i64,
    pub pasted_contents: BTreeMap<usize, PastedContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogEntry {
    display: String,
    #[serde(default)]
    pasted_contents: serde_json::Value,
    timestamp: i64,
    project: String,
    session_id: Option<String>,
}

pub fn history_file_path() -> PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir().join("history.jsonl")
}

/// Maps to: `addToHistory(command)`.
pub fn add_to_history(command: impl AsRef<str>) {
    add_to_history_with_pasted(command, &BTreeMap::new());
}

pub fn add_to_history_with_pasted(
    command: impl AsRef<str>,
    pasted_contents: &BTreeMap<usize, PastedContent>,
) {
    if !history_write_enabled()
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_SKIP_PROMPT_HISTORY")
                .as_deref(),
        )
    {
        return;
    }

    let display = command.as_ref();
    if display.trim().is_empty() {
        return;
    }

    let entry = LogEntry {
        display: display.to_string(),
        pasted_contents: pasted_contents_to_json(pasted_contents),
        timestamp: Utc::now().timestamp_millis(),
        project: current_project_root(),
        session_id: Some(current_session_id().to_string()),
    };

    if let Err(_err) = append_log_entry(&entry) {
        // Match CC's behavior: history write failures are non-fatal and should
        // never disturb the prompt input path.
    }
}

/// Maps to: `getHistory()`; returns current-project entries, newest first, with
/// this process' session entries before other sessions within the scanned window.
pub fn get_history() -> Vec<HistoryEntry> {
    let path = history_file_path();
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let project = current_project_root();
    let session = current_session_id();
    let mut current_session = Vec::new();
    let mut other_sessions = Vec::new();

    for line in content.lines().rev() {
        let Ok(entry) = serde_json::from_str::<LogEntry>(line) else {
            continue;
        };
        if entry.project != project {
            continue;
        }

        let history_entry = HistoryEntry {
            display: entry.display,
            timestamp_ms: entry.timestamp,
            pasted_contents: pasted_contents_from_json(&entry.pasted_contents),
        };
        if entry.session_id.as_deref() == Some(session) {
            current_session.push(history_entry);
        } else {
            other_sessions.push(history_entry);
        }

        if current_session.len() + other_sessions.len() >= MAX_HISTORY_ITEMS {
            break;
        }
    }

    current_session
        .into_iter()
        .chain(other_sessions)
        .take(MAX_HISTORY_ITEMS)
        .collect()
}

/// Maps to Ctrl-R reverse history search. CC uses case-sensitive `lastIndexOf`.
pub fn find_history_match(query: &str, seen: &[String]) -> Option<HistoryEntry> {
    if query.is_empty() {
        return None;
    }

    get_history().into_iter().find(|entry| {
        entry.display.rfind(query).is_some() && !seen.iter().any(|seen| seen == &entry.display)
    })
}

fn pasted_contents_to_json(contents: &BTreeMap<usize, PastedContent>) -> serde_json::Value {
    serde_json::Value::Object(
        contents
            .iter()
            .map(|(id, content)| {
                let value = match content {
                    PastedContent::Text { content, .. } => json!({
                        "id": id,
                        "type": "text",
                        "content": content,
                    }),
                    PastedContent::Image {
                        media_type,
                        data,
                        filename,
                        dimensions,
                        source_path,
                        ..
                    } => json!({
                        "id": id,
                        "type": "image",
                        "mediaType": media_type,
                        "content": data,
                        "filename": filename,
                        "dimensions": dimensions,
                        "sourcePath": source_path,
                    }),
                };
                (id.to_string(), value)
            })
            .collect(),
    )
}

fn pasted_contents_from_json(value: &serde_json::Value) -> BTreeMap<usize, PastedContent> {
    value
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter_map(|(key, value)| {
            let id = value
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .map(|id| id as usize)
                .or_else(|| key.parse::<usize>().ok())?;
            let kind = value.get("type").and_then(serde_json::Value::as_str);
            let content = if kind == Some("image") || value.get("mediaType").is_some() {
                PastedContent::Image {
                    id,
                    media_type: value
                        .get("mediaType")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    data: value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    filename: value
                        .get("filename")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    dimensions: value
                        .get("dimensions")
                        .cloned()
                        .and_then(|dimensions| serde_json::from_value(dimensions).ok()),
                    source_path: value
                        .get("sourcePath")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                }
            } else {
                PastedContent::Text {
                    id,
                    content: value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }
            };
            Some((id, content))
        })
        .collect()
}

fn history_write_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("COMETIX_WRITE_ENABLED").as_deref())
}

fn append_log_entry(entry: &LogEntry) -> anyhow::Result<()> {
    let path = history_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;
    writeln!(file, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

fn current_session_id() -> &'static str {
    static SESSION_ID: OnceLock<String> = OnceLock::new();
    SESSION_ID.get_or_init(|| Uuid::new_v4().to_string())
}

fn current_project_root() -> String {
    std::env::current_dir()
        .ok()
        .map(|path| config::normalize_project_path(&path.to_string_lossy()))
        .unwrap_or_else(|| ".".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvLock};

    fn env_lock() -> &'static TestEnvLock {
        &TEST_ENV_LOCK
    }

    fn temp_history_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cometix-history-test-{}", Uuid::new_v4()))
    }

    #[test]
    fn add_to_history_is_readonly_without_explicit_write_opt_in() {
        let _lock = env_lock().lock().unwrap();
        let config_home = temp_history_home();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        let _skip_guard = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");

        add_to_history("/resume");

        assert!(!history_file_path().exists());
        let _ = fs::remove_dir_all(config_home);
    }

    #[test]
    fn add_to_history_writes_only_with_explicit_write_opt_in() {
        let _lock = env_lock().lock().unwrap();
        let config_home = temp_history_home();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip_guard = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");

        add_to_history("/resume");

        let content = fs::read_to_string(history_file_path()).unwrap();
        assert!(content.contains("/resume"));
        let _ = fs::remove_dir_all(config_home);
    }

    #[test]
    fn pasted_contents_round_trip_text_and_image_metadata() {
        let contents = BTreeMap::from([
            (
                3,
                PastedContent::Text {
                    id: 3,
                    content: "large paste".to_string(),
                },
            ),
            (
                4,
                PastedContent::Image {
                    id: 4,
                    media_type: Some("image/png".to_string()),
                    data: Some("AAAA".to_string()),
                    filename: Some("Pasted image".to_string()),
                    dimensions: Some(crate::utils::image_resizer::ImageDimensions {
                        original_width: Some(10),
                        original_height: Some(20),
                        display_width: Some(10),
                        display_height: Some(20),
                    }),
                    source_path: Some("/tmp/pasted.png".to_string()),
                },
            ),
        ]);
        assert_eq!(
            pasted_contents_from_json(&pasted_contents_to_json(&contents)),
            contents
        );
    }

    #[test]
    fn find_history_match_skips_seen_entries() {
        let seen = vec!["hello world".to_string()];
        let entry = HistoryEntry {
            display: "hello world".to_string(),
            timestamp_ms: 0,
            pasted_contents: BTreeMap::new(),
        };
        assert!(seen.iter().any(|s| s == &entry.display));
    }

    #[test]
    fn parse_references_matches_image_pasted_and_truncated_chips() {
        let input = "a [Image #1] b [Pasted text #2 +3 lines] c [...Truncated text #4 +9 lines...]";
        let refs = parse_references(input);
        assert_eq!(
            refs.iter()
                .map(|r| (r.id, r.match_text.as_str(), r.index))
                .collect::<Vec<_>>(),
            vec![
                (1, "[Image #1]", input.find("[Image #1]").unwrap()),
                (
                    2,
                    "[Pasted text #2 +3 lines]",
                    input.find("[Pasted text #2 +3 lines]").unwrap()
                ),
                (
                    4,
                    "[...Truncated text #4 +9 lines...]",
                    input.find("[...Truncated text #4 +9 lines...]").unwrap()
                ),
            ]
        );
        assert_eq!(format_image_ref(12), "[Image #12]");
    }
}
