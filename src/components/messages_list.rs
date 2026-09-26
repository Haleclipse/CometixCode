//! Maps to: CC `components/Messages.tsx`.
//! `messages_list.rs` is the documented L1 filename required because Rust's
//! `components/messages/` module directory already occupies `messages`.
//! The exported component keeps the official `Messages` name and owns only the
//! renderable message list; concrete variants dispatch through `MessageRow` →
//! `Message` → `components/messages/*`.

use super::design_system::divider::Divider;
use super::logo::Logo;
use super::message_row::{MessageRow, message_row_static_memo_key};
use super::messages::StreamingAssistantTextMessage;
use super::status_notices::StatusNotices;
use crate::screens::repl::Screen;
use crate::utils::classifier_approvals::{ClassifierApprovalsState, ClassifierChecking};
use crate::utils::collapse_read_search::collapse_read_search_groups;
use crate::utils::debug::component_profile_enabled;
use crate::utils::status_notice_definitions::{StatusNoticeContext, get_active_notices};
use iocraft::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::types::message::{
    Attachment, GroupedToolUseMessage, ReadResultKind, RenderableMessage, RenderableMessageKind,
    SearchResultMode, StructuredDiffHunk, SystemMessage, SystemMessageLevel, ToolResultStatus,
    ToolUseProgressMessage, ToolUseStatus,
};
#[cfg(test)]
use crate::types::message::{
    CollapsedReadSearchEntry, CollapsedReadSearchGroup, RelevantMemory, StopHookInfo,
};

/// Rust-side subset of official `buildMessageLookups(...)` output.
/// It precomputes the relationships already represented in `RenderableMessage`:
/// tool_use ⇄ tool_result, progress snapshots by tool-use id, resolved/errored
/// tool IDs, and resolved hook counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageLookups {
    pub sibling_tool_use_ids: HashMap<String, BTreeSet<String>>,
    pub progress_messages_by_tool_use_id: HashMap<String, Vec<ToolUseProgressMessage>>,
    pub tool_use_by_tool_use_id: HashMap<String, RenderableMessage>,
    pub tool_result_by_tool_use_id: HashMap<String, RenderableMessage>,
    pub resolved_hook_counts: HashMap<String, HashMap<String, usize>>,
    pub normalized_message_count: usize,
    pub resolved_tool_use_ids: BTreeSet<String>,
    pub errored_tool_use_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Maps to: CC `Messages.tsx#SliceAnchor`.
pub(crate) struct SliceAnchor {
    uuid: String,
    idx: usize,
}

/// iocraft L1 memo payload for CC `MessagesImpl`'s expensive `useMemo` block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MessagesPreparedPipeline {
    pub collapsed: Arc<Vec<RenderableMessage>>,
    pub lookups: Arc<MessageLookups>,
    pub has_truncated_messages: bool,
    pub hidden_message_count: usize,
}

/// Borrow-preserving Rust view over CC `computeSliceStart(...)`'s suffix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessagesRenderSlice {
    pub messages: Arc<Vec<RenderableMessage>>,
    pub start: usize,
    pub end: usize,
}

impl MessagesRenderSlice {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &RenderableMessage> {
        self.messages[self.start..self.end].iter()
    }
}

/// Maps to: CC `Messages.tsx#MAX_MESSAGES_WITHOUT_VIRTUALIZATION`.
const MAX_MESSAGES_WITHOUT_VIRTUALIZATION: usize = 200;
/// Maps to: CC `MAX_MESSAGES_TO_SHOW_IN_TRANSCRIPT_MODE`.
const MAX_MESSAGES_TO_SHOW_IN_TRANSCRIPT_MODE: usize = 30;
const MESSAGE_CAP_STEP: usize = 50;

/// True for the `compact_boundary` system row CC slices history at.
use crate::utils::messages::is_compact_boundary_row;

/// Maps to: CC `Messages.tsx:579-584`.
///
/// ```text
/// verbose || isFullscreenEnvEnabled()
///   ? normalizedMessages
///   : getMessagesAfterCompactBoundary(normalizedMessages, { includeSnipped: true })
/// ```
///
/// The history array keeps every pre-compact row — CC appends the post-compact
/// block rather than replacing the array — and hiding is a render-time slice.
/// Main-screen mode slices because those rows are still reachable in the
/// terminal's native scrollback; fullscreen has no native scrollback (the alt
/// buffer would simply lose them) and verbose is an explicit request to see
/// everything, so both skip the slice. `includeSnipped` is implicit here: the
/// snip projection is a no-op against CC 2.1.88's generated stub.
fn compact_aware_messages(
    normalized: Vec<RenderableMessage>,
    verbose: bool,
) -> Vec<RenderableMessage> {
    if verbose || crate::utils::fullscreen::is_fullscreen_env_enabled() {
        return normalized;
    }
    match normalized.iter().rposition(is_compact_boundary_row) {
        // The boundary itself is included, matching upstream.
        Some(index) => normalized[index..].to_vec(),
        None => normalized,
    }
}

/// Maps to official `normalizeMessages(messages).filter(isNotEmptyMessage)`.
/// Current `RenderableMessage` values are already normalized at the query or
/// session-loading mapper seam, so this is intentionally identity-preserving.
pub(crate) fn normalize_messages(messages: &[RenderableMessage]) -> Vec<RenderableMessage> {
    messages.to_vec()
}

