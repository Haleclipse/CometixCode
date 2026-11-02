//! Typed session JSONL → `Vec<Message>` deserialization pipeline for resume.
//! Maps to: CC `utils/conversationRecovery.ts` — this file owns the
//! **typed-message** segment of that module: `deserializeMessages`:154,
//! `deserializeMessagesWithInterruptDetection`:164, `detectTurnInterruption`,
//! `restoreSkillStateFromMessages`:382, the "Continue from where you left off."
//! continuation sentinel, `DeserializeResult` / `TurnInterruptionState`, and the
//! raw orphan/unresolved boundary filters. Whitespace filtering and user merging
//! import their canonical `utils/messages.ts` owner.
//! The **renderable-projection** segment of the same CC module lives in
//! `utils/conversation_recovery.rs` (`renderable_messages_from_entries`,
//! `load_conversation_for_resume`). The two are complementary, not duplicates:
//! resume call sites invoke both — typed output feeds state, renderable output
//! feeds display (see `session_restore.rs` / `screens/repl.rs`).
//!
//! ```text
//! SerializedMessage[] (already selected by sessionStorage.ts buildConversationChain)
//!     ↓
//! migrateLegacyAttachmentTypes + permissionMode validation
//!     ↓
//! filterUnresolvedToolUses + filterOrphanedThinkingOnly + filterWhitespaceOnlyAssistant
//!     ↓
//! detectTurnInterruption + continuation/sentinel insertion
//!     ↓
//! DeserializeResult { messages, turn_interruption_state }
//! ```
//! ## Remaining alignment items
//! - [x] `isTerminalToolResult` — brief-mode SendUserMessage detection inside `detect_turn_interruption`
//! - [x] `mergeUserMessages` — basic content merge + tool_result hoist
//! - [x] `normalizeMessages` — split multi-block messages + stable derived UUIDs
//! - [x] `progressBridge` — handled at `session_storage::load_session_structured_from_path`
//! - [ ] `normalizeMessagesForAPI` same message.id assistant merge (API path, not main-screen)
//! - [x] `loadConversationForResume` outer in-memory restore → `utils/session_restore.rs`
//! - [ ] session-file adoption / worktree chdir side effects (disabled with session writes)

use crate::types::ids::ToolUseId;
use crate::types::message::{
    AssistantContent, AssistantMessage, AttachmentMessage, HookResultMessage, Message, StopReason,
    TokenUsage, ToolResult, ToolUseBlock, UserContent, UserMessage,
};
pub use crate::utils::messages::NO_RESPONSE_REQUESTED;

use chrono::{DateTime, Utc};
use std::collections::HashSet;

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub const CONTINUATION_MESSAGE: &str = "Continue from where you left off.";

pub const TERMINAL_TOOL_NAMES: &[&str] = &[
    "SendUserMessage",   // BRIEF_TOOL_NAME (current)
    "send_user_message", // LEGACY_BRIEF_TOOL_NAME (legacy snake_case)
    "SendUserFile",      // SEND_USER_FILE_TOOL_NAME
];

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub struct DeserializeResult {
    pub messages: Vec<serde_json::Value>,
    pub turn_interruption_state: TurnInterruptionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnInterruptionState {
    None,
    /// Official converts interrupted_turn into this state after appending a
    /// synthetic continuation message.
    InterruptedPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InternalInterruptionState {
    None,
    InterruptedTurn,
    InterruptedPrompt,
}

/// Read-only counterpart of official `restoreSkillStateFromMessages()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillRestoreState {
    pub invoked_skills: Vec<InvokedSkill>,
    pub suppress_next_skill_listing: bool,
}

/// Stored skill payload restored from `invoked_skills` attachments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokedSkill {
    pub name: String,
    pub path: String,
    pub content: String,
}

// ════════════════════════════════════════════════════════════
// deserializeMessages / deserializeMessagesWithInterruptDetection
// ════════════════════════════════════════════════════════════

pub fn deserialize_messages(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    deserialize_messages_with_interrupt_detection(messages).messages
}

///   1. migrateLegacyAttachmentTypes
///   2. validate permissionMode
///   3. filterUnresolvedToolUses
///   4. filterOrphanedThinkingOnlyMessages
///   5. filterWhitespaceOnlyAssistantMessages
///   6. detectTurnInterruption
///   7. interrupted_turn → append continuation
pub fn deserialize_messages_with_interrupt_detection(
    messages: Vec<serde_json::Value>,
) -> DeserializeResult {
    // 1. migrateLegacyAttachmentTypes
    let migrated: Vec<serde_json::Value> = messages
        .into_iter()
        .map(migrate_legacy_attachment)
        .collect();

    let validated = validate_permission_modes(migrated);

    // 3. filterUnresolvedToolUses
    let filtered_tools = filter_unresolved_tool_uses(validated);

    // 4. filterOrphanedThinkingOnlyMessages
    let filtered_thinking = filter_orphaned_thinking_only(filtered_tools);

    // 5. filterWhitespaceOnlyAssistantMessages
    let filtered_whitespace =
        crate::utils::messages::filter_whitespace_only_assistant_messages(filtered_thinking);

    let (final_messages, interruption) = detect_and_append_sentinel(filtered_whitespace);

    DeserializeResult {
        messages: final_messages,
        turn_interruption_state: interruption,
    }
}

// Step 1/2 live in `utils/session_storage.rs` as `build_conversation_chain`
// and `remove_extra_fields`, matching official `sessionStorage.ts`.

/// Rust subset of official `messages.ts#normalizeMessages()`.
/// It splits multi-block user/assistant messages into one message per content
/// block and derives stable UUID-shaped keys after the first split. This is
/// also used for synthetic resume continuation messages so their shape matches
/// the official normalized user message (`content: [{ type: 'text', text }]`).
pub fn normalize_messages(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut is_new_chain = false;
    let mut normalized = Vec::new();

    for message in messages {
        match message.get("type").and_then(|value| value.as_str()) {
            Some("assistant") => {
                let content = assistant_content_blocks(&message);
                if content.len() > 1 {
                    is_new_chain = true;
                }
                for (index, block) in content.into_iter().enumerate() {
                    normalized.push(with_single_content_block(
                        &message,
                        block,
                        if is_new_chain { Some(index) } else { None },
                        true,
                    ));
                }
            }
            Some("user") => {
                let content = user_content_blocks(&message);
                if content.len() > 1 {
                    is_new_chain = true;
                }
                for (index, block) in content.into_iter().enumerate() {
                    normalized.push(with_single_content_block(
                        &message,
                        block,
                        if is_new_chain { Some(index) } else { None },
                        false,
                    ));
                }
            }
            _ => normalized.push(message),
        }
    }

    normalized
}

fn assistant_content_blocks(message: &serde_json::Value) -> Vec<serde_json::Value> {
    match message.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::Array(blocks)) => blocks.clone(),
        Some(serde_json::Value::String(text)) => {
            vec![serde_json::json!({ "type": "text", "text": text })]
        }
        _ => Vec::new(),
    }
}

