//! Maps to: CC `hooks/useTurnDiffs.ts:7-213`.
//! Incrementally extracts per-turn file edits from the business `Message`
//! history. It never reads the render-only `RenderableMessage` tree.
//!
//! CC stores a tool's typed output on `UserMessage.toolUseResult`; the Rust
//! business message normalizes it onto the enclosed `UserContent::ToolResult`
//! block as non-API raw `toolUseResult` data. This hook reads the same official
//! `filePath`/`structuredPatch`/`type`/`content` payload.

use crate::types::message::StructuredDiffHunk;
use crate::types::message::{Message, UserContent, UserMessage};
use iocraft::prelude::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnFileDiff {
    pub file_path: String,
    pub hunks: Vec<StructuredDiffHunk>,
    pub is_new_file: bool,
    pub lines_added: usize,
    pub lines_removed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnDiffStats {
    pub files_changed: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnDiff {
    pub turn_index: usize,
    pub user_prompt_preview: String,
    pub timestamp: String,
    pub files: BTreeMap<String, TurnFileDiff>,
    pub stats: TurnDiffStats,
}

#[derive(Clone, Debug, Default)]
pub struct TurnDiffCache {
    completed_turns: Vec<TurnDiff>,
    current_turn: Option<TurnDiff>,
    last_processed_index: usize,
    last_turn_index: usize,
}

#[derive(Clone, Debug)]
struct FileEditResult {
    file_path: String,
    structured_patch: Vec<StructuredDiffHunk>,
    is_new_file: bool,
    content: Option<String>,
}

fn hunk_from_value(value: &serde_json::Value) -> Option<StructuredDiffHunk> {
    Some(StructuredDiffHunk {
        old_start: value.get("oldStart")?.as_u64()? as usize,
        old_lines: value.get("oldLines")?.as_u64()? as usize,
        new_start: value.get("newStart")?.as_u64()? as usize,
        new_lines: value.get("newLines")?.as_u64()? as usize,
        lines: value
            .get("lines")?
            .as_array()?
            .iter()
            .map(|line| line.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()?,
    })
}

/// Maps to: CC `hooks/useTurnDiffs.ts:36-53`
/// `isFileEditResult`/`isFileWriteOutput`.
fn file_edit_result(message: &UserMessage) -> Option<FileEditResult> {
    let result = message.content.iter().find_map(|content| match content {
        UserContent::ToolResult(result) => Some(result),
        _ => None,
    })?;
    let value = result.tool_use_result.as_ref()?;
    let file_path = value.get("filePath")?.as_str()?.to_string();
    let structured_patch = value
        .get("structuredPatch")
        .and_then(serde_json::Value::as_array)
        .and_then(|hunks| {
            hunks
                .iter()
                .map(hunk_from_value)
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default();
    let is_new_file = value.get("type").and_then(serde_json::Value::as_str) == Some("create")
        && value
            .get("content")
            .is_some_and(serde_json::Value::is_string);
    if structured_patch.is_empty() && !is_new_file {
        return None;
    }
    Some(FileEditResult {
        file_path,
        structured_patch,
        is_new_file,
        content: value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

/// Maps to: CC `hooks/useTurnDiffs.ts:55-68` `countHunkLines`.
pub fn count_hunk_lines(hunks: &[StructuredDiffHunk]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in hunks.iter().flat_map(|hunk| hunk.lines.iter()) {
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// Maps to: CC `hooks/useTurnDiffs.ts:70-77` `getUserPromptPreview`.
pub fn user_prompt_preview(message: &UserMessage) -> String {
    let text = message
        .content
        .iter()
        .find_map(|content| match content {
            UserContent::Text(text) | UserContent::MetaText(text) => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("");
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= 30 {
        text.to_string()
    } else {
        format!("{}…", chars[..29].iter().collect::<String>())
    }
}

/// Maps to: CC `hooks/useTurnDiffs.ts:79-91` `computeTurnStats`.
fn compute_turn_stats(turn: &mut TurnDiff) {
    turn.stats = TurnDiffStats {
        files_changed: turn.files.len(),
        lines_added: turn.files.values().map(|file| file.lines_added).sum(),
        lines_removed: turn.files.values().map(|file| file.lines_removed).sum(),
    };
}

fn is_tool_result(message: &UserMessage) -> bool {
    message
        .content
        .iter()
        .any(|content| matches!(content, UserContent::ToolResult(_)))
}

/// Pure incremental core of CC `useTurnDiffs` (`hooks/useTurnDiffs.ts:100-213`).
pub fn extract_turn_diffs(messages: &[Message], cache: &mut TurnDiffCache) -> Vec<TurnDiff> {
    if messages.len() < cache.last_processed_index {
        *cache = TurnDiffCache::default();
    }

    for message in messages.iter().skip(cache.last_processed_index) {
        let Message::User(user) = message else {
            continue;
        };
        let tool_result = is_tool_result(user);
        if !tool_result && !user.is_compact_summary {
            if let Some(mut current) = cache.current_turn.take()
                && !current.files.is_empty()
            {
                compute_turn_stats(&mut current);
                cache.completed_turns.push(current);
            }
            cache.last_turn_index += 1;
            cache.current_turn = Some(TurnDiff {
                turn_index: cache.last_turn_index,
                user_prompt_preview: user_prompt_preview(user),
                timestamp: user.timestamp.to_rfc3339(),
                ..TurnDiff::default()
            });
            continue;
        }

        let Some(current_turn) = cache.current_turn.as_mut() else {
            continue;
        };
        let Some(result) = file_edit_result(user) else {
            continue;
        };
        let entry = current_turn
            .files
            .entry(result.file_path.clone())
            .or_insert_with(|| TurnFileDiff {
                file_path: result.file_path.clone(),
                is_new_file: result.is_new_file,
                ..TurnFileDiff::default()
            });

        if result.is_new_file && result.structured_patch.is_empty() {
            let lines = result
                .content
                .as_deref()
                .unwrap_or_default()
                .split('\n')
                .collect::<Vec<_>>();
            entry.hunks.push(StructuredDiffHunk {
                old_start: 0,
                old_lines: 0,
                new_start: 1,
                new_lines: lines.len(),
                lines: lines.iter().map(|line| format!("+{line}")).collect(),
            });
            entry.lines_added += lines.len();
        } else {
            let (added, removed) = count_hunk_lines(&result.structured_patch);
            entry.hunks.extend(result.structured_patch);
            entry.lines_added += added;
            entry.lines_removed += removed;
        }
        if result.is_new_file {
            entry.is_new_file = true;
        }
    }

    cache.last_processed_index = messages.len();
    let mut result = cache.completed_turns.clone();
    if let Some(current) = cache.current_turn.as_ref()
        && !current.files.is_empty()
    {
        let mut current = current.clone();
        compute_turn_stats(&mut current);
        result.push(current);
    }
    result.reverse();
    result
}

/// Maps to: CC `hooks/useTurnDiffs.ts:100-213` `useTurnDiffs`.
pub fn use_turn_diffs(hooks: &mut Hooks<'_, '_>, messages: &[Message]) -> Vec<TurnDiff> {
    let mut cache = hooks.use_ref(TurnDiffCache::default);
    let mut cache = cache.write();
    extract_turn_diffs(messages, &mut cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::ToolUseId;
    use crate::types::message::{ToolResult, UserMessage};
    use chrono::{TimeZone, Utc};

    fn user_text(text: &str, second: i64) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc.timestamp_opt(second, 0).unwrap(),
            content: vec![UserContent::Text(text.to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })
    }

    fn edit_result(value: serde_json::Value, second: i64) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc.timestamp_opt(second, 0).unwrap(),
            content: vec![UserContent::ToolResult(ToolResult {
                tool_use_id: ToolUseId("toolu_1".to_string()),
                content: value.to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: Some(value),
            })],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })
    }

    #[test]
    fn extract_turn_diffs_matches_official_accumulation_order_and_stats() {
        let messages = vec![
            user_text("first prompt that is deliberately longer than thirty", 1),
            edit_result(
                serde_json::json!({
                    "filePath": "src/lib.rs",
                    "structuredPatch": [{
                        "oldStart": 1, "oldLines": 1,
                        "newStart": 1, "newLines": 2,
                        "lines": ["-old", "+new", "+more"]
                    }]
                }),
                2,
            ),
            user_text("second", 3),
            edit_result(
                serde_json::json!({
                    "type": "create", "filePath": "new.txt",
                    "structuredPatch": [], "content": "a\nb"
                }),
                4,
            ),
        ];
        let mut cache = TurnDiffCache::default();
        let turns = extract_turn_diffs(&messages, &mut cache);

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].turn_index, 2);
        assert_eq!(turns[0].files["new.txt"].lines_added, 2);
        assert!(turns[0].files["new.txt"].is_new_file);
        assert_eq!(turns[1].stats.lines_added, 2);
        assert_eq!(turns[1].stats.lines_removed, 1);
        assert_eq!(turns[1].user_prompt_preview.chars().count(), 30);
        assert!(turns[1].user_prompt_preview.ends_with('…'));
    }

    #[test]
    fn extract_turn_diffs_resets_when_messages_shrink_like_official_rewind() {
        let mut cache = TurnDiffCache::default();
        let first = vec![user_text("first", 1), user_text("second", 2)];
        assert!(extract_turn_diffs(&first, &mut cache).is_empty());

        let rewound = vec![user_text("replacement", 3)];
        assert!(extract_turn_diffs(&rewound, &mut cache).is_empty());
        assert_eq!(cache.last_turn_index, 1);
    }
}