/// Maps to the null-rendering/user-message filters in `Messages.tsx`.
/// Filtering happens before grouping and the 200-message cap so model-only
/// attachments never consume visible scrollback budget.
pub(crate) fn filter_non_rendering_messages(
    messages: Vec<RenderableMessage>,
    is_transcript_mode: bool,
) -> Vec<RenderableMessage> {
    // The emit-UI gate resolves the tool by name exactly as CC's render layer
    // does (`UserToolResultMessage.tsx:43` via the lookups); rows carry no
    // tool name, so pair tool_use ids here before filtering.
    let tool_names: std::collections::HashMap<String, String> = messages
        .iter()
        .filter_map(|message| match &message.kind {
            RenderableMessageKind::Assistant { message } => match message.first_content_block() {
                Some(crate::types::message::AssistantContent::ToolUse(tool_use))
                    if !tool_use.id.0.is_empty() =>
                {
                    Some((tool_use.id.0.clone(), tool_use.name.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    messages
        .into_iter()
        .filter(|message| {
            // Maps to: CC `Messages.tsx:588-590` — progress messages feed the
            // lookups (built from the unfiltered normalized array) but never
            // render as rows themselves.
            !matches!(&message.kind, RenderableMessageKind::Progress { .. })
                && !crate::components::messages::null_rendering_attachments::is_null_rendering_attachment(
                    message,
                )
                // Maps to: CC `Messages.tsx:597`
                // `.filter(_ => shouldShowUserMessage(_, isTranscriptMode))`
                // (utils/messages.ts:4658-4677): the envelope-isMeta early-out
                // plus the isVisibleInTranscriptOnly transcript gate.
                && crate::utils::messages::should_show_user_message(message, is_transcript_mode)
                // The tool_result rows stay whole in history (CC appends
                // everything); a success row whose tool renders null (no
                // renderToolResultMessage, or a missing/schema-rejected raw)
                // is dropped from the render list here — CC's React-null
                // equivalent.
                && crate::components::messages::user_tool_result_message::transcript_tool_result_should_emit_ui(
                    message,
                    user_tool_result_id(message)
                        .and_then(|id| tool_names.get(id))
                        .map(String::as_str),
                )
                // Streamed-assistant convergence: whole per-block envelopes
                // enter history with nonvisual tool_use blocks intact (CC
                // appends every yielded assistant, REPL.tsx:3496, and
                // AssistantToolUseMessage returns null for tools with an
                // empty userFacingName). Dropping the row here keeps the
                // row-count/index space identical to the pre-convergence
                // seam gate (`assistant_content_to_source_event_kind` → None)
                // and to the recovery parser's parse-time filter.
                && !assistant_row_is_nonvisual_tool_use(message)
        })
        .collect()
}

/// True for an assistant row whose block is a tool_use that CC's
/// `AssistantToolUseMessage` null-renders (empty `userFacingName` /
/// `renderToolUseMessage` returning null).
fn assistant_row_is_nonvisual_tool_use(message: &RenderableMessage) -> bool {
    assistant_tool_use_block(message).is_some_and(|tool_use| {
        // CC nulls the whole row when `findToolByName` misses on the WIRE
        // name (`AssistantToolUseMessage.tsx:84-110`) — a retired tool from
        // an old transcript renders nothing. The row's block carries the wire
        // name; the component prop downstream holds the display projection,
        // so this is the only layer that can ask the registry.
        !crate::components::messages::user_tool_result_message::tool_name_resolves(
            tool_use.name.as_str(),
        ) || crate::components::messages::assistant_tool_use_message::assistant_tool_use_is_nonvisual(
            tool_use.name.as_str(),
            Some(&tool_use.input),
        )
    })
}

/// Maps to: CC `getToolUseID(msg)` — the id comes off the `tool_use` content
/// block. The row's block is the first non-identity block.
fn assistant_tool_use_block(
    message: &RenderableMessage,
) -> Option<&crate::types::message::ToolUseBlock> {
    match &message.kind {
        RenderableMessageKind::Assistant { message } => match message.first_content_block() {
            Some(crate::types::message::AssistantContent::ToolUse(tool_use)) => Some(tool_use),
            _ => None,
        },
        _ => None,
    }
}

fn assistant_tool_use_id(message: &RenderableMessage) -> Option<&str> {
    assistant_tool_use_block(message)
        .map(|tool_use| tool_use.id.0.as_str())
        .filter(|id| !id.is_empty())
}

/// One block per row (normalize-guaranteed): the row's block is the first
/// content block.
fn user_tool_result_block(
    message: &RenderableMessage,
) -> Option<&crate::types::message::ToolResult> {
    match &message.kind {
        RenderableMessageKind::User { message } => match message.first_content_block() {
            Some(crate::types::message::UserContent::ToolResult(tool_result)) => Some(tool_result),
            _ => None,
        },
        _ => None,
    }
}

fn user_tool_result_id(message: &RenderableMessage) -> Option<&str> {
    user_tool_result_block(message)
        .map(|tool_result| tool_result.tool_use_id.0.as_str())
        .filter(|id| !id.is_empty())
}

/// Maps to: CC grouping by the owning API message id — same-message tool uses
/// group together. The id lives on the `MessageIdentity` envelope block; rows
/// without one (the live SDK seam today) never group, as before.
fn assistant_tool_use_group_info(message: &RenderableMessage) -> Option<(&str, &str, &str)> {
    let tool_use = assistant_tool_use_block(message)?;
    if tool_use.id.0.is_empty() {
        return None;
    }
    let RenderableMessageKind::Assistant { message: inner, .. } = &message.kind else {
        return None;
    };
    let message_id = inner.content.iter().find_map(|block| match block {
        crate::types::message::AssistantContent::MessageIdentity(identity) => {
            identity.api_message_id.as_deref()
        }
        _ => None,
    })?;
    Some((message_id, tool_use.id.0.as_str(), tool_use.name.as_str()))
}

// The old `user_tool_result_status` helper is gone with the stored status:
// CC marks erroredToolUseIDs from `content.is_error` alone
// (utils/messages.ts:1243-1245); rejected/canceled results are NOT errored.

fn hook_attachment_parts(message: &RenderableMessage) -> Option<(&str, &str, &str)> {
    // Batch D2: the flattened `Hook` row kind is gone; the typed CC hook
    // family carries `hookName`/`toolUseID`/`hookEvent` directly.
    // `async_hook_response` has no `toolUseID` in CC and stays excluded, as it
    // was under the old `tool_use_id: Some(..)` guard.
    let (name, tool_use_id, event) = match &message.kind {
        RenderableMessageKind::Attachment(
            Attachment::HookBlockingError {
                hook_name,
                tool_use_id,
                hook_event,
                ..
            }
            | Attachment::HookNonBlockingError {
                hook_name,
                tool_use_id,
                hook_event,
                ..
            }
            | Attachment::HookErrorDuringExecution {
                hook_name,
                tool_use_id,
                hook_event,
                ..
            }
            | Attachment::HookStoppedContinuation {
                hook_name,
                tool_use_id,
                hook_event,
                ..
            }
            | Attachment::HookSystemMessage {
                hook_name,
                tool_use_id,
                hook_event,
                ..
            },
        ) => (
            hook_name.as_str(),
            tool_use_id.as_str(),
            hook_event.as_str(),
        ),
        RenderableMessageKind::Attachment(Attachment::HookPermissionDecision {
            tool_use_id,
            hook_event,
            ..
        }) => ("Hook", tool_use_id.as_str(), hook_event.as_str()),
        _ => return None,
    };
    (!tool_use_id.is_empty() && !event.is_empty()).then_some((tool_use_id, event, name))
}

fn hook_attachment_group(message: &RenderableMessage) -> Option<(&str, HookGroup)> {
    let (tool_use_id, event, _) = hook_attachment_parts(message)?;
    match event {
        "PreToolUse" => Some((tool_use_id, HookGroup::Pre)),
        "PostToolUse" => Some((tool_use_id, HookGroup::Post)),
        _ => None,
    }
}

fn is_api_error_message(message: &RenderableMessage) -> bool {
    matches!(
        message.kind,
        RenderableMessageKind::System(SystemMessage::ApiError { .. })
    )
}

#[derive(Clone, Copy)]
enum HookGroup {
    Pre,
    Post,
}

/// Maps to official `reorderMessagesInUI(...)`.
/// Tool results can be recorded after several sibling tool_use blocks. Official
/// Claude Code groups by `tool_use_id` and renders related rows as
/// `tool_use → pre-hooks → tool_result → post-hooks`, then suppresses stale API
/// retry rows except for the tail API error.
/// Maps to: CC `components/Messages.tsx:209-247` `dropTextInBriefTurns` —
/// when a turn called SendUserMessage, the model's plain text is
/// working-notes duplicating the SendUserMessage content, so the turn's
/// assistant text rows drop; tool calls and their results stay visible.
/// Turns without a Brief call keep their text ("if the model forgets, text
/// still shows — otherwise the user would see nothing").
///
/// The drop list is `[BRIEF_TOOL_NAME]` alone (`Messages.tsx:607-612`):
/// `SEND_USER_FILE_TOOL_NAME` is KAIROS-null in the external build, and CC
/// deliberately excludes it anyway since a file-only turn has no replacement
/// text. Name matching is exact — the legacy `Brief` alias is not in CC's
/// nameSet either.
pub(crate) fn drop_text_in_brief_turns(messages: Vec<RenderableMessage>) -> Vec<RenderableMessage> {
    use crate::types::message::{AssistantContent, UserContent};
    let brief_name = crate::tools::brief_tool::prompt::BRIEF_TOOL_NAME;
    // First pass: number turns at non-meta, non-tool_result user rows and tag
    // each assistant text row with its turn; record turns that called Brief.
    let mut turns_with_brief = std::collections::HashSet::new();
    let mut text_index_to_turn = vec![None::<usize>; messages.len()];
    let mut turn = 0usize;
    for (index, message) in messages.iter().enumerate() {
        match &message.kind {
            RenderableMessageKind::User { message } => {
                let block = message.first_content_block();
                let is_tool_result = matches!(block, Some(UserContent::ToolResult(_)));
                // CC reads the envelope `isMeta`; the Rust rows carry the
                // same fact as the Meta* content variants
                // (`should_show_user_message` reads it identically).
                let is_meta = matches!(
                    block,
                    Some(
                        UserContent::MetaText(_)
                            | UserContent::MetaImage { .. }
                            | UserContent::RawImage { is_meta: true, .. }
                            | UserContent::MetaDocument { .. }
                    )
                );
                if !is_tool_result && !is_meta {
                    turn += 1;
                }
            }
            RenderableMessageKind::Assistant { message } => match message.first_content_block() {
                Some(AssistantContent::Text(_)) => {
                    text_index_to_turn[index] = Some(turn);
                }
                Some(AssistantContent::ToolUse(tool_use)) if tool_use.name == brief_name => {
                    turns_with_brief.insert(turn);
                }
                _ => {}
            },
            _ => {}
        }
    }
    if turns_with_brief.is_empty() {
        return messages;
    }
    // Second pass: drop text rows whose turn called Brief.
    messages
        .into_iter()
        .enumerate()
        .filter(|(index, _)| match text_index_to_turn[*index] {
            Some(turn) => !turns_with_brief.contains(&turn),
            None => true,
        })
        .map(|(_, message)| message)
        .collect()
}

pub(crate) fn reorder_messages_for_ui(messages: Vec<RenderableMessage>) -> Vec<RenderableMessage> {
    use std::collections::HashSet;

    #[derive(Default)]
    struct ToolUseGroup {
        tool_use: Option<RenderableMessage>,
        pre_hooks: Vec<RenderableMessage>,
        tool_result: Option<RenderableMessage>,
        post_hooks: Vec<RenderableMessage>,
    }

    let mut groups: HashMap<String, ToolUseGroup> = HashMap::new();
    for message in &messages {
        if let Some(tool_use_id) = assistant_tool_use_id(message) {
            groups.entry(tool_use_id.to_string()).or_default().tool_use = Some(message.clone());
        } else if let Some((tool_use_id, group)) = hook_attachment_group(message) {
            let entry = groups.entry(tool_use_id.to_string()).or_default();
            match group {
                HookGroup::Pre => entry.pre_hooks.push(message.clone()),
                HookGroup::Post => entry.post_hooks.push(message.clone()),
            }
        } else if let Some(tool_use_id) = user_tool_result_id(message) {
            groups
                .entry(tool_use_id.to_string())
                .or_default()
                .tool_result = Some(message.clone());
        }
    }

    let mut result = Vec::with_capacity(messages.len());
    let mut processed_tool_uses = HashSet::new();
    for message in messages {
        if let Some(tool_use_id) = assistant_tool_use_id(&message).map(str::to_string) {
            if processed_tool_uses.insert(tool_use_id.clone()) {
                if let Some(group) = groups.get(&tool_use_id) {
                    if let Some(tool_use) = &group.tool_use {
                        result.push(tool_use.clone());
                    }
                    result.extend(group.pre_hooks.iter().cloned());
                    if let Some(tool_result) = &group.tool_result {
                        result.push(tool_result.clone());
                    }
                    result.extend(group.post_hooks.iter().cloned());
                }
            }
            continue;
        }

        if hook_attachment_group(&message).is_some() {
            continue;
        }

        if let Some(tool_use_id) = user_tool_result_id(&message) {
            // Only skip results that were actually moved after a visible tool
            // use. Some official-null tool uses are suppressed earlier in this
            // Rust mapper while their result remains user-facing.
            if groups
                .get(tool_use_id)
                .and_then(|group| group.tool_use.as_ref())
                .is_some()
            {
                continue;
            }
        }

        if is_api_error_message(&message) {
            if result.last().is_some_and(is_api_error_message) {
                *result.last_mut().expect("last checked above") = message;
            } else {
                result.push(message);
            }
            continue;
        }

        result.push(message);
    }

    let last_idx = result.len().checked_sub(1);
    result
        .into_iter()
        .enumerate()
        .filter_map(|(idx, message)| {
            (!is_api_error_message(&message) || Some(idx) == last_idx).then_some(message)
        })
        .collect()
}

/// Maps to official `applyGrouping(...)`.
/// Official grouping is opt-in per tool (`tool.renderGroupedToolUse`) and is
/// currently used by Agent/legacy Task tool calls. This main-screen subset
/// groups 2+ Agent/Task tool uses from the same assistant API message, then
/// suppresses their matching tool_result rows because the grouped row owns that
/// display, matching the official second pass.
pub(crate) fn apply_grouping(
    messages: Vec<RenderableMessage>,
    verbose: bool,
) -> Vec<RenderableMessage> {
    use std::collections::{HashMap, HashSet};

    // Maps to: CC `utils/groupToolUses.ts:59-64` — in verbose mode nothing
    // groups; every tool use/result renders at its original position
    // (`applyGrouping(messagesToShow, tools, verbose)`, Messages.tsx:633-637).
    if verbose {
        return messages;
    }

    fn supports_grouped_rendering(tool_name: &str) -> bool {
        matches!(tool_name.to_ascii_lowercase().as_str(), "agent" | "task")
    }

    fn group_key(message_id: &str, tool_name: &str) -> String {
        format!("{message_id}:{tool_name}")
    }

    let mut candidates: HashMap<String, Vec<RenderableMessage>> = HashMap::new();
    for message in &messages {
        let Some((message_id, _, tool_name)) = assistant_tool_use_group_info(message) else {
            continue;
        };
        if supports_grouped_rendering(tool_name) {
            candidates
                .entry(group_key(message_id, tool_name))
                .or_default()
                .push(message.clone());
        }
    }

    let valid_groups = candidates
        .into_iter()
        .filter(|(_, group)| group.len() >= 2)
        .collect::<HashMap<_, _>>();
    if valid_groups.is_empty() {
        return messages;
    }

    let mut grouped_tool_use_ids = HashSet::new();
    for group in valid_groups.values() {
        for message in group {
            if let Some(tool_use_id) = assistant_tool_use_id(message) {
                grouped_tool_use_ids.insert(tool_use_id.to_string());
            }
        }
    }

    // Maps to: CC `groupToolUses.ts:102-117` — collect the user rows carrying
    // results for grouped tool uses, keyed by tool_use_id. The row is kept
    // whole (CC keeps the `NormalizedUserMessage`); the status/content
    // derivations that used to be baked here moved to
    // `GroupedToolUseContent` render time (CC `GroupedToolUseContent.tsx:35-48`).
    let mut results_by_tool_use_id: HashMap<String, RenderableMessage> = HashMap::new();
    for message in &messages {
        if let Some(tool_use_id) = user_tool_result_id(message) {
            if grouped_tool_use_ids.contains(tool_use_id) {
                results_by_tool_use_id.insert(tool_use_id.to_string(), message.clone());
            }
        }
    }

    let mut result = Vec::with_capacity(messages.len());
    let mut emitted_groups = HashSet::new();
    for message in messages {
        if let Some((message_id, _, tool_name)) = assistant_tool_use_group_info(&message) {
            let key = group_key(message_id, tool_name);
            if let Some(group) = valid_groups.get(&key) {
                if emitted_groups.insert(key.clone()) {
                    // Maps to: CC `groupToolUses.ts:135-157` — per group
                    // member, in order, attach the matching result row when
                    // present. Summary/count/per-item state are render-time
                    // derivations now, not baked fields.
                    let results = group
                        .iter()
                        .filter_map(|member| {
                            let tool_use_id = assistant_tool_use_id(member)?;
                            results_by_tool_use_id.get(tool_use_id).cloned()
                        })
                        .collect::<Vec<_>>();
                    result.push(RenderableMessage {
                        uuid: format!("grouped-{}", group[0].uuid),
                        kind: RenderableMessageKind::GroupedToolUse(GroupedToolUseMessage {
                            tool_name: tool_name.to_string(),
                            messages: group.clone(),
                            results,
                        }),
                    });
                }
                continue;
            }
        }

        if user_tool_result_id(&message)
            .is_some_and(|tool_use_id| grouped_tool_use_ids.contains(tool_use_id))
        {
            continue;
        }

        result.push(message);
    }

    result
}

/// Maps to official `collapseHookSummaries(...)`.
/// Consecutive labeled hook summaries (for example several parallel
/// `PreToolUse` summaries) collapse into a single row with aggregated counts,
/// errors and continuation/output flags. Unlabeled stop summaries remain
/// standalone, matching the official guard on `hookLabel !== undefined`.
pub(crate) fn collapse_hook_summaries(messages: Vec<RenderableMessage>) -> Vec<RenderableMessage> {
    fn labeled_stop_hook_summary(message: &RenderableMessage) -> Option<&str> {
        match &message.kind {
            RenderableMessageKind::System(SystemMessage::StopHookSummary {
                hook_label: Some(label),
                ..
            }) => Some(label.as_str()),
            _ => None,
        }
    }

    fn merge_stop_hook_group(
        mut first: RenderableMessage,
        group: &[RenderableMessage],
    ) -> RenderableMessage {
        // The merged row carries only the CC fields; the "Ran N ... hooks"
        // copy derives at render (`SystemTextMessage.tsx:222,258`).
        let RenderableMessageKind::System(SystemMessage::StopHookSummary {
            hook_count,
            hook_infos,
            hook_errors,
            prevented_continuation,
            has_output,
            total_duration_ms,
            ..
        }) = &mut first.kind
        else {
            return first;
        };

        for message in group.iter().skip(1) {
            let RenderableMessageKind::System(SystemMessage::StopHookSummary {
                hook_count: next_count,
                hook_infos: next_infos,
                hook_errors: next_errors,
                prevented_continuation: next_prevented,
                has_output: next_has_output,
                total_duration_ms: next_duration,
                ..
            }) = &message.kind
            else {
                continue;
            };
            *hook_count += *next_count;
            hook_infos.extend(next_infos.iter().cloned());
            hook_errors.extend(next_errors.iter().cloned());
            *prevented_continuation |= *next_prevented;
            *has_output |= *next_has_output;
            *total_duration_ms = match (*total_duration_ms, *next_duration) {
                (Some(current), Some(next)) => Some(current.max(next)),
                (None, Some(next)) => Some(next),
                (current, None) => current,
            };
        }

        first
    }

    let mut result = Vec::with_capacity(messages.len());
    let mut i = 0;
    while i < messages.len() {
        let message = &messages[i];
        let Some(label) = labeled_stop_hook_summary(message).map(str::to_string) else {
            result.push(message.clone());
            i += 1;
            continue;
        };

        let start = i;
        while i < messages.len()
            && labeled_stop_hook_summary(&messages[i]).is_some_and(|next| next == label)
        {
            i += 1;
        }

        if i - start == 1 {
            result.push(messages[start].clone());
        } else {
            result.push(merge_stop_hook_group(
                messages[start].clone(),
                &messages[start..i],
            ));
        }
    }

    result
}

/// Maps to official `collapseTeammateShutdowns(...)`.
/// `collapseBackgroundBashNotifications(...)` is guarded out of the official
/// main-screen path, so task-notification user rows stay untouched. Consecutive
/// completed in-process teammate `task_status` attachments collapse here.
pub(crate) fn collapse_teammate_shutdowns(
    messages: Vec<RenderableMessage>,
) -> Vec<RenderableMessage> {
    fn is_teammate_shutdown(message: &RenderableMessage) -> bool {
        matches!(
            &message.kind,
            RenderableMessageKind::Attachment(Attachment::TaskStatus {
                task_type,
                status,
                ..
            }) if task_type == "in_process_teammate" && status == "completed"
        )
    }

    let mut result = Vec::with_capacity(messages.len());
    let mut i = 0;
    while i < messages.len() {
        let message = &messages[i];
        if is_teammate_shutdown(message) {
            let start = i;
            while i < messages.len() && is_teammate_shutdown(&messages[i]) {
                i += 1;
            }
            let count = i - start;
            if count == 1 {
                result.push(messages[start].clone());
            } else {
                result.push(RenderableMessage {
                    uuid: messages[start].uuid.clone(),
                    kind: RenderableMessageKind::Attachment(Attachment::TeammateShutdownBatch {
                        count,
                    }),
                });
            }
            continue;
        }

        result.push(message.clone());
        i += 1;
    }

    result
}

pub(crate) fn build_message_lookups(
    normalized_messages: &[RenderableMessage],
    messages_to_show: &[RenderableMessage],
) -> MessageLookups {
    let mut lookups = MessageLookups {
        normalized_message_count: normalized_messages.len(),
        ..MessageLookups::default()
    };

    let mut tool_use_ids_by_message_id: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut tool_use_id_to_message_id: HashMap<String, String> = HashMap::new();

    for message in messages_to_show {
        let Some((message_id, tool_use_id, _)) = assistant_tool_use_group_info(message)
            .map(|(m, t, n)| (Some(m.to_string()), t.to_string(), n))
            .or_else(|| assistant_tool_use_id(message).map(|t| (None, t.to_string(), "")))
        else {
            continue;
        };
        let message_id = &message_id;
        let tool_use_id = &tool_use_id;

        lookups
            .tool_use_by_tool_use_id
            .insert(tool_use_id.clone(), message.clone());

        if let Some(message_id) = message_id {
            tool_use_ids_by_message_id
                .entry(message_id.clone())
                .or_default()
                .insert(tool_use_id.clone());
            tool_use_id_to_message_id.insert(tool_use_id.clone(), message_id.clone());
        } else {
            lookups
                .sibling_tool_use_ids
                .entry(tool_use_id.clone())
                .or_default()
                .insert(tool_use_id.clone());
        }
    }

    for (tool_use_id, message_id) in tool_use_id_to_message_id {
        if let Some(siblings) = tool_use_ids_by_message_id.get(&message_id) {
            lookups
                .sibling_tool_use_ids
                .insert(tool_use_id, siblings.clone());
        }
    }

    // CC's `useGetToolFromMessages` pairs results against the FULL message
    // array — a tool_use row that AssistantToolUseMessage null-renders (e.g.
    // NotebookEdit without cell_type, UI.tsx:36) still resolves its result's
    // tool. The Rust pipeline physically drops nonvisual rows from
    // `messages_to_show`, so backfill the pairing map from the unfiltered
    // set; group/sibling structures stay show-scoped (those rows never
    // render).
    for message in normalized_messages {
        let Some(tool_use_id) = assistant_tool_use_id(message) else {
            continue;
        };
        if !lookups.tool_use_by_tool_use_id.contains_key(tool_use_id) {
            lookups
                .tool_use_by_tool_use_id
                .insert(tool_use_id.to_string(), message.clone());
        }
    }

    let mut resolved_hook_names: HashMap<String, HashMap<String, BTreeSet<String>>> =
        HashMap::new();

    for message in normalized_messages {
        // Maps to: CC `utils/messages.ts:1214-1223` — progress lookups are
        // built from independent progress MESSAGES, grouped by
        // `parentToolUseID`. This is the authoritative source.
        if let RenderableMessageKind::Progress {
            parent_tool_use_id,
            data,
            ..
        } = &message.kind
        {
            record_progress_messages(&mut lookups, parent_tool_use_id, std::slice::from_ref(data));
        }

        // Maps to: CC `utils/messages.ts:1250-1271` — server-side results live
        // INSIDE the assistant message, not in a following user turn, so they
        // have to be collected here too. Without this an advisor / web_search
        // result never marks its tool_use resolved and
        // `AssistantToolUseMessage` renders it as pending forever.
        if let RenderableMessageKind::Assistant { message } = &message.kind {
            for content in &message.content {
                // CC keys on the presence of `tool_use_id` (:1253-1261), the
                // same rule `ensure_tool_result_pairing` follows.
                if let Some(tool_use_id) = crate::utils::messages::server_tool_result_id(content) {
                    lookups
                        .resolved_tool_use_ids
                        .insert(tool_use_id.to_string());
                }
                // CC :1262-1270 — only the advisor error subtype marks errored;
                // a redacted result is a success that simply withholds text.
                if let crate::types::message::AssistantContent::Advisor {
                    tool_use_id,
                    content: crate::types::message::AdvisorResult::Error { .. },
                } = content
                {
                    lookups.errored_tool_use_ids.insert(tool_use_id.0.clone());
                }
            }
        }

        if let Some(tool_use_id) = user_tool_result_id(message).map(str::to_string) {
            lookups.resolved_tool_use_ids.insert(tool_use_id.clone());
            lookups
                .tool_result_by_tool_use_id
                .insert(tool_use_id.clone(), message.clone());
            // CC utils/messages.ts:1243-1245: errored ⇔ `content.is_error`.
            if user_tool_result_block(message).is_some_and(|tool_result| tool_result.is_error) {
                lookups.errored_tool_use_ids.insert(tool_use_id);
            }
        }

        if let Some((tool_use_id, event, hook_name)) = hook_attachment_parts(message) {
            resolved_hook_names
                .entry(tool_use_id.to_string())
                .or_default()
                .entry(event.to_string())
                .or_default()
                .insert(hook_name.to_string());
        }
    }

    for (tool_use_id, by_event) in resolved_hook_names {
        let counts = by_event
            .into_iter()
            .map(|(event, names)| (event, names.len()))
            .collect::<HashMap<_, _>>();
        lookups.resolved_hook_counts.insert(tool_use_id, counts);
    }

    lookups
}

fn record_progress_messages(
    lookups: &mut MessageLookups,
    tool_use_id: &str,
    progress_messages: &[ToolUseProgressMessage],
) {
    if progress_messages.is_empty() {
        return;
    }

    let entry = lookups
        .progress_messages_by_tool_use_id
        .entry(tool_use_id.to_string())
        .or_default();
    for progress in progress_messages {
        if !entry.contains(progress) {
            entry.push(progress.clone());
        }
    }
}

pub(crate) fn compute_render_slice(
    messages: &Arc<Vec<RenderableMessage>>,
    anchor: &mut Option<SliceAnchor>,
) -> MessagesRenderSlice {
    let start = compute_slice_start(messages.as_ref().as_slice(), anchor);
    MessagesRenderSlice {
        messages: Arc::clone(messages),
        start,
        end: messages.len(),
    }
}

/// Maps to: CC `Messages.tsx#computeSliceStart`.
fn compute_slice_start(messages: &[RenderableMessage], anchor: &mut Option<SliceAnchor>) -> usize {
    let anchor_idx = anchor.as_ref().and_then(|anchor| {
        messages
            .iter()
            .position(|message| message.uuid == anchor.uuid)
    });

    let mut start = anchor_idx
        .or_else(|| {
            anchor.as_ref().map(|anchor| {
                anchor.idx.min(
                    messages
                        .len()
                        .saturating_sub(MAX_MESSAGES_WITHOUT_VIRTUALIZATION),
                )
            })
        })
        .unwrap_or(0);

    if messages.len().saturating_sub(start) > MAX_MESSAGES_WITHOUT_VIRTUALIZATION + MESSAGE_CAP_STEP
    {
        start = messages
            .len()
            .saturating_sub(MAX_MESSAGES_WITHOUT_VIRTUALIZATION);
    }

    if let Some(message) = messages.get(start) {
        let next_anchor = SliceAnchor {
            uuid: message.uuid.clone(),
            idx: start,
        };
        if anchor.as_ref() != Some(&next_anchor) {
            *anchor = Some(next_anchor);
        }
    } else if anchor.is_some() {
        *anchor = None;
    }

    start
}

pub(crate) fn is_user_continuation(messages: &[RenderableMessage], index: usize) -> bool {
    matches!(
        messages.get(index).map(|message| &message.kind),
        Some(RenderableMessageKind::User { .. })
    ) && matches!(
        index
            .checked_sub(1)
            .and_then(|prev| messages.get(prev))
            .map(|message| &message.kind),
        Some(RenderableMessageKind::User { .. })
    )
}

pub(crate) fn has_content_after_index(messages: &[RenderableMessage], index: usize) -> bool {
    if !matches!(
        messages.get(index).map(|message| &message.kind),
        Some(RenderableMessageKind::CollapsedReadSearch(_))
    ) {
        return false;
    }

    messages
        .iter()
        .skip(index + 1)
        .any(is_non_skippable_after_collapsed_group)
}

fn is_non_skippable_after_collapsed_group(message: &RenderableMessage) -> bool {
    if let RenderableMessageKind::Assistant { message } = &message.kind {
        return !matches!(
            message.first_content_block(),
            Some(
                crate::types::message::AssistantContent::Thinking { .. }
                    | crate::types::message::AssistantContent::RedactedThinking { .. }
            )
        );
    }
    if user_tool_result_block(message).is_some() {
        return false;
    }
    !matches!(
        &message.kind,
        RenderableMessageKind::System(_)
            | RenderableMessageKind::Attachment(_)
            | RenderableMessageKind::GroupedToolUse(_)
    )
}

#[cfg(test)]
thread_local! {
    static LOGO_HEADER_RENDER_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_logo_header_render_count() {
    LOGO_HEADER_RENDER_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn logo_header_render_count() -> usize {
    LOGO_HEADER_RENDER_COUNT.with(std::cell::Cell::get)
}

/// Maps to: CC `components/Messages.tsx` `LogoHeader`.
///
/// The parent wraps this component in iocraft `Memo`, the L1 equivalent of
/// `React.memo(LogoHeader)`. The outer OffscreenFreeze protects both LogoV2 and
/// StatusNotices after the header enters native scrollback; LogoV2 retains its
/// own inner freeze for AppState/terminal-size driven updates.
#[derive(Default, Props)]
struct LogoHeaderProps {
    status_notice_context: StatusNoticeContext,
}

#[component]
fn LogoHeader(props: &LogoHeaderProps) -> impl Into<AnyElement<'static>> {
    #[cfg(test)]
    LOGO_HEADER_RENDER_COUNT.with(|count| count.set(count.get() + 1));

    let status_notices =
        (!get_active_notices(&props.status_notice_context).is_empty()).then(|| {
            element! {
                View(margin_top: 1u32) {
                    StatusNotices(context: props.status_notice_context.clone())
                }
            }
        });

    element! {
        OffscreenFreeze {
            View(flex_direction: FlexDirection::Column) {
                Logo
                #(status_notices)
            }
        }
    }
}

/// Maps to: CC `Messages.tsx:764-767` `canAnimate`. Its first term,
/// `!toolJSX || !!toolJSX.shouldContinueAnimation`, is the negation of
/// `local_command_ui_pauses_animation`.
fn can_animate(
    local_command_ui_pauses_animation: bool,
    tool_use_confirm_queue_len: usize,
    is_message_selector_visible: bool,
) -> bool {
    !local_command_ui_pauses_animation
        && tool_use_confirm_queue_len == 0
        && !is_message_selector_visible
}

#[derive(Default, Props)]
struct MessagesImplProps {
    /// Maps to: CC `Messages.tsx:314-320,677-684` post-collapse export slice.
    pub render_range: Option<(usize, usize)>,
    pub messages: Arc<Vec<RenderableMessage>>,
    /// Maps to: CC `Messages.tsx:262` `conversationId: string` — REPL-owned
    /// (`REPL.tsx:2017`), bumped on compact resets so `messageKey`
    /// (`Messages.tsx:792-795`, `${msg.uuid}-${conversationId}`) changes and
    /// stale memoized rows remount. Cometix carries it as a generation counter.
    pub conversation_id: u64,
    pub is_loading: bool,
    /// Mirrors official `verbose`; default main-screen messages are collapsed.
    pub verbose: bool,
    /// Mirrors official Ctrl+O transcript mode; expands tool/message details.
    pub is_transcript_mode: bool,
    /// Maps to: CC `showAllInTranscript` for the native-scrollback 30-row cap.
    pub show_all_in_transcript: bool,
    /// Mirrors official `Messages.tsx` LogoHeader ownership for non-empty
    /// main-screen transcripts. Empty-session Cometix still renders Logo in
    /// REPL because there is no empty Messages node yet.
    pub show_logo: bool,
    /// Maps to CC `components/Messages.tsx` LogoHeader → `StatusNotices`.
    /// Official `StatusNotices` builds this from global config/auth/memory/IDE
    /// reads; Cometix accepts a pure startup snapshot to preserve safe UI
    /// boundaries until runtime producers are wired.
    pub status_notice_context: StatusNoticeContext,
    /// Maps to: CC `Messages.tsx:254-258` `toolJSX`, of which Messages reads
    /// only `shouldContinueAnimation` (`:764-765`). This port's counterpart
    /// is an active local command UI (CC local-jsx, `CommandKind::LocalUi`)
    /// or the `!` shell row; true when one is up without letting animation
    /// continue.
    pub local_command_ui_pauses_animation: bool,
    /// Maps to: CC `Messages.tsx:259` `toolUseConfirmQueue` — Messages reads
    /// only its length (`:766`).
    pub tool_use_confirm_queue_len: usize,
    /// Maps to: CC `Messages.tsx:261` `isMessageSelectorVisible`.
    pub is_message_selector_visible: bool,
    /// Transient assistant text preview. Maps to CC `Messages.streamingText`:
    /// it is rendered after formal rows and never participates in message
    /// normalization/grouping/lookups.
    pub streaming_text: Option<String>,
    /// Tool-use id currently being classifier-checked, matching official
    /// `useIsClassifierChecking(param.id)` as a pure display seam.
    pub classifier_checking_tool_use_id: Option<String>,
    /// Whether the classifier row uses the auto-mode label.
    pub classifier_checking_is_auto: bool,
    /// Maps to: CC `Messages.tsx:260` `inProgressToolUseIDs: Set<string>`.
    pub in_progress_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `Messages.tsx:690` `new Set(streamingToolUses.map(…))`.
    pub streaming_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `Messages.tsx:251` `tools: Tools` (destructured at `:397`).
    /// Forwarded to every row at `:822`; CC additionally feeds it to
    /// `applyGrouping` (`:634`), `collapseReadSearchGroups` (`:642`) and
    /// `hasContentAfterIndex` (`:811`), whose port counterparts still classify
    /// by tool name.
    pub tools: Arc<Vec<crate::types::tools::Tool>>,
}

fn messages_memo_key(
    messages: &Arc<Vec<RenderableMessage>>,
) -> (usize, usize, Option<String>, Option<String>) {
    (
        Arc::as_ptr(messages) as usize,
        messages.len(),
        messages.first().map(|message| message.uuid.clone()),
        messages.last().map(|message| message.uuid.clone()),
    )
}

/// iocraft L1 comparator payload for the extracted `MessageRows` subtree.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MessageRowsMemoKey {
    /// Maps to: CC `Messages.tsx:314-320,677-684` post-collapse export slice.
    pub render_range: Option<(usize, usize)>,
    collapsed_ptr: usize,
    collapsed_len: usize,
    conversation_id: u64,
    first_uuid: Option<String>,
    last_uuid: Option<String>,
    lookups_ptr: usize,
    is_loading: bool,
    verbose: bool,
    is_transcript_mode: bool,
    expand_thinking: bool,
    expand_collapsed_read_search: bool,
    columns: u16,
    can_animate: bool,
    classifier_checking_tool_use_id: Option<String>,
    classifier_checking_is_auto: bool,
    /// CC has no `MessageRows` component — this memo boundary is a port L1
    /// extraction of `renderableMessages.flatMap(renderMessageRow)`, so it must
    /// carry every term CC's enclosing `Messages` comparator carries, `tools`
    /// (`Messages.tsx:1079-1087`) included. Individual rows still bail on their
    /// own static key, which is where CC's `areMessageRowPropsEqual` deliberately
    /// ignores the pool.
    tool_pool: String,
}

#[allow(clippy::too_many_arguments)]
fn message_rows_memo_key(
    prepared: &MessagesPreparedPipeline,
    conversation_id: u64,
    is_loading: bool,
    verbose: bool,
    is_transcript_mode: bool,
    expand_thinking: bool,
    expand_collapsed_read_search: bool,
    columns: u16,
    classifier_checking_tool_use_id: Option<&str>,
    classifier_checking_is_auto: bool,
    tools: &[crate::types::tools::Tool],
) -> MessageRowsMemoKey {
    MessageRowsMemoKey {
        render_range: None,
        tool_pool: tool_pool_memo_key(tools),
        collapsed_ptr: Arc::as_ptr(&prepared.collapsed) as usize,
        collapsed_len: prepared.collapsed.len(),
        conversation_id,
        first_uuid: prepared
            .collapsed
            .first()
            .map(|message| message.uuid.clone()),
        last_uuid: prepared
            .collapsed
            .last()
            .map(|message| message.uuid.clone()),
        lookups_ptr: Arc::as_ptr(&prepared.lookups) as usize,
        is_loading,
        verbose,
        is_transcript_mode,
        expand_thinking,
        expand_collapsed_read_search,
        columns,
        // Set by the caller, like `render_range`.
        can_animate: true,
        classifier_checking_tool_use_id: classifier_checking_tool_use_id.map(str::to_string),
        classifier_checking_is_auto,
    }
}

#[derive(Default, Props)]
struct MessageRowsProps {
    /// Maps to: CC `Messages.tsx:314-320,677-684` post-collapse export slice.
    pub render_range: Option<(usize, usize)>,
    pub prepared: MessagesPreparedPipeline,
    /// Maps to: CC `Messages.tsx:262` `conversationId`, part of every row key
    /// (`Messages.tsx:792-795`).
    pub conversation_id: u64,
    pub is_loading: bool,
    pub verbose: bool,
    pub is_transcript_mode: bool,
    pub expand_thinking: bool,
    pub expand_collapsed_read_search: bool,
    pub columns: u16,
    /// Maps to: CC `Messages.tsx:832` `canAnimate={canAnimate}` on each row.
    pub can_animate: bool,
    pub classifier_checking_tool_use_id: Option<String>,
    pub classifier_checking_is_auto: bool,
    /// Maps to: CC `Messages.tsx:260` `inProgressToolUseIDs: Set<string>`.
    pub in_progress_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `Messages.tsx:690` `new Set(streamingToolUses.map(…))`.
    pub streaming_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `Messages.tsx:822` `tools={tools}` on each `MessageRow`.
    pub tools: Arc<Vec<crate::types::tools::Tool>>,
}

#[derive(Default)]
/// Maps to: CC `components/Messages.tsx#MessagesImpl:797-861,976`
/// `renderMessageRow` / `renderableMessages.flatMap(renderMessageRow)`.
/// iocraft L1 extraction of CC `MessagesImpl`'s
/// `renderableMessages.flatMap(renderMessageRow)` block. The component boundary
/// gives retained rows stable identity; the row data and names remain CC's.
struct MessageRows {
    slice_anchor: Option<SliceAnchor>,
    last_key: Option<MessageRowsMemoKey>,
}

impl Component for MessageRows {
    type Props<'a> = MessageRowsProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let profile_start = component_profile_enabled().then(Instant::now);
        let mut next_key = message_rows_memo_key(
            &props.prepared,
            props.conversation_id,
            props.is_loading,
            props.verbose,
            props.is_transcript_mode,
            props.expand_thinking,
            props.expand_collapsed_read_search,
            props.columns,
            props.classifier_checking_tool_use_id.as_deref(),
            props.classifier_checking_is_auto,
            &props.tools,
        );

        next_key.render_range = props.render_range;
        next_key.can_animate = props.can_animate;

        // Match CC's Messages-level memo boundary for the native scrollback
        // path: ordinary PromptInput state changes do not change transcript
        // props, so the retained row subtree is reused without rebuilding rows
        // or dirtying layout. While loading, keep entering rows so dynamic
        // in-progress rows can be gated individually by MessageRow/OffscreenFreeze.
        if !props.is_loading && self.last_key.as_ref() == Some(&next_key) {
            if let Some(start) = profile_start {
                let elapsed = start.elapsed();
                if elapsed >= Duration::from_millis(5) {
                    eprintln!(
                        "cometix-component name=MessageRows action=retain elapsed={:?} collapsed_len={} is_loading={} columns={}",
                        elapsed,
                        props.prepared.collapsed.len(),
                        props.is_loading,
                        props.columns,
                    );
                }
            }
            return;
        }
        self.last_key = Some(next_key);

        let mut style = iocraft::taffy::style::Style::default();
        style.flex_direction = iocraft::taffy::style::FlexDirection::Column;
        style.size.width = iocraft::taffy::style::Dimension::percent(1.0);
        updater.set_layout_style_if_changed(style);

        // Maps to: CC `Messages.tsx:677-684`: slice AFTER grouping/collapse,
        // while retaining the lookups prepared from the complete normalized array.
        let messages = if let Some((start, end)) = props.render_range {
            let all = &props.prepared.collapsed;
            let start = start.min(all.len());
            let end = end.min(all.len()).max(start);
            Arc::new(all[start..end].to_vec())
        } else {
            Arc::clone(&props.prepared.collapsed)
        };
        let (start, end) = if props.render_range.is_some() || props.is_transcript_mode {
            // The legacy 30-message transcript cap was already applied by
            // MessagesImpl to the raw shown list before grouping/collapse.
            // MessageRows renders that complete projection.
            self.slice_anchor = None;
            (0, messages.len())
        } else {
            let render_slice = compute_render_slice(&messages, &mut self.slice_anchor);
            (render_slice.start, render_slice.end)
        };
        let lookups = Arc::clone(&props.prepared.lookups);
        let conversation_id = props.conversation_id;
        let columns = props.columns;
        let verbose = props.verbose;
        let is_transcript_mode = props.is_transcript_mode;
        let expand_thinking = props.expand_thinking;
        let expand_collapsed_read_search = props.expand_collapsed_read_search;
        let is_loading = props.is_loading;
        let can_animate = props.can_animate;
        let in_progress_tool_use_ids = Arc::clone(&props.in_progress_tool_use_ids);
        let streaming_tool_use_ids = Arc::clone(&props.streaming_tool_use_ids);
        let tools = Arc::clone(&props.tools);
        let rendered_count = end.saturating_sub(start);

        updater.update_children(
            (start..end).map(move |idx| {
                let message = &messages[idx];
                let all_messages = messages.as_ref().as_slice();
                // Official MessageRow: `addMargin={!hasMetadata}`. Outside
                // transcript metadata chrome that is always true — including
                // the first row after LogoHeader — so the prompt/first turn
                // keeps a 1-row gap under the logo. Cometix has no metadata
                // chrome yet, so every row gets the margin (user-continuation
                // image edge cases still use `is_user_continuation` below).
                let add_margin = true;
                let is_user_continuation = idx > start && is_user_continuation(all_messages, idx);
                let has_content_after = has_content_after_index(all_messages, idx);
                // Mirror official `React.memo(MessageRow, areMessageRowPropsEqual)` at
                // the row boundary. Dynamic rows stay transparent; static rows skip
                // child updates before `MessageRow` is entered.
                let static_memo_key = message_row_static_memo_key(
                    message,
                    conversation_id,
                    add_margin,
                    is_user_continuation,
                    has_content_after,
                    Some(lookups.as_ref()),
                    verbose,
                    is_transcript_mode,
                    expand_thinking,
                    expand_collapsed_read_search,
                    columns,
                    &in_progress_tool_use_ids,
                    &streaming_tool_use_ids,
                );
                let is_static = static_memo_key.is_some();
                let memo_key = static_memo_key
                    .unwrap_or_else(|| format!("dynamic-message-row-{}", message.uuid));
                let compare = is_static.then_some(memo_key_eq as MemoComparator);

                element! {
                    // Maps to: CC `Messages.tsx:792-795` `messageKey` —
                    // `${msg.uuid}-${conversationId}`. The generation term
                    // forces a remount (fresh child identity) after a compact
                    // reset (REPL.tsx:3461-3463), so a kept row whose uuid
                    // survives the reset cannot reuse a stale retained tree.
                    Memo(key: format!("{}-{}", message.uuid, conversation_id), memo_key: memo_key, compare: compare) {
                        MessageRow(
                            messages: Arc::clone(&messages),
                            index: idx,
                            conversation_id: conversation_id,
                            add_margin: add_margin,
                            is_user_continuation: is_user_continuation,
                            has_content_after: has_content_after,
                            can_animate: can_animate,
                            is_loading: is_loading,
                            lookups: Some(Arc::clone(&lookups)),
                            verbose: verbose,
                            is_transcript_mode: is_transcript_mode,
                            expand_thinking: expand_thinking,
                            expand_collapsed_read_search: expand_collapsed_read_search,
                            columns: columns,
                            in_progress_tool_use_ids: Arc::clone(&in_progress_tool_use_ids),
                            streaming_tool_use_ids: Arc::clone(&streaming_tool_use_ids),
                            // CC `Messages.tsx:822` `tools={tools}`.
                            tools: Arc::clone(&tools),
                        )
                    }
                }
            }),
            None,
        );
        if let Some(start_time) = profile_start {
            let elapsed = start_time.elapsed();
            if elapsed >= Duration::from_millis(5) {
                eprintln!(
                    "cometix-component name=MessageRows action=update elapsed={:?} rendered_count={} collapsed_len={} slice_start={} slice_end={} is_loading={} columns={}",
                    elapsed,
                    rendered_count,
                    props.prepared.collapsed.len(),
                    start,
                    end,
                    props.is_loading,
                    props.columns,
                );
            }
        }
    }
}

/// Maps to: CC `Messages.tsx:1079-1087` — the ONE `tools` case in
/// `areMessagesPropsEqual`: a new pool array is treated as equal when
/// `p.length === n.length && p.every((tool, i) => tool.name === n[i]?.name)`.
/// So the memo term is the ORDERED name list, not the array identity; the
/// per-frame `Arc` this port threads would otherwise be compared by pointer and
/// bust every memo below it.
pub(crate) fn tool_pool_memo_key(tools: &[crate::types::tools::Tool]) -> String {
    let mut key = String::with_capacity(tools.len() * 12);
    for tool in tools {
        key.push_str(&tool.name);
        key.push('\u{1}');
    }
    key
}

/// iocraft L1 comparator payload for CC's `React.memo(MessagesImpl, ...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MessagesMemoKey {
    /// Maps to: CC `Messages.tsx:314-320,677-684` post-collapse export slice.
    pub render_range: Option<(usize, usize)>,
    messages: (usize, usize, Option<String>, Option<String>),
    /// CC: `conversationId` is a `MessagesImpl` prop, so `React.memo`'s
    /// default comparison covers it.
    conversation_id: u64,
    is_loading: bool,
    verbose: bool,
    is_transcript_mode: bool,
    show_all_in_transcript: bool,
    toggle_show_all_shortcut: String,
    expand_thinking: bool,
    expand_collapsed_read_search: bool,
    show_logo: bool,
    status_notice_context: StatusNoticeContext,
    columns: u16,
    local_command_ui_pauses_animation: bool,
    tool_use_confirm_queue_len: usize,
    is_message_selector_visible: bool,
    classifier_checking_tool_use_id: Option<String>,
    classifier_checking_is_auto: bool,
    streaming_text: Option<String>,
    /// CC `Messages.tsx:1079-1087` — see [`tool_pool_memo_key`].
    tool_pool: String,
}

#[derive(Default)]
/// Maps to: CC `components/Messages.tsx#MessagesImpl` retained render body.
struct MessagesImpl {
    last_key: Option<MessagesMemoKey>,
}

impl Component for MessagesImpl {
    type Props<'a> = MessagesImplProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        // A hand-written `Component` must attach the context stack itself; the
        // `#[component]` macro does it for generated ones
        // (`iocraft-macros/src/lib.rs:540`). Without this every
        // `try_use_context` below silently returns `None`, which is exactly
        // what happened: `expand_thinking` and `expand_collapsed_read_search`
        // took their `.unwrap_or(true)` fallback on EVERY render, so the
        // `/config` previews for those two settings never reached this list.
        // The fallback matched the AppState default, which is why it was
        // invisible. Made visible by the P7 flip to the strict read.
        let mut hooks = hooks.with_context_stack(updater.component_context_stack());
        let profile_start = component_profile_enabled().then(Instant::now);
        let prepare_start = component_profile_enabled().then(Instant::now);
        let messages_key = messages_memo_key(&props.messages);
        let keybinding_runtime = hooks
            .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
            .map(|runtime| runtime.clone());
        // CC `Messages.tsx:430-434` `useShortcutDisplay('transcript:toggleShowAll',
        // 'Transcript', 'Ctrl+E')`: the resolved binding displays via
        // chordToString; the capitalized literal is only the no-context /
        // action-not-found fallback.
        let toggle_show_all_shortcut = keybinding_runtime.as_ref().map_or_else(
            || "Ctrl+E".to_string(),
            |runtime| {
                crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
                    "transcript:toggleShowAll",
                    &crate::keybindings::types::ContextName::Transcript,
                    "Ctrl+E",
                    runtime.bindings().as_slice(),
                )
            },
        );
        // CC's expensive-transform useMemo lists `isTranscriptMode`,
        // `shouldTruncate`, and therefore `showAllInTranscript` in its deps
        // (`Messages.tsx:553-555,656-668`). The raw transcript cap lives in
        // this prepared pipeline, so Ctrl+E must rebuild it before grouping.
        let prepared = hooks.use_memo(
            || {
                let normalized = normalize_messages(&props.messages);
                let compact_aware = compact_aware_messages(normalized.clone(), props.verbose);
                let filtered =
                    filter_non_rendering_messages(compact_aware, props.is_transcript_mode);
                let messages_to_show_not_truncated = reorder_messages_for_ui(filtered);
                // CC reads `messagesToShowNotTruncated.length` after the brief
                // dispatch (`Messages.tsx:655-657`); only the length survives
                // here so the vector can move through that dispatch un-cloned.
                let not_truncated_len = messages_to_show_not_truncated.len();
                // CC's three-tier brief dispatch (`Messages.tsx:600-623`):
                // transcript mode is unfiltered; the brief-only tier routes to
                // `filterForBriefTool`, which is KAIROS-gated and unreachable
                // in the external build projection.
                let brief_filtered = if props.is_transcript_mode {
                    messages_to_show_not_truncated
                } else {
                    drop_text_in_brief_turns(messages_to_show_not_truncated)
                };
                // CC caps the filtered/reordered raw rows before grouping and
                // collapse. `virtualScrollRuntimeGate` requires a live
                // scrollRef; the fullscreen env flag alone is not equivalent,
                // and this Rust Messages path has no VirtualMessageList carrier.
                let should_truncate = props.is_transcript_mode && !props.show_all_in_transcript;
                let has_truncated_messages = should_truncate
                    && brief_filtered.len() > MAX_MESSAGES_TO_SHOW_IN_TRANSCRIPT_MODE;
                let hidden_message_count =
                    not_truncated_len.saturating_sub(MAX_MESSAGES_TO_SHOW_IN_TRANSCRIPT_MODE);
                let messages_to_show = if should_truncate {
                    brief_filtered[brief_filtered
                        .len()
                        .saturating_sub(MAX_MESSAGES_TO_SHOW_IN_TRANSCRIPT_MODE)..]
                        .to_vec()
                } else {
                    brief_filtered
                };
                let lookups = build_message_lookups(&normalized, &messages_to_show);
                let grouped = apply_grouping(messages_to_show, props.verbose);
                let collapsed_read_search_groups = collapse_read_search_groups(grouped);
                let collapsed_teammate_shutdowns =
                    collapse_teammate_shutdowns(collapsed_read_search_groups);
                let collapsed = collapse_hook_summaries(collapsed_teammate_shutdowns);

                MessagesPreparedPipeline {
                    collapsed: Arc::new(collapsed),
                    lookups: Arc::new(lookups),
                    has_truncated_messages,
                    hidden_message_count,
                }
            },
            // `verbose` joins the deps because the compact-boundary slice reads
            // it (CC `Messages.tsx:579-584`).
            (
                messages_key.clone(),
                props.is_transcript_mode,
                props.show_all_in_transcript,
                props.verbose,
            ),
        );
        let prepare_elapsed = prepare_start.map(|start| start.elapsed());
        let (terminal_cols, _) = hooks.use_terminal_size();
        // Cometix display prefs live in AppState (Config preview). The
        // store-absent fallbacks recorded here on 2026-08-01 are gone:
        // retained Messages always mounts under AppStateProvider, and the
        // values come from the state defaults themselves (both true).
        let expand_thinking =
            crate::state::app_state::use_app_state(&mut hooks, |state| state.expand_thinking);
        let expand_collapsed_read_search =
            crate::state::app_state::use_app_state(&mut hooks, |state| {
                state.expand_collapsed_read_search
            });
        let next_key = MessagesMemoKey {
            render_range: props.render_range,
            messages: messages_key,
            conversation_id: props.conversation_id,
            is_loading: props.is_loading,
            verbose: props.verbose,
            is_transcript_mode: props.is_transcript_mode,
            show_all_in_transcript: props.show_all_in_transcript,
            toggle_show_all_shortcut: toggle_show_all_shortcut.clone(),
            expand_thinking,
            expand_collapsed_read_search,
            show_logo: props.show_logo,
            status_notice_context: props.status_notice_context.clone(),
            columns: terminal_cols,
            local_command_ui_pauses_animation: props.local_command_ui_pauses_animation,
            tool_use_confirm_queue_len: props.tool_use_confirm_queue_len,
            is_message_selector_visible: props.is_message_selector_visible,
            classifier_checking_tool_use_id: props.classifier_checking_tool_use_id.clone(),
            classifier_checking_is_auto: props.classifier_checking_is_auto,
            streaming_text: props.streaming_text.clone(),
            tool_pool: tool_pool_memo_key(&props.tools),
        };

        // Match official `Messages = React.memo(...)`: prompt-only frames keep
        // the message subtree mounted without touching layout or children.
        if !props.is_loading && self.last_key.as_ref() == Some(&next_key) {
            if let Some(start) = profile_start {
                let elapsed = start.elapsed();
                if elapsed >= Duration::from_millis(5) {
                    eprintln!(
                        "cometix-component name=Messages action=retain elapsed={:?} prepare={:?} messages_len={} collapsed_len={} is_loading={} streaming_len={} columns={}",
                        elapsed,
                        prepare_elapsed.unwrap_or_default(),
                        props.messages.len(),
                        prepared.collapsed.len(),
                        props.is_loading,
                        props.streaming_text.as_ref().map_or(0, String::len),
                        terminal_cols,
                    );
                }
            }
            return;
        }
        self.last_key = Some(next_key);

        let mut style = iocraft::taffy::style::Style::default();
        style.flex_direction = iocraft::taffy::style::FlexDirection::Column;
        updater.set_layout_style_if_changed(style);

        let has_rendered_rows = props
            .render_range
            .map_or(!prepared.collapsed.is_empty(), |(start, end)| {
                start < end.min(prepared.collapsed.len())
            });
        let mut children = Vec::<AnyElement<'static>>::new();
        if props.show_logo && !props.render_range.is_some_and(|(start, _)| start > 0) {
            let status_notice_context = props.status_notice_context.clone();
            // CC React.memo compares LogoHeader props only. Terminal width is
            // deliberately absent: LogoV2's own terminal-size hook invalidates
            // its child subtree without dirtying the header from parent frames.
            let logo_memo_key = format!("logo-header:{status_notice_context:?}");
            children.push(
                element! {
                    Memo(key: "logo-header".to_string(), memo_key: logo_memo_key, compare: memo_key_eq as MemoComparator) {
                        LogoHeader(status_notice_context: status_notice_context)
                    }
                }
                .into(),
            );
        }
        // Maps to CC `Messages.tsx:923-942`: both legacy transcript cap
        // indicators are Messages-owned siblings before the rendered rows.
        if prepared.has_truncated_messages {
            children.push(
                element! {
                    Divider(
                        title: format!(
                            "{} to show \x1b[1m{}\x1b[22m previous messages",
                            toggle_show_all_shortcut,
                            prepared.hidden_message_count,
                        ),
                        width: terminal_cols as u32,
                    )
                }
                .into(),
            );
        }
        // CC's fourth condition `!disableRenderCap` (Messages.tsx:935-938) is
        // the `[` dump-to-scrollback escape hatch; that REPL prop has no Rust
        // counterpart, so the condition is constant-true here.
        if props.is_transcript_mode
            && props.show_all_in_transcript
            && prepared.hidden_message_count > 0
        {
            children.push(
                element! {
                    Divider(
                        title: format!(
                            "{} to hide \x1b[1m{}\x1b[22m previous messages",
                            toggle_show_all_shortcut,
                            prepared.hidden_message_count,
                        ),
                        width: terminal_cols as u32,
                    )
                }
                .into(),
            );
        }
        let classifier_context = props
            .classifier_checking_tool_use_id
            .as_ref()
            .map(|tool_use_id| {
                ClassifierApprovalsState::start_checking(ClassifierChecking {
                    tool_use_id: tool_use_id.clone(),
                    is_auto: props.classifier_checking_is_auto,
                })
            })
            .unwrap_or_default();
        // Keep `streamingText` out of the formal row memo key. In CC
        // `components/Messages.tsx`, the preview is rendered as a sibling after
        // `messageRows`, so text deltas do not re-run message normalization,
        // grouping, or row construction.
        // Maps to: CC `Messages.tsx:764-767`.
        let can_animate = can_animate(
            props.local_command_ui_pauses_animation,
            props.tool_use_confirm_queue_len,
            props.is_message_selector_visible,
        );
        let mut rows_key = message_rows_memo_key(
            &prepared,
            props.conversation_id,
            props.is_loading,
            props.verbose,
            props.is_transcript_mode,
            expand_thinking,
            expand_collapsed_read_search,
            terminal_cols,
            props.classifier_checking_tool_use_id.as_deref(),
            props.classifier_checking_is_auto,
            &props.tools,
        );
        rows_key.can_animate = can_animate;
        let rows_memo_key = format!("{:?}:{:?}", props.render_range, rows_key);
        children.push(
            element! {
                Memo(key: "message-rows".to_string(), memo_key: rows_memo_key, compare: memo_key_eq as MemoComparator) {
                    ContextProvider(value: Context::owned(classifier_context)) {
                        MessageRows(
                            render_range: props.render_range,
                            prepared: prepared.clone(),
                            conversation_id: props.conversation_id,
                            is_loading: props.is_loading,
                            verbose: props.verbose,
                            is_transcript_mode: props.is_transcript_mode,
                            expand_thinking: expand_thinking,
                            expand_collapsed_read_search: expand_collapsed_read_search,
                            columns: terminal_cols,
                            can_animate: can_animate,
                            classifier_checking_tool_use_id: props.classifier_checking_tool_use_id.clone(),
                            classifier_checking_is_auto: props.classifier_checking_is_auto,
                            in_progress_tool_use_ids: Arc::clone(&props.in_progress_tool_use_ids),
                            streaming_tool_use_ids: Arc::clone(&props.streaming_tool_use_ids),
                            // CC `Messages.tsx:822` `tools={tools}` — the row
                            // map is this component in the port's L1 split.
                            tools: Arc::clone(&props.tools),
                        )
                    }
                }
            }
            .into(),
        );

        if let Some(streaming_text) = props.streaming_text.clone() {
            children.push(
                element! {
                    // Official Messages.tsx streaming preview uses marginTop={1}.
                    StreamingAssistantTextMessage(content: streaming_text, add_margin: true)
                }
                .into(),
            );
        }

        // REPL owns SpinnerWithVerb as a sibling of Messages (CC
        // `screens/REPL.tsx`), so Messages only retains the trailing spacer
        // when it owns the final visible row. During non-streaming loading the
        // REPL-owned spinner supplies the final row and spacer.
        // The existing interactive terminal spacer is not a CC Messages row;
        // headless exports preserve the source fragment's exact final newline.
        if props.render_range.is_none()
            && (props.streaming_text.is_some() || (has_rendered_rows && !props.is_loading))
        {
            children.push(element! { View(height: 1u32) {} }.into());
        }

        let child_count = children.len();
        updater.update_children(children, None);
        if let Some(start) = profile_start {
            let elapsed = start.elapsed();
            if elapsed >= Duration::from_millis(5) {
                eprintln!(
                    "cometix-component name=Messages action=update elapsed={:?} prepare={:?} children={} messages_len={} collapsed_len={} is_loading={} streaming_len={} columns={}",
                    elapsed,
                    prepare_elapsed.unwrap_or_default(),
                    child_count,
                    props.messages.len(),
                    prepared.collapsed.len(),
                    props.is_loading,
                    props.streaming_text.as_ref().map_or(0, String::len),
                    terminal_cols,
                );
            }
        }
    }
}

/// Maps to CC `components/Messages.tsx:249-313` `Props` subset.
#[derive(Default, Props)]
pub struct MessagesProps {
    /// Maps to: CC `Messages.tsx:314-320,677-684` post-collapse export slice.
    pub render_range: Option<(usize, usize)>,
    pub messages: Arc<Vec<RenderableMessage>>,
    /// Maps to: CC `Messages.tsx:262` `conversationId` (REPL.tsx:2017).
    pub conversation_id: u64,
    pub verbose: bool,
    pub screen: Screen,
    pub show_all_in_transcript: bool,
    pub hide_logo: bool,
    pub is_loading: bool,
    pub status_notice_context: StatusNoticeContext,
    /// Maps to: CC `Messages.tsx:254-258` `toolJSX` — see
    /// `MessagesImplProps::local_command_ui_pauses_animation`. The prompt site
    /// passes REPL's (REPL.tsx:6165); the transcript site passes `null`
    /// (:5824), which is this field's default.
    pub local_command_ui_pauses_animation: bool,
    /// Maps to: CC `Messages.tsx:259` `toolUseConfirmQueue`, of which only
    /// the length is read. Transcript site: `[]` (:5825).
    pub tool_use_confirm_queue_len: usize,
    /// Maps to: CC `Messages.tsx:261` `isMessageSelectorVisible`. Transcript
    /// site: `false` (:5827).
    pub is_message_selector_visible: bool,
    pub streaming_text: Option<String>,
    pub classifier_checking_tool_use_id: Option<String>,
    pub classifier_checking_is_auto: bool,
    /// Maps to: CC `Messages.tsx:260` `inProgressToolUseIDs: Set<string>` —
    /// REPL-owned state (`REPL.tsx:1897`) passed down, not re-derived here.
    pub in_progress_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `Messages.tsx:264` `streamingToolUses`, reduced at `:690` to
    /// the id set. REPL-owned (`REPL.tsx:1255`).
    pub streaming_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `Messages.tsx:251` `tools: Tools` — the live main-loop pool
    /// REPL hands down at `REPL.tsx:5821` (transcript) and `:6162` (prompt).
    pub tools: Arc<Vec<crate::types::tools::Tool>>,
}

/// Maps to CC `components/Messages.tsx`.
///
/// Message normalization, LogoHeader, rows, streaming preview, and the
/// complete retained frame remain in this component. Static rows stay mounted;
/// iocraft's inline backend owns any geometry-triggered terminal reset.
#[component]
pub fn Messages(props: &MessagesProps) -> impl Into<AnyElement<'static>> {
    let is_transcript_mode = props.screen == Screen::Transcript;

    element! {
        MessagesImpl(
            render_range: props.render_range,
            messages: Arc::clone(&props.messages),
            conversation_id: props.conversation_id,
            is_loading: props.is_loading,
            verbose: props.verbose,
            is_transcript_mode: is_transcript_mode,
            show_all_in_transcript: props.show_all_in_transcript,
            show_logo: !props.hide_logo,
            status_notice_context: props.status_notice_context.clone(),
            local_command_ui_pauses_animation: props.local_command_ui_pauses_animation,
            tool_use_confirm_queue_len: props.tool_use_confirm_queue_len,
            is_message_selector_visible: props.is_message_selector_visible,
            streaming_text: props.streaming_text.clone(),
            classifier_checking_tool_use_id: props.classifier_checking_tool_use_id.clone(),
            classifier_checking_is_auto: props.classifier_checking_is_auto,
            in_progress_tool_use_ids: Arc::clone(&props.in_progress_tool_use_ids),
            streaming_tool_use_ids: Arc::clone(&props.streaming_tool_use_ids),
            tools: Arc::clone(&props.tools),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::status_notice_definitions::{MAX_MEMORY_CHARACTER_COUNT, MemoryFileInfo};
    use crate::utils::theme;
    use futures::StreamExt;

    /// Maps to: CC `Messages.tsx:209-247` `dropTextInBriefTurns` — a turn
    /// that called SendUserMessage drops its assistant text rows; tool rows,
    /// the Brief call itself, and text in turns without a Brief call all
    /// stay. Tool_result and meta user rows do not advance the turn counter.
    #[test]
    fn drop_text_in_brief_turns_matches_official_turn_scoping() {
        use crate::types::message::{AssistantContent, ToolUseBlock, UserContent};
        let user = |uuid: &str, text: &str| {
            RenderableMessage::user_block(uuid, UserContent::Text(text.to_string()))
        };
        let assistant_text = |uuid: &str, text: &str| {
            RenderableMessage::assistant_block(uuid, AssistantContent::Text(text.to_string()))
        };
        let brief_use = |uuid: &str, id: &str| {
            RenderableMessage::assistant_block(
                uuid,
                AssistantContent::ToolUse(ToolUseBlock {
                    id: crate::types::ids::ToolUseId(id.to_string()),
                    name: crate::tools::brief_tool::prompt::BRIEF_TOOL_NAME.to_string(),
                    input: serde_json::json!({"message": "answer"}),
                }),
            )
        };
        let tool_result = |uuid: &str, id: &str| {
            RenderableMessage::user_block(
                uuid,
                UserContent::ToolResult(crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId(id.to_string()),
                    content: "Message delivered to user.".to_string(),
                    is_error: false,
                    content_blocks: Vec::new(),
                    tool_use_result: Some(serde_json::json!({"message": "answer"})),
                }),
            )
        };

        // Turn 1 calls Brief: its working-notes text drops, even the text
        // that follows the tool_result (same turn — tool_result rows do not
        // advance the counter). Turn 2 has no Brief call: text stays.
        let history = vec![
            user("u1", "question"),
            assistant_text("a1", "working notes"),
            brief_use("b1", "toolu-brief-1"),
            tool_result("r1", "toolu-brief-1"),
            assistant_text("a2", "post-result notes"),
            user("u2", "follow-up"),
            assistant_text("a3", "plain answer"),
        ];
        let kept = drop_text_in_brief_turns(history);
        let uuids: Vec<&str> = kept.iter().map(|row| row.uuid.as_str()).collect();
        assert_eq!(uuids, vec!["u1", "b1", "r1", "u2", "a3"]);

        // No Brief call anywhere: the pass is the identity.
        let untouched = vec![user("u1", "question"), assistant_text("a1", "plain answer")];
        assert_eq!(drop_text_in_brief_turns(untouched.clone()), untouched);

        // The legacy `Brief` alias is NOT in CC's nameSet — exact match only.
        let legacy = vec![
            user("u1", "question"),
            assistant_text("a1", "working notes"),
            RenderableMessage::assistant_block(
                "b1",
                AssistantContent::ToolUse(ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu-legacy".to_string()),
                    name: "Brief".to_string(),
                    input: serde_json::json!({"message": "answer"}),
                }),
            ),
        ];
        assert_eq!(drop_text_in_brief_turns(legacy.clone()), legacy);
    }

    struct TestEnvVarGuard {
        _env: crate::utils::env_utils::EnvVarGuard,
    }

    impl TestEnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::set(key, value),
            }
        }
    }

    #[cfg(feature = "anthropic_internal")]
    struct TestGlobalConfigGuard(Option<crate::utils::config::GlobalConfig>);

    #[cfg(feature = "anthropic_internal")]
    impl Drop for TestGlobalConfigGuard {
        fn drop(&mut self) {
            crate::utils::config::replace_test_global_config(self.0.take());
        }
    }

    fn message(id: usize) -> RenderableMessage {
        RenderableMessage::user(format!("msg-{id}"), format!("message {id}"))
    }

    fn messages(count: usize) -> Vec<RenderableMessage> {
        (0..count).map(message).collect()
    }

    /// CC `Messages` keeps static rows in the complete next frame. The Rust
    /// component must therefore render the full idle projection rather than
    /// submitting and removing a completed prefix through a second transport.
    #[test]
    fn messages_retains_complete_idle_frame_matches_official_messages() {
        let current_theme = *theme::current();
        let retained = Arc::new(messages(3));
        let rendered = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Messages(
                            messages: Arc::clone(&retained),
                            hide_logo: true,
                            is_loading: false,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(80))
        .to_string();

        for index in 0..3 {
            assert!(
                rendered.contains(&format!("message {index}")),
                "complete retained frame lost row {index}; canvas=\n{rendered}"
            );
        }
    }

    /// The whole `tools` prop chain, mounted at its head.
    ///
    /// CC threads the main-loop pool `REPL.tsx:5821`/`:6162` → `Messages.tsx:251`
    /// → `:822` → `MessageRow.tsx:201` → `Message.tsx:250` →
    /// `GroupedToolUseContent.tsx:16` → `:69` → `AgentTool/UI.tsx:835` → `:847`
    /// → `:1096` `findToolByName`. Every hop here is an iocraft prop, and
    /// iocraft props derive `Default`, so a dropped hop compiles — it silently
    /// degrades to an EMPTY pool instead. This test is that compile error's
    /// stand-in: rendering `Messages` with a pool that carries the nested tool
    /// must reach the agent progress line, and rendering it without must not.
    /// Drop the prop at ANY hop and the first assertion fails.
    #[test]
    fn the_tools_prop_reaches_the_agent_progress_line_from_the_messages_root() {
        // Two Agent tool_uses out of ONE assistant message: `apply_grouping`
        // keeps a singleton ungrouped (CC `groupToolUses.ts`), and only the
        // grouped row reaches `GroupedToolUseContent`.
        let agent_row = |uuid: &str, tool_use_id: &str, description: &str| {
            tool_use_in_message(
                uuid,
                "msg-agents",
                tool_use_id,
                "Agent",
                Some(serde_json::json!({
                    "description": description,
                    "prompt": "do the work",
                })),
                description,
            )
        };
        let nested =
            |uuid: &str, block: crate::types::message::AssistantContent| RenderableMessage {
                uuid: format!("progress-{uuid}"),
                kind: RenderableMessageKind::Progress {
                    tool_use_id: "toolu_agent_1".to_string(),
                    parent_tool_use_id: "toolu_agent_1".to_string(),
                    data: ToolUseProgressMessage::AgentProgress {
                        message: Box::new(RenderableMessage::assistant_block(
                            format!("nested-{uuid}"),
                            block,
                        )),
                        prompt: String::new(),
                        agent_id: "agent-1".to_string(),
                    },
                },
            };
        let input = Arc::new(vec![
            agent_row("agent-1", "toolu_agent_1", "Inspect auth"),
            agent_row("agent-2", "toolu_agent_2", "Inspect storage"),
            nested(
                "use",
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("nested_1".to_string()),
                        name: "Read".to_string(),
                        input: serde_json::json!({ "file_path": "src/a.rs" }),
                    },
                ),
            ),
            RenderableMessage {
                uuid: "progress-result".to_string(),
                kind: RenderableMessageKind::Progress {
                    tool_use_id: "toolu_agent_1".to_string(),
                    parent_tool_use_id: "toolu_agent_1".to_string(),
                    data: ToolUseProgressMessage::AgentProgress {
                        message: Box::new(RenderableMessage::user_tool_result(
                            "nested-result",
                            "nested_1",
                            "ok",
                            false,
                        )),
                        prompt: String::new(),
                        agent_id: "agent-1".to_string(),
                    },
                },
            },
        ]);

        let render = |names: &[&str]| {
            let input = Arc::clone(&input);
            let tools = Arc::new(
                names
                    .iter()
                    .map(|name| crate::types::tools::Tool {
                        name: (*name).to_string(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            );
            let current_theme = *theme::current();
            element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            Messages(
                                messages: Arc::clone(&input),
                                hide_logo: true,
                                is_loading: false,
                                tools: Arc::clone(&tools),
                            )
                        }.into_any()),
                    )
                }
            }
            .render(Some(100))
            .to_string()
        };

        // Read is in the pool: `tool.userFacingName` + `tool.getToolUseSummary`
        // (`UI.tsx:1105-1116`).
        let pooled = render(&["Agent", "Read"]);
        assert!(pooled.contains("Read: src/a.rs"), "canvas=\n{pooled}");

        // Read narrowed out of the pool: the raw wire name and no summary
        // (`UI.tsx:1096-1098`).
        let unpooled = render(&["Agent"]);
        assert!(
            !unpooled.contains("src/a.rs"),
            "an unpooled tool must not reach its own getToolUseSummary; canvas=\n{unpooled}"
        );
    }

    #[derive(Default, Props)]
    struct MessagesPromptOnlyRerenderProbeProps {
        pub is_loading: bool,
    }

    #[component]
    fn MessagesPromptOnlyRerenderProbe(
        props: &MessagesPromptOnlyRerenderProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        let stable_messages = hooks.use_const(|| Arc::new(messages(3)));
        if tick.get() < 2 {
            tick += 1;
        } else {
            system.exit();
        }

        let current_theme = *theme::current();
        // Captured by value: the children thunk is `Fn` and outlives this body.
        let is_loading_for_tree = props.is_loading;
        let tick_for_tree = tick.get();

        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                // MessagesImpl reads `expand_thinking` / `expand_collapsed_read_search`.
                // Default AppState has both `true`, which is exactly what the
                // removed `.unwrap_or(true)` supplied — so these harnesses render
                // what they always rendered, they just get it from the store now.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(flex_direction: FlexDirection::Column, width: 80u32) {
                            Messages(messages: Arc::clone(&stable_messages), is_loading: is_loading_for_tree)
                            Text(content: format!("tick {}", tick_for_tree))
                        }
                    }.into_any()),
                )
            }
        }
    }

    fn render_prompt_only_probe(is_loading: bool) -> String {
        let canvases = futures::executor::block_on(
            element!(MessagesPromptOnlyRerenderProbe(is_loading: is_loading))
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(80, 24))
                .collect::<Vec<_>>(),
        );
        canvases
            .last()
            .expect("mock render should produce a final canvas")
            .to_string()
    }

    #[test]
    fn messages_logo_header_renders_status_notices_at_official_boundary() {
        let current_theme = *theme::current();
        let messages = Arc::new(vec![RenderableMessage::user("u1", "hello")]);
        let status_notice_context = StatusNoticeContext {
            cwd: "/repo".to_string(),
            memory_files: vec![MemoryFileInfo {
                path: "/repo/CLAUDE.md".to_string(),
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1),
            }],
            ..StatusNoticeContext::default()
        };

        let rendered = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Messages(
                            messages: Arc::clone(&messages),
                            hide_logo: false,
                            status_notice_context: status_notice_context.clone(),
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            rendered.contains("Large CLAUDE.md will impact performance"),
            "canvas=\n{rendered}"
        );
        assert!(rendered.contains("hello"), "canvas=\n{rendered}");
    }

    #[test]
    fn messages_loading_state_does_not_render_repl_owned_spinner() {
        let current_theme = *theme::current();
        let rendered = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Messages(
                            messages: Arc::new(Vec::new()),
                            hide_logo: true,
                            is_loading: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(80))
        .to_string();

        assert!(
            rendered.trim().is_empty(),
            "Messages must not render REPL-owned SpinnerWithVerb; canvas=\n{rendered}"
        );
    }

    /// AppState of a swarm worker waiting on its leader for `tool_use_id`
    /// (swarmWorkerHandler.ts:62-65).
    fn worker_waiting_on(tool_use_id: &str) -> crate::state::app_state_store::AppState {
        crate::state::app_state_store::AppState {
            pending_worker_request: Some(Arc::new(
                crate::hooks::use_inbox_poller::PendingWorkerRequest {
                    tool_name: "Bash".to_string(),
                    tool_use_id: tool_use_id.to_string(),
                    description: "echo permission-gated".to_string(),
                },
            )),
            ..Default::default()
        }
    }

    #[test]
    fn messages_tool_use_reads_pending_worker_request_for_waiting_permission_row() {
        // CC AssistantToolUseMessage.tsx:58-60,122: the row reads
        // `pendingWorkerRequest` from AppState; Messages passes nothing.
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _fullscreen = TestEnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let current_theme = *theme::current();
        let messages = Arc::new(vec![tool_use_named_with_input(
            "assistant-tool",
            "toolu_1",
            "Bash",
            None,
            "echo permission-gated",
        )]);

        let rendered = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    initial_state: Some(worker_waiting_on("toolu_1")),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Messages(
                            messages: Arc::clone(&messages),
                            is_loading: false,
                            // The worker's tool is running while it waits
                            // (CC :204 `!isResolved && !isQueued`).
                            in_progress_tool_use_ids: Arc::new(
                                ["toolu_1".to_string()].into_iter().collect::<HashSet<_>>(),
                            ),
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(
            rendered.contains("Waiting for permission…"),
            "canvas=\n{rendered}"
        );
        assert!(!rendered.contains("Waiting…"), "canvas=\n{rendered}");
    }

    #[test]
    fn messages_classifier_checking_tool_use_renders_official_auxiliary_row() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _fullscreen = TestEnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let current_theme = *theme::current();
        let messages = Arc::new(vec![tool_use_named_with_input(
            "assistant-tool",
            "toolu_1",
            "Bash",
            None,
            "rm -rf tmp",
        )]);

        let rendered = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    initial_state: Some(worker_waiting_on("toolu_1")),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Messages(
                            messages: Arc::clone(&messages),
                            is_loading: false,
                            classifier_checking_tool_use_id: Some("toolu_1".to_string()),
                            classifier_checking_is_auto: true,
                            // Liveness is set membership (CC `REPL.tsx:1897`),
                            // not a status stored on the row.
                            in_progress_tool_use_ids: Arc::new(
                                ["toolu_1".to_string()].into_iter().collect::<HashSet<_>>(),
                            ),
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(
            rendered.contains("Auto classifier checking…"),
            "canvas=\n{rendered}"
        );
        assert!(
            !rendered.contains("Waiting for permission…"),
            "canvas=\n{rendered}"
        );
    }

    #[test]
    fn messages_tool_use_dot_does_not_dim_title_in_mock_rows() {
        // Guard against sibling env pollution (Review A F3):
        // fullscreen_collapse_surfaces_git_outcomes_without_double_counting_bash
        // sets CLAUDE_CODE_NO_FLICKER=1 under TEST_ENV_LOCK; without holding
        // the lock and forcing a falsy value here, the lone Bash fixture
        // (input: None) becomes collapsible via is_fullscreen_env_enabled()
        // and the "Bash" row assertion fails in parallel runs.
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _fullscreen = TestEnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let current_theme = *theme::current();
        for status in [ToolUseStatus::Running, ToolUseStatus::Queued] {
            let messages = Arc::new(vec![tool_use_named_with_input(
                &format!("assistant-tool-{status:?}"),
                &format!("toolu_{status:?}"),
                "Bash",
                None,
                "echo hi",
            )]);

            let canvas = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            Messages(messages: Arc::clone(&messages), hide_logo: true, is_loading: false)
                        }.into_any()),
                    )
                }
            }
            .render(Some(100));

            let rendered = canvas.to_string();
            let row = rendered
                .lines()
                .position(|line| line.contains("Bash"))
                .unwrap_or_else(|| panic!("tool row missing; canvas=\n{rendered}"));
            let dot_style = canvas.resolved_text_style(0, row).expect("dot style");
            assert_eq!(dot_style.color, None, "canvas=\n{rendered}");
            assert!(dot_style.dim);
            assert_eq!(dot_style.weight, Weight::Light);
            let title_style = canvas.resolved_text_style(2, row).expect("title style");
            assert_eq!(title_style.color, None);
            assert_eq!(title_style.weight, Weight::Bold);
            assert!(!title_style.dim, "canvas=\n{rendered}");
        }
    }

    #[test]
    fn message_rows_remain_mounted_across_prompt_only_rerenders() {
        let rendered = render_prompt_only_probe(false);

        assert!(rendered.contains("message 0"));
        assert!(rendered.contains("message 2"));
        assert!(rendered.contains("tick 2"));
    }

    #[test]
    fn logo_header_memo_skips_idle_parent_rerenders() {
        reset_logo_header_render_count();
        crate::components::logo_v2::logo_v2::reset_logo_display_data_current_call_count();

        let rendered = render_prompt_only_probe(false);

        assert!(rendered.contains("message 0"));
        assert_eq!(logo_header_render_count(), 1);
        assert_eq!(
            crate::components::logo_v2::logo_v2::logo_display_data_current_call_count(),
            1,
            "LogoV2 data helpers must run once across idle parent frames"
        );
    }

    #[test]
    fn static_message_rows_remain_mounted_while_loading_rerenders() {
        let rendered = render_prompt_only_probe(true);

        assert!(rendered.contains("message 0"));
        assert!(rendered.contains("message 2"));
        assert!(rendered.contains("tick 2"));
    }

    fn tool_use(id: &str, tool_use_id: &str) -> RenderableMessage {
        tool_use_named(id, tool_use_id, "Bash", "echo hi")
    }

    fn tool_use_named(
        id: &str,
        tool_use_id: &str,
        tool_name: &str,
        description: &str,
    ) -> RenderableMessage {
        let input = match tool_name {
            "Bash" | "PowerShell" => Some(serde_json::json!({"command": description})),
            "Read" => Some(serde_json::json!({"file_path": description})),
            "Write" => Some(serde_json::json!({
                "file_path": description,
                "content": "fixture"
            })),
            "Edit" => Some(serde_json::json!({
                "file_path": description,
                "old_string": "before",
                "new_string": "after"
            })),
            _ => None,
        };
        tool_use_named_with_input(id, tool_use_id, tool_name, input, description)
    }

    fn tool_use_in_message(
        uuid: &str,
        message_id: &str,
        tool_use_id: &str,
        tool_name: &str,
        input: Option<serde_json::Value>,
        description: &str,
    ) -> RenderableMessage {
        let input = input.unwrap_or_else(|| serde_json::json!({ "command": description }));
        RenderableMessage::assistant_blocks(
            uuid,
            vec![
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId(tool_use_id.to_string()),
                        name: tool_name.to_string(),
                        input,
                    },
                ),
                crate::types::message::AssistantContent::MessageIdentity(
                    crate::types::message::AssistantMessageIdentity {
                        api_message_id: Some(message_id.to_string()),
                        ..Default::default()
                    },
                ),
            ],
        )
    }

    fn tool_use_named_with_input(
        id: &str,
        tool_use_id: &str,
        tool_name: &str,
        input: Option<serde_json::Value>,
        description: &str,
    ) -> RenderableMessage {
        // Grouping keys off the owning API message id, carried by the
        // MessageIdentity sibling (same shape the recovery path produces).
        // The description parameter is absorbed into the input when none is
        // given so Bash-style summaries still render.
        let input = input.unwrap_or_else(|| serde_json::json!({ "command": description }));
        RenderableMessage::assistant_blocks(
            id,
            vec![
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId(tool_use_id.to_string()),
                        name: tool_name.to_string(),
                        input,
                    },
                ),
                crate::types::message::AssistantContent::MessageIdentity(
                    crate::types::message::AssistantMessageIdentity {
                        api_message_id: Some(id.to_string()),
                        ..Default::default()
                    },
                ),
            ],
        )
    }

    fn tool_result(id: &str, tool_use_id: &str, status: ToolResultStatus) -> RenderableMessage {
        // The row carries wire data — the status the old field stored is
        // derived from the content sentinels + is_error at consumption time.
        let (content, is_error) = match status {
            ToolResultStatus::Success => ("stdout".to_string(), false),
            ToolResultStatus::Error => ("stdout".to_string(), true),
            ToolResultStatus::Rejected => {
                (crate::utils::messages::REJECT_MESSAGE.to_string(), true)
            }
            ToolResultStatus::Canceled => {
                (crate::utils::messages::CANCEL_MESSAGE.to_string(), true)
            }
        };
        RenderableMessage::user_tool_result(id, tool_use_id, content, is_error)
    }

    fn collapsible_read_transcript(read_count: usize) -> Vec<RenderableMessage> {
        let mut rows = Vec::with_capacity(read_count * 2);
        for index in 0..read_count {
            let tool_use_id = format!("toolu_read_{index}");
            let path = format!("src/file_{index}.rs");
            rows.push(tool_use_named(
                &format!("read-{index}"),
                &tool_use_id,
                "Read",
                &path,
            ));
            rows.push(
                RenderableMessage::user_tool_result(
                    format!("result-read-{index}"),
                    &tool_use_id,
                    format!("     1→file {index}"),
                    false,
                )
                .with_tool_use_result(Some(serde_json::json!({
                    "type": "text",
                    "file": {
                        "filePath": path,
                        "content": format!("file {index}"),
                        "numLines": 1,
                        "startLine": 1,
                        "totalLines": 1
                    }
                }))),
            );
        }
        rows
    }

    fn hook(id: &str, tool_use_id: &str, event: &str, name: &str) -> RenderableMessage {
        RenderableMessage {
            uuid: id.to_string(),
            kind: RenderableMessageKind::Attachment(Attachment::HookSystemMessage {
                content: String::new(),
                hook_name: name.to_string(),
                tool_use_id: tool_use_id.to_string(),
                hook_event: event.to_string(),
            }),
        }
    }

    fn api_error(id: &str, error: &str) -> RenderableMessage {
        RenderableMessage {
            uuid: id.to_string(),
            kind: RenderableMessageKind::System(SystemMessage::ApiError {
                base: crate::types::message::SystemBase::with_uuid(id),
                error: error.to_string(),
                retry_in_ms: 2000,
                retry_attempt: 1,
                max_retries: 3,
            }),
        }
    }

    fn teammate_shutdown(id: &str) -> RenderableMessage {
        RenderableMessage {
            uuid: id.to_string(),
            kind: RenderableMessageKind::Attachment(Attachment::TaskStatus {
                task_id: id.to_string(),
                task_type: "in_process_teammate".to_string(),
                status: "completed".to_string(),
                description: "teammate".to_string(),
                delta_summary: None,
                output_file_path: None,
            }),
        }
    }

    fn stop_hook_summary(
        id: &str,
        label: Option<&str>,
        hook_count: usize,
        prevented_continuation: bool,
        has_output: bool,
        total_duration_ms: Option<u64>,
    ) -> RenderableMessage {
        RenderableMessage {
            uuid: id.to_string(),
            kind: RenderableMessageKind::System(SystemMessage::StopHookSummary {
                base: crate::types::message::SystemBase::with_uuid(id),
                hook_label: label.map(String::from),
                hook_count,
                hook_infos: vec![StopHookInfo {
                    command: Some(format!("hook-{id}")),
                    duration_ms: total_duration_ms,
                    ..StopHookInfo::default()
                }],
                hook_errors: Vec::new(),
                prevented_continuation,
                stop_reason: prevented_continuation.then(|| "blocked".to_string()),
                has_output,
                level: SystemMessageLevel::Info,
                tool_use_id: None,
                total_duration_ms,
            }),
        }
    }

    /// CC `Messages.tsx:553-652,923-942` caps the filtered/reordered raw list
    /// before Read/Search collapse, then renders the Messages-owned divider.
    /// Twenty Read pairs are forty raw rows but collapse to one visible row, so
    /// this rendered test catches the former post-collapse cap directly.
    #[test]
    fn transcript_dividers_cap_raw_rows_and_use_configured_shortcut_matches_official() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _fullscreen = TestEnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let input = Arc::new(collapsible_read_transcript(20));
        let render = |show_all_in_transcript: bool| {
            let input = Arc::clone(&input);
            let runtime = crate::keybindings::keybinding_context::KeybindingRuntime::new(vec![
                crate::keybindings::types::ParsedBinding {
                    chord: crate::keybindings::parser::parse_chord("ctrl+y"),
                    action: Some("transcript:toggleShowAll".to_string()),
                    context: crate::keybindings::types::ContextName::Transcript,
                },
            ]);
            let current_theme = *theme::current();
            element! {
                ContextProvider(value: Context::owned(runtime)) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(move || element! {
                                Messages(
                                    messages: Arc::clone(&input),
                                    screen: Screen::Transcript,
                                    verbose: true,
                                    show_all_in_transcript: show_all_in_transcript,
                                    hide_logo: true,
                                    is_loading: false,
                                )
                            }.into_any()),
                        )
                    }
                }
            }
            .render(Some(100))
        };

        let capped = render(false);
        let capped_text = capped.to_string();
        assert!(
            capped_text.contains("src/file_5.rs"),
            "canvas=\n{capped_text}"
        );
        assert!(
            !capped_text.contains("src/file_0.rs"),
            "the first five Read/result pairs must be outside the raw 30-row slice; canvas=\n{capped_text}"
        );
        assert!(
            capped_text.contains("ctrl+y to show 10 previous messages"),
            "canvas=\n{capped_text}"
        );
        let (row, line) = capped_text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("ctrl+y to show 10 previous messages"))
            .expect("truncation divider row");
        let chars = line.chars().collect::<Vec<_>>();
        let count_column = chars
            .windows(2)
            .position(|window| window == ['1', '0'])
            .expect("bold count column");
        let count_style = capped
            .resolved_text_style(count_column, row)
            .expect("count style");
        assert_eq!(count_style.weight, Weight::Bold);
        assert!(count_style.dim);
        assert!(count_style.is_dim());

        let expanded = render(true).to_string();
        assert!(
            expanded.contains("src/file_0.rs"),
            "Ctrl+E must restore rows omitted before grouping; canvas=\n{expanded}"
        );
        assert!(
            expanded.contains("ctrl+y to hide 10 previous messages"),
            "canvas=\n{expanded}"
        );
        assert!(
            !expanded.contains("ctrl+y to show 10 previous messages"),
            "canvas=\n{expanded}"
        );

        let fullscreen_env_only = {
            let _fullscreen = TestEnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "1");
            render(false).to_string()
        };
        assert!(
            fullscreen_env_only.contains("src/file_5.rs")
                && !fullscreen_env_only.contains("src/file_0.rs")
                && fullscreen_env_only.contains("ctrl+y to show 10 previous messages"),
            "the fullscreen env flag alone is not CC's live virtualScrollRuntimeGate; canvas=\n{fullscreen_env_only}"
        );
    }

    #[test]
    fn compute_render_slice_keeps_short_lists_unchanged() {
        let input = Arc::new(messages(12));
        let mut anchor = None;
        let slice = compute_render_slice(&input, &mut anchor);

        assert_eq!(
            slice.iter().cloned().collect::<Vec<_>>(),
            input.as_ref().clone()
        );
        assert_eq!(anchor.as_ref().map(|anchor| anchor.idx), Some(0));
    }

    #[test]
    fn compute_render_slice_owns_main_screen_render_cap() {
        let input = Arc::new(messages(
            MAX_MESSAGES_WITHOUT_VIRTUALIZATION + MESSAGE_CAP_STEP + 10,
        ));
        let mut anchor = None;
        let slice = compute_render_slice(&input, &mut anchor);

        assert_eq!(slice.len(), MAX_MESSAGES_WITHOUT_VIRTUALIZATION);
        assert_eq!(
            slice.iter().next().map(|message| message.uuid.as_str()),
            Some("msg-60")
        );
        assert_eq!(anchor.as_ref().map(|anchor| anchor.idx), Some(60));
    }

    #[test]
    fn render_slice_anchor_advances_only_after_step_budget() {
        let mut anchor = None;

        let first_messages = Arc::new(messages(
            MAX_MESSAGES_WITHOUT_VIRTUALIZATION + MESSAGE_CAP_STEP + 10,
        ));
        let first = compute_render_slice(&first_messages, &mut anchor);
        assert_eq!(
            first.iter().next().map(|message| message.uuid.as_str()),
            Some("msg-60")
        );

        let within_step_messages = Arc::new(messages(
            MAX_MESSAGES_WITHOUT_VIRTUALIZATION + MESSAGE_CAP_STEP + 60,
        ));
        let within_step = compute_render_slice(&within_step_messages, &mut anchor);
        assert_eq!(
            within_step
                .iter()
                .next()
                .map(|message| message.uuid.as_str()),
            Some("msg-60")
        );

        let beyond_step_messages = Arc::new(messages(
            MAX_MESSAGES_WITHOUT_VIRTUALIZATION + MESSAGE_CAP_STEP + 61,
        ));
        let beyond_step = compute_render_slice(&beyond_step_messages, &mut anchor);
        assert_eq!(
            beyond_step
                .iter()
                .next()
                .map(|message| message.uuid.as_str()),
            Some("msg-111")
        );
    }

    /// Maps to: CC `Messages.tsx:579-584`. The history array keeps every
    /// pre-compact row (CC appends the post-compact block rather than
    /// replacing the array), so hiding them is a render-time slice that
    /// verbose and fullscreen skip — otherwise ctrl+o/verbose and the
    /// scrollback-less alt buffer would have no way to reach them.
    #[test]
    fn compact_boundary_slice_hides_history_only_outside_verbose() {
        // The slice is also skipped in fullscreen (`Messages.tsx:579-584`
        // `verbose || isFullscreenEnvEnabled()`), and fullscreen defaults ON
        // for ant builds (CLAUDE_CODE_NO_FLICKER's audience default). Pin the
        // env so BOTH audiences exercise the main-screen slice this test is
        // about; the fullscreen skip itself is fullscreen.rs's contract.
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_NO_FLICKER", "0");
        let boundary = RenderableMessage {
            uuid: "boundary".to_string(),
            kind: RenderableMessageKind::System(
                crate::types::message::SystemMessage::CompactBoundary {
                    base: crate::types::message::SystemBase::with_uuid("boundary"),
                    compact_metadata: None,
                    logical_parent_uuid: None,
                },
            ),
        };
        let rows = vec![
            RenderableMessage::user("before-1", "old prompt"),
            RenderableMessage::assistant_block(
                "before-2",
                crate::types::message::AssistantContent::Text("old reply".to_string()),
            ),
            boundary,
            RenderableMessage::user("after-1", "new prompt"),
        ];

        let hidden = compact_aware_messages(rows.clone(), false);
        assert_eq!(
            hidden
                .iter()
                .map(|row| row.uuid.as_str())
                .collect::<Vec<_>>(),
            vec!["boundary", "after-1"],
            "the boundary itself is included, matching upstream"
        );

        let shown = compact_aware_messages(rows.clone(), true);
        assert_eq!(shown.len(), rows.len(), "verbose sees the whole history");

        // Without a boundary nothing is sliced away.
        let no_boundary = vec![RenderableMessage::user("only", "prompt")];
        assert_eq!(compact_aware_messages(no_boundary.clone(), false).len(), 1);
        crate::utils::process_env::remove("CLAUDE_CODE_NO_FLICKER");
    }

    #[test]
    fn compute_render_slice_returns_shared_window_without_cloning_messages() {
        let messages = Arc::new(messages(
            MAX_MESSAGES_WITHOUT_VIRTUALIZATION + MESSAGE_CAP_STEP + 10,
        ));
        let mut anchor = None;

        let slice = compute_render_slice(&messages, &mut anchor);

        assert!(Arc::ptr_eq(&slice.messages, &messages));
        assert_eq!(slice.start, 60);
        assert_eq!(slice.len(), MAX_MESSAGES_WITHOUT_VIRTUALIZATION);
        assert_eq!(
            slice.iter().next().map(|message| message.uuid.as_str()),
            Some("msg-60")
        );
    }

    #[test]
    fn messages_memo_key_is_stable_for_prompt_only_rerenders() {
        let messages = Arc::new(messages(24));
        let same_messages = Arc::clone(&messages);
        let rebuilt_same_content = Arc::new(messages.as_ref().clone());

        assert_eq!(
            messages_memo_key(&messages),
            messages_memo_key(&same_messages)
        );
        assert_ne!(
            messages_memo_key(&messages),
            messages_memo_key(&rebuilt_same_content)
        );
    }

    #[test]
    fn message_rows_memo_key_is_stable_for_prompt_only_rerenders() {
        let messages = Arc::new(messages(24));
        let prepared = MessagesPreparedPipeline {
            collapsed: Arc::clone(&messages),
            ..MessagesPreparedPipeline::default()
        };
        let same_prepared = prepared.clone();
        let rebuilt_prepared = MessagesPreparedPipeline {
            collapsed: Arc::new(messages.as_ref().clone()),
            ..MessagesPreparedPipeline::default()
        };

        let tools = |names: &[&str]| {
            names
                .iter()
                .map(|name| crate::types::tools::Tool {
                    name: (*name).to_string(),
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };
        let pool = tools(&["Bash", "Read"]);
        let rebuilt_pool = tools(&["Bash", "Read"]);
        let narrowed_pool = tools(&["Bash"]);

        // The terms each assertion varies; the rest stay at one baseline
        // (not transcript mode, both expand preferences on, 120 columns).
        let key = |prepared: &MessagesPreparedPipeline,
                   conversation_id: u64,
                   verbose: bool,
                   classifier: Option<&str>,
                   classifier_is_auto: bool,
                   pool: &[crate::types::tools::Tool]| {
            message_rows_memo_key(
                prepared,
                conversation_id,
                false,
                verbose,
                false,
                true,
                true,
                120,
                classifier,
                classifier_is_auto,
                pool,
            )
        };
        let baseline = key(&prepared, 0, false, None, false, &pool);

        assert_eq!(baseline, key(&same_prepared, 0, false, None, false, &pool));
        assert_ne!(baseline, key(&rebuilt_prepared, 0, false, None, false, &pool));
        assert_ne!(baseline, key(&prepared, 0, true, None, false, &pool));
        assert_ne!(baseline, key(&prepared, 0, false, Some("toolu_1"), true, &pool));
        // CC Messages.tsx:764-767: `canAnimate` flipping (a permission dialog
        // queued, the selector opening) must reach the rows.
        let mut blocked = key(&prepared, 0, false, None, false, &pool);
        blocked.can_animate = false;
        assert_ne!(baseline, blocked);
        // CC Messages.tsx:792-795: a conversationId bump alone must change the
        // row keys (compact-reset remount, REPL.tsx:3461-3463).
        assert_ne!(baseline, key(&prepared, 1, false, None, false, &pool));
        // CC Messages.tsx:1079-1087: a rebuilt pool with the SAME names is
        // equal, a pool that lost a tool is not — the rows must re-render so a
        // narrowed-out tool stops reading its own members
        // (`AgentTool/UI.tsx:1096-1098`).
        assert_eq!(baseline, key(&prepared, 0, false, None, false, &rebuilt_pool));
        assert_ne!(baseline, key(&prepared, 0, false, None, false, &narrowed_pool));
    }

    #[test]
    fn can_animate_matches_official_terms() {
        // CC Messages.tsx:764-767, with the transcript site's inputs first
        // (`null`, `[]`, `false`, REPL.tsx:5824-5827).
        assert!(can_animate(false, 0, false));
        assert!(!can_animate(true, 0, false));
        assert!(!can_animate(false, 1, false));
        assert!(!can_animate(false, 0, true));
    }

    #[test]
    fn official_pipeline_stages_are_identity_for_current_mock_shape() {
        let input = vec![
            RenderableMessage::user("u1", "hello"),
            RenderableMessage::system("s1", "notice"),
            RenderableMessage::assistant_block(
                "a1",
                crate::types::message::AssistantContent::Text("reply".to_string()),
            ),
        ];

        let normalized = normalize_messages(&input);
        let filtered = filter_non_rendering_messages(normalized.clone(), false);
        let reordered = reorder_messages_for_ui(filtered.clone());
        let grouped = apply_grouping(reordered.clone(), false);
        let collapsed = collapse_hook_summaries(collapse_teammate_shutdowns(
            collapse_read_search_groups(grouped.clone()),
        ));

        assert_eq!(normalized, input);
        assert_eq!(filtered, input);
        assert_eq!(reordered, input);
        assert_eq!(grouped, input);
        assert_eq!(collapsed, input);
    }

    #[test]
    fn null_rendering_attachments_are_removed_before_visible_pipeline_count() {
        let input = vec![
            RenderableMessage {
                uuid: "hidden".to_string(),
                kind: RenderableMessageKind::Attachment(Attachment::CriticalSystemReminder {
                    content: "model only".to_string(),
                }),
            },
            RenderableMessage {
                uuid: "max-turns".to_string(),
                kind: RenderableMessageKind::Attachment(Attachment::MaxTurnsReached {
                    max_turns: 10,
                    turn_count: 10,
                }),
            },
            RenderableMessage::user("visible", "hello"),
        ];
        // Compare by clone of the surviving row — the user constructor stamps
        // Utc::now(), so two separate constructions never compare equal.
        let expected = input[2].clone();
        assert_eq!(filter_non_rendering_messages(input, false), vec![expected]);
    }

    #[test]
    fn background_bash_task_notifications_do_not_collapse_in_main_screen_path() {
        let input = vec![
            RenderableMessage::user(
                "bg-1",
                "<task-notification><status>completed</status><summary>Background bash: cargo check</summary></task-notification>",
            ),
            RenderableMessage::user(
                "bg-2",
                "<task-notification><status>completed</status><summary>Background bash: cargo test</summary></task-notification>",
            ),
        ];

        let collapsed = collapse_teammate_shutdowns(input.clone());

        assert_eq!(collapsed, input);
    }

    #[test]
    fn reorder_messages_pairs_tool_results_with_originating_tool_use() {
        let input = vec![
            tool_use_in_message("tool-1", "msg-1", "toolu_1", "Bash", None, "grep summary"),
            tool_use_in_message("tool-2", "msg-1", "toolu_2", "Bash", None, "grep subtype"),
            RenderableMessage::user_tool_result("result-1", "toolu_1", "(No output)", false),
            RenderableMessage::user_tool_result("result-2", "toolu_2", "stdout", false),
        ];

        let reordered = reorder_messages_for_ui(input);
        let ids = reordered
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["tool-1", "result-1", "tool-2", "result-2"]);
    }

    #[test]
    fn reorder_messages_places_hooks_around_tool_result_like_official_ui() {
        let input = vec![
            tool_use("tool-1", "toolu_1"),
            tool_use("tool-2", "toolu_2"),
            hook("pre-1", "toolu_1", "PreToolUse", "guard"),
            tool_result("result-1", "toolu_1", ToolResultStatus::Success),
            hook("post-1", "toolu_1", "PostToolUse", "notify"),
            tool_result("result-2", "toolu_2", ToolResultStatus::Success),
        ];

        let reordered = reorder_messages_for_ui(input);
        let ids = reordered
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            vec![
                "tool-1", "pre-1", "result-1", "post-1", "tool-2", "result-2"
            ]
        );
    }

    #[test]
    fn reorder_messages_keeps_only_tail_api_error_retry_row() {
        let input = vec![
            RenderableMessage::user("u1", "hello"),
            api_error("api-1", "first retry"),
            api_error("api-2", "second retry"),
        ];

        let reordered = reorder_messages_for_ui(input);
        let ids = reordered
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["u1", "api-2"]);
    }

    fn advisor_row(
        uuid: &str,
        tool_use_id: &str,
        content: crate::types::message::AdvisorResult,
    ) -> RenderableMessage {
        RenderableMessage {
            uuid: uuid.to_string(),
            kind: RenderableMessageKind::Assistant {
                message: crate::types::message::AssistantMessage {
                    uuid: uuid.to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::AssistantContent::Advisor {
                        tool_use_id: crate::types::ids::ToolUseId(tool_use_id.to_string()),
                        content,
                    }],
                    model: None,
                    stop_reason: None,
                    usage: None,
                },
            },
        }
    }

    /// Maps to: CC `utils/messages.ts:1250-1261`. A server-side result lives
    /// inside the assistant message; collecting only from user tool_results
    /// left it unresolved, so the tool_use rendered as pending forever.
    #[test]
    fn build_message_lookups_resolves_server_side_results_from_the_assistant() {
        let input = vec![
            tool_use("tool-1", "srvtoolu_advisor"),
            advisor_row(
                "advisor-1",
                "srvtoolu_advisor",
                crate::types::message::AdvisorResult::Result {
                    text: "advice".to_string(),
                },
            ),
        ];

        let lookups = build_message_lookups(&input, &input);

        assert!(lookups.resolved_tool_use_ids.contains("srvtoolu_advisor"));
        assert!(
            !lookups.errored_tool_use_ids.contains("srvtoolu_advisor"),
            "a plain advisor result is not an error"
        );
    }

    /// CC `:1262-1270` marks errored only for the `advisor_tool_result_error`
    /// subtype — a redacted result is a success that withholds its text.
    #[test]
    fn build_message_lookups_distinguishes_advisor_error_from_redacted() {
        let errored = vec![advisor_row(
            "advisor-err",
            "srvtoolu_err",
            crate::types::message::AdvisorResult::Error {
                error_code: "unavailable".to_string(),
            },
        )];
        let redacted = vec![advisor_row(
            "advisor-red",
            "srvtoolu_red",
            crate::types::message::AdvisorResult::Redacted {
                encrypted_content: "AAAA".to_string(),
            },
        )];

        let errored_lookups = build_message_lookups(&errored, &errored);
        let redacted_lookups = build_message_lookups(&redacted, &redacted);

        assert!(
            errored_lookups
                .errored_tool_use_ids
                .contains("srvtoolu_err")
        );
        assert!(
            redacted_lookups
                .resolved_tool_use_ids
                .contains("srvtoolu_red")
        );
        assert!(
            !redacted_lookups
                .errored_tool_use_ids
                .contains("srvtoolu_red"),
            "redacted withholds text but did not fail"
        );
    }

    /// Maps to: CC `useGetToolFromMessages` pairing over the FULL array — a
    /// tool_use row that AssistantToolUseMessage null-renders (NotebookEdit
    /// without cell_type, UI.tsx:36) is physically dropped from the show set
    /// here, but its result must still resolve the tool for the success leaf.
    #[test]
    fn nonvisual_tool_use_rows_still_pair_their_results_in_lookups() {
        let mut nonvisual = tool_use_named("tool-nb", "toolu_nb", "NotebookEdit", "");
        if let RenderableMessageKind::Assistant { message } = &mut nonvisual.kind {
            if let Some(crate::types::message::AssistantContent::ToolUse(tool_use)) =
                message.content.first_mut()
            {
                // No cell_type → renderToolUseMessage None → nonvisual.
                tool_use.input = serde_json::json!({
                    "notebook_path": "/tmp/nb.ipynb",
                    "cell_id": "cell-a",
                    "new_source": "print('x')",
                    "edit_mode": "replace"
                });
            }
        }
        assert!(
            assistant_row_is_nonvisual_tool_use(&nonvisual),
            "fixture must be the nonvisual shape"
        );
        let normalized = vec![
            nonvisual,
            tool_result("result-nb", "toolu_nb", ToolResultStatus::Success),
        ];
        // The show set drops the nonvisual row exactly like the pipeline.
        let to_show: Vec<_> = normalized
            .iter()
            .filter(|row| !assistant_row_is_nonvisual_tool_use(row))
            .cloned()
            .collect();
        let lookups = build_message_lookups(&normalized, &to_show);
        assert!(
            lookups.tool_use_by_tool_use_id.contains_key("toolu_nb"),
            "the result row must still resolve its tool"
        );
    }

    #[test]
    fn build_message_lookups_tracks_tool_results_and_resolved_hooks() {
        let input = vec![
            tool_use("tool-1", "toolu_1"),
            hook("pre-a", "toolu_1", "PreToolUse", "guard"),
            hook("pre-b", "toolu_1", "PreToolUse", "guard"),
            tool_result("result-1", "toolu_1", ToolResultStatus::Error),
        ];

        let lookups = build_message_lookups(&input, &input);

        assert_eq!(lookups.normalized_message_count, input.len());
        assert!(lookups.tool_use_by_tool_use_id.contains_key("toolu_1"));
        assert!(lookups.tool_result_by_tool_use_id.contains_key("toolu_1"));
        assert!(lookups.resolved_tool_use_ids.contains("toolu_1"));
        assert!(lookups.errored_tool_use_ids.contains("toolu_1"));
        assert_eq!(
            lookups
                .sibling_tool_use_ids
                .get("toolu_1")
                .cloned()
                .unwrap_or_default(),
            BTreeSet::from(["toolu_1".to_string()])
        );
        assert_eq!(
            lookups
                .resolved_hook_counts
                .get("toolu_1")
                .and_then(|by_event| by_event.get("PreToolUse")),
            Some(&1)
        );
    }

    #[test]
    fn build_message_lookups_tracks_progress_messages_by_tool_use_id() {
        // Maps to: CC `utils/messages.ts:1214-1223` — progress arrives as an
        // independent message and the lookups group it by `parentToolUseID`.
        // The tool-use row itself carries none.
        let progress = ToolUseProgressMessage::QueryUpdate {
            query: "native scrollback".to_string(),
        };
        let input = vec![
            tool_use_in_message(
                "tool-1",
                "msg-1",
                "toolu_1",
                "WebSearch",
                None,
                "native scrollback",
            ),
            RenderableMessage {
                uuid: "progress-1".to_string(),
                kind: RenderableMessageKind::Progress {
                    tool_use_id: "toolu_1".to_string(),
                    parent_tool_use_id: "toolu_1".to_string(),
                    data: progress.clone(),
                },
            },
        ];

        let lookups = build_message_lookups(&input, &input);

        assert_eq!(
            lookups.progress_messages_by_tool_use_id.get("toolu_1"),
            Some(&vec![progress])
        );
    }

    // The Agent display variant (and its embedded progress carrier) is
    // gone — Progress rows are the single lookup source, so the old
    // variant-collection and dual-source dedup tests dissolved with it.
    #[test]
    fn build_message_lookups_deduplicates_duplicate_progress_snapshots() {
        // The same snapshot arriving twice as live Progress rows must not
        // double up in the lookups.
        let progress = ToolUseProgressMessage::AgentProgress {
            message: Box::new(RenderableMessage::assistant_block(
                "nested-assistant",
                crate::types::message::AssistantContent::Text("Read auth files".to_string()),
            )),
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        };
        let input = vec![
            RenderableMessage {
                uuid: "progress-1".to_string(),
                kind: RenderableMessageKind::Progress {
                    tool_use_id: "toolu_agent".to_string(),
                    parent_tool_use_id: "toolu_agent".to_string(),
                    data: progress.clone(),
                },
            },
            RenderableMessage {
                uuid: "progress-1".to_string(),
                kind: RenderableMessageKind::Progress {
                    tool_use_id: "toolu_agent".to_string(),
                    parent_tool_use_id: "toolu_agent".to_string(),
                    data: progress.clone(),
                },
            },
        ];

        let lookups = build_message_lookups(&input, &input);

        assert_eq!(
            lookups.progress_messages_by_tool_use_id.get("toolu_agent"),
            Some(&vec![progress])
        );
    }

    #[test]
    fn build_message_lookups_tracks_sibling_tool_uses_by_message_id() {
        let input = vec![
            tool_use_in_message("tool-1", "msg-siblings", "toolu_1", "Bash", None, "rg one"),
            tool_use_in_message("tool-2", "msg-siblings", "toolu_2", "Bash", None, "rg two"),
        ];

        let lookups = build_message_lookups(&input, &input);

        assert_eq!(
            lookups
                .sibling_tool_use_ids
                .get("toolu_1")
                .cloned()
                .unwrap_or_default(),
            BTreeSet::from(["toolu_1".to_string(), "toolu_2".to_string()])
        );
        assert_eq!(
            lookups
                .sibling_tool_use_ids
                .get("toolu_2")
                .cloned()
                .unwrap_or_default(),
            BTreeSet::from(["toolu_1".to_string(), "toolu_2".to_string()])
        );
    }

    /// CC Messages.tsx:633-650,677-684: renderRange indexes the collapsed
    /// array, not raw messages; grouped tool siblings and results stay intact.
    #[test]
    fn messages_render_range_matches_official_post_group_slice() {
        let input = Arc::new(vec![
            tool_use_in_message(
                "agent-1",
                "msg-agents",
                "toolu_agent_1",
                "Agent",
                Some(serde_json::json!({"description": "Inspect auth"})),
                "Inspect auth",
            ),
            tool_use_in_message(
                "agent-2",
                "msg-agents",
                "toolu_agent_2",
                "Agent",
                Some(serde_json::json!({"description": "Inspect storage"})),
                "Inspect storage",
            ),
            tool_result("result-1", "toolu_agent_1", ToolResultStatus::Success),
            tool_result("result-2", "toolu_agent_2", ToolResultStatus::Success),
            RenderableMessage::user("after", "after-group-sentinel"),
        ]);
        let render = |range| {
            let input = Arc::clone(&input);
            futures::executor::block_on(crate::utils::static_render::render_to_string(
                element! {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            Messages(messages: Arc::clone(&input), render_range: Some(range), hide_logo: true)
                        }.into_any())
                    )
                }.into_any(), Some(80),
            ))
        };
        let second = render((1, 2));
        assert!(second.contains("after-group-sentinel"), "{second:?}");
        assert!(!second.contains("Inspect auth"));
        assert!(render((2, 3)).trim().is_empty());
    }

    #[test]
    fn apply_grouping_groups_agent_tools_from_same_message_and_skips_results() {
        let input = vec![
            tool_use_in_message(
                "agent-1",
                "msg-agents",
                "toolu_agent_1",
                "Agent",
                Some(serde_json::json!({"description": "Inspect auth"})),
                "Inspect auth",
            ),
            tool_use_in_message(
                "agent-2",
                "msg-agents",
                "toolu_agent_2",
                "Agent",
                Some(serde_json::json!({"description": "Inspect storage"})),
                "Inspect storage",
            ),
            tool_result("agent-result-1", "toolu_agent_1", ToolResultStatus::Success),
            tool_result("agent-result-2", "toolu_agent_2", ToolResultStatus::Success),
        ];

        let grouped = apply_grouping(input, false);

        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].uuid, "grouped-agent-1");
        let RenderableMessageKind::GroupedToolUse(GroupedToolUseMessage {
            tool_name,
            messages,
            results,
        }) = &grouped[0].kind
        else {
            panic!("expected grouped tool-use message");
        };
        assert_eq!(tool_name, "Agent");
        // CC `groupToolUses.ts:150-151`: the group carries the tool_use rows
        // and their matching result rows whole; nothing is pre-rendered.
        assert_eq!(
            messages
                .iter()
                .map(|row| assistant_tool_use_id(row).unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["toolu_agent_1", "toolu_agent_2"]
        );
        assert_eq!(
            results
                .iter()
                .map(|row| user_tool_result_id(row).unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["toolu_agent_1", "toolu_agent_2"]
        );
    }

    #[test]
    fn apply_grouping_keeps_singletons_and_non_groupable_tools() {
        let input = vec![
            tool_use_named("bash-1", "toolu_bash_1", "Bash", "echo one"),
            tool_use_in_message(
                "agent-1",
                "msg-agent-1",
                "toolu_agent_1",
                "Agent",
                Some(serde_json::json!({"description": "Inspect auth"})),
                "Inspect auth",
            ),
            tool_use_in_message(
                "agent-2",
                "msg-agent-2",
                "toolu_agent_2",
                "Agent",
                Some(serde_json::json!({"description": "Inspect storage"})),
                "Inspect storage",
            ),
        ];

        let grouped = apply_grouping(input.clone(), false);

        assert_eq!(grouped, input);
    }

    #[test]
    fn apply_grouping_is_identity_in_verbose_mode() {
        // CC groupToolUses.ts:59-64: verbose renders each tool use/result at
        // its original position — even 2+ Agent rows stay ungrouped.
        let input = vec![
            tool_use_in_message(
                "agent-1",
                "msg-agents-verbose",
                "toolu_agent_1",
                "Agent",
                Some(serde_json::json!({"description": "Audit auth"})),
                "Audit auth",
            ),
            tool_use_in_message(
                "agent-2",
                "msg-agents-verbose",
                "toolu_agent_2",
                "Agent",
                Some(serde_json::json!({"description": "Inspect storage"})),
                "Inspect storage",
            ),
            tool_result("agent-result-1", "toolu_agent_1", ToolResultStatus::Success),
            tool_result("agent-result-2", "toolu_agent_2", ToolResultStatus::Success),
        ];
        assert_eq!(apply_grouping(input.clone(), true), input);
        // The same shape groups when verbose is off.
        assert_ne!(apply_grouping(input.clone(), false), input);
    }

    #[test]
    fn collapse_read_search_groups_consecutive_tools_and_results() {
        let input = vec![
            tool_use_named("read-1", "toolu_read", "Read", "src/main.rs"),
            tool_result("result-read", "toolu_read", ToolResultStatus::Success),
            tool_use_named_with_input(
                "search-1",
                "toolu_search",
                "Grep",
                Some(serde_json::json!({"pattern": "needle", "path": "src"})),
                "pattern: \"needle\", path: \"src\"",
            ),
            tool_result("result-search", "toolu_search", ToolResultStatus::Success),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 1,
                search_count: 1,
                list_count: 0,
                bash_count: 0,
                mcp_call_count: 0,
                hint,
                active: false,
                errored: false,
                ..
            }) if hint == "\"needle\""
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn fullscreen_collapse_surfaces_git_outcomes_without_double_counting_bash() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _env = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "1");
        let input = vec![
            tool_use_named(
                "bash-commit",
                "toolu_bash_commit",
                "Bash",
                "git commit -m test",
            ),
            RenderableMessage::user_tool_result(
                "result-commit",
                "toolu_bash_commit",
                "[main abc1234] test",
                false,            )
            // Git-operation detection reads the raw `toolUseResult`.
            .with_tool_use_result(Some(serde_json::json!({
                "stdout": "[main abc1234] test",
                "stderr": "",
                "interrupted": false,
                "noOutputExpected": false
            }))),
        ];

        let collapsed = collapse_read_search_groups(input);
        let RenderableMessageKind::CollapsedReadSearch(group) = &collapsed[0].kind else {
            panic!("expected collapsed Bash group");
        };
        assert_eq!(group.bash_count, 1);
        assert_eq!(group.git_op_bash_count, 1);
        assert_eq!(group.commits.len(), 1);
        assert_eq!(group.commits[0].sha, "abc123");
        assert_eq!(
            group.commits[0].kind,
            crate::tools::shared::git_operation_tracking::CommitKind::Committed
        );
    }

    #[test]
    fn collapse_read_search_counts_unique_file_reads_like_official() {
        let input = vec![
            tool_use_named("read-1", "toolu_read_1", "Read", "src/main.rs"),
            tool_result("result-read-1", "toolu_read_1", ToolResultStatus::Success),
            tool_use_named("read-2", "toolu_read_2", "Read", "src/main.rs"),
            tool_result("result-read-2", "toolu_read_2", ToolResultStatus::Success),
            tool_use_named("bash-read", "toolu_bash_read", "Bash", "cat Cargo.toml"),
            tool_result(
                "result-bash-read",
                "toolu_bash_read",
                ToolResultStatus::Success,
            ),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 1,
                search_count: 0,
                list_count: 0,
                ..
            })
        ));
    }

    #[test]
    fn collapse_read_search_keeps_verbose_tool_use_and_result_entries() {
        // CC `CollapsedReadSearchContent.tsx:46-118,228-255` follows each
        // assistant Read use and resolves its matching result by tool-use ID.
        // Keep both fixture rows Read-shaped instead of borrowing the Bash-only
        // helpers used by neighboring shell-collapse tests.
        // No Read display shape — the raw rides the row and collapse
        // carries it on the verbose entry.
        let read_raw = serde_json::json!({
            "type": "text",
            "file": {
                "filePath": "src/main.rs",
                "content": "fn main() {}",
                "numLines": 1,
                "startLine": 1,
                "totalLines": 1
            }
        });
        let input = vec![
            tool_use_named_with_input(
                "read-1",
                "toolu_read",
                "Read",
                Some(serde_json::json!({"file_path":"src/main.rs"})),
                "src/main.rs",
            ),
            RenderableMessage::user_tool_result(
                "result-read",
                "toolu_read",
                "     1→fn main() {}",
                false,
            )
            .with_tool_use_result(Some(read_raw.clone())),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                verbose_entries,
                ..
            }) if verbose_entries == &vec![
                CollapsedReadSearchEntry::ToolUse {
                    tool_name: "Read".to_string(),
                    input: Some(serde_json::json!({"file_path":"src/main.rs"})),
                    tool_use_id: Some("toolu_read".to_string()),
                    description: "src/main.rs".to_string(),
                    // Seeded unresolved, then overwritten by the arriving
                    // ToolResult in the same pass (CC
                    // `CollapsedReadSearchContent.tsx:66-108` marks the loader
                    // from the resolved result).
                    status: ToolUseStatus::Succeeded,
                },
                CollapsedReadSearchEntry::ToolResult {
                    tool_name: "Read".to_string(),
                    status: ToolResultStatus::Success,
                    content: "     1→fn main() {}".to_string(),                    tool_use_result: Some(read_raw),
                },
            ]
        ));
    }

    #[test]
    fn collapse_classification_keeps_raw_read_identity_independent_of_ui_schema_parse() {
        let invalid_use = tool_use_in_message(
            "invalid-read",
            "invalid-read",
            "toolu_invalid",
            "Read",
            Some(serde_json::json!({"path": "src/main.rs"})),
            "src/main.rs",
        );
        let collapsed = collapse_read_search_groups(vec![
            invalid_use,
            RenderableMessage::user("u-invalid", "next prompt"),
        ]);
        assert!(
            collapsed.iter().any(|message| matches!(
                &message.kind,
                RenderableMessageKind::CollapsedReadSearch(_)
            ))
        );
    }

    #[test]
    fn collapse_read_search_excludes_read_error_result_rows() {
        let error_result =
            RenderableMessage::user_tool_result("result-read", "toolu_read", "disk exploded", true)
                .with_tool_use_result(Some(serde_json::Value::String("disk exploded".to_string())));
        let collapsed = collapse_read_search_groups(vec![
            tool_use_named("read", "toolu_read", "Read", "src/main.rs"),
            error_result,
            RenderableMessage::user("u-error", "next prompt"),
        ]);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                verbose_entries,
                errored: true,
                ..
            }) if matches!(
                verbose_entries.as_slice(),
                [CollapsedReadSearchEntry::ToolUse {
                    tool_use_id: Some(tool_use_id),
                    status: ToolUseStatus::Failed,
                    ..
                }] if tool_use_id == "toolu_read"
            )
        ));
    }

    #[test]
    fn collapse_read_search_schema_invalid_success_matches_official_hidden_summary() {
        let malformed_result = RenderableMessage::user_tool_result(
            "result-read",
            "toolu_read",
            "raw model content",
            false,
        )
        .with_tool_use_result(Some(serde_json::json!({
            "type": "text",
            "file": {"filePath": "src/main.rs"}
        })));
        let collapsed = collapse_read_search_groups(vec![
            tool_use_named("read", "toolu_read", "Read", "src/main.rs"),
            malformed_result,
            RenderableMessage::user("u-malformed", "next prompt"),
        ]);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                verbose_entries,
                errored: false,
                ..
            }) if matches!(
                verbose_entries.as_slice(),
                [CollapsedReadSearchEntry::ToolUse {
                    tool_use_id: Some(tool_use_id),
                        ..
                }] if tool_use_id == "toolu_read"
            )
        ));
    }

    #[test]
    fn collapse_read_search_classifies_bash_search_read_and_list_commands() {
        let input = vec![
            tool_use_named("bash-search", "toolu_bash_search", "Bash", "rg needle src"),
            tool_result(
                "result-search",
                "toolu_bash_search",
                ToolResultStatus::Success,
            ),
            tool_use_named("bash-read", "toolu_bash_read", "Bash", "cat Cargo.toml"),
            tool_result("result-read", "toolu_bash_read", ToolResultStatus::Success),
            tool_use_named("bash-list", "toolu_bash_list", "Bash", "ls src"),
            tool_result("result-list", "toolu_bash_list", ToolResultStatus::Success),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 1,
                search_count: 1,
                list_count: 1,
                bash_count: 0,
                mcp_call_count: 0,
                hint,
                active: false,
                errored: false,
                ..
            }) if hint == "$ ls src"
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn collapse_read_search_counts_official_mcp_search_read_calls_by_server() {
        let input = vec![
            tool_use_named_with_input(
                "mcp-1",
                "toolu_mcp_1",
                "mcp__claude.ai slack__slack_search_public",
                Some(serde_json::json!({"query": "deploys"})),
                "query: \"deploys\"",
            ),
            tool_result("result-mcp-1", "toolu_mcp_1", ToolResultStatus::Success),
            tool_use_named_with_input(
                "mcp-2",
                "toolu_mcp_2",
                "mcp__claude.ai slack__slack_read_channel",
                Some(serde_json::json!({"channel": "eng"})),
                "channel: \"eng\"",
            ),
            tool_result("result-mcp-2", "toolu_mcp_2", ToolResultStatus::Success),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 0,
                search_count: 0,
                list_count: 0,
                bash_count: 0,
                mcp_call_count: 2,
                mcp_server_names,
                hint,
                active: false,
                errored: false,
                ..
            }) if mcp_server_names == &vec!["claude.ai slack".to_string()] && hint == "channel: \"eng\""
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn collapse_read_search_keeps_unknown_mcp_tool_visible_like_official_classifier() {
        let input = vec![
            tool_use_named(
                "mcp-unknown",
                "toolu_mcp_unknown",
                "mcp__claude.ai slack__slack_post_message",
                "channel: \"eng\"",
            ),
            tool_result(
                "result-mcp-unknown",
                "toolu_mcp_unknown",
                ToolResultStatus::Success,
            ),
        ];

        let collapsed = collapse_read_search_groups(input.clone());

        assert_eq!(collapsed, input);
    }

    #[test]
    fn collapse_read_search_tracks_managed_memory_operations_separately() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-collapse-session-memory-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config = root.join("config");
        let config_text = config.display().to_string();
        let _config = TestEnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_text);
        let session_file = config.join("session-memory/nested/session.md");
        let session_search_dir = config.join("session-memory/nested");
        let session_file_text = session_file.display().to_string();
        let session_search_dir_text = session_search_dir.display().to_string();
        let input = vec![
            tool_use_named("mem-read", "toolu_mem_read", "Read", &session_file_text),
            tool_result(
                "result-mem-read",
                "toolu_mem_read",
                ToolResultStatus::Success,
            ),
            tool_use_named_with_input(
                "mem-search",
                "toolu_mem_search",
                "Grep",
                Some(serde_json::json!({
                    "pattern": "todo",
                    "path": session_search_dir_text
                })),
                "pattern: \"todo\"",
            ),
            tool_result(
                "result-mem-search",
                "toolu_mem_search",
                ToolResultStatus::Success,
            ),
            tool_use_named("mem-write", "toolu_mem_write", "Write", &session_file_text),
            tool_result(
                "result-mem-write",
                "toolu_mem_write",
                ToolResultStatus::Success,
            ),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 0,
                search_count: 0,
                list_count: 0,
                memory_read_count: 1,
                memory_search_count: 1,
                memory_write_count: 1,
                active: false,
                errored: false,
                ..
            })
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn collapse_read_search_tracks_memory_edit_operations_separately() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-collapse-memory-edit-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_text = root.join("config").display().to_string();
        let _config = TestEnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_text);
        let session_file = root
            .join("config/session-memory/nested/session.md")
            .display()
            .to_string();
        let input = vec![
            tool_use_named("mem-edit", "toolu_mem_edit", "Edit", &session_file),
            tool_result(
                "result-mem-edit",
                "toolu_mem_edit",
                ToolResultStatus::Success,
            ),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 0,
                search_count: 0,
                list_count: 0,
                memory_write_count: 1,
                team_memory_write_count: 0,
                active: false,
                errored: false,
                ..
            })
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn collapse_read_search_team_memory_tracking_ignores_growthbook_delivery() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-collapse-team-memory-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let root_text = root.display().to_string();
        let _override = TestEnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &root_text);
        let _enabled = TestEnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_herring_clock".to_string(),
            serde_json::json!(true),
        )]));
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_herring_clock".to_string(),
            serde_json::json!(true),
        )]));
        let _config = TestGlobalConfigGuard(crate::utils::config::replace_test_global_config(
            Some(config),
        ));
        let team_file = root.join("team/MEMORY.md").display().to_string();
        let team_search_dir = root.join("team/topics").display().to_string();
        let input = vec![
            tool_use_named("team-read", "toolu_team_read", "Read", &team_file),
            tool_result(
                "result-team-read",
                "toolu_team_read",
                ToolResultStatus::Success,
            ),
            tool_use_named_with_input(
                "team-search",
                "toolu_team_search",
                "Grep",
                Some(serde_json::json!({
                    "pattern": "todo",
                    "path": team_search_dir
                })),
                "pattern: \"todo\"",
            ),
            tool_result(
                "result-team-search",
                "toolu_team_search",
                ToolResultStatus::Success,
            ),
            tool_use_named("team-write", "toolu_team_write", "Write", &team_file),
            tool_result(
                "result-team-write",
                "toolu_team_write",
                ToolResultStatus::Success,
            ),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        // The path shape is still a team-memory path; only the cohort gate,
        // which no longer reads the injected GrowthBook cache, keeps the
        // team-memory counters at zero.
        assert!(crate::memdir::team_mem_paths::is_team_mem_path(
            std::path::Path::new(&team_file)
        ));
        assert!(!crate::memdir::team_mem_paths::is_team_memory_enabled());
        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                team_memory_read_count: 0,
                team_memory_search_count: 0,
                team_memory_write_count: 0,
                active: false,
                errored: false,
                ..
            })
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn collapse_read_search_absorbs_toolsearch_without_counts_or_hint_override() {
        let input = vec![
            tool_use_named("read-1", "toolu_read", "Read", "src/lib.rs"),
            tool_result("result-read", "toolu_read", ToolResultStatus::Success),
            tool_use_named(
                "toolsearch-1",
                "toolu_toolsearch",
                "ToolSearch",
                "load deferred tool schemas",
            ),
            RenderableMessage::user_tool_result(
                "result-toolsearch",
                "toolu_toolsearch",
                "loaded",
                false,
            ),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 1,
                search_count: 0,
                list_count: 0,
                bash_count: 0,
                mcp_call_count: 0,
                hint,
                verbose_entries,
                ..
            }) if hint == "src/lib.rs"
                && verbose_entries.iter().any(|entry| matches!(
                    entry,
                    CollapsedReadSearchEntry::ToolUse { tool_name, .. } if tool_name == "ToolSearch"
                ))
                && verbose_entries.iter().any(|entry| matches!(
                    entry,
                    CollapsedReadSearchEntry::ToolResult { tool_name, content, .. }
                        if tool_name == "ToolSearch" && content == "loaded"
                ))
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn collapse_read_search_absorbs_repl_wrapper_without_counts_or_hint_override() {
        let input = vec![
            tool_use_named("read-1", "toolu_read", "Read", "src/lib.rs"),
            tool_result("result-read", "toolu_read", ToolResultStatus::Success),
            tool_use_named("repl-1", "toolu_repl", "REPL", "batch file operations"),
            RenderableMessage::user_tool_result("result-repl", "toolu_repl", "ok", false),
            tool_use_named_with_input(
                "grep-1",
                "toolu_grep",
                "Grep",
                Some(serde_json::json!({"pattern": "needle"})),
                "pattern: \"needle\"",
            ),
            tool_result("result-grep", "toolu_grep", ToolResultStatus::Success),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 1,
                search_count: 1,
                list_count: 0,
                bash_count: 0,
                mcp_call_count: 0,
                hint,
                verbose_entries,
                ..
            }) if hint == "\"needle\""
                && verbose_entries.iter().any(|entry| matches!(
                    entry,
                    CollapsedReadSearchEntry::ToolUse { tool_name, .. } if tool_name == "REPL"
                ))
                && verbose_entries.iter().any(|entry| matches!(
                    entry,
                    CollapsedReadSearchEntry::ToolResult { tool_name, content, .. }
                        if tool_name == "REPL" && content == "ok"
                ))
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn collapse_read_search_keeps_non_search_bash_visible_on_main_screen() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _fullscreen = TestEnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let input = vec![
            tool_use_named("bash-run", "toolu_bash_run", "Bash", "npm test"),
            tool_result("result-run", "toolu_bash_run", ToolResultStatus::Success),
        ];

        let collapsed = collapse_read_search_groups(input.clone());

        assert_eq!(collapsed, input);
    }

    #[test]
    fn collapse_read_search_classifies_powershell_read_and_search_like_official() {
        let input = vec![
            tool_use_named(
                "ps-read",
                "toolu_ps_read",
                "PowerShell",
                "Get-Content .\\README.md",
            ),
            tool_result("result-read", "toolu_ps_read", ToolResultStatus::Success),
            tool_use_named(
                "ps-search",
                "toolu_ps_search",
                "PowerShell",
                "Select-String -Path src\\*.rs -Pattern needle",
            ),
            tool_result(
                "result-search",
                "toolu_ps_search",
                ToolResultStatus::Success,
            ),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 1,
                search_count: 1,
                list_count: 0,
                bash_count: 0,
                mcp_call_count: 0,
                hint,
                verbose_entries,
                ..
            }) if hint == "$ Select-String -Path src\\*.rs -Pattern needle"
                && verbose_entries.iter().any(|entry| matches!(
                    entry,
                    CollapsedReadSearchEntry::ToolUse { tool_name, description, .. }
                        if tool_name == "PowerShell" && description.contains("Get-Content")
                ))
                && verbose_entries.iter().any(|entry| matches!(
                    entry,
                    CollapsedReadSearchEntry::ToolUse { tool_name, description, .. }
                        if tool_name == "PowerShell" && description.contains("Select-String")
                ))
        ));
        assert_eq!(collapsed[1].uuid, "u1");
    }

    #[test]
    fn collapse_read_search_defers_skippable_rows_after_summary() {
        let input = vec![
            tool_use_named("read-1", "toolu_read", "Read", "src/lib.rs"),
            RenderableMessage::assistant_block(
                "thinking",
                crate::types::message::AssistantContent::Thinking {
                    text: "thinking".to_string(),
                    signature: String::new(),
                },
            ),
            tool_result("result-read", "toolu_read", ToolResultStatus::Error),
            RenderableMessage::assistant_block(
                "a1",
                crate::types::message::AssistantContent::Text("done".to_string()),
            ),
        ];

        let collapsed = collapse_read_search_groups(input);
        let ids = collapsed
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["collapsed-read-1", "thinking", "a1"]);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                errored: true,
                ..
            })
        ));
    }

    #[test]
    fn collapse_read_search_absorbs_pre_tool_use_hook_timing() {
        let input = vec![
            tool_use_named("read-1", "toolu_read", "Read", "src/lib.rs"),
            stop_hook_summary("hook-1", Some("PreToolUse"), 2, false, false, Some(250)),
            tool_result("result-read", "toolu_read", ToolResultStatus::Success),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);
        let ids = collapsed
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["collapsed-read-1", "u1"]);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                hook_total_ms: Some(250),
                hook_count: 2,
                hook_infos,
                ..
            }) if hook_infos.len() == 1
        ));
    }

    #[test]
    fn collapse_read_search_keeps_nested_memory_attachment_outside_deferred_badge() {
        let input = vec![
            tool_use_named("read-1", "toolu_read", "Read", "src/lib.rs"),
            RenderableMessage {
                uuid: "nested-memory".to_string(),
                kind: RenderableMessageKind::Attachment(Attachment::NestedMemory {
                    path: "project-memory.md".to_string(),
                    content: crate::utils::claudemd::MemoryFileInfo {
                        path: "project-memory.md".to_string(),
                        memory_type: "Project".to_string(),
                        content: String::new(),
                        parent: None,
                        globs: None,
                        content_differs_from_disk: None,
                        raw_content: None,
                    },
                    display_path: "project-memory.md".to_string(),
                }),
            },
            tool_result("result-read", "toolu_read", ToolResultStatus::Success),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);
        let ids = collapsed
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["nested-memory", "collapsed-read-1", "u1"]);
    }

    #[test]
    fn collapse_read_search_absorbs_relevant_memories_into_memory_read_count() {
        let input = vec![
            tool_use_named("read-1", "toolu_read", "Read", "src/lib.rs"),
            RenderableMessage {
                uuid: "memories".to_string(),
                kind: RenderableMessageKind::Attachment(Attachment::RelevantMemories {
                    memories: vec![
                        RelevantMemory {
                            path: "/tmp/alpha.md".to_string(),
                            content: "alpha".to_string(),
                            mtime_ms: Some(1),
                            header: None,
                            limit: None,
                        },
                        RelevantMemory {
                            path: "/tmp/beta.md".to_string(),
                            content: "beta".to_string(),
                            mtime_ms: Some(2),
                            header: None,
                            limit: None,
                        },
                    ],
                }),
            },
            tool_result("result-read", "toolu_read", ToolResultStatus::Success),
            RenderableMessage::user("u1", "next prompt"),
        ];

        let collapsed = collapse_read_search_groups(input);
        let ids = collapsed
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["collapsed-read-1", "u1"]);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: 1,
                memory_read_count: 2,
                relevant_memories,
                ..
            }) if relevant_memories.len() == 2
        ));
    }

    #[test]
    fn collapse_teammate_shutdowns_batches_consecutive_teammate_shutdowns() {
        let input = vec![
            teammate_shutdown("mate-1"),
            teammate_shutdown("mate-2"),
            RenderableMessage::assistant_block(
                "text",
                crate::types::message::AssistantContent::Text("done".to_string()),
            ),
            teammate_shutdown("mate-3"),
        ];

        let collapsed = collapse_teammate_shutdowns(input);

        assert_eq!(collapsed.len(), 3);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::Attachment(Attachment::TeammateShutdownBatch { count: 2 })
        ));
        assert_eq!(collapsed[1].uuid, "text");
        assert!(matches!(
            &collapsed[2].kind,
            RenderableMessageKind::Attachment(Attachment::TaskStatus { .. })
        ));
    }

    #[test]
    fn collapse_hook_summaries_merges_consecutive_same_labeled_hooks() {
        let input = vec![
            stop_hook_summary("hook-1", Some("PreToolUse"), 1, false, false, Some(120)),
            stop_hook_summary("hook-2", Some("PreToolUse"), 2, true, true, Some(250)),
            stop_hook_summary("hook-3", Some("PostToolUse"), 1, false, false, Some(50)),
        ];

        let collapsed = collapse_hook_summaries(input);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            &collapsed[0].kind,
            RenderableMessageKind::System(SystemMessage::StopHookSummary {
                hook_label: Some(label),
                hook_count: 3,
                prevented_continuation: true,
                has_output: true,
                total_duration_ms: Some(250),
                ..
            }) if label == "PreToolUse"
        ));
        assert_eq!(collapsed[1].uuid, "hook-3");
    }

    #[test]
    fn collapse_hook_summaries_leaves_unlabeled_stop_summaries_standalone() {
        let input = vec![
            stop_hook_summary("stop-1", None, 1, false, false, None),
            stop_hook_summary("stop-2", None, 1, false, false, None),
        ];

        let collapsed = collapse_hook_summaries(input);
        let ids = collapsed
            .iter()
            .map(|message| message.uuid.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["stop-1", "stop-2"]);
    }

    #[test]
    fn row_metadata_matches_official_message_row_boundaries() {
        let rows = vec![
            RenderableMessage::user("u1", "first"),
            RenderableMessage::user("u2", "second"),
            RenderableMessage {
                uuid: "collapsed".to_string(),
                kind: RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                    read_count: 1,
                    search_count: 0,
                    list_count: 0,
                    bash_count: 0,
                    git_op_bash_count: 0,
                    commits: Vec::new(),
                    pushes: Vec::new(),
                    branches: Vec::new(),
                    prs: Vec::new(),
                    mcp_call_count: 0,
                    mcp_server_names: Vec::new(),
                    memory_search_count: 0,
                    memory_read_count: 0,
                    memory_write_count: 0,
                    team_memory_search_count: 0,
                    team_memory_read_count: 0,
                    team_memory_write_count: 0,
                    hook_total_ms: None,
                    hook_count: 0,
                    hook_infos: Vec::new(),
                    relevant_memories: Vec::new(),
                    verbose_entries: Vec::new(),
                    hint: "Reading".to_string(),
                    active: true,
                    errored: false,
                }),
            },
            RenderableMessage::assistant_block(
                "thinking",
                crate::types::message::AssistantContent::Thinking {
                    text: "skip".to_string(),
                    signature: String::new(),
                },
            ),
            RenderableMessage::assistant_block(
                "a1",
                crate::types::message::AssistantContent::Text("content".to_string()),
            ),
        ];

        assert!(!is_user_continuation(&rows, 0));
        assert!(is_user_continuation(&rows, 1));
        assert!(has_content_after_index(&rows, 2));
        assert!(!has_content_after_index(&rows, 0));
    }
}