fn user_content_blocks(message: &serde_json::Value) -> Vec<serde_json::Value> {
    match message.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::Array(blocks)) => blocks.clone(),
        Some(serde_json::Value::String(text)) => {
            vec![serde_json::json!({ "type": "text", "text": text })]
        }
        _ => Vec::new(),
    }
}

fn with_single_content_block(
    message: &serde_json::Value,
    block: serde_json::Value,
    derived_index: Option<usize>,
    ensure_context_management: bool,
) -> serde_json::Value {
    let mut output = message.clone();
    if let Some(index) = derived_index
        && let Some(uuid) = output.get("uuid").and_then(|value| value.as_str())
    {
        let derived = crate::utils::messages::derive_uuid(uuid, index);
        if let Some(obj) = output.as_object_mut() {
            obj.insert("uuid".to_string(), serde_json::Value::String(derived));
        }
    }
    if let Some(message_obj) = output
        .get_mut("message")
        .and_then(|value| value.as_object_mut())
    {
        message_obj.insert("content".to_string(), serde_json::Value::Array(vec![block]));
        if ensure_context_management && !message_obj.contains_key("context_management") {
            message_obj.insert("context_management".to_string(), serde_json::Value::Null);
        }
    }
    output
}

/// Scans resumed transcript messages for skill state that official Claude Code
/// restores before deserializing. This does not mutate global state in the
/// Rust UI-only path; callers keep the payload on the resume target instead.
pub fn restore_skill_state_from_messages(messages: &[serde_json::Value]) -> SkillRestoreState {
    let mut state = SkillRestoreState::default();

    for message in messages {
        if message.get("type").and_then(|value| value.as_str()) != Some("attachment") {
            continue;
        }
        let Some(attachment) = message.get("attachment") else {
            continue;
        };
        match attachment.get("type").and_then(|value| value.as_str()) {
            Some("invoked_skills") => {
                let Some(skills) = attachment.get("skills").and_then(|value| value.as_array())
                else {
                    continue;
                };
                for skill in skills {
                    let name = skill.get("name").and_then(|value| value.as_str());
                    let path = skill.get("path").and_then(|value| value.as_str());
                    let content = skill.get("content").and_then(|value| value.as_str());
                    if let (Some(name), Some(path), Some(content)) = (name, path, content)
                        && !name.is_empty()
                        && !path.is_empty()
                        && !content.is_empty()
                    {
                        state.invoked_skills.push(InvokedSkill {
                            name: name.to_string(),
                            path: path.to_string(),
                            content: content.to_string(),
                        });
                    }
                }
            }
            Some("skill_listing") => {
                state.suppress_next_skill_listing = true;
            }
            _ => {}
        }
    }

    state
}

// ════════════════════════════════════════════════════════════
// Step 3: filter_invalid_messages (three sub-filters)
// ════════════════════════════════════════════════════════════

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum JavascriptSetValue<'a> {
    Undefined,
    Null,
    Bool(bool),
    /// JavaScript parses every JSON number as IEEE-754 and `Set` uses
    /// SameValueZero, so normalize signed zero and compare the resulting bits.
    Number(u64),
    String(&'a str),
    /// JS objects compare by identity. The source `Value`s stay borrowed and
    /// immobile for this scan; the pointer is only hashed, never dereferenced.
    Composite(*const serde_json::Value),
}

fn javascript_set_value(value: Option<&serde_json::Value>) -> JavascriptSetValue<'_> {
    match value {
        None => JavascriptSetValue::Undefined,
        Some(serde_json::Value::Null) => JavascriptSetValue::Null,
        Some(serde_json::Value::Bool(value)) => JavascriptSetValue::Bool(*value),
        Some(serde_json::Value::Number(value)) => {
            let value = value.as_f64().unwrap_or(f64::NAN);
            let bits = if value == 0.0 {
                0
            } else if value.is_nan() {
                f64::NAN.to_bits()
            } else {
                value.to_bits()
            };
            JavascriptSetValue::Number(bits)
        }
        Some(serde_json::Value::String(value)) => JavascriptSetValue::String(value),
        Some(value @ (serde_json::Value::Array(_) | serde_json::Value::Object(_))) => {
            JavascriptSetValue::Composite(value as *const serde_json::Value)
        }
    }
}

/// Returns assistant-message indexes removed by CC's unresolved-tool-use filter.
///
/// Maps to: CC `utils/messages.ts:2795-2840`
/// `filterUnresolvedToolUses(...)`. CC removes the whole assistant message only
/// when all of its `tool_use` blocks lack a `tool_result`; a mixed message is
/// retained intact. Keeping this borrowed helper shared by the typed and
/// renderable recovery projections avoids cloning a cold transcript twice.
/// The JS-shaped key also preserves the unvalidated-JSON behavior for missing,
/// primitive, and object IDs instead of silently treating malformed IDs as if
/// no `tool_use` block existed.
pub(crate) fn unresolved_tool_use_assistant_message_indexes(
    messages: &[serde_json::Value],
) -> HashSet<usize> {
    let mut tool_use_ids: HashSet<JavascriptSetValue<'_>> = HashSet::new();
    let mut tool_result_ids: HashSet<JavascriptSetValue<'_>> = HashSet::new();

    for msg in messages {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }
        let Some(content) = msg
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };

        for block in content {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("tool_use") => {
                    tool_use_ids.insert(javascript_set_value(block.get("id")));
                }
                Some("tool_result") => {
                    tool_result_ids.insert(javascript_set_value(block.get("tool_use_id")));
                }
                _ => {}
            }
        }
    }

    let unresolved: HashSet<JavascriptSetValue<'_>> =
        tool_use_ids.difference(&tool_result_ids).copied().collect();
    if unresolved.is_empty() {
        return HashSet::new();
    }

    messages
        .iter()
        .enumerate()
        .filter_map(|(index, msg)| {
            if msg.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                return None;
            }
            let content = msg
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())?;
            let tool_use_block_ids: Vec<JavascriptSetValue<'_>> = content
                .iter()
                .filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                .map(|block| javascript_set_value(block.get("id")))
                .collect();
            (!tool_use_block_ids.is_empty()
                && tool_use_block_ids.iter().all(|id| unresolved.contains(id)))
            .then_some(index)
        })
        .collect()
}

/// 2. unresolved = tool_use_id - tool_result_id
pub fn filter_unresolved_tool_uses(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let filtered_indexes = unresolved_tool_use_assistant_message_indexes(&messages);
    if filtered_indexes.is_empty() {
        return messages;
    }
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(index, message)| (!filtered_indexes.contains(&index)).then_some(message))
        .collect()
}

