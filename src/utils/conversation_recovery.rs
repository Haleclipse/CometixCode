//! Resume-load orchestration for the cold session transcript.
//! Maps to: CC `utils/conversationRecovery.ts` — `loadConversationForResume`
//! (:456), `restoreSkillStateFromMessages` (:376), and the render-facing
//! composition over `deserializeMessages` (:154).
//!
//! Batch D3 (item 6): the dual-parser split died. There is ONE cold parser —
//! `conversation.rs::into_typed_messages` (this module's
//! [`messages_from_entries`] is a thin wrapper) — and every visibility
//! decision happens at render, exactly like CC: `should_show_user_message` +
//! `filter_non_rendering_messages` (Messages.tsx:588-597), component-level
//! null renders (AssistantTextMessage.tsx:73-92, AttachmentMessage.tsx:162),
//! and the nonvisual tool_use / suppressed tool_result gates in
//! `components/messages_list.rs`. Per-tool result/use display parsing lives
//! in each `tools/<tool>/ui.rs`, and result/use routing in
//! `components/messages/user_tool_result_message` and
//! `components/messages/assistant_tool_use_message`, matching the CC Message
//! component ownership. The deserialize-stage filters/sentinels
//! (conversationRecovery.ts:164-245) live in `utils/conversation.rs` and run
//! once in `commands/resume`. This module also owns the official resumed
//! `SessionStart` hook boundary; file adoption/writes occur afterward in
//! `session_restore`/`session_storage`, never in the render projection.

use crate::components::messages::user_tool_result_message::utils::first_string;
use crate::types::message::{Attachment, RenderableMessage, StopHookInfo, SystemMessageLevel};
use std::sync::Arc;

/// Maps to: CC `utils/conversationRecovery.ts:456-589`
/// `loadConversationForResume`.
///
/// `ResumeTarget` already owns the selected/deserialized JSONL chain in the
/// current storage layer. This step constructs the official `ResumeLoadResult`
/// and appends `SessionStart('resume')` messages before
/// `process_resumed_conversation` runs.
pub fn load_conversation_for_resume(
    target: &crate::commands::resume::ResumeTarget,
) -> Result<crate::utils::session_restore::ResumeLoadResult, String> {
    let mut result = crate::utils::session_restore::ResumeLoadResult::try_from(target)?;
    // CC :540-546 establishes the original target identity before restoring its plan.
    if let Some(session_id) = result.session_id.as_deref() {
        let _ = crate::utils::process_runtime::block_on_from_sync(
            crate::utils::plans::copy_plan_for_resume(
                target
                    .metadata
                    .original_messages
                    .as_deref()
                    .unwrap_or(&target.entries),
                Some(session_id),
            ),
        );
    }
    let hook_messages = crate::utils::session_start::process_session_start_hooks(
        "resume",
        result.session_id.as_deref(),
        None,
        None,
    );
    let (messages, renderable_messages) =
        crate::utils::session_start::project_hook_result_messages(&hook_messages);
    Arc::make_mut(&mut result.messages).extend(messages);
    Arc::make_mut(&mut result.renderable_messages).extend(renderable_messages);
    Ok(result)
}

/// Maps to CC `utils/conversationRecovery.ts:376-400`
/// `restoreSkillStateFromMessages(...)`.
pub fn restore_skill_state_from_messages(messages: &[crate::types::message::Message]) {
    for message in messages {
        let crate::types::message::Message::Attachment(crate::types::message::AttachmentMessage {
            attachment: Attachment::InvokedSkills { skills },
            ..
        }) = message
        else {
            continue;
        };
        for skill in skills {
            if !skill.name.is_empty() && !skill.path.is_empty() && !skill.content.is_empty() {
                crate::bootstrap::state::add_invoked_skill(
                    &skill.name,
                    &skill.path,
                    &skill.content,
                    None,
                );
            }
        }
        // `suppressNextSkillListing()` has no production skill-listing latch
        // in this tree; do not fabricate one here.
    }
}

/// Maps to: CC resume rendering — `deserializeMessages(...)` recovers whole
/// `Message` values (`conversationRecovery.ts:154`) and `normalizeMessages`
/// splits them one content block per row at the render pipeline
/// (`utils/messages.ts:741-820`). This wrapper is that composition for the
/// cold path (session preview + the `ResumeLoadResult` render projection).
///
/// Batch D3 (item 6): the render-approximation parser died — the rows are the
/// normalize projection of the ONE typed parse. Every pre-drop it used to
/// make lives at CC's render-side home instead:
/// - envelope-isMeta user entries: `should_show_user_message`
///   (`Messages.tsx:597`, utils/messages.ts:4658-4677) in
///   `filter_non_rendering_messages`;
/// - hidden assistant text (empty / NO_RESPONSE_REQUESTED):
///   `AssistantTextMessage` null-render (CC AssistantTextMessage.tsx:73-75,
///   :91-92);
/// - nonvisual tool_use rows: `assistant_row_is_nonvisual_tool_use` in the
///   render-list filter (CC AssistantToolUseMessage returns null);
/// - null-rendering / initial attachments: `null_rendering_attachments` +
///   the `AttachmentMessage` component switch (CC AttachmentMessage.tsx:162,
///   nullRenderingAttachments.ts:15-49);
/// - image data / thinking signatures: kept whole — CC keeps restored
///   messages verbatim and the renderers never read those payloads.
pub fn renderable_messages_from_entries(entries: &[serde_json::Value]) -> Vec<RenderableMessage> {
    crate::utils::messages::normalize_messages(&messages_from_entries(entries))
}

/// The ONE cold parser (batch D3 item 6). Maps to: CC
/// `utils/conversationRecovery.ts:154` `deserializeMessages` — recovery
/// yields whole `Message` values, one per JSONL entry, with the entry's real
/// envelope uuid/timestamp and every content payload intact. The value is the
/// typed owner's parse (`conversation.rs::into_typed_messages`); render-side
/// hiding happens at the components/filter (see
/// [`renderable_messages_from_entries`]).
///
/// The deserialize-stage filters (`filterUnresolvedToolUses`, orphan
/// thinking, whitespace assistants, sentinel insertion —
/// conversationRecovery.ts:187-245) are NOT applied here: they run once in
/// `conversation::deserialize_messages_with_interrupt_detection`, which
/// produces `ResumeTarget.entries` (`commands/resume`), exactly like CC's
/// single-application pipeline.
///
/// Rows carry only CC's wire fields; every tool renders at render time from
/// the raw `toolUseResult` via its own `parse_output`
/// (CC `Tool.renderToolResultMessage` + `outputSchema.safeParse`).
/// `content` stays the raw wire text and the API projection builds tool_result
/// params from tool_use_id/content/is_error only (services/api/claude.rs).
pub fn messages_from_entries(entries: &[serde_json::Value]) -> Vec<crate::types::message::Message> {
    let mut messages = crate::utils::conversation::into_typed_messages(entries.to_vec());
    messages
}