pub fn filter_orphaned_thinking_only(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut ids_with_non_thinking: HashSet<String> = HashSet::new();
    for msg in &messages {
        if msg.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let content = msg
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());
        let Some(content) = content else { continue };

        let has_non_thinking = content.iter().any(|b| {
            let t = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
            t != "thinking" && t != "redacted_thinking"
        });
        if has_non_thinking
            && let Some(id) = msg
                .get("message")
                .and_then(|m| m.get("id"))
                .and_then(|v| v.as_str())
        {
            ids_with_non_thinking.insert(id.to_string());
        }
    }

    messages
        .into_iter()
        .filter(|msg| {
            if msg.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                return true;
            }
            let content = msg
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());
            let Some(content) = content else { return true };
            if content.is_empty() {
                return true;
            }

            let all_thinking = content.iter().all(|b| {
                let t = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
                t == "thinking" || t == "redacted_thinking"
            });
            if !all_thinking {
                return true;
            }

            let msg_id = msg
                .get("message")
                .and_then(|m| m.get("id"))
                .and_then(|v| v.as_str());
            if let Some(id) = msg_id
                && ids_with_non_thinking.contains(id)
            {
                return true;
            }
            false
        })
        .collect()
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub fn detect_and_append_sentinel(
    mut messages: Vec<serde_json::Value>,
) -> (Vec<serde_json::Value>, TurnInterruptionState) {
    let interruption = detect_turn_interruption(&messages);

    // Convert interrupted_turn into a synthetic continuation user message.
    let final_interruption = match interruption {
        InternalInterruptionState::InterruptedTurn => {
            if let Some(continuation) =
                normalize_messages(vec![crate::utils::messages::create_user_message_value(
                    CONTINUATION_MESSAGE,
                    true,
                )])
                .into_iter()
                .next()
            {
                messages.push(continuation);
            }
            // Match official behavior: consumers only see interrupted_prompt.
            TurnInterruptionState::InterruptedPrompt
        }
        InternalInterruptionState::InterruptedPrompt => TurnInterruptionState::InterruptedPrompt,
        InternalInterruptionState::None => TurnInterruptionState::None,
    };

    let last_relevant_idx = messages.iter().rposition(|m| {
        let t = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
        t != "system" && t != "progress"
    });
    if let Some(idx) = last_relevant_idx {
        let is_user = messages[idx].get("type").and_then(|v| v.as_str()) == Some("user");
        if is_user {
            // Maps to: CC conversationRecovery.ts:240-244: the sentinel uses
            // messages.ts#createAssistantMessage, including its API envelope.
            let sentinel = crate::utils::messages::create_assistant_message_value(
                NO_RESPONSE_REQUESTED.to_string(),
            );
            messages.insert(idx + 1, sentinel);
        }
    }

    (messages, final_interruption)
}

fn detect_turn_interruption(messages: &[serde_json::Value]) -> InternalInterruptionState {
    if messages.is_empty() {
        return InternalInterruptionState::None;
    }

    let last_idx = messages.iter().rposition(|m| {
        let t = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "system" || t == "progress" {
            return false;
        }
        if t == "assistant" && m.get("isApiErrorMessage").and_then(|v| v.as_bool()) == Some(true) {
            return false;
        }
        true
    });

    let Some(idx) = last_idx else {
        return InternalInterruptionState::None;
    };
    let last = &messages[idx];
    let last_type = last.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match last_type {
        "assistant" => InternalInterruptionState::None,
        "user" => {
            if last.get("isMeta").and_then(|v| v.as_bool()) == Some(true)
                || last.get("isCompactSummary").and_then(|v| v.as_bool()) == Some(true)
            {
                return InternalInterruptionState::None;
            }
            if crate::utils::messages::is_tool_use_result_message(last) {
                if is_terminal_tool_result(last, messages, idx) {
                    return InternalInterruptionState::None;
                }
                return InternalInterruptionState::InterruptedTurn;
            }
            InternalInterruptionState::InterruptedPrompt
        }
        "attachment" => InternalInterruptionState::InterruptedTurn,
        _ => InternalInterruptionState::None,
    }
}

fn is_terminal_tool_result(
    result: &serde_json::Value,
    messages: &[serde_json::Value],
    result_idx: usize,
) -> bool {
    let content = result
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array());
    let Some(content) = content else { return false };

    let Some(first_block) = content.first() else {
        return false;
    };
    if first_block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
        return false;
    }

    let Some(tool_use_id) = first_block.get("tool_use_id").and_then(|v| v.as_str()) else {
        return false;
    };

    for i in (0..result_idx).rev() {
        let msg = &messages[i];
        if msg.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(asst_content) = msg
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };

        for block in asst_content {
            if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                continue;
            }
            if block.get("id").and_then(|v| v.as_str()) == Some(tool_use_id) {
                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                return TERMINAL_TOOL_NAMES.contains(&name);
            }
        }
    }
    false
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// - new_file → file
/// - new_directory → directory
fn migrate_legacy_attachment(mut msg: serde_json::Value) -> serde_json::Value {
    if msg.get("type").and_then(|v| v.as_str()) != Some("attachment") {
        return msg;
    }

    let Some(attachment) = msg.get_mut("attachment").and_then(|a| a.as_object_mut()) else {
        return msg;
    };

    let current_type = attachment
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // new_file → file
    if current_type == "new_file" {
        attachment.insert(
            "type".to_string(),
            serde_json::Value::String("file".to_string()),
        );
        let filename = attachment
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !filename.is_empty() && !attachment.contains_key("displayPath") {
            attachment.insert(
                "displayPath".to_string(),
                serde_json::Value::String(relative_to_cwd(&filename)),
            );
        }
        return msg;
    }

    // new_directory → directory
    if current_type == "new_directory" {
        attachment.insert(
            "type".to_string(),
            serde_json::Value::String("directory".to_string()),
        );
        let path = attachment
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !path.is_empty() && !attachment.contains_key("displayPath") {
            attachment.insert(
                "displayPath".to_string(),
                serde_json::Value::String(relative_to_cwd(&path)),
            );
        }
        return msg;
    }

    if !attachment.contains_key("displayPath") {
        let path = attachment
            .get("filename")
            .and_then(|v| v.as_str())
            .or_else(|| attachment.get("path").and_then(|v| v.as_str()))
            .or_else(|| attachment.get("skillDir").and_then(|v| v.as_str()))
            .map(String::from);
        if let Some(p) = path {
            attachment.insert(
                "displayPath".to_string(),
                serde_json::Value::String(relative_to_cwd(&p)),
            );
        }
    }

    msg
}

fn relative_to_cwd(path: &str) -> String {
    let Ok(cwd) = std::env::current_dir() else {
        return path.to_string();
    };
    let p = std::path::Path::new(path);
    p.strip_prefix(&cwd)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn validate_permission_modes(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let valid_modes = crate::utils::permissions::permission_mode::PERMISSION_MODES;

    messages
        .into_iter()
        .map(|mut msg| {
            let msg_type = msg
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if msg_type != "user" {
                return msg;
            }
            if let Some(obj) = msg.as_object_mut()
                && let Some(mode) = obj.get("permissionMode").and_then(|v| v.as_str())
                && !valid_modes.contains(&mode)
            {
                obj.remove("permissionMode");
            }
            msg
        })
        .collect()
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub fn into_typed_messages(messages: Vec<serde_json::Value>) -> Vec<Message> {
    messages.into_iter().filter_map(value_to_message).collect()
}

fn value_to_message(value: serde_json::Value) -> Option<Message> {
    let msg_type = value.get("type")?.as_str()?;
    let timestamp = parse_timestamp(value.get("timestamp"));
    // Real identity from the cold session entry; mint only when absent (C3a).
    let entry_uuid = value
        .get("uuid")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match msg_type {
        "user" => {
            let content = parse_user_content(&value);
            let is_compact_summary = value
                .get("isCompactSummary")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(Message::User(UserMessage {
                uuid: entry_uuid,
                timestamp,
                content,
                is_compact_summary,
                plan_content: None,
                image_paste_ids: None,
                // CC persists the whole envelope (`types/message.ts:50-62`);
                // the typed owner reads the real values back.
                is_visible_in_transcript_only: value
                    .get("isVisibleInTranscriptOnly")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                mcp_meta: value.get("mcpMeta").cloned(),
                source_tool_assistant_uuid: value
                    .get("sourceToolAssistantUUID")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                permission_mode: value
                    .get("permissionMode")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                origin: value.get("origin").cloned(),
                summarize_metadata: value
                    .get("summarizeMetadata")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok()),
            }))
        }
        "assistant" => {
            let mut content = parse_assistant_content(&value);
            content.push(AssistantContent::MessageIdentity(
                crate::types::message::AssistantMessageIdentity {
                    // No uuid here: the envelope carries it. See the type doc —
                    // CC has one uuid per message.
                    request_id: value
                        .get("requestId")
                        .or_else(|| value.get("request_id"))
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    api_message_id: value
                        .pointer("/message/id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    // CC persists these on the envelope (`types/message.ts:41-44`).
                    is_api_error_message: value
                        .get("isApiErrorMessage")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    api_error: value.get("apiError").cloned(),
                    error_details: value
                        .get("errorDetails")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                },
            ));
            let model = value
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let stop_reason = parse_assistant_stop_reason(&value);
            let usage = parse_assistant_usage(&value);
            Some(Message::Assistant(AssistantMessage {
                uuid: entry_uuid,
                timestamp,
                content,
                model,
                stop_reason,
                usage,
            }))
        }
        "system" => Some(Message::System(
            // Shared wire→union adapter (batch D1); subtype/level fallbacks
            // live there.
            crate::utils::conversation_recovery::system_message_from_entry(
                &value, entry_uuid, timestamp,
            ),
        )),
        "hook_result" => Some(Message::HookResult(HookResultMessage {
            message_type: "hook_result".to_string(),
            uuid: value
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            timestamp,
            attachment: value.get("attachment")?.clone(),
        })),
        "attachment" => {
            let attachment = value.get("attachment")?;
            let attachment_type = attachment
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            if attachment_type.starts_with("hook_") {
                return Some(Message::HookResult(HookResultMessage {
                    message_type: "attachment".to_string(),
                    uuid: value
                        .get("uuid")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    timestamp,
                    attachment: attachment.clone(),
                }));
            }
            // Typed at the seam (batch D2); unrecognized payloads ride
            // `Attachment::Unknown`, and the verbatim wire form is kept so
            // fields the typed enum does not declare survive a round-trip.
            Some(Message::Attachment(AttachmentMessage::from_wire_payload(
                entry_uuid,
                timestamp,
                attachment.clone(),
            )))
        }
        _ => None,
    }
}

fn parse_timestamp(value: Option<&serde_json::Value>) -> DateTime<Utc> {
    value
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

fn parse_user_content(value: &serde_json::Value) -> Vec<UserContent> {
    let content = value.get("message").and_then(|m| m.get("content"));
    let is_meta = value.get("isMeta").and_then(|v| v.as_bool()) == Some(true);

    match content {
        Some(serde_json::Value::String(s)) => {
            vec![if is_meta {
                UserContent::MetaText(s.clone())
            } else {
                UserContent::Text(s.clone())
            }]
        }
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|block| parse_user_content_block(block, value))
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_user_content_block(
    block: &serde_json::Value,
    enclosing_message: &serde_json::Value,
) -> Option<UserContent> {
    let block_type = block.get("type")?.as_str()?;
    match block_type {
        "text" => {
            let text = block.get("text")?.as_str()?.to_string();
            if enclosing_message
                .get("isMeta")
                .and_then(|value| value.as_bool())
                == Some(true)
            {
                Some(UserContent::MetaText(text))
            } else {
                Some(UserContent::Text(text))
            }
        }
        "image" => Some(UserContent::from_image_block(
            block.clone(),
            enclosing_message
                .get("isMeta")
                .and_then(serde_json::Value::as_bool)
                == Some(true),
        )),
        "document" => {
            let source = block.get("source")?;
            let media_type = source.get("media_type")?.as_str()?.to_string();
            let data = source.get("data")?.as_str()?.to_string();
            if enclosing_message
                .get("isMeta")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                Some(UserContent::MetaDocument { media_type, data })
            } else {
                Some(UserContent::Document { media_type, data })
            }
        }
        "tool_result" => {
            let tool_use_id = block.get("tool_use_id")?.as_str()?.to_string();
            let is_error = block
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content_blocks = match block.get("content") {
                Some(serde_json::Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|content_block| {
                        use crate::types::message::{
                            ToolResultContentBlock, ToolResultMediaSource,
                        };
                        match content_block
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                        {
                            Some("tool_reference") => content_block
                                .get("tool_name")
                                .and_then(serde_json::Value::as_str)
                                .map(|tool_name| ToolResultContentBlock::ToolReference {
                                    tool_name: tool_name.to_string(),
                                }),
                            Some("text") => content_block
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .map(|text| ToolResultContentBlock::Text {
                                    text: text.to_string(),
                                }),
                            Some("image") => serde_json::from_value::<ToolResultContentBlock>(
                                content_block.clone(),
                            )
                            .ok(),
                            Some("document") => {
                                let source = content_block.get("source")?;
                                let media_source = ToolResultMediaSource {
                                    kind: source
                                        .get("type")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("base64")
                                        .to_string(),
                                    media_type: source
                                        .get("media_type")
                                        .and_then(serde_json::Value::as_str)?
                                        .to_string(),
                                    data: source
                                        .get("data")
                                        .and_then(serde_json::Value::as_str)?
                                        .to_string(),
                                };
                                Some(ToolResultContentBlock::Document {
                                    source: media_source,
                                })
                            }
                            _ => None,
                        }
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            let content = match block.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|v| v.as_str()).map(String::from))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            Some(UserContent::ToolResult(ToolResult {
                tool_use_id: ToolUseId(tool_use_id),
                content,
                is_error,
                content_blocks,
                tool_use_result: enclosing_message.get("toolUseResult").cloned(),
            }))
        }
        _ => None,
    }
}

fn parse_assistant_stop_reason(value: &serde_json::Value) -> Option<StopReason> {
    let raw = value
        .get("message")
        .and_then(|message| {
            message
                .get("stop_reason")
                .or_else(|| message.get("stopReason"))
        })
        .and_then(|value| value.as_str())?;
    match raw {
        "end_turn" | "EndTurn" => Some(StopReason::EndTurn),
        "tool_use" | "ToolUse" => Some(StopReason::ToolUse),
        "max_tokens" | "MaxTokens" => Some(StopReason::MaxTokens),
        "stop_sequence" | "StopSequence" => Some(StopReason::StopSequence),
        "pause_turn" | "PauseTurn" => Some(StopReason::PauseTurn),
        "refusal" | "Refusal" => Some(StopReason::Refusal),
        _ => None,
    }
}

fn parse_assistant_usage(value: &serde_json::Value) -> Option<TokenUsage> {
    let usage = value
        .get("message")
        .and_then(|message| message.get("usage"))?;
    Some(TokenUsage {
        input_tokens: usage_u64(usage, "input_tokens"),
        output_tokens: usage_u64(usage, "output_tokens"),
        cache_creation_input_tokens: usage_u64(usage, "cache_creation_input_tokens"),
        cache_read_input_tokens: usage_u64(usage, "cache_read_input_tokens"),
        cache_deleted_input_tokens: usage_u64(usage, "cache_deleted_input_tokens"),
    })
}

fn usage_u64(usage: &serde_json::Value, key: &str) -> u64 {
    usage
        .get(key)
        .or_else(|| match key {
            "input_tokens" => usage.get("inputTokens"),
            "output_tokens" => usage.get("outputTokens"),
            "cache_creation_input_tokens" => usage.get("cacheCreationInputTokens"),
            "cache_read_input_tokens" => usage.get("cacheReadInputTokens"),
            "cache_deleted_input_tokens" => usage.get("cacheDeletedInputTokens"),
            _ => None,
        })
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
}

fn parse_assistant_content(value: &serde_json::Value) -> Vec<AssistantContent> {
    let content = value.get("message").and_then(|m| m.get("content"));

    match content {
        Some(serde_json::Value::String(s)) => {
            vec![AssistantContent::Text(s.clone())]
        }
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(parse_assistant_content_block)
            .collect(),
        _ => Vec::new(),
    }
}

/// Parses one assistant content block.
fn parse_assistant_content_block(block: &serde_json::Value) -> Option<AssistantContent> {
    let block_type = block.get("type")?.as_str()?;
    match block_type {
        "text" => {
            let text = block.get("text")?.as_str()?.to_string();
            Some(AssistantContent::Text(text))
        }
        "thinking" => {
            let text = block
                .get("thinking")
                .or_else(|| block.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let signature = block
                .get("signature")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AssistantContent::Thinking { text, signature })
        }
        "redacted_thinking" => {
            let data = block
                .get("data")
                .or_else(|| block.get("redacted_thinking"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AssistantContent::RedactedThinking { data })
        }
        "tool_use" | "server_tool_use" => {
            let id = block.get("id")?.as_str()?.to_string();
            let name = block.get("name")?.as_str()?.to_string();
            let input = block
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let tool_use = ToolUseBlock {
                id: ToolUseId(id),
                name,
                input,
            };
            if block_type == "server_tool_use" {
                Some(AssistantContent::ServerToolUse(tool_use))
            } else {
                Some(AssistantContent::ToolUse(tool_use))
            }
        }
        "web_search_tool_result" => Some(AssistantContent::WebSearchToolResult {
            tool_use_id: ToolUseId(block.get("tool_use_id")?.as_str()?.to_string()),
            content: block
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        }),
        // CC `AdvisorToolResultBlock` (utils/advisor.ts:16-32): `content` is a
        // union of `advisor_result{text}` / `advisor_redacted_result` /
        // `advisor_tool_result_error`; render dispatch at Message.tsx:467.
        // The text extraction mirrors the retired recovery parser's
        // `block_content_text` (redacted/error payloads yield an empty string).
        "advisor_tool_result" => Some(AssistantContent::Advisor {
            tool_use_id: crate::types::ids::ToolUseId(
                block
                    .get("tool_use_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
            content: serde_json::from_value(block.get("content")?.clone()).ok()?,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_text(id: &str, text: &str) -> serde_json::Value {
        json!({
            "type": "user",
            "uuid": id,
            "message": {"role": "user", "content": text},
            "timestamp": "2026-06-13T13:00:00.000Z"
        })
    }

    #[test]
    fn typed_messages_preserve_hook_result_attachment_payloads() {
        let messages = into_typed_messages(vec![json!({
            "type": "hook_result",
            "uuid": "hook-1",
            "timestamp": "2026-07-12T00:00:00.000Z",
            "attachment": {
                "type": "hook_additional_context",
                "content": ["resume context"],
                "hookName": "SessionStart"
            }
        })]);

        assert!(matches!(
            messages.first(),
            Some(Message::HookResult(message))
                if message.uuid == "hook-1"
                    && message.attachment.pointer("/content/0")
                        .and_then(serde_json::Value::as_str) == Some("resume context")
        ));
    }

    fn assistant_tool(
        id: &str,
        message_id: &str,
        tool_use_id: &str,
        name: &str,
    ) -> serde_json::Value {
        json!({
            "type": "assistant",
            "uuid": id,
            "message": {
                "id": message_id,
                "role": "assistant",
                "content": [{"type": "tool_use", "id": tool_use_id, "name": name, "input": {}}]
            },
            "timestamp": "2026-06-13T13:00:01.000Z"
        })
    }

    fn user_tool_result(id: &str, tool_use_id: &str) -> serde_json::Value {
        json!({
            "type": "user",
            "uuid": id,
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": tool_use_id, "content": "ok"}]
            },
            "timestamp": "2026-06-13T13:00:02.000Z"
        })
    }

    fn assistant_text_value(message: &serde_json::Value) -> Option<&str> {
        message
            .get("message")?
            .get("content")?
            .as_array()?
            .first()?
            .get("text")?
            .as_str()
    }

    fn user_text_value(message: &serde_json::Value) -> Option<&str> {
        let content = message.get("message")?.get("content")?;
        if let Some(text) = content.as_str() {
            return Some(text);
        }
        content.as_array()?.first()?.get("text")?.as_str()
    }

    #[test]
    fn normalize_messages_splits_multiblock_and_derives_stable_uuids() {
        let normalized = normalize_messages(vec![
            json!({
                "type": "assistant",
                "uuid": "12345678-1234-1234-1234-123456789abc",
                "message": {
                    "id": "msg-1",
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "hmm"},
                        {"type": "text", "text": "answer"}
                    ]
                }
            }),
            user_text("user-after", "next"),
        ]);

        assert_eq!(normalized.len(), 3);
        assert_eq!(
            normalized[0].get("uuid").and_then(|value| value.as_str()),
            Some("12345678-1234-1234-1234-000000000000")
        );
        assert_eq!(
            normalized[1].get("uuid").and_then(|value| value.as_str()),
            Some("12345678-1234-1234-1234-000000000001")
        );
        assert_eq!(
            normalized[2].get("uuid").and_then(|value| value.as_str()),
            Some("user-after000000000000")
        );
        assert_eq!(assistant_text_value(&normalized[1]), Some("answer"));
        assert_eq!(user_text_value(&normalized[2]), Some("next"));
        assert_eq!(
            normalized[0]
                .get("message")
                .and_then(|message| message.get("context_management")),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn restore_skill_state_collects_invoked_skills_and_suppresses_listing() {
        let state = restore_skill_state_from_messages(&[
            json!({
                "type": "attachment",
                "uuid": "skills-1",
                "attachment": {
                    "type": "invoked_skills",
                    "skills": [
                        {"name": "review", "path": "/skills/review", "content": "Use review rules."},
                        {"name": "missing-content", "path": "/skills/bad", "content": ""}
                    ]
                }
            }),
            json!({
                "type": "attachment",
                "uuid": "listing-1",
                "attachment": {"type": "skill_listing", "content": "review"}
            }),
        ]);

        assert!(state.suppress_next_skill_listing);
        assert_eq!(state.invoked_skills.len(), 1);
        assert_eq!(state.invoked_skills[0].name, "review");
        assert_eq!(state.invoked_skills[0].path, "/skills/review");
        assert_eq!(state.invoked_skills[0].content, "Use review rules.");
    }

    #[test]
    fn into_typed_messages_preserves_assistant_stop_reason_and_usage() {
        let typed = into_typed_messages(vec![json!({
            "type": "assistant",
            "uuid": "assistant-usage",
            "message": {
                "id": "msg-usage",
                "role": "assistant",
                "model": "claude-sonnet-4-20250514",
                "stop_reason": "pause_turn",
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 34,
                    "cache_creation_input_tokens": 5,
                    "cache_read_input_tokens": 6
                },
                "content": [{"type": "text", "text": "paused"}]
            },
            "timestamp": "2026-06-13T13:00:01.000Z"
        })]);

        assert!(matches!(
            typed.as_slice(),
            [Message::Assistant(assistant)]
                if assistant.model.as_deref() == Some("claude-sonnet-4-20250514")
                    && assistant.stop_reason == Some(StopReason::PauseTurn)
                    && assistant.usage.as_ref().is_some_and(|usage|
                        usage.input_tokens == 12
                            && usage.output_tokens == 34
                            && usage.cache_creation_input_tokens == 5
                            && usage.cache_read_input_tokens == 6
                    )
        ));
    }

    #[test]
    fn parse_assistant_content_preserves_redacted_thinking_data() {
        let value = json!({
            "message": {
                "content": [
                    {"type": "redacted_thinking", "data": "encrypted-payload"}
                ]
            }
        });

        let content = parse_assistant_content(&value);

        assert!(matches!(
            content.as_slice(),
            [AssistantContent::RedactedThinking { data }] if data == "encrypted-payload"
        ));
    }

    #[test]
    fn parse_assistant_content_preserves_server_tool_use_without_local_tool_use() {
        let value = json!({
            "message": {
                "content": [
                    {
                        "type": "server_tool_use",
                        "id": "srvu_1",
                        "name": "web_search",
                        "input": {"query": "nyan"}
                    },
                    {
                        "type": "web_search_tool_result",
                        "tool_use_id": "srvu_1",
                        "content": [{
                            "type": "web_search_result",
                            "encrypted_content": "encrypted",
                            "title": "Nyan",
                            "url": "https://example.com"
                        }]
                    }
                ]
            }
        });

        let content = parse_assistant_content(&value);

        assert!(matches!(
            content.as_slice(),
            [
                AssistantContent::ServerToolUse(block),
                AssistantContent::WebSearchToolResult {
                    tool_use_id,
                    content,
                }
            ] if block.id.0 == "srvu_1"
                    && block.name == "web_search"
                    && block.input.get("query").and_then(|value| value.as_str())
                        == Some("nyan")
                    && tool_use_id.0 == "srvu_1"
                    && content.as_array().is_some_and(|items| items.len() == 1)
        ));
    }

    #[test]
    fn unresolved_tool_use_filter_matches_javascript_set_semantics_for_unvalidated_ids() {
        let matching_primitive_ids = vec![
            json!({
                "type": "assistant",
                "uuid": "number-use",
                "message": {"content": [{"type":"tool_use","id":1.0,"name":"Glob","input":{}}]}
            }),
            json!({
                "type": "assistant",
                "uuid": "null-use",
                "message": {"content": [{"type":"tool_use","id":null,"name":"Glob","input":{}}]}
            }),
            json!({
                "type": "assistant",
                "uuid": "undefined-use",
                "message": {"content": [{"type":"tool_use","name":"Glob","input":{}}]}
            }),
            json!({
                "type": "user",
                "uuid": "primitive-results",
                "message": {"content": [
                    {"type":"tool_result","tool_use_id":1,"content":"ok"},
                    {"type":"tool_result","tool_use_id":null,"content":"ok"},
                    {"type":"tool_result","content":"ok"}
                ]}
            }),
        ];
        assert_eq!(
            filter_unresolved_tool_uses(matching_primitive_ids)
                .iter()
                .filter(
                    |message| message.get("type").and_then(|value| value.as_str())
                        == Some("assistant")
                )
                .count(),
            3
        );

        let unresolved_malformed_ids = vec![
            json!({
                "type": "assistant",
                "uuid": "number-use",
                "message": {"content": [{"type":"tool_use","id":2,"name":"Glob","input":{}}]}
            }),
            json!({
                "type": "assistant",
                "uuid": "null-use",
                "message": {"content": [{"type":"tool_use","id":null,"name":"Glob","input":{}}]}
            }),
            json!({
                "type": "assistant",
                "uuid": "undefined-use",
                "message": {"content": [{"type":"tool_use","name":"Glob","input":{}}]}
            }),
            json!({
                "type": "assistant",
                "uuid": "object-use",
                "message": {"content": [{"type":"tool_use","id":{"legacy":true},"name":"Glob","input":{}}]}
            }),
            // Structurally equal JSON objects are distinct JS object identities,
            // so this malformed result must not resolve the object-valued use.
            json!({
                "type": "user",
                "uuid": "object-result",
                "message": {"content": [{
                    "type":"tool_result","tool_use_id":{"legacy":true},"content":"ok"
                }]}
            }),
        ];
        let filtered = filter_unresolved_tool_uses(unresolved_malformed_ids);
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].get("uuid").and_then(|value| value.as_str()),
            Some("object-result")
        );
    }

    #[test]
    fn deserialize_filters_unresolved_tool_uses_and_inserts_sentinel_before_trailing_system() {
        let result = deserialize_messages_with_interrupt_detection(vec![
            user_text("user-1", "hello"),
            assistant_tool("assistant-1", "msg-1", "toolu_missing", "Read"),
            json!({
                "type": "system",
                "uuid": "system-1",
                "message": {"content": "bookkeeping"},
                "timestamp": "2026-06-13T13:00:03.000Z"
            }),
        ]);

        assert_eq!(result.messages.len(), 3);
        assert_eq!(
            result.messages[0].get("uuid").and_then(|v| v.as_str()),
            Some("user-1")
        );
        assert_eq!(
            assistant_text_value(&result.messages[1]),
            Some(NO_RESPONSE_REQUESTED)
        );
        assert_eq!(
            result.messages[2].get("uuid").and_then(|v| v.as_str()),
            Some("system-1")
        );
    }

    #[test]
    fn deserialize_interrupted_tool_result_adds_continuation_then_sentinel() {
        let result = deserialize_messages_with_interrupt_detection(vec![
            assistant_tool("assistant-1", "msg-1", "toolu_1", "Read"),
            user_tool_result("result-1", "toolu_1"),
        ]);

        assert!(matches!(
            result.turn_interruption_state,
            TurnInterruptionState::InterruptedPrompt
        ));
        assert_eq!(result.messages.len(), 4);
        assert_eq!(
            user_text_value(&result.messages[2]),
            Some(CONTINUATION_MESSAGE)
        );
        assert_eq!(
            assistant_text_value(&result.messages[3]),
            Some(NO_RESPONSE_REQUESTED)
        );
    }

    #[test]
    fn deserialize_terminal_tool_result_does_not_add_continuation() {
        let result = deserialize_messages_with_interrupt_detection(vec![
            assistant_tool("assistant-1", "msg-1", "toolu_1", "SendUserMessage"),
            user_tool_result("result-1", "toolu_1"),
        ]);

        assert!(matches!(
            result.turn_interruption_state,
            TurnInterruptionState::None
        ));
        assert_eq!(result.messages.len(), 3);
        assert!(
            result
                .messages
                .iter()
                .all(|message| user_text_value(message) != Some(CONTINUATION_MESSAGE))
        );
        assert_eq!(
            assistant_text_value(&result.messages[2]),
            Some(NO_RESPONSE_REQUESTED)
        );
    }

    #[test]
    fn deserialize_strips_invalid_permission_mode() {
        let result = deserialize_messages_with_interrupt_detection(vec![json!({
            "type": "user",
            "uuid": "user-1",
            "permissionMode": "future-mode",
            "message": {"role": "user", "content": "hello"},
            "timestamp": "2026-06-13T13:00:00.000Z"
        })]);

        assert!(result.messages[0].get("permissionMode").is_none());
    }

    #[test]
    fn typed_resume_messages_preserve_official_raw_tool_use_result() {
        let typed = into_typed_messages(vec![json!({
            "type": "user",
            "timestamp": "2026-07-12T00:00:00.000Z",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": "done",
                "is_error": false
            }]},
            "toolUseResult": {"stdout": "done", "interrupted": false}
        })]);

        assert!(matches!(
            typed.as_slice(),
            [Message::User(user)]
                if matches!(
                    user.content.as_slice(),
                    [UserContent::ToolResult(result)]
                        if result.tool_use_result.as_ref()
                            .and_then(|value| value.get("stdout"))
                            .and_then(serde_json::Value::as_str) == Some("done")
                )
        ));
    }

    #[test]
    fn typed_resume_preserves_meta_read_media_and_tool_result_block_order() {
        let typed = into_typed_messages(vec![
            json!({
                "type": "user",
                "timestamp": "2026-07-12T00:00:00.000Z",
                "isMeta": true,
                "message": {"role": "user", "content": [
                    {"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"img"}},
                    {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"pdf"}}
                ]}
            }),
            json!({
                "type": "user",
                "timestamp": "2026-07-12T00:00:01.000Z",
                "message": {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_media",
                    "is_error": false,
                    "content": [
                        {"type":"text","text":"metadata"},
                        {"type":"image","source":{"type":"base64","media_type":"image/png","data":"page"}},
                        {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"doc"}},
                        {"type":"tool_reference","tool_name":"Read"}
                    ]
                }]}
            }),
        ]);

        assert!(matches!(
            &typed[0],
            Message::User(user) if matches!(
                user.content.as_slice(),
                [UserContent::MetaImage { .. }, UserContent::MetaDocument { .. }]
            )
        ));
        let Message::User(user) = &typed[1] else {
            panic!("expected user result")
        };
        let UserContent::ToolResult(result) = &user.content[0] else {
            panic!("expected tool result")
        };
        assert!(matches!(
            result.content_blocks.as_slice(),
            [
                crate::types::message::ToolResultContentBlock::Text { .. },
                crate::types::message::ToolResultContentBlock::Image { .. },
                crate::types::message::ToolResultContentBlock::Document { .. },
                crate::types::message::ToolResultContentBlock::ToolReference { .. }
            ]
        ));
    }

    /// Maps to CC conversationRecovery.ts:205-246 and messages.ts:355-433.
    /// Bun factory oracle: .test/cold-sentinel-followup/bun-factory.json.
    #[test]
    fn cold_sentinel_matches_official_shared_factory_envelope() {
        for interrupted_tool in [false, true] {
            let mut input = if interrupted_tool {
                vec![
                    assistant_tool("assistant-1", "msg-1", "toolu_1", "Read"),
                    user_tool_result("result-1", "toolu_1"),
                ]
            } else {
                vec![user_text("user-1", "hello")]
            };
            input.push(json!({"type":"system","uuid":"trailing-system"}));
            input.push(json!({"type":"progress","uuid":"trailing-progress"}));
            let result = deserialize_messages_with_interrupt_detection(input);
            assert_eq!(
                result.turn_interruption_state,
                TurnInterruptionState::InterruptedPrompt
            );
            let sentinel_index = if interrupted_tool { 5 } else { 1 };
            if interrupted_tool {
                assert_eq!(result.messages.len(), 6);
                assert_eq!(result.messages[2]["uuid"], "trailing-system");
                assert_eq!(result.messages[3]["uuid"], "trailing-progress");
                assert_eq!(
                    user_text_value(&result.messages[4]),
                    Some(CONTINUATION_MESSAGE)
                );
                assert_eq!(result.messages[4]["isMeta"], true);
                // CC conversationRecovery.ts:209-214 delegates to
                // messages.ts:460-524, then normalizeMessages :783-797.
                let mut continuation = result.messages[4].clone();
                let id = continuation["uuid"].as_str().unwrap();
                assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                assert_ne!(id, result.messages[5]["uuid"].as_str().unwrap());
                let timestamp = continuation["timestamp"].as_str().unwrap();
                assert_eq!(timestamp.len(), 24);
                assert!(timestamp.ends_with('Z'));
                continuation.as_object_mut().unwrap().remove("uuid");
                continuation.as_object_mut().unwrap().remove("timestamp");
                assert_eq!(
                    continuation,
                    json!({
                        "type":"user", "isMeta":true,
                        "message":{"role":"user","content":[{
                            "type":"text","text":"Continue from where you left off."
                        }]}
                    })
                );
                let repeated =
                    deserialize_messages_with_interrupt_detection(result.messages.clone());
                assert_eq!(repeated.messages, result.messages);
            } else {
                assert_eq!(result.messages[2]["uuid"], "trailing-system");
                assert_eq!(result.messages[3]["uuid"], "trailing-progress");
            }
            let mut sentinel = result.messages[sentinel_index].clone();
            let uuid = sentinel["uuid"].as_str().unwrap().to_owned();
            let id = sentinel["message"]["id"]
                .as_str()
                .expect("source factory API id")
                .to_owned();
            assert!(uuid::Uuid::parse_str(&uuid).is_ok());
            assert!(uuid::Uuid::parse_str(&id).is_ok());
            assert_ne!(uuid, id);
            let timestamp = sentinel["timestamp"].as_str().unwrap();
            assert!(chrono::DateTime::parse_from_rfc3339(timestamp).is_ok());
            assert_eq!(timestamp.len(), 24);
            assert!(timestamp.ends_with('Z'));
            sentinel.as_object_mut().unwrap().remove("uuid");
            sentinel.as_object_mut().unwrap().remove("timestamp");
            sentinel["message"].as_object_mut().unwrap().remove("id");
            assert_eq!(
                sentinel,
                json!({
                    "type":"assistant", "isApiErrorMessage":false,
                    "message": {
                        "container":null, "model":"<synthetic>", "role":"assistant",
                        "stop_reason":"stop_sequence", "stop_sequence":"", "type":"message",
                        "content":[{"type":"text","text":"No response requested."}],
                        "context_management":null,
                        "usage": {
                            "input_tokens":0, "output_tokens":0,
                            "cache_creation_input_tokens":0, "cache_read_input_tokens":0,
                            "server_tool_use":{"web_search_requests":0,"web_fetch_requests":0},
                            "service_tier":null,
                            "cache_creation":{"ephemeral_1h_input_tokens":0,"ephemeral_5m_input_tokens":0},
                            "inference_geo":null,"iterations":null,"speed":null
                        }
                    }
                })
            );
        }
    }

    /// Maps to CC conversationRecovery.ts:164-246 / 272-333 and
    /// messages.ts:843-852. Bun executes the actual predicate and detector;
    /// this test drives the full Rust deserialize/continuation/sentinel chain.
    #[test]
    fn tool_result_recovery_matches_official_interruption_and_continuation() {
        // The predicate accepts a null first block, but the source's earlier
        // filterUnresolvedToolUses (:2808-2812) throws for that malformed log.
        // Its case stays in the predicate test, not this recovery-success set.
        let cases: serde_json::Value = serde_json::from_str(r##"[
{"message":{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_read","content":"ok"},{"type":"text","text":"hello"}]}},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":[{"type":"text","text":"hello"},{"type":"tool_result","tool_use_id":"toolu_read","content":"ok"}]}},"interruption":"interrupted_prompt"},
{"message":{"type":"user","message":{"content":"hello"}},"interruption":"interrupted_prompt"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":null},"interruption":"interrupted_prompt"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":false},"interruption":"interrupted_prompt"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":true},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":0},"interruption":"interrupted_prompt"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":0},"interruption":"interrupted_prompt"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":1},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":-1},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":""},"interruption":"interrupted_prompt"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":"0"},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":" "},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":[]},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":"hello"},"toolUseResult":{}},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":[]},"toolUseResult":{}},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":[{"type":"text","text":"hello"},{"type":"tool_result","tool_use_id":"toolu_read","content":"ok"}]},"toolUseResult":{}},"interruption":"interrupted_turn"},
{"message":{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_read","content":"ok"}]},"isMeta":true},"interruption":"none"},
{"message":{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_read","content":"ok"}]},"isCompactSummary":true},"interruption":"none"}
]"##).unwrap();
        for case in cases.as_array().unwrap() {
            let mut original = case["message"].clone();
            original["uuid"] = json!("original-user");
            original["timestamp"] = json!("2026-09-13T00:00:00.000Z");
            original["message"]["role"] = json!("user");
            let result = deserialize_messages_with_interrupt_detection(vec![original.clone()]);
            let source_state = case["interruption"].as_str().unwrap();
            let continued = source_state == "interrupted_turn";
            assert_eq!(
                result.turn_interruption_state,
                if source_state == "none" {
                    TurnInterruptionState::None
                } else {
                    TurnInterruptionState::InterruptedPrompt
                },
                "input: {original}"
            );
            assert_eq!(result.messages[0], original);
            assert_eq!(
                result.messages.len(),
                if continued { 3 } else { 2 },
                "input: {original}"
            );
            if continued {
                assert_eq!(result.messages[1]["isMeta"], true);
                assert_eq!(
                    user_text_value(&result.messages[1]),
                    Some(CONTINUATION_MESSAGE)
                );
            }
            assert_eq!(
                assistant_text_value(result.messages.last().unwrap()),
                Some(NO_RESPONSE_REQUESTED)
            );
            // A second load of the recovered transcript must not duplicate the pair.
            assert_eq!(
                deserialize_messages(result.messages.clone()),
                result.messages
            );
        }
    }

    /// Bun source whitespace filter + interruption detector; Rust actual
    /// deserialize pipeline must insert a sentinel only after the remaining user.
    #[test]
    fn whitespace_cold_recovery_matches_official_bun_interruption() {
        let cases: serde_json::Value = serde_json::from_str(r#"[{"input":[{"type":"user","uuid":"recovery-user-0","timestamp":"2026-09-13T00:00:00.000Z","message":{"role":"user","content":"continue"}},{"type":"assistant","uuid":"recovery-assistant-0","timestamp":"2026-09-13T00:00:00.000Z","message":{"id":"msg_recovery-assistant-0","role":"assistant","content":[{"type":"text","text":"\ufeff"}]}}],"filtered":[{"type":"user","uuid":"recovery-user-0","timestamp":"2026-09-13T00:00:00.000Z","message":{"role":"user","content":"continue"}}],"interruption":"interrupted_prompt"},{"input":[{"type":"user","uuid":"recovery-user-1","timestamp":"2026-09-13T00:00:00.000Z","message":{"role":"user","content":"continue"}},{"type":"assistant","uuid":"recovery-assistant-1","timestamp":"2026-09-13T00:00:00.000Z","message":{"id":"msg_recovery-assistant-1","role":"assistant","content":[{"type":"text","text":"\u0085"}]}}],"filtered":[{"type":"user","uuid":"recovery-user-1","timestamp":"2026-09-13T00:00:00.000Z","message":{"role":"user","content":"continue"}},{"type":"assistant","uuid":"recovery-assistant-1","timestamp":"2026-09-13T00:00:00.000Z","message":{"id":"msg_recovery-assistant-1","role":"assistant","content":[{"type":"text","text":"\u0085"}]}}],"interruption":"none"}]"#).unwrap();
        for case in cases.as_array().unwrap() {
            let input = case["input"].as_array().unwrap().clone();
            let expected = case["filtered"].as_array().unwrap();
            let recovered = deserialize_messages_with_interrupt_detection(input);
            let interrupted = case["interruption"] == "interrupted_prompt";
            assert_eq!(
                recovered.turn_interruption_state == TurnInterruptionState::InterruptedPrompt,
                interrupted,
                "{case}"
            );
            assert_eq!(
                recovered.messages.len(),
                expected.len() + usize::from(interrupted)
            );
            assert_eq!(&recovered.messages[..expected.len()], expected);
            if interrupted {
                assert_eq!(
                    assistant_text_value(recovered.messages.last().unwrap()),
                    Some(NO_RESPONSE_REQUESTED)
                );
            }
        }
    }
}