/// Model projection of a system entry — the wire→union adapter shared by the
/// cold-session parsers (`session_storage`, `conversation`, and this module).
///
/// Maps to: CC's structural cast — `parseJSONL<Entry>(buf)`
/// (`utils/sessionStorage.ts:3614`) does no subtype validation, so the entry's
/// fields flow straight into the typed union. Unknown subtypes (including the
/// retired Cometix-only `rate_limit`/`shutdown`/`snip_*`/`agent_notification`
/// wire tags) fall back to `Informational`, which is render-equivalent to CC:
/// an unrecognized subtype reaches `SystemTextMessage.tsx:158-174` and is
/// rendered from `content`/`level` alone.
pub(crate) fn system_message_from_entry(
    entry: &serde_json::Value,
    id: String,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> crate::types::message::SystemMessage {
    use crate::types::message::{SystemBase, SystemMessage};

    let subtype = entry
        .get("subtype")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let base = SystemBase {
        uuid: id,
        timestamp,
        is_meta: entry
            .get("isMeta")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    };
    let text = system_text(entry);
    match subtype {
        "compact_boundary" => SystemMessage::CompactBoundary {
            base,
            logical_parent_uuid: entry
                .get("logicalParentUuid")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            compact_metadata: entry
                .get("compactMetadata")
                .cloned()
                .and_then(|metadata| serde_json::from_value(metadata).ok()),
        },
        "microcompact_boundary" => SystemMessage::MicrocompactBoundary {
            base,
            microcompact_metadata: entry
                .get("microcompactMetadata")
                .cloned()
                .and_then(|metadata| serde_json::from_value(metadata).ok()),
        },
        // CC runtime shape: `createSystemAPIErrorMessage`
        // (utils/messages.ts:4585-4603) writes `error`/`retryInMs`/
        // `retryAttempt`/`maxRetries`. `retryInSeconds` is the pre-D1 Cometix
        // wire spelling, kept readable.
        "api_error" => SystemMessage::ApiError {
            base,
            error: if text.is_empty() {
                "API error".to_string()
            } else {
                text
            },
            retry_in_ms: entry
                .get("retryInMs")
                .and_then(|value| value.as_u64())
                .or_else(|| {
                    entry
                        .get("retryInSeconds")
                        .and_then(|value| value.as_u64())
                        .map(|seconds| seconds * 1000)
                })
                .unwrap_or(0),
            retry_attempt: entry
                .get("retryAttempt")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
            max_retries: entry
                .get("maxRetries")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
        },
        "local_command" => SystemMessage::LocalCommand {
            base,
            content: text,
        },
        "permission_retry" => SystemMessage::PermissionRetry {
            base,
            content: text,
            commands: string_array_field(entry, &["commands"]),
        },
        "bridge_status" => SystemMessage::BridgeStatus {
            base,
            content: text,
            url: first_string(entry, &["url"]).unwrap_or_default(),
            upgrade_nudge: first_string(entry, &["upgradeNudge", "upgrade_nudge"]),
        },
        "scheduled_task_fire" => SystemMessage::ScheduledTaskFire {
            base,
            content: text,
        },
        "turn_duration" => SystemMessage::TurnDuration {
            base,
            duration_ms: entry
                .get("durationMs")
                .or_else(|| entry.get("duration_ms"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0),
            budget_tokens: entry
                .get("budgetTokens")
                .or_else(|| entry.get("budget_tokens"))
                .and_then(|value| value.as_u64()),
            budget_limit: entry
                .get("budgetLimit")
                .or_else(|| entry.get("budget_limit"))
                .and_then(|value| value.as_u64()),
            budget_nudges: entry
                .get("budgetNudges")
                .or_else(|| entry.get("budget_nudges"))
                .and_then(|value| value.as_u64()),
            message_count: entry
                .get("messageCount")
                .or_else(|| entry.get("message_count"))
                .and_then(|value| value.as_u64())
                .map(|count| count as usize),
        },
        "away_summary" => SystemMessage::AwaySummary {
            base,
            content: text,
        },
        "memory_saved" => SystemMessage::MemorySaved {
            base,
            written_paths: string_array_field(entry, &["writtenPaths", "written_paths"]),
        },
        "agents_killed" => SystemMessage::AgentsKilled { base },
        "api_metrics" => SystemMessage::ApiMetrics {
            base,
            ttft_ms: entry
                .get("ttftMs")
                .and_then(|value| value.as_number())
                .cloned(),
            otps: entry
                .get("otps")
                .and_then(|value| value.as_number())
                .cloned(),
            is_p50: entry.get("isP50").and_then(|value| value.as_bool()),
            hook_duration_ms: entry.get("hookDurationMs").and_then(|value| value.as_u64()),
            turn_duration_ms: entry.get("turnDurationMs").and_then(|value| value.as_u64()),
            tool_duration_ms: entry.get("toolDurationMs").and_then(|value| value.as_u64()),
            classifier_duration_ms: entry
                .get("classifierDurationMs")
                .and_then(|value| value.as_u64()),
            tool_count: entry.get("toolCount").and_then(|value| value.as_u64()),
            hook_count: entry.get("hookCount").and_then(|value| value.as_u64()),
            classifier_count: entry
                .get("classifierCount")
                .and_then(|value| value.as_u64()),
            config_write_count: entry
                .get("configWriteCount")
                .and_then(|value| value.as_u64()),
        },
        "file_snapshot" => SystemMessage::FileSnapshot { base },
        "thinking" => SystemMessage::Thinking {
            base,
            content: text,
        },
        "stop_hook_summary" => SystemMessage::StopHookSummary {
            base,
            hook_count: entry
                .get("hookCount")
                .or_else(|| entry.get("hook_count"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as usize,
            hook_infos: stop_hook_infos_field(entry, &["hookInfos", "hook_infos"]),
            hook_errors: string_array_field(entry, &["hookErrors", "hook_errors"]),
            prevented_continuation: entry
                .get("preventedContinuation")
                .or_else(|| entry.get("prevented_continuation"))
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            stop_reason: first_string(entry, &["stopReason", "stop_reason"]),
            has_output: entry
                .get("hasOutput")
                .or_else(|| entry.get("has_output"))
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            level: system_level(entry, subtype),
            tool_use_id: first_string(entry, &["toolUseID", "tool_use_id"]),
            hook_label: first_string(entry, &["hookLabel", "hook_label"]),
            total_duration_ms: entry
                .get("totalDurationMs")
                .or_else(|| entry.get("total_duration_ms"))
                .and_then(|value| value.as_u64()),
        },
        // "informational", the legacy Cometix "text" tag, and every unknown
        // subtype (see doc comment).
        _ => SystemMessage::Informational {
            base,
            content: text,
            level: system_level(entry, subtype),
            tool_use_id: first_string(entry, &["toolUseID", "tool_use_id"]),
            prevent_continuation: entry
                .get("preventContinuation")
                .and_then(|value| value.as_bool())
                .filter(|flag| *flag),
        },
    }
}

// `format_duration_ms` / `format_stop_hook_summary_text` died with the
// render-product fields (batch D1): the model union carries `duration_ms` and
// the CC field set; display strings derive at render like
// `SystemTextMessage.tsx:222,258,362`.

fn stop_hook_infos_field(entry: &serde_json::Value, keys: &[&str]) -> Vec<StopHookInfo> {
    keys.iter()
        .find_map(|key| entry.get(*key).and_then(|value| value.as_array()))
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let command = first_string(item, &["command", "hookName", "hook_name"]);
                    let prompt_text = first_string(item, &["promptText", "prompt_text"]);
                    let output = first_string(item, &["output"]);
                    let error = first_string(item, &["error"]);
                    let duration_ms = item
                        .get("durationMs")
                        .or_else(|| item.get("duration_ms"))
                        .and_then(|value| value.as_u64());
                    let prevented_continuation = item
                        .get("preventedContinuation")
                        .or_else(|| item.get("prevented_continuation"))
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false);
                    (command.is_some()
                        || prompt_text.is_some()
                        || output.is_some()
                        || error.is_some()
                        || duration_ms.is_some()
                        || prevented_continuation)
                        .then_some(StopHookInfo {
                            command,
                            prompt_text,
                            duration_ms,
                            output,
                            error,
                            prevented_continuation,
                        })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn string_array_field(entry: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| entry.get(*key).and_then(|value| value.as_array()))
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn system_text(entry: &serde_json::Value) -> String {
    entry
        .get("message")
        .and_then(|message| message.get("content"))
        .map(crate::components::messages::user_tool_result_message::utils::value_to_text)
        .filter(|text| !text.is_empty())
        .or_else(|| first_string(entry, &["content", "text", "message"]))
        .unwrap_or_default()
}

fn system_level(entry: &serde_json::Value, subtype: &str) -> SystemMessageLevel {
    match entry
        .get("level")
        .and_then(|value| value.as_str())
        .unwrap_or(subtype)
    {
        "error" | "api_error" => SystemMessageLevel::Error,
        "warning" | "warn" | "rate_limit" => SystemMessageLevel::Warning,
        _ => SystemMessageLevel::Info,
    }
}

// The render-approximation machinery is gone (batch D3 item 6):
// `migrate_legacy_attachment_value` (CC's migration runs once in
// `conversation::deserialize_messages_with_interrupt_detection`, matching
// conversationRecovery.ts:168-171), `attachment_renderable_messages` and the
// string-tag null list (the typed attachment row passes through normalize and
// `components/messages/attachment_message.rs` + `null_rendering_attachments`
// decide at render, CC AttachmentMessage.tsx:162 / nullRenderingAttachments.ts),
// and `parse_relevant_memories`/`attachment_summary` (batch D2).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::messages::user_tool_result_message::utils::ToolRenderOptions;
    use crate::types::message::{
        ReadResultKind, RenderableMessageKind, SearchResultMode, ToolResultStatus, ToolUseStatus,
    };
    use crate::utils::messages::{
        CANCEL_MESSAGE, NO_RESPONSE_REQUESTED, PLAN_REJECTION_PREFIX, REJECT_MESSAGE,
        REJECT_MESSAGE_WITH_REASON_PREFIX,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;

    /// A2.3 extractor: rows carry the real message; the old assertions on the
    /// baked `(tool_name, description)` pair now derive both the way the
    /// renderer does — display name + render-time summary from the block.
    fn recovered_tool_rows(messages: &[RenderableMessage]) -> Vec<(String, String)> {
        messages
            .iter()
            .filter_map(|row| {
                let RenderableMessageKind::Assistant { message } = &row.kind else {
                    return None;
                };
                let Some(crate::types::message::AssistantContent::ToolUse(tool_use)) =
                    message.first_content_block()
                else {
                    return None;
                };
                // Mirror the renderer's own projection: the mount site hands
                // the component the canonical name and the component derives
                // the user-facing name (message.rs mount-site note).
                let name = if tool_use.name.eq_ignore_ascii_case("Read") {
                    tool_use.name.clone()
                } else {
                    crate::components::messages::assistant_tool_use_message::tool_use_display_name(
                        &tool_use.name,
                        Some(&tool_use.input),
                    )
                };
                let description =
                    crate::components::messages::assistant_tool_use_message::render_tool_use_message(
                        &tool_use.name,
                        &tool_use.input,
                        ToolRenderOptions::default(),
                    )
                    .unwrap_or_default();
                Some((name, description))
            })
            .collect()
    }

    fn owned_pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
            .collect()
    }

    /// User rows carry the real `UserMessage`; read the row's tool-result
    /// block the way the renderer does (CC `Message.tsx:284` tool_result
    /// branch).
    fn tool_result_of(message: &RenderableMessage) -> Option<&crate::types::message::ToolResult> {
        match &message.kind {
            RenderableMessageKind::User { message } => match message.first_content_block() {
                Some(crate::types::message::UserContent::ToolResult(result)) => Some(result),
                _ => None,
            },
            _ => None,
        }
    }

    /// Owning twin of [`tool_result_of`] for `.into_iter().find_map(...)`
    /// chains that consume the recovered messages.
    fn into_tool_result(message: RenderableMessage) -> Option<crate::types::message::ToolResult> {
        match message.kind {
            RenderableMessageKind::User { message } => match message.content.into_iter().next() {
                Some(crate::types::message::UserContent::ToolResult(result)) => Some(result),
                _ => None,
            },
            _ => None,
        }
    }

    /// Render-side view of a tool_result row (batch D3 item 6): the visible
    /// text derives at render — `message.rs` resolves the tool through the
    /// lookups (CC `UserToolResultMessage.tsx:43`) and
    /// `render_tool_result_lines` derives the line(s) from the RAW content
    /// plus the raw `toolUseResult`. The old parser-side content bake is gone
    /// (CC keeps restored messages whole, conversationRecovery.ts:154), so
    /// the assertions that used to read baked `content` read this instead.
    fn rendered_tool_result_text(
        messages: &[RenderableMessage],
        result: &crate::types::message::ToolResult,
    ) -> String {
        let lookups = crate::components::messages_list::build_message_lookups(messages, messages);
        let tool_use_row = lookups
            .tool_use_by_tool_use_id
            .get(result.tool_use_id.0.as_str());
        let tool_name = tool_use_row
            .and_then(|row| crate::components::message_row::assistant_tool_use_name(row))
            .unwrap_or_default();
        let tool_input = tool_use_row
            .and_then(|row| crate::components::message_row::assistant_tool_use_input(row));
        crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result(
            &tool_name,
            result.derived_status(),
            &result.content,
            result.tool_use_result.as_ref(),
            tool_input.as_ref(),
            &[],
            crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
        )
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n")
    }

    #[test]
    fn recovered_glob_tool_uses_reflect_success_failure_cancel_and_mixed_unresolved_status() {
        let entries = vec![
            json!({
                "type": "assistant",
                "uuid": "assistant-glob-status",
                "message": {
                    "id": "msg-glob-status",
                    "content": [
                        {"type":"tool_use","id":"toolu-success","name":"Glob","input":{"pattern":"*.rs"}},
                        {"type":"tool_use","id":"toolu-error","name":"Glob","input":{"pattern":"*.rs"}},
                        {"type":"tool_use","id":"toolu-cancel","name":"Glob","input":{"pattern":"*.rs"}},
                        {"type":"tool_use","id":"toolu-unresolved","name":"Glob","input":{"pattern":"*.rs"}}
                    ]
                }
            }),
            json!({
                "type": "user",
                "uuid": "user-glob-status",
                "message": {
                    "role": "user",
                    "content": [
                        {"type":"tool_result","tool_use_id":"toolu-success","content":"src/lib.rs"},
                        {"type":"tool_result","tool_use_id":"toolu-error","content":"ripgrep failed","is_error":true},
                        {"type":"tool_result","tool_use_id":"toolu-cancel","content":CANCEL_MESSAGE,"is_error":true}
                    ]
                }
            }),
        ];
        // The same facts, asserted through CC's authority. Recovery no longer
        // bakes a status onto the ToolUse row (CC `conversationRecovery.ts` has
        // no status concept); it restores the tool_result rows and
        // `buildMessageLookups` derives resolution from them, exactly as during
        // a live turn. Reading the sets here is what proves the old per-row map
        // was redundant rather than load-bearing.
        let messages = renderable_messages_from_entries(&entries);
        let lookups = crate::components::messages_list::build_message_lookups(&messages, &messages);
        let derive = |tool_use_id: &str| {
            crate::components::messages::derive_tool_use_status(
                Some(tool_use_id),
                &std::collections::HashSet::new(),
                Some(&lookups),
            )
        };

        assert_eq!(derive("toolu-success"), ToolUseStatus::Succeeded);
        assert_eq!(derive("toolu-error"), ToolUseStatus::Failed);
        // CC's cancel block carries `is_error: true` on the wire
        // (createToolResultStopMessage, utils/messages.ts:627-628), and the
        // lookups mark errored from `is_error` alone (:1243-1245) — the
        // fixture mirrors that wire shape.
        assert_eq!(derive("toolu-cancel"), ToolUseStatus::Failed);
        // CC keeps a mixed assistant message intact when at least one tool use
        // has a result, so its unmatched sibling stays unresolved. On resume the
        // in-progress set is empty, so unresolved reads as queued.
        assert_eq!(derive("toolu-unresolved"), ToolUseStatus::Queued);
    }

    #[test]
    fn recovered_glob_assistant_message_is_filtered_when_all_tool_uses_are_unresolved() {
        let entries = vec![json!({
            "type": "assistant",
            "uuid": "assistant-unresolved-glob",
            "message": {
                "id": "msg-unresolved-glob",
                "content": [
                    {"type":"text","text":"partial text is removed with the message"},
                    {"type":"tool_use","id":"toolu-unresolved-a","name":"Glob","input":{"pattern":"*.rs"}},
                    {"type":"tool_use","id":"toolu-unresolved-b","name":"Glob","input":{"pattern":"*.md"}},
                    {"type":"tool_use","id":7,"name":"Glob","input":{"pattern":"*.txt"}},
                    {"type":"tool_use","id":null,"name":"Glob","input":{"pattern":"*.json"}},
                    {"type":"tool_use","name":"Glob","input":{"pattern":"*.toml"}},
                    {"type":"tool_use","id":{"legacy":true},"name":"Glob","input":{"pattern":"*.yaml"}}
                ]
            }
        })];

        // `filterUnresolvedToolUses` runs once at the deserialize stage
        // (CC conversationRecovery.ts:187-189), which produces the entries the
        // parser receives (`commands/resume` → ResumeTarget.entries). The
        // parser itself no longer re-filters (batch D3 item 6).
        let deserialized = crate::utils::conversation::deserialize_messages(entries);
        assert!(renderable_messages_from_entries(&deserialized).is_empty());
    }

    #[test]
    fn messages_from_entries_yields_whole_messages_with_entry_envelope() {
        let entries = vec![
            json!({
                "type": "assistant",
                "uuid": "abcdefgh-1234-5678-9abc-def012345678",
                "timestamp": "2026-07-12T00:00:00.000Z",
                "message": {"id": "msg_1", "content": [
                    {"type": "text", "text": "one"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "echo hi"}}
                ]}
            }),
            json!({
                "type": "user",
                "uuid": "11111111-2222-3333-4444-555555555555",
                "timestamp": "2026-07-12T00:00:01.000Z",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "hi"}]}
            }),
        ];

        // One whole Message per JSONL entry: real envelope uuid + timestamp,
        // whole content vec, ONE MessageIdentity sibling per assistant message
        // (not per row) carrying the api message id.
        let messages = messages_from_entries(&entries);
        assert_eq!(messages.len(), 2);
        let crate::types::message::Message::Assistant(assistant) = &messages[0] else {
            panic!("expected assistant message, got {:?}", messages[0]);
        };
        assert_eq!(assistant.uuid, "abcdefgh-1234-5678-9abc-def012345678");
        assert_eq!(
            assistant.timestamp.to_rfc3339(),
            "2026-07-12T00:00:00+00:00"
        );
        assert_eq!(assistant.content.len(), 3);
        assert_eq!(
            assistant
                .content
                .iter()
                .filter(|block| matches!(
                    block,
                    crate::types::message::AssistantContent::MessageIdentity(_)
                ))
                .count(),
            1
        );
        assert_eq!(assistant.api_message_id(), Some("msg_1"));
        let crate::types::message::Message::User(user) = &messages[1] else {
            panic!("expected user message, got {:?}", messages[1]);
        };
        assert_eq!(user.uuid, "11111111-2222-3333-4444-555555555555");
        assert_eq!(user.content.len(), 1);
    }

    #[test]
    fn recovered_split_rows_use_official_derive_uuid_shape() {
        // CC utils/messages.ts:725-728 — parent[..24] + 12-hex index; the
        // invented `{id}-user-{idx}` / `{id}-assistant-{idx}` recovery ids
        // died with the normalize wrapper.
        let entries = vec![
            json!({
                "type": "assistant",
                "uuid": "abcdefgh-1234-5678-9abc-def012345678",
                "message": {"id": "msg_1", "content": [
                    {"type": "text", "text": "one"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "echo hi"}}
                ]}
            }),
            json!({
                "type": "user",
                "uuid": "11111111-2222-3333-4444-555555555555",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "hi"}]}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let uuids = messages
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            uuids,
            vec![
                "abcdefgh-1234-5678-9abc-000000000000",
                "abcdefgh-1234-5678-9abc-000000000001",
                // isNewChain stays flipped: every later row derives too
                // (utils/messages.ts:742-748).
                "11111111-2222-3333-4444-000000000000",
            ]
        );
        // The envelope identity rides every split assistant row — it is the
        // grouping key for same-message tool uses.
        for message in &messages[..2] {
            let RenderableMessageKind::Assistant { message, .. } = &message.kind else {
                panic!("expected assistant rows, got {message:?}");
            };
            assert_eq!(message.api_message_id(), Some("msg_1"));
        }
    }

    fn append_hook_marker_command(path: &std::path::Path, marker: &str) -> String {
        #[cfg(windows)]
        {
            format!("echo {marker}>>\"{}\"", path.display())
        }
        #[cfg(not(windows))]
        {
            let path = path.display().to_string().replace('\'', "'\\''");
            format!(
                "printf '%s\\n' '{marker}' >> '{path}'; printf '%s' '{{\"systemMessage\":\"{marker} hook message\"}}'"
            )
        }
    }

    #[test]
    fn load_conversation_for_resume_runs_only_session_start_resume_hooks() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let marker_path = std::env::temp_dir().join(format!(
            "cometix-startup-resume-hooks-{}",
            uuid::Uuid::new_v4()
        ));
        let settings = crate::utils::settings::SettingsJson {
            hooks: Some(json!({
                "SessionEnd": [{
                    "matcher": "*",
                    "hooks": [{
                        "type": "command",
                        "command": append_hook_marker_command(&marker_path, "end"),
                        "timeout": 5
                    }]
                }],
                "SessionStart": [{
                    "matcher": "resume",
                    "hooks": [{
                        "type": "command",
                        "command": append_hook_marker_command(&marker_path, "start"),
                        "timeout": 5
                    }]
                }]
            })),
            ..crate::utils::settings::SettingsJson::default()
        };
        crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &settings, None, false,
        );
        let target = crate::commands::resume::ResumeTarget {
            session_id: "startup-resume-session".to_string(),
            project_path: None,
            entries: vec![json!({
                "type": "user",
                "timestamp": "2026-07-12T00:00:00.000Z",
                "message": {"role": "user", "content": "resume hooks"}
            })],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: crate::commands::resume::ResumeMetadata::default(),
            entrypoint: Some(crate::types::command::ResumeEntrypoint::CliFlag),
        };

        let loaded = load_conversation_for_resume(&target).expect("conversation should load");
        assert_eq!(
            std::fs::read_to_string(&marker_path)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["start"]
        );
        assert_eq!(loaded.session_id.as_deref(), Some("startup-resume-session"));
        #[cfg(not(windows))]
        {
            assert!(loaded.messages.iter().any(|message| matches!(
                message,
                crate::types::message::Message::HookResult(hook)
                    if hook.attachment.get("content").and_then(serde_json::Value::as_str)
                        == Some("start hook message")
            )));
            assert!(loaded.renderable_messages.iter().any(|message| matches!(
                &message.kind,
                RenderableMessageKind::System(
                    crate::types::message::SystemMessage::Informational { content, .. }
                ) if content == "start hook message"
            )));
        }

        crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
        let _ = std::fs::remove_file(marker_path);
    }

    #[test]
    fn tool_result_uses_originating_tool_name_lookup() {
        let entries = vec![
            json!({
                "type": "assistant",
                "uuid": "a",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "Bash",
                        "input": {"command": "echo hi"}
                    }]
                }
            }),
            json!({
                "type": "user",
                "uuid": "u",
                "message": {
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_1",
                        "content": "hi\n"
                    }]
                }
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_result = messages
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");

        // The row no longer stores a tool_name or a display shape.
        // This entry has no `toolUseResult`; visibility is a render-time
        // decision (`transcript_tool_result_should_emit_ui`) — CC's success
        // leaf returns null when the raw is missing
        // (`UserToolSuccessMessage.tsx:72`).
        assert_eq!(tool_result.tool_use_id.0, "toolu_1");
        assert_eq!(tool_result.derived_status(), ToolResultStatus::Success);
        assert_eq!(tool_result.content, "hi\n");
    }

    #[test]
    fn assistant_tool_use_uses_official_facing_name_and_summary() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Edit", "input": {"file_path": "a.txt", "old_string": "old", "new_string": "new"}}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "Grep", "input": {"pattern": "needle", "path": "/tmp/project"}}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_3", "name": "TaskOutput", "input": {"task_id": "task-abc", "block": false}}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_4", "name": "Agent", "input": {"description": "Review auth", "prompt": "Inspect auth", "subagent_type": "reviewer"}}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_5", "name": "Task", "input": {"description": "Run tests", "prompt": "Run the suite", "subagent_type": "worker", "name": "runner"}}]}
            }),
            json!({
                "type": "assistant",
                // `prompt` is required on every real Agent tool_use; CC hides
                // the row entirely when it (or `description`) is missing
                // (AgentTool/UI.tsx:472-483).
                "message": {"content": [{"type": "tool_use", "id": "toolu_6", "name": "Agent", "input": {"description": "Review storage", "prompt": "Inspect storage", "subagent_type": "reviewer", "model": "opus"}}]}
            }),
            // Cold recovery filters all-unresolved assistant messages. Pair the
            // uses so this fixture tests facing names rather than active state.
            json!({
                "type": "user",
                "message": {"content": [
                    {"type":"tool_result","tool_use_id":"toolu_1","content":"ok"},
                    {"type":"tool_result","tool_use_id":"toolu_2","content":"ok"},
                    {"type":"tool_result","tool_use_id":"toolu_3","content":"ok"},
                    {"type":"tool_result","tool_use_id":"toolu_4","content":"ok"},
                    {"type":"tool_result","tool_use_id":"toolu_5","content":"ok"},
                    {"type":"tool_result","tool_use_id":"toolu_6","content":"ok"}
                ]}
            }),
        ];

        let tool_uses = renderable_messages_from_entries(&entries)
            .into_iter()
            .collect::<Vec<_>>();
        let tool_uses_pairs = recovered_tool_rows(&tool_uses);
        let tool_uses = tool_uses_pairs;

        assert_eq!(
            tool_uses,
            vec![
                ("Update".to_string(), "a.txt".to_string()),
                (
                    "Search".to_string(),
                    "pattern: \"needle\", path: \"/tmp/project\"".to_string()
                ),
                // Same rule as the `model` note below, applied to TaskOutput:
                // `task_id` is a separate `renderToolUseTag`
                // (`TaskOutputTool.tsx:377-382`) and `renderToolUseMessage`
                // reads only `block` (`:369-375`). The summary never carries
                // the id, so this row is `non-blocking` alone.
                ("Task Output".to_string(), "non-blocking".to_string()),
                // CC `AgentTool/UI.tsx:989-1011#userFacingName` is the row's
                // bold name and `:472-483#renderToolUseMessage` is the
                // parenthesised text — the type never appears inside the
                // parens, and `@name` never appears in an UNGROUPED row at all
                // (only `renderGroupedAgentToolUse` prefixes it, `:862`).
                ("reviewer".to_string(), "Review auth".to_string()),
                // `subagent_type: "worker"` displays as "Agent" (`:1004-1007`).
                ("Agent".to_string(), "Run tests".to_string()),
                // `model` is a separate `renderToolUseTag` (`:485-512`), not a
                // suffix on the summary.
                ("reviewer".to_string(), "Review storage".to_string()),
            ]
        );
    }

    #[test]
    fn skill_tool_use_and_result_match_official_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Skill", "input": {"skill": "review-pr"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Launching skill: review-pr", "is_error": false}]},
                "toolUseResult": {"success": true, "commandName": "review-pr", "allowedTools": ["Read", "Grep"], "model": "opus", "status": "inline"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "Skill", "input": {"skill": "legacy", "loadedFrom": "commands_DEPRECATED"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "Skill completed", "is_error": false}]},
                "toolUseResult": {"success": true, "commandName": "legacy", "status": "forked", "agentId": "agent-1", "result": "done"}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_uses = messages.iter().cloned().collect::<Vec<_>>();
        let tool_uses = recovered_tool_rows(&tool_uses);
        assert_eq!(
            tool_uses,
            owned_pairs(&[("Skill", "review-pr"), ("Skill", "/legacy")])
        );

        // No Skill display shape — the raw `toolUseResult` rides each
        // row and the by-tool-name renderer consumes it.
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        assert_eq!(
            rendered_tool_result_text(&messages, results[0]),
            "Successfully loaded skill · 2 tools allowed · opus"
        );
        assert_eq!(rendered_tool_result_text(&messages, results[1]), "Done");
    }

    #[test]
    fn agent_tool_result_maps_official_completed_and_launch_rows() {
        let completed_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent", "input": {"description": "Review auth", "prompt": "Inspect auth"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_agent", "content": "raw", "is_error": false}]},
                "toolUseResult": {
                    "status": "completed",
                    "agentId": "agent-1",
                    "prompt": "Inspect auth",
                    "content": [{"type": "text", "text": "All good"}],
                    "totalToolUseCount": 3,
                    "totalDurationMs": 4200,
                    "totalTokens": 12500,
                    // The agentToolResultSchema usage object requires all
                    // seven keys (five nullable).
                    "usage": {
                        "input_tokens": 1,
                        "output_tokens": 2,
                        "cache_creation_input_tokens": null,
                        "cache_read_input_tokens": null,
                        "server_tool_use": null,
                        "service_tier": null,
                        "cache_creation": null
                    }
                }
            }),
        ];
        let completed_rows = renderable_messages_from_entries(&completed_entries);
        let completed = completed_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected completed agent result");
        // The parser keeps CC's raw wire content; the visible line derives at
        // render from the raw `toolUseResult` (batch D3 item 6).
        assert_eq!(completed.content, "raw");
        assert!(
            rendered_tool_result_text(&completed_rows, completed)
                .starts_with("Done (3 tool uses · 12.5k tokens · 4s)")
        );

        let async_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_async", "name": "Task", "input": {"description": "Run tests", "prompt": "cargo test"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_async", "content": "raw", "is_error": false}]},
                "toolUseResult": {"status": "async_launched", "agentId": "agent-2", "description": "Run tests", "prompt": "cargo test", "outputFile": "/tmp/agent.out"}
            }),
        ];
        let async_rows = renderable_messages_from_entries(&async_entries);
        let async_result = async_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected async agent result");
        assert_eq!(async_result.content, "raw");
        assert!(
            rendered_tool_result_text(&async_rows, async_result).starts_with("Backgrounded agent")
        );
    }

    #[test]
    fn worktree_tool_use_and_result_match_official_visible_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "EnterWorktree", "input": {"name": "feature"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Created worktree", "is_error": false}]},
                "toolUseResult": {"worktreePath": "/tmp/project-feature", "worktreeBranch": "feature", "message": "Created worktree"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "ExitWorktree", "input": {"action": "keep"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "Kept worktree", "is_error": false}]},
                "toolUseResult": {"action": "keep", "originalCwd": "/tmp/project", "worktreePath": "/tmp/project-feature", "worktreeBranch": "feature", "message": "Kept worktree"}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_uses = messages.iter().cloned().collect::<Vec<_>>();
        let tool_uses = recovered_tool_rows(&tool_uses);
        assert_eq!(
            tool_uses,
            owned_pairs(&[
                ("Creating worktree", "Creating worktree…"),
                ("Exiting worktree", "Exiting worktree…"),
            ])
        );

        // No worktree display shapes — the raw rides the rows and the
        // by-tool-name renderers derive the bodies from it.
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        let rendered = crate::tools::enter_worktree_tool::ui::render_tool_result_message(
            results[0].tool_use_result.as_ref(),
        );
        assert_eq!(rendered[0].text, "Switched to worktree on branch feature");
        assert_eq!(rendered[1].text, "/tmp/project-feature");
        let rendered = crate::tools::exit_worktree_tool::ui::render_tool_result_message(
            results[1].tool_use_result.as_ref(),
        );
        assert_eq!(rendered[0].text, "Kept worktree (branch feature)");
        assert_eq!(rendered[1].text, "Returned to /tmp/project");
    }

    #[test]
    fn config_tool_use_and_result_match_official_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Config", "input": {"setting": "theme"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "theme = \"dark\"", "is_error": false}]},
                "toolUseResult": {"success": true, "operation": "get", "setting": "theme", "value": "dark"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "Config", "input": {"setting": "theme", "value": "light"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "Set theme to \"light\"", "is_error": false}]},
                "toolUseResult": {"success": true, "operation": "set", "setting": "theme", "previousValue": "dark", "newValue": "light"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_3", "name": "Config", "input": {}}]}
            }),
        ];

        // The unresolved toolu_3 use is dropped by the deserialize-stage
        // filter (CC conversationRecovery.ts:187-189), which runs before the
        // parser in the real resume pipeline.
        let messages = renderable_messages_from_entries(
            &crate::utils::conversation::deserialize_messages(entries),
        );
        let tool_uses = messages.iter().cloned().collect::<Vec<_>>();
        let tool_uses = recovered_tool_rows(&tool_uses);
        assert_eq!(
            tool_uses,
            owned_pairs(&[
                ("Config", "Getting theme"),
                ("Config", "Setting theme to \"light\""),
            ])
        );

        // No display shape — the raw `toolUseResult` rides each row and
        // the by-tool-name renderer consumes it.
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        assert_eq!(
            rendered_tool_result_text(&messages, results[0]),
            "theme = \"dark\""
        );
        assert_eq!(
            rendered_tool_result_text(&messages, results[1]),
            "Set theme to \"light\""
        );
    }

    #[test]
    fn lsp_tool_use_and_result_match_official_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "LSP", "input": {"operation": "findReferences", "filePath": "/tmp/project/src/main.rs", "line": 7, "character": 12}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Found 2 references", "is_error": false}]},
                "toolUseResult": {"operation": "findReferences", "result": "src/main.rs:7:12\nsrc/lib.rs:3:4", "filePath": "/tmp/project/src/main.rs", "resultCount": 2, "fileCount": 2}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_use = messages
            .iter()
            .find_map(|message| {
                recovered_tool_rows(std::slice::from_ref(message))
                    .into_iter()
                    .next()
            })
            .expect("expected LSP tool use");
        assert_eq!(tool_use.0, "LSP");
        assert_eq!(
            tool_use.1,
            "operation: \"findReferences\", file: \"/tmp/project/src/main.rs\", position: 7:12"
        );

        // No display shape — the raw `toolUseResult` rides the row and
        // the by-tool-name renderer consumes it (LSPResultSummary collapsed).
        let result = messages
            .iter()
            .find_map(tool_result_of)
            .expect("expected LSP tool result");
        assert_eq!(
            rendered_tool_result_text(&messages, result),
            "Found 2 references across 2 files (ctrl+o to expand)"
        );
    }

    #[test]
    fn remote_trigger_tool_use_and_result_match_official_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "RemoteTrigger", "input": {"action": "run", "trigger_id": "nightly"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "HTTP 202\n{\"ok\":true}", "is_error": false}]},
                "toolUseResult": {"status": 202, "json": "{\n  \"ok\": true\n}"}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_use = messages
            .iter()
            .find_map(|message| {
                recovered_tool_rows(std::slice::from_ref(message))
                    .into_iter()
                    .next()
            })
            .expect("expected RemoteTrigger tool use");
        assert_eq!(
            tool_use,
            ("RemoteTrigger".to_string(), "run nightly".to_string())
        );

        let result = messages
            .iter()
            .find_map(tool_result_of)
            .expect("expected RemoteTrigger tool result");
        // Raw wire content stays (D3 item 6); the pretty JSON derives at
        // render from the raw `toolUseResult`.
        assert_eq!(result.content, "HTTP 202\n{\"ok\":true}");
        // CC RemoteTriggerTool/UI.tsx:11-19 renders the line count, never the
        // JSON body — `HTTP {status} ({lines} lines)`.
        assert_eq!(
            rendered_tool_result_text(&messages, result),
            "HTTP 202 (3 lines)"
        );
        // No display shape — the raw `toolUseResult` rides the row.
        assert_eq!(
            result.tool_use_result,
            Some(json!({"status": 202, "json": "{\n  \"ok\": true\n}"}))
        );
    }

    #[test]
    fn task_stop_tool_matches_official_use_and_result_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "TaskStop", "input": {"task_id": "task_1"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "{\"task_id\":\"task_1\"}", "is_error": false}]},
                "toolUseResult": {"message": "Successfully stopped task", "task_id": "task_1", "task_type": "local_bash", "command": "cargo test"}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_use = messages
            .iter()
            .find_map(|message| {
                recovered_tool_rows(std::slice::from_ref(message))
                    .into_iter()
                    .next()
            })
            .expect("expected TaskStop tool use");
        assert_eq!(tool_use, ("Stop Task".to_string(), String::new()));

        let result = messages
            .iter()
            .find_map(tool_result_of)
            .expect("expected TaskStop tool result");
        // No display shape — the raw `toolUseResult` rides the row and
        // the by-tool-name renderer consumes it.
        assert_eq!(
            rendered_tool_result_text(&messages, result),
            "cargo test · stopped"
        );
    }

    #[test]
    fn schedule_cron_tools_match_official_use_and_result_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "CronCreate", "input": {"cron": "7 * * * *", "prompt": "check the deployment status and report a concise summary"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Scheduled recurring job cron_1", "is_error": false}]},
                "toolUseResult": {"id": "cron_1", "humanSchedule": "hourly at :07", "recurring": true}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "CronDelete", "input": {"id": "cron_1"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "Cancelled job cron_1.", "is_error": false}]},
                "toolUseResult": {"id": "cron_1"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_3", "name": "CronList", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_3", "content": "cron_2 — daily", "is_error": false}]},
                "toolUseResult": {"jobs": [{"id": "cron_2", "humanSchedule": "daily at 09:00", "prompt": "standup", "cron": "0 9 * * *"}]}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_uses = messages.iter().cloned().collect::<Vec<_>>();
        let tool_uses = recovered_tool_rows(&tool_uses);
        assert_eq!(
            tool_uses,
            owned_pairs(&[
                (
                    "CronCreate",
                    "7 * * * *: check the deployment status and report a concise summary",
                ),
                ("CronDelete", "cron_1"),
                ("CronList", ""),
            ])
        );

        // No cron display shapes — the raw rides the rows and the
        // by-tool-name renderers derive the summaries from it.
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        let rendered = crate::tools::schedule_cron_tool::ui::render_create_result_message(
            results[0].tool_use_result.as_ref(),
        );
        assert_eq!(rendered[0].text, "Scheduled cron_1 (hourly at :07)");
        let rendered = crate::tools::schedule_cron_tool::ui::render_delete_result_message(
            results[1].tool_use_result.as_ref(),
        );
        assert_eq!(rendered[0].text, "Cancelled cron_1");
        let rendered = crate::tools::schedule_cron_tool::ui::render_list_result_message(
            results[2].tool_use_result.as_ref(),
        );
        assert_eq!(rendered[0].text, "cron_2 daily at 09:00");
    }

    #[test]
    fn generic_mcp_tool_matches_official_input_and_result_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "mcp__slack__send_message", "input": {"channel": "ops", "text": "hello"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "sent", "is_error": false}]},
                "toolUseResult": "sent"
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "mcp__media__describe", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "", "is_error": false}]},
                "toolUseResult": [{"type": "text", "text": "caption"}, {"type": "image", "source": {"type": "base64"}}]
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_uses = messages.iter().cloned().collect::<Vec<_>>();
        let tool_uses = recovered_tool_rows(&tool_uses);
        assert_eq!(
            tool_uses,
            owned_pairs(&[
                (
                    "mcp__slack__send_message",
                    "channel: \"ops\", text: \"hello\"",
                ),
                ("mcp__media__describe", ""),
            ])
        );

        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        // No Mcp display shape — the raw `toolUseResult` rides each row
        // and the by-tool-name dispatch renders it with the lookup input.
        assert_eq!(results[0].content, "sent");
        // Raw wire content stays empty (D3 item 6); the caption derives at
        // render from the row's raw content blocks.
        assert_eq!(results[1].content, "");
        let rendered = rendered_tool_result_text(
            &messages,
            messages.iter().filter_map(tool_result_of).nth(1).unwrap(),
        );
        assert!(rendered.contains("caption"));
        assert!(rendered.contains("[Image]"));
    }

    #[test]
    fn mcp_resource_tools_match_official_use_and_result_ui_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "ListMcpResourcesTool", "input": {"server": "memory"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "[]", "is_error": false}]},
                "toolUseResult": []
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "ReadMcpResourceTool", "input": {"server": "memory", "uri": "mem://note"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "{}", "is_error": false}]},
                "toolUseResult": {"contents": [{"uri": "mem://note", "mimeType": "text/plain", "text": "hello"}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_3", "name": "ReadMcpResourceTool", "input": {"server": "memory"}}]}
            }),
        ];

        // The unresolved toolu_3 use drops at the deserialize stage
        // (CC conversationRecovery.ts:187-189), before the parser.
        let messages = renderable_messages_from_entries(
            &crate::utils::conversation::deserialize_messages(entries),
        );
        let tool_uses = messages.iter().cloned().collect::<Vec<_>>();
        let tool_uses = recovered_tool_rows(&tool_uses);
        assert_eq!(
            tool_uses,
            owned_pairs(&[
                (
                    "listMcpResources",
                    "List MCP resources from server \"memory\"",
                ),
                (
                    "readMcpResource",
                    "Read resource \"mem://note\" from server \"memory\"",
                ),
            ])
        );

        // No MCP-resource display shapes — the raw rides the rows and
        // the by-tool-name renderers derive the bodies from it.
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        let rendered = crate::tools::list_mcp_resources_tool::ui::render_tool_result_message(
            results[0].tool_use_result.as_ref(),
        );
        assert_eq!(rendered[0].text, "(No resources found)");
        let rendered = crate::tools::read_mcp_resource_tool::ui::render_tool_result_message(
            results[1].tool_use_result.as_ref(),
        );
        assert!(rendered[0].text.contains("mem://note"));
    }

    #[test]
    fn config_tool_reject_and_error_paths_use_official_visible_copy() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Config", "input": {"setting": "theme", "value": "dark"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": REJECT_MESSAGE}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "Config", "input": {"setting": "theme", "value": "invalid"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "Error: Invalid value", "is_error": true}]},
                "toolUseResult": {"success": false, "operation": "set", "setting": "theme", "error": "Invalid value"}
            }),
        ];

        let results = renderable_messages_from_entries(&entries)
            .into_iter()
            .filter_map(into_tool_result)
            .map(|result| (result.derived_status(), result.content))
            .collect::<Vec<_>>();

        assert_eq!(results[0].0, ToolResultStatus::Rejected);
        // Reject rows keep the sentinel verbatim; the renderer derives
        // the "Config change rejected" copy at render time.
        assert_eq!(results[0].1, REJECT_MESSAGE);
        // An error row carries no display shape; the error leaf renders
        // from the wire content (`Error: {error}`, is_error: true).
        assert_eq!(results[1].0, ToolResultStatus::Error);
        assert_eq!(results[1].1, "Error: Invalid value");
    }

    #[test]
    fn synthetic_empty_assistant_messages_are_suppressed_like_official_renderer() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": NO_RESPONSE_REQUESTED}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": "(no content)"}]}
            }),
        ];

        // CC keeps the rows (Messages.tsx has no empty-text filter) and
        // `AssistantTextMessage` null-renders them (AssistantTextMessage.tsx
        // :73-75 isEmptyMessageText, :91-92 NO_RESPONSE_REQUESTED). The
        // converged parser keeps them too; suppression is the component's.
        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 2);
        use iocraft::prelude::ElementExt as _;
        for row in &messages {
            let RenderableMessageKind::Assistant { message } = &row.kind else {
                panic!("expected assistant text rows, got {row:?}");
            };
            let Some(crate::types::message::AssistantContent::Text(text)) =
                message.first_content_block()
            else {
                panic!("expected text block");
            };
            let canvas = iocraft::prelude::element! {
                iocraft::prelude::ContextProvider(
                    value: iocraft::prelude::Context::owned(*crate::utils::theme::current()),
                ) {
                    crate::components::messages::assistant_text_message::AssistantTextMessage(
                        content: text.clone(),
                    )
                }
            }
            .render(None)
            .to_string();
            assert_eq!(canvas.trim(), "");
        }
    }

    #[test]
    fn hidden_tool_use_and_success_result_are_suppressed_when_official_renderer_is_null() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "TodoWrite", "input": {"todos": []}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Todos have been modified successfully.", "is_error": false}]},
                "toolUseResult": {"oldTodos": [], "newTodos": []}
            }),
        ];

        // The parser keeps both rows whole (CC keeps restored messages whole);
        // the nonvisual tool_use row and the nonvisual success result are both
        // hidden by the render-list filter.
        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 2);
        assert!(
            crate::components::messages_list::filter_non_rendering_messages(messages, false)
                .is_empty()
        );
    }

    #[test]
    fn hidden_task_v2_tool_uses_and_success_results_follow_official_null_renderers() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_create", "name": "TaskCreate", "input": {"subject": "Review auth", "description": "Inspect auth"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_create", "content": "Task #1 created successfully: Review auth", "is_error": false}]},
                "toolUseResult": {"task": {"id": "1", "subject": "Review auth"}}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_get", "name": "TaskGet", "input": {"taskId": "1"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_get", "content": "Task #1: Review auth", "is_error": false}]},
                "toolUseResult": {"task": {"id": "1", "subject": "Review auth", "description": "Inspect auth", "status": "pending", "blocks": [], "blockedBy": []}}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_list", "name": "TaskList", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_list", "content": "#1 [pending] Review auth", "is_error": false}]},
                "toolUseResult": {"tasks": [{"id": "1", "subject": "Review auth", "status": "pending", "blockedBy": []}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_update", "name": "TaskUpdate", "input": {"taskId": "1", "status": "completed"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_update", "content": "Updated task #1 status", "is_error": false}]},
                "toolUseResult": {"success": true, "taskId": "1", "updatedFields": ["status"]}
            }),
        ];

        // All eight rows survive the parse whole; the four nonvisual tool uses
        // and the four nonvisual success results are all hidden by the
        // render-list filter.
        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 8);
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 4);
        assert!(
            crate::components::messages_list::filter_non_rendering_messages(messages, false)
                .is_empty()
        );
    }

    #[test]
    fn hidden_team_tool_use_and_success_results_are_suppressed_like_official_empty_name_tools() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "TeamCreate", "input": {"team_name": "reviewers"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": {"team_name": "reviewers"}, "is_error": false}]},
                "toolUseResult": {"team_name": "reviewers", "team_file_path": "/tmp/team.json", "lead_agent_id": "team-lead@reviewers"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "TeamDelete", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": {"success": true}, "is_error": false}]},
                "toolUseResult": {"success": true, "team_name": "reviewers", "message": "deleted"}
            }),
        ];

        // All four rows survive the parse whole; the two nonvisual tool uses
        // and the two nonvisual success results are all hidden by the
        // render-list filter.
        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 4);
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 2);
        assert!(
            crate::components::messages_list::filter_non_rendering_messages(messages, false)
                .is_empty()
        );
    }

    #[test]
    fn brief_tool_use_is_hidden_but_result_uses_official_message_payload() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "SendUserMessage", "input": {"message": "ignored input"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Message delivered to user.", "is_error": false}]},
                "toolUseResult": {
                    "message": "Hello **there**",
                    "attachments": [{"path": "/tmp/report.txt", "size": 2048, "isImage": false}]
                }
            }),
        ];

        // The nonvisual SendUserMessage tool_use row hides at the render-list
        // filter (D3 item 6).
        let messages = crate::components::messages_list::filter_non_rendering_messages(
            renderable_messages_from_entries(&entries),
            false,
        );
        assert_eq!(messages.len(), 1);
        let result = tool_result_of(&messages[0]).expect("expected SendUserMessage result");
        assert_eq!(result.content, "Message delivered to user.");
        // No Brief display shape — the raw `toolUseResult` rides the
        // row and the success leaf renders the component from it.
        let output = crate::tools::brief_tool::ui::parse_output(
            result
                .tool_use_result
                .as_ref()
                .expect("raw should ride the row"),
        )
        .expect("raw should parse with the Brief output schema");
        assert_eq!(output.message, "Hello **there**");
        assert_eq!(output.attachments.as_deref().map(<[_]>::len), Some(1));
    }

    #[test]
    fn structured_output_tool_use_and_result_match_official_minimal_ui() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "StructuredOutput", "input": {"ok": true, "count": 2}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Structured output provided successfully", "is_error": false}]},
                // CC's `toolUseResult` for this tool is the bare Output string
                // (`call()` data, SyntheticOutputTool.ts:59-65).
                "toolUseResult": "Structured output provided successfully"
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "StructuredOutput", "input": {}}]}
            }),
        ];

        // The unresolved toolu_2 use drops at the deserialize stage
        // (CC conversationRecovery.ts:187-189), before the parser. That leaves
        // a trailing tool_result whose tool is not one of the three terminal
        // ones (conversationRecovery.ts:364-368), so the stage reads it as
        // `interrupted_turn` and appends the continuation user message
        // (:213-225) plus the NO_RESPONSE_REQUESTED sentinel after it
        // (:231-245) — four rows. The sentinel null-renders at the component
        // (AssistantTextMessage.tsx:91-92).
        let messages = renderable_messages_from_entries(
            &crate::utils::conversation::deserialize_messages(entries),
        );
        assert_eq!(messages.len(), 4);
        let (_, description) = recovered_tool_rows(std::slice::from_ref(&messages[0]))
            .into_iter()
            .next()
            .expect("expected StructuredOutput tool use");
        assert_eq!(description, "ok: true, count: 2");

        let result = tool_result_of(&messages[1]).expect("expected StructuredOutput result");
        assert_eq!(result.content, "Structured output provided successfully");
        // No display shape — the raw string rides the row and renders
        // verbatim (`renderToolResultMessage(output) => output`).
        assert_eq!(
            rendered_tool_result_text(&messages, result),
            "Structured output provided successfully"
        );
    }

    #[test]
    fn send_message_tool_use_and_result_follow_official_visibility_rules() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "SendMessage", "input": {"to": "alice", "message": {"type": "plan_approval_response", "approve": true}}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Response sent", "is_error": false}]},
                "toolUseResult": {"success": true, "message": "Response sent"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "SendMessage", "input": {"to": "bob", "message": "hello"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "Message sent", "is_error": false}]},
                "toolUseResult": {"success": true, "message": "Message sent", "routing": {"target": "@bob"}}
            }),
        ];

        // All four rows survive the parse; the nonvisual plain "hello"
        // tool_use row and its nonvisual result both hide at the render-list
        // filter, while the visible plan-approval pair is unchanged.
        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 4);
        let (tool_name, description) = recovered_tool_rows(std::slice::from_ref(&messages[0]))
            .into_iter()
            .next()
            .expect("expected SendMessage tool use");
        assert_eq!(tool_name, "SendMessage");
        assert_eq!(description, "approve plan from: alice");
        assert_eq!(
            crate::components::messages_list::filter_non_rendering_messages(
                messages.clone(),
                false
            )
            .len(),
            2
        );

        let result = tool_result_of(&messages[1]).expect("expected SendMessage result");
        assert_eq!(result.content, "Response sent");
        // No SendMessage display shape — the raw rides the row and the
        // by-tool-name renderer consumes it.
        assert_eq!(
            rendered_tool_result_text(&messages, result),
            "Response sent"
        );
    }

    #[test]
    fn webfetch_success_uses_official_received_summary() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "WebFetch", "input": {"url": "https://example.com"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "long fetched markdown", "is_error": false}]},
                "toolUseResult": {"bytes": 25540, "code": 200, "codeText": "OK", "result": "long fetched markdown", "durationMs": 12, "url": "https://example.com"}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_result = tool_result_of(&messages[1]).expect("expected WebFetch result");

        // No WebFetch display shape — the raw rides the row and the
        // by-tool-name renderer derives the received summary from it.
        assert_eq!(tool_result.content, "long fetched markdown");
        assert_eq!(
            rendered_tool_result_text(&messages, tool_result),
            "Received 25 KB (200 OK)"
        );
    }

    #[test]
    fn websearch_success_counts_official_result_objects_only() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "WebSearch", "input": {"query": "rust"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "done", "is_error": false}]},
                "toolUseResult": {
                    "query": "rust",
                    "durationSeconds": 1.6,
                    "results": [
                        {"tool_use_id": "srvtoolu_1", "content": [{"title": "a", "url": "https://a"}]},
                        "diagnostic",
                        {"tool_use_id": "srvtoolu_2", "content": []}
                    ]
                }
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let result = tool_result_of(&messages[1]).expect("expected WebSearch result");

        // No WebSearch display shape — the raw rides the row; string
        // entries in results[] never count as searches (UI.tsx:13-30).
        assert_eq!(result.content, "done");
        assert_eq!(
            rendered_tool_result_text(&messages, result),
            "Did 2 searches in 2s"
        );
    }

    #[test]
    fn ask_user_question_success_uses_answers_summary() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "AskUserQuestion", "input": {"questions": []}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Your questions have been answered", "is_error": false}]},
                // CC's call() echoes questions alongside answers
                // (AskUserQuestionTool.tsx:296-300); the output schema
                // requires both.
                "toolUseResult": {"questions": [], "answers": {"Proceed?": "Yes"}}
            }),
        ];

        // The nonvisual AskUserQuestion tool_use row is hidden by the
        // render-list filter (CC AssistantToolUseMessage returns null), not
        // pre-dropped by the parser (batch D3 item 6). Lookups stay built
        // from the unfiltered rows, like the real pipeline.
        let all_rows = renderable_messages_from_entries(&entries);
        let messages = crate::components::messages_list::filter_non_rendering_messages(
            all_rows.clone(),
            false,
        );
        assert_eq!(messages.len(), 1);
        let result = tool_result_of(&messages[0]).expect("expected ask-user-question result");
        // Raw wire content stays; the answers copy derives at render from the
        // raw `toolUseResult`.
        assert_eq!(result.content, "Your questions have been answered");
        assert_eq!(
            rendered_tool_result_text(&all_rows, result),
            "⏺ User answered Claude's questions:\n· Proceed? → Yes"
        );
    }

    #[test]
    fn ask_user_question_rejected_uses_official_declined_copy() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "AskUserQuestion", "input": {"questions": []}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": REJECT_MESSAGE}]}
            }),
        ];

        // Nonvisual tool_use row hidden at the render-list filter (D3 item 6).
        let messages = crate::components::messages_list::filter_non_rendering_messages(
            renderable_messages_from_entries(&entries),
            false,
        );
        assert_eq!(messages.len(), 1);
        let result =
            tool_result_of(&messages[0]).expect("expected ask-user-question rejection result");
        assert_eq!(result.derived_status(), ToolResultStatus::Rejected);
        // The reject sentinel stays in `content` verbatim; the declined
        // copy is derived from the display at render time.
        assert_eq!(result.content, REJECT_MESSAGE);
        // The rejected leaf renders from the tool name.
    }

    #[test]
    fn enter_exit_plan_mode_tools_use_official_hidden_tool_use_and_result_display_shape() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "EnterPlanMode", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Entered plan mode", "is_error": false}]},
                "toolUseResult": {"message": "Entered plan mode"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "ExitPlanMode", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "User has approved your plan", "is_error": false}]},
                "toolUseResult": {"plan": "## Plan\nShip it", "filePath": "/tmp/project/.claude/plans/plan.md", "isAgent": false}
            }),
        ];

        // Hidden EnterPlanMode/ExitPlanMode tool_use rows drop at the
        // render-list filter (D3 item 6), and the visible copy derives at
        // render from the raw `toolUseResult` — raw wire content stays.
        let all_rows = renderable_messages_from_entries(&entries);
        let messages = crate::components::messages_list::filter_non_rendering_messages(
            all_rows.clone(),
            false,
        );
        assert_eq!(messages.len(), 2);
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .collect::<Vec<_>>();

        // Rows carry no tool_name — the EnterPlanMode/ExitPlanMode
        // display variants themselves prove which tool the lookup resolved.
        assert_eq!(results[0].content, "Entered plan mode");
        assert_eq!(
            rendered_tool_result_text(&all_rows, results[0]),
            "⏺ Entered plan mode\nClaude is now exploring and designing an implementation approach."
        );
        assert_eq!(results[1].content, "User has approved your plan");
        // CC ExitPlanModeTool/UI.tsx:58-64 leads with the mode-colored
        // BLACK_CIRCLE, exactly like the EnterPlanMode line above.
        assert!(
            rendered_tool_result_text(&all_rows, results[1])
                .starts_with("⏺ User approved Claude's plan")
        );
        assert!(rendered_tool_result_text(&all_rows, results[1]).contains("Plan saved to:"));
    }

    #[test]
    fn enter_exit_plan_mode_rejected_paths_use_official_visible_copy() {
        let rejected_plan = format!("{PLAN_REJECTION_PREFIX}## Plan\nRevise it");
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "EnterPlanMode", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": REJECT_MESSAGE}]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_2", "name": "ExitPlanMode", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": rejected_plan, "is_error": true}]}
            }),
        ];

        // Hidden plan-mode tool_use rows drop at the render-list filter (D3
        // item 6).
        let messages = crate::components::messages_list::filter_non_rendering_messages(
            renderable_messages_from_entries(&entries),
            false,
        );
        assert_eq!(messages.len(), 2);
        let results = messages
            .iter()
            .filter_map(tool_result_of)
            .map(|result| (result.derived_status(), &result.content))
            .collect::<Vec<_>>();

        assert_eq!(results[0].0, ToolResultStatus::Rejected);
        // The reject sentinel stays in `content` verbatim; the declined
        // copy derives at render time from the tool name
        // (`UserToolRejectMessage`).
        assert_eq!(results[0].1, REJECT_MESSAGE);
        assert_eq!(results[1].0, ToolResultStatus::Error);
        assert!(results[1].1.starts_with(PLAN_REJECTION_PREFIX));
        let rejected_lines =
            crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result(
                "ExitPlanMode",
                ToolResultStatus::Error,
                results[1].1,
                None,
                None,
                &[],
                Default::default(),
            );
        assert_eq!(rejected_lines[0].text, "User rejected Claude's plan:");
        assert_eq!(rejected_lines[1].text, "## Plan");
        assert_eq!(rejected_lines[2].text, "Revise it");
    }

    #[test]
    fn task_output_local_bash_uses_task_output_text() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "TaskOutput", "input": {"task_id": "abc", "block": true}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "<output>ignored xml</output>", "is_error": false}]},
                "toolUseResult": {"retrieval_status": "success", "task": {"task_id": "abc", "task_type": "local_bash", "status": "completed", "description": "build", "output": "build ok", "exitCode": 0}}
            }),
        ];

        let rows = renderable_messages_from_entries(&entries);
        let result = rows.iter().find_map(tool_result_of).unwrap();
        // Raw wire content stays (D3 item 6); the task output text derives at
        // render from the raw `toolUseResult`.
        assert_eq!(result.content, "<output>ignored xml</output>");
        assert_eq!(rendered_tool_result_text(&rows, result), "build ok");
        // No TaskOutput display shape — the raw `toolUseResult` rides the row.
    }

    #[test]
    fn orphan_tool_result_is_suppressed_like_official_lookup_miss() {
        let entries = vec![json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "missing", "content": "orphan"}]}
        })];

        // CC keeps the message whole (conversationRecovery.ts:154); the null
        // happens at render — `UserToolResultMessage.tsx:43-46` returns null
        // when the tool_use lookup misses. The row therefore exists and
        // renders nothing (empty tool_name = lookup miss).
        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 1);
        let result = tool_result_of(&messages[0]).expect("orphan tool_result row");
        use iocraft::prelude::ElementExt as _;
        let canvas = iocraft::prelude::element! {
            iocraft::prelude::ContextProvider(
                value: iocraft::prelude::Context::owned(*crate::utils::theme::current()),
            ) {
                crate::components::messages::user_tool_result_message::UserToolResultMessage(
                    tool_name: String::new(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert_eq!(canvas.trim(), "");
    }

    #[test]
    fn task_status_attachment_maps_for_teammate_shutdown_collapse() {
        let entries = vec![json!({
            "type": "attachment",
            "attachment": {
                "type": "task_status",
                "taskId": "task-1",
                "taskType": "in_process_teammate",
                "status": "completed",
                "description": "reviewer"
            }
        })];

        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0].kind,
            RenderableMessageKind::Attachment(Attachment::TaskStatus {
                task_id,
                task_type,
                status,
                description,
                ..
            }) if task_id == "task-1" && task_type == "in_process_teammate" && status == "completed" && description == "reviewer"
        ));
    }

    #[test]
    fn relevant_memories_attachment_maps_official_memory_array() {
        let entries = vec![json!({
            "type": "attachment",
            "attachment": {
                "type": "relevant_memories",
                "memories": [
                    {"path": "/tmp/alpha.md", "content": "alpha", "mtimeMs": 10},
                    {"path": "/tmp/beta.md", "content": "beta", "mtimeMs": 20}
                ]
            }
        })];

        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0].kind,
            RenderableMessageKind::Attachment(Attachment::RelevantMemories { memories })
                if memories.len() == 2
                    && memories[0].path == "/tmp/alpha.md"
                    && memories[0].content == "alpha"
                    && memories[0].mtime_ms == Some(10)
        ));
    }

    #[test]
    fn dynamic_skill_attachment_recovers_local_transcript_display_fields() {
        let entries = vec![json!({
            "type": "attachment",
            "attachment": {
                "type": "dynamic_skill",
                "skillDir": "/tmp/project/.claude/skills",
                "skillNames": ["alpha", "beta"],
                "displayPath": ".claude/skills"
            }
        })];

        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0].kind,
            RenderableMessageKind::Attachment(Attachment::DynamicSkill {
                skill_dir,
                skill_names,
                display_path,
            }) if skill_dir == "/tmp/project/.claude/skills"
                && skill_names == &["alpha".to_string(), "beta".to_string()]
                && display_path == ".claude/skills"
        ));
    }

    #[test]
    fn stop_hook_summary_maps_official_fields_for_pipeline_collapse() {
        let entries = vec![json!({
            "type": "system",
            "subtype": "stop_hook_summary",
            "hookCount": 2,
            "hookLabel": "PreToolUse",
            "hookInfos": [{"command": "cargo check", "durationMs": 1500}],
            "hookErrors": ["warning"],
            "preventedContinuation": true,
            "hasOutput": true,
            "totalDurationMs": 1500
        })];

        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0].kind,
            RenderableMessageKind::System(crate::types::message::SystemMessage::StopHookSummary {
                hook_label: Some(label),
                hook_count: 2,
                hook_infos,
                hook_errors,
                prevented_continuation: true,
                has_output: true,
                total_duration_ms: Some(1500),
                ..
            }) if label == "PreToolUse" && hook_infos.len() == 1 && hook_errors == &vec!["warning".to_string()]
        ));
    }

    #[test]
    fn fetch_cc_command_metadata_matches_official_visibility() {
        let entries = vec![
            json!({
                "type": "system",
                "subtype": "turn_duration",
                "durationMs": 1234,
                "isMeta": false
            }),
            json!({
                "type": "user",
                "message": {"content": "<command-message>fetch-cc</command-message>\n<command-name>/fetch-cc</command-name>"}
            }),
            json!({
                "type": "user",
                "isMeta": true,
                "message": {"content": [{"type": "text", "text": "hidden model-facing command expansion"}]}
            }),
            // Wire shapes are the official ones (attachments.ts:686-690 and
            // :531-536) — a payload missing required fields would ride
            // `Attachment::Unknown` and get dropped for the wrong reason.
            json!({
                "type": "attachment",
                "attachment": {
                    "type": "deferred_tools_delta",
                    "addedNames": ["ToolSearch"],
                    "addedLines": ["- ToolSearch: search deferred tools"],
                    "removedNames": []
                }
            }),
            json!({
                "type": "attachment",
                "attachment": {
                    "type": "skill_listing",
                    "content": "- kitty-tui-test: terminal UI smoke",
                    "skillCount": 1,
                    "isInitial": false
                }
            }),
            json!({
                "type": "attachment",
                "attachment": {
                    "type": "skill_listing",
                    "content": "- a\n- b",
                    "skillCount": 2,
                    "isInitial": true
                }
            }),
        ];

        // D3 item 6: the parser keeps all six rows; visibility is decided at
        // render — the isMeta expansion drops at `should_show_user_message`
        // (Messages.tsx:597) and `deferred_tools_delta` at the null-rendering
        // list (nullRenderingAttachments.ts), both in the render-list filter.
        // The isInitial skill listing keeps its row and null-renders at the
        // component (CC AttachmentMessage.tsx:281-283).
        let messages = crate::components::messages_list::filter_non_rendering_messages(
            renderable_messages_from_entries(&entries),
            false,
        );
        assert_eq!(messages.len(), 4);
        assert!(matches!(
            &messages[0].kind,
            RenderableMessageKind::System(crate::types::message::SystemMessage::TurnDuration {
                duration_ms: 1234,
                ..
            })
        ));
        assert!(matches!(
            &messages[1].kind,
            // The command breadcrumb recovers as a plain Text block; the
            // renderer's xml-tag dispatch (not the type layer) classifies it.
            RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::Text(text))
                    if text.contains("<command-message>fetch-cc</command-message>")
            )
        ));
        assert!(matches!(
            &messages[2].kind,
            RenderableMessageKind::Attachment(Attachment::SkillListing {
                skill_count: 1,
                is_initial: false,
                ..
            })
        ));
        assert!(matches!(
            &messages[3].kind,
            RenderableMessageKind::Attachment(Attachment::SkillListing {
                skill_count: 2,
                is_initial: true,
                ..
            })
        ));
    }

    #[test]
    fn conversation_recovery_tool_result_display_keeps_bash_command_and_streams() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "cargo test"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "fallback", "is_error": false}]},
                // CC wire shape: stdout/stderr/interrupted are required.
                "toolUseResult": {"stdout": "ok", "stderr": "", "interrupted": false, "noOutputExpected": false}
            }),
        ];

        // The row carries no display shape; the raw `toolUseResult`
        // feeds the by-tool-name renderer.
        let rows = renderable_messages_from_entries(&entries);
        let result = rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");
        assert_eq!(rendered_tool_result_text(&rows, result), "ok");
    }

    #[test]
    fn bash_display_splits_cwd_reset_warning_and_sandbox_violations_like_official() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "pwd"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "fallback", "is_error": false}]},
                "toolUseResult": {"stdout": "", "stderr": "first error\n<sandbox_violations>hidden</sandbox_violations>\nShell cwd was reset to /tmp/project", "interrupted": false, "noOutputExpected": false}
            }),
        ];

        // The sandbox-violation strip and cwd-reset split happen inside
        // the by-tool-name renderer (BashToolResultMessage), fed by the raw.
        let rows = renderable_messages_from_entries(&entries);
        let result = rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");
        assert_eq!(
            rendered_tool_result_text(&rows, result),
            "first error\nShell cwd was reset to /tmp/project"
        );
    }

    #[test]
    fn bash_image_and_background_results_match_official_empty_rows() {
        let image_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_image", "name": "Bash", "input": {"command": "cat image.png"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_image", "content": "", "is_error": false}]},
                "toolUseResult": {"stdout": "raw image bytes", "stderr": "", "interrupted": false, "isImage": true}
            }),
        ];

        let image_rows = renderable_messages_from_entries(&image_entries);
        let image_result = image_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected image tool result");
        // Raw wire content stays empty (D3 item 6); the image sentinel is a
        // render-side branch fed by the raw (BashToolResultMessage.tsx:88-94).
        assert_eq!(image_result.content, "");
        assert!(
            rendered_tool_result_text(&image_rows, image_result)
                .starts_with("[Image data detected and sent to Claude]")
        );

        let background_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_bg", "name": "Bash", "input": {"command": "npm run dev"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_bg", "content": "", "is_error": false}]},
                "toolUseResult": {"stdout": "", "stderr": "", "interrupted": false, "backgroundTaskId": "task-1"}
            }),
        ];

        // Raw wire content stays empty (D3 item 6); the background line is a
        // render-side branch (CC BashToolResultMessage.tsx:107-115, shown only
        // when stdout/stderr are both empty).
        let background_rows = renderable_messages_from_entries(&background_entries);
        let background_result = background_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected background tool result");
        assert_eq!(background_result.content, "");
        assert_eq!(
            rendered_tool_result_text(&background_rows, background_result),
            "Running in the background (↓ to manage)"
        );

        let done_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_done", "name": "Bash", "input": {"command": "mkdir -p /tmp/mock"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_done", "content": "", "is_error": false}]},
                "toolUseResult": {"stdout": "", "stderr": "", "interrupted": false, "noOutputExpected": true}
            }),
        ];
        let done_rows = renderable_messages_from_entries(&done_entries);
        let done_result = done_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected no-output tool result");
        assert_eq!(done_result.content, "");
        assert!(rendered_tool_result_text(&done_rows, done_result).starts_with("Done"));

        let interpreted_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_interp", "name": "Bash", "input": {"command": "grep missing file"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_interp", "content": "", "is_error": false}]},
                "toolUseResult": {"stdout": "", "stderr": "", "interrupted": false, "returnCodeInterpretation": "No matches found"}
            }),
        ];
        let interpreted_rows = renderable_messages_from_entries(&interpreted_entries);
        let interpreted_result = interpreted_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected interpreted tool result");
        assert_eq!(interpreted_result.content, "");
        assert!(
            rendered_tool_result_text(&interpreted_rows, interpreted_result)
                .starts_with("No matches found")
        );
    }

    #[test]
    fn notebook_edit_tool_use_and_result_match_official_ui_subset() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_nb", "name": "NotebookEdit", "input": {
                    "notebook_path": "/repo/notebooks/demo.ipynb",
                    "cell_id": "cell-1",
                    "new_source": "print('hello')",
                    "cell_type": "code"
                }}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_nb", "content": "Updated cell cell-1 with print('hello')", "is_error": false}]},
                "toolUseResult": {
                    "new_source": "print('hello')",
                    "cell_id": "cell-1",
                    "cell_type": "code",
                    "language": "python",
                    "edit_mode": "replace",
                    "error": "",
                    "notebook_path": "/repo/notebooks/demo.ipynb",
                    "original_file": "{\"source\":\"old\"}",
                    "updated_file": "{\"source\":\"print('hello')\"}"
                }
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            recovered_tool_rows(std::slice::from_ref(&messages[0]))
                .into_iter()
                .next(),
            Some((
                "Edit Notebook".to_string(),
                "/repo/notebooks/demo.ipynb@cell-1".to_string()
            ))
        );
        let result = tool_result_of(&messages[1]).expect("expected NotebookEdit result");
        // No NotebookEdit display shape — the raw rides the row and the
        // by-tool-name renderer draws the updated-cell body from it.
        let raw = result.tool_use_result.as_ref().expect("raw rides the row");
        assert_eq!(raw["cell_id"], json!("cell-1"));
        assert_eq!(raw["new_source"], json!("print('hello')"));
        let rendered = crate::tools::notebook_edit_tool::ui::render_tool_result_lines(
            Some(raw),
            crate::types::message::ToolResultStatus::Success,
            "",
            &crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
        );
        assert_eq!(rendered[0].text, "Updated cell cell-1:");
    }

    #[test]
    fn notebook_edit_rejected_result_matches_official_ui_subset() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_nb_reject", "name": "NotebookEdit", "input": {
                    "notebook_path": "/repo/notebooks/demo.ipynb",
                    "cell_id": "cell-2",
                    "new_source": "print('nope')",
                    "cell_type": "code",
                    "edit_mode": "replace"
                }}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_nb_reject", "content": crate::utils::messages::REJECT_MESSAGE, "is_error": true}]}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 2);
        let result = tool_result_of(&messages[1]).expect("expected NotebookEdit rejection result");
        assert_eq!(result.derived_status(), ToolResultStatus::Rejected);
        // The reject sentinel stays in `content` verbatim; the
        // "User rejected replace cell in …" copy is render-derived.
        assert_eq!(result.content, crate::utils::messages::REJECT_MESSAGE);
        // The rejected leaf renders from the tool INPUT.
    }

    #[test]
    fn mcp_auth_pseudo_tool_retains_registry_identity_and_hides_success_result() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_auth", "name": "mcp__linear__authenticate", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_auth", "content": "Ask the user to open this URL", "is_error": false}]},
                "toolUseResult": {"status": "auth_url", "message": "Ask the user to open this URL"}
            }),
        ];

        // The auth-url success result is retained as a nonvisual row the
        // render-list filter hides; the tool_use row is unchanged.
        let messages = renderable_messages_from_entries(&entries);
        assert_eq!(messages.len(), 2);
        // Keep the generated registry identity in the recovered carrier. The
        // retained AssistantToolUseMessage resolves the live pseudo-tool from
        // AppStore and invokes its source-owned presentation closures.
        assert_eq!(
            recovered_tool_rows(std::slice::from_ref(&messages[0]))
                .into_iter()
                .next(),
            Some(("mcp__linear__authenticate".to_string(), String::new()))
        );
        let rendered =
            crate::components::messages_list::filter_non_rendering_messages(messages, false);
        assert_eq!(rendered.len(), 1);
        assert_eq!(recovered_tool_rows(&rendered).len(), 1);
    }

    #[test]
    fn powershell_tool_use_and_result_match_official_ui_subset() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_ps", "name": "PowerShell", "input": {"command": "one\ntwo\nthree"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_ps", "content": "", "is_error": false}]},
                "toolUseResult": {"stdout": "", "stderr": "", "interrupted": false, "isImage": false, "returnCodeInterpretation": "No matches found"}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let tool_use_description = recovered_tool_rows(&messages)
            .into_iter()
            .find_map(|(tool_name, description)| (tool_name == "PowerShell").then_some(description))
            .expect("expected PowerShell tool use");
        assert_eq!(tool_use_description, "one\ntwo…");

        let result = messages
            .iter()
            .find_map(tool_result_of)
            .expect("expected PowerShell tool result");
        // Raw wire content stays empty (D3 item 6); the interpretation line
        // derives at render from the raw's returnCodeInterpretation.
        assert_eq!(result.content, "");
        assert!(rendered_tool_result_text(&messages, result).starts_with("No matches found"));
    }

    #[test]
    fn bash_success_prefers_structured_tool_use_result() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "echo hi"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "fallback", "is_error": false}]},
                "toolUseResult": {"stdout": "hi", "stderr": "", "interrupted": false, "isImage": false, "noOutputExpected": false}
            }),
        ];

        let rows = renderable_messages_from_entries(&entries);
        let result = rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");

        // Raw wire content stays (D3 item 6); the structured stdout renders
        // from the raw `toolUseResult` and wins over the fallback text.
        assert_eq!(result.content, "fallback");
        assert!(rendered_tool_result_text(&rows, result).starts_with("hi"));
    }

    #[test]
    fn read_success_uses_official_summary_from_structured_result() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "a.txt"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "full file content", "is_error": false}]},
                "toolUseResult": {"type": "text", "file": {"filePath": "a.txt", "content": "full file content", "numLines": 2, "startLine": 1, "totalLines": 2}}
            }),
        ];

        let messages = renderable_messages_from_entries(&entries);
        let result = messages
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");

        // Raw wire content stays (D3 item 6); the read summary derives at
        // render from the raw `toolUseResult`.
        assert_eq!(result.content, "full file content");
        assert_eq!(rendered_tool_result_text(&messages, result), "Read 2 lines");
        // No Read display shape — the raw rides the row and the summary
        // above renders from it by tool name.
        assert!(result.tool_use_result.is_some());
    }

    #[test]
    fn read_tool_use_pages_summary_matches_official_renderer_contract() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "id": "toolu_pages",
                    "name": "Read",
                    "input": {"file_path": "manual.pdf", "pages": "2-4"}
                }]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{
                    "type":"tool_result","tool_use_id":"toolu_pages","content":"ok"
                }]}
            }),
        ];

        let description = recovered_tool_rows(&renderable_messages_from_entries(&entries))
            .into_iter()
            .map(|(_, description)| description)
            .next()
            .expect("expected Read tool-use");
        assert_eq!(description, "manual.pdf · pages 2-4");
    }

    #[test]
    fn read_agent_output_recovery_keeps_canonical_identity_for_read_ui() {
        let task_output = crate::utils::task::disk_output::get_task_output_dir()
            .join("task-abc.output")
            .display()
            .to_string();
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "id": "toolu_agent_output",
                    "name": "Read",
                    "input": {"file_path": task_output}
                }]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{
                    "type":"tool_result","tool_use_id":"toolu_agent_output","content":"ok"
                }]}
            }),
        ];

        let (tool_name, description) =
            recovered_tool_rows(&renderable_messages_from_entries(&entries))
                .into_iter()
                .next()
                .expect("expected Read tool-use");
        assert_eq!(tool_name, "Read");
        assert!(description.is_empty());
    }

    #[test]
    fn malformed_read_data_is_retained_but_has_no_cold_render_projection() {
        let malformed_tool_use_result = json!({
            "type": "text",
            "file": {"filePath": "src/main.rs", "content": "raw"}
        });
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "id": "toolu_bad_read",
                    "name": "Read",
                    "input": {"path": "src/main.rs"}
                }]}
            }),
            json!({
                "type": "user",
                "toolUseResult": malformed_tool_use_result.clone(),
                "message": {"content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_bad_read",
                    "content": "raw model content",
                    "is_error": false
                }]}
            }),
        ];

        let recovered = renderable_messages_from_entries(&entries);
        // A2.3: the block is retained like CC retains restored messages; the
        // "no cold render projection" is now expressed by the renderer — the
        // malformed input yields no summary rather than deleting the row.
        let rows = recovered_tool_rows(&recovered);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "Read");
        assert!(rows[0].1.is_empty());
        // The malformed raw survives on the row and the render-time
        // gate hides it; rendering from the raw yields nothing, exactly as
        // CC's safeParse-failure branch.
        let result = recovered
            .iter()
            .find_map(tool_result_of)
            .expect("malformed Read result row is retained");
        assert_eq!(result.content, "raw model content");
        assert_eq!(
            result.tool_use_result.as_ref(),
            Some(&malformed_tool_use_result)
        );
        assert!(
            crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result(
                "Read",
                ToolResultStatus::Success,
                &result.content,
                result.tool_use_result.as_ref(),
                None,
                &[],
                crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
            )
            .is_empty()
        );
    }

    #[test]
    fn read_non_text_results_preserve_official_display_fields() {
        let image_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_image", "name": "Read", "input": {"file_path": "diagram.png"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_image", "content": "image", "is_error": false}]},
                "toolUseResult": {"type": "image", "file": {"originalSize": 2048, "base64": "", "type": "image/png"}}
            }),
        ];
        let image_rows = renderable_messages_from_entries(&image_entries);
        let image_result = image_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected image read result");
        // Raw wire content stays (D3 item 6); the read summary derives at
        // render from the raw `toolUseResult`.
        assert_eq!(image_result.content, "image");
        assert_eq!(
            rendered_tool_result_text(&image_rows, image_result),
            "Read image (2KB)"
        );

        let parts_entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_parts", "name": "Read", "input": {"file_path": "paper.pdf"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_parts", "content": "pages", "is_error": false}]},
                "toolUseResult": {"type": "parts", "file": {"filePath": "paper.pdf", "originalSize": 4096, "count": 3, "outputDir": "/tmp/pages"}}
            }),
        ];
        let parts_rows = renderable_messages_from_entries(&parts_entries);
        let parts_result = parts_rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected parts read result");
        assert_eq!(parts_result.content, "pages");
        assert_eq!(
            rendered_tool_result_text(&parts_rows, parts_result),
            "Read 3 pages (4KB)"
        );
    }

    #[test]
    fn grep_success_uses_result_summary() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Grep", "input": {"pattern": "needle"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "a.txt\nb.txt", "is_error": false}]},
                "toolUseResult": {"mode": "files_with_matches", "numFiles": 2, "filenames": ["a.txt", "b.txt"]}
            }),
        ];

        let rows = renderable_messages_from_entries(&entries);
        let result = rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");

        // Raw wire content stays (D3 item 6); the search summary derives at
        // render from the raw `toolUseResult`.
        assert_eq!(result.content, "a.txt\nb.txt");
        // CC GrepTool/UI.tsx:65-71 appends `CtrlOToExpand` whenever count > 0.
        assert_eq!(
            rendered_tool_result_text(&rows, result),
            "Found 2 files (ctrl+o to expand)"
        );
    }

    #[test]
    fn grep_display_preserves_official_output_mode_counts_and_detail() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Grep", "input": {"pattern": "needle", "output_mode": "count"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "a.txt:2", "is_error": false}]},
                "toolUseResult": {"mode": "count", "numMatches": 3, "numFiles": 1, "filenames": ["a.txt"], "content": "a.txt:2"}
            }),
        ];

        let result = renderable_messages_from_entries(&entries)
            .into_iter()
            .find_map(into_tool_result)
            .expect("expected tool result");

        // No Grep display shape — the raw rides the row and the
        // by-tool-name renderer derives the count summary from it.
        let raw = result
            .tool_use_result
            .expect("raw toolUseResult rides the row");
        assert_eq!(raw["mode"], serde_json::json!("count"));
        assert_eq!(raw["numMatches"], serde_json::json!(3));
        assert_eq!(raw["numFiles"], serde_json::json!(1));
        let rendered = crate::tools::grep_tool::ui::render_tool_result_message(
            Some(&raw),
            crate::types::message::ToolResultStatus::Success,
            "",
            &crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
        );
        assert!(
            rendered[0]
                .text
                .starts_with("Found 3 matches across 1 file")
        );
    }

    #[test]
    fn edit_success_uses_diff_summary() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Edit", "input": {"file_path": "a.txt"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "updated", "is_error": false}]},
                // CC wire shape: hunkSchema requires all five keys.
                "toolUseResult": {
                    "filePath": "a.txt",
                    "oldString": "old",
                    "newString": "new",
                    "originalFile": "prefix\nold\n",
                    "userModified": true,
                    "replaceAll": false,
                    "structuredPatch": [{
                        "oldStart": 10, "oldLines": 2, "newStart": 10, "newLines": 2,
                        "lines": ["-old", "+new", " context"]
                    }],
                    "gitDiff": {
                        "filename": "a.txt",
                        "status": "modified",
                        "additions": 1,
                        "deletions": 1,
                        "changes": 2,
                        "patch": "@@ -1 +1 @@\n-old\n+new\n"
                    }
                }
            }),
        ];

        let rows = renderable_messages_from_entries(&entries);
        let result = rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");

        // Raw wire content stays (D3 item 6); the diff summary derives at
        // render from the raw `toolUseResult`.
        assert_eq!(result.content, "updated");
        let rendered = rendered_tool_result_text(&rows, result);
        assert!(rendered.starts_with("Added 1 line, removed 1 line"));
        assert!(rendered.contains(" 10 -old"));
        assert!(rendered.contains(" 10 +new"));
        assert!(rendered.contains(" 11  context"));
    }

    #[test]
    fn write_update_preserves_structured_diff_lines_with_hunk_separator() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Write", "input": {"file_path": "a.txt"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "updated", "is_error": false}]},
                // CC wire shape: type/filePath/content/structuredPatch/
                // originalFile are all required; hunks carry all five keys.
                "toolUseResult": {
                    "type": "update",
                    "filePath": "a.txt",
                    "content": "two\nthree",
                    "structuredPatch": [
                        {"oldStart": 10, "oldLines": 2, "newStart": 10, "newLines": 2,
                         "lines": [" old", "-one", "+two"]},
                        {"oldStart": 20, "oldLines": 1, "newStart": 20, "newLines": 2,
                         "lines": [" context", "+three"]}
                    ],
                    "originalFile": "old\none\ncontext"
                }
            }),
        ];

        let rows = renderable_messages_from_entries(&entries);
        let result = rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");

        // Raw wire content stays (D3 item 6); the diff summary derives at
        // render from the raw `toolUseResult`.
        assert_eq!(result.content, "updated");
        let rendered = rendered_tool_result_text(&rows, result);
        assert!(rendered.starts_with("Added 2 lines, removed 1 line"));
        assert!(rendered.contains(" 11 -one"));
        assert!(rendered.contains(" 21 +three"));
    }

    #[test]
    fn write_create_success_uses_created_summary() {
        // CC FileWriteTool/UI.tsx:59 resolves the relative wire filePath using
        // process cwd, then displays it relative to session cwd. Pin both for
        // this fixture's intentionally bare filename expectation.
        struct RestoreCwd(std::path::PathBuf);
        impl Drop for RestoreCwd {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(self.0.clone());
            }
        }
        let _cwd = RestoreCwd(crate::bootstrap::state::get_original_cwd());
        crate::bootstrap::state::set_original_cwd(std::env::current_dir().unwrap());
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Write", "input": {"file_path": "a.txt"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "created", "is_error": false}]},
                // CC wire shape: structuredPatch/originalFile are required
                // (empty array / null for a create).
                "toolUseResult": {"type": "create", "filePath": "a.txt", "content": "one\ntwo\n", "structuredPatch": [], "originalFile": null}
            }),
        ];

        let rows = renderable_messages_from_entries(&entries);
        let result = rows
            .iter()
            .find_map(tool_result_of)
            .expect("expected tool result");

        // Raw wire content stays (D3 item 6); the created summary derives at
        // render from the raw `toolUseResult`.
        assert_eq!(result.content, "created");
        assert!(rendered_tool_result_text(&rows, result).starts_with("Wrote 2 lines to a.txt"));
    }

    #[test]
    fn reject_with_reason_remains_error_content_for_error_renderer() {
        let content = format!("{REJECT_MESSAGE_WITH_REASON_PREFIX}use a safer command");
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": content, "is_error": true}]}
            }),
        ];

        let (status, rendered_content) = renderable_messages_from_entries(&entries)
            .into_iter()
            .find_map(into_tool_result)
            .map(|result| (result.derived_status(), result.content))
            .expect("expected tool result");

        assert_eq!(status, ToolResultStatus::Error);
        assert!(rendered_content.starts_with(REJECT_MESSAGE_WITH_REASON_PREFIX));
    }

    #[test]
    fn edit_reject_uses_originating_input_for_tool_specific_summary() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Edit", "input": {"file_path": "a.txt", "old_string": "old", "new_string": "new"}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": REJECT_MESSAGE}]}
            }),
        ];

        let (status, content) = renderable_messages_from_entries(&entries)
            .into_iter()
            .find_map(into_tool_result)
            .map(|result| (result.derived_status(), result.content))
            .expect("expected tool result");

        assert_eq!(status, ToolResultStatus::Rejected);
        // The reject sentinel stays in `content` verbatim; the
        // tool-specific "Rejected update to a.txt" copy is render-derived
        // from the originating input via the lookups.
        assert_eq!(content, REJECT_MESSAGE);
    }

    #[test]
    fn tool_result_status_detects_canceled_and_rejected_control_messages() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Edit", "input": {}}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": CANCEL_MESSAGE}]}
            }),
            json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": REJECT_MESSAGE}]}
            }),
        ];

        let statuses = renderable_messages_from_entries(&entries)
            .into_iter()
            .filter_map(into_tool_result)
            .map(|result| (result.derived_status(), result.content))
            .collect::<Vec<_>>();

        // Cancel/reject sentinels survive in `content` verbatim — the
        // render-time discrimination (CC UserToolResultMessage.tsx:43-90)
        // needs them; `derived_status()` grades them the same way.
        assert_eq!(
            statuses,
            vec![
                (ToolResultStatus::Canceled, CANCEL_MESSAGE.to_string()),
                (ToolResultStatus::Rejected, REJECT_MESSAGE.to_string()),
            ]
        );
    }
}
