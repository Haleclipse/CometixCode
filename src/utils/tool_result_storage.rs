//! Tool-result storage and replacement helpers.
//! Maps to CC `utils/toolResultStorage.ts`.
//!
//! The official implementation can persist large tool results and replace
//! them with previews (`buildLargeToolResultMessage`:189,
//! `generatePreview`:339, driven by `Tool.maxResultSizeChars` Tool.ts:466).
//! Session persistence is live; mapped text results use the per-tool threshold
//! before model delivery, and the aggregate per-message budget remains a
//! separate replay-stable pass. Non-text result persistence stays explicit.

pub const TOOL_RESULT_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";
/// Maps to CC `utils/toolResultStorage.ts` `PERSISTED_OUTPUT_TAG`.
pub const PERSISTED_OUTPUT_TAG: &str = "<persisted-output>";
/// Maps to CC `utils/toolResultStorage.ts` `PERSISTED_OUTPUT_CLOSING_TAG`.
pub const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";
pub use crate::constants::tool_limits::MAX_TOOL_RESULTS_PER_MESSAGE_CHARS;
pub const TOOL_RESULTS_SUBDIR: &str = "tool-results";
/// Maps to CC `utils/toolResultStorage.ts` `PREVIEW_SIZE_BYTES`.
pub const PREVIEW_SIZE_BYTES: usize = 2000;
/// Maps to CC `utils/toolResultStorage.ts` `PERSIST_THRESHOLD_OVERRIDE_FLAG`'s
/// sibling gate used by `provisionContentReplacementState(...)`.
pub const CONTENT_REPLACEMENT_FEATURE: &str = "tengu_hawthorn_steeple";
/// Maps to CC `PERSIST_THRESHOLD_OVERRIDE_FLAG`.
pub const PERSIST_THRESHOLD_OVERRIDE_FEATURE: &str = "tengu_satin_quoll";
/// Maps to CC `utils/toolResultStorage.ts` `getPerMessageBudgetLimit()`
/// GrowthBook override name.
pub const PER_MESSAGE_BUDGET_LIMIT_FEATURE: &str = "tengu_hawthorn_window";

/// Maps to CC `utils/toolResultStorage.ts` `ContentReplacementState`.
///
/// Cometix keeps session writes disabled, so replacements are in-memory clear
/// markers rather than persisted-output preview strings. The important cache
/// contract still applies: once a tool_use_id has been seen, its replacement
/// decision is frozen and re-applied byte-identically on later passes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentReplacementState {
    pub seen_ids: std::collections::HashSet<String>,
    pub replacements: std::collections::HashMap<String, String>,
}

impl ContentReplacementState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Maps to CC `utils/toolResultStorage.ts` `ContentReplacementRecord`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentReplacementRecord {
    /// Official discriminator. Currently always `tool-result`.
    #[serde(default = "tool_result_content_replacement_kind")]
    pub kind: String,
    pub tool_use_id: String,
    pub replacement: String,
}

fn tool_result_content_replacement_kind() -> String {
    "tool-result".to_string()
}

impl ContentReplacementRecord {
    pub fn tool_result(tool_use_id: String, replacement: String) -> Self {
        Self {
            kind: tool_result_content_replacement_kind(),
            tool_use_id,
            replacement,
        }
    }
}

/// Maps to CC `utils/toolResultStorage.ts` transcript-loaded
/// `ContentReplacementRecord[]` handling before
/// `reconstructContentReplacementState(...)`.
pub fn content_replacement_records_from_values(
    values: &[serde_json::Value],
) -> Vec<ContentReplacementRecord> {
    values
        .iter()
        .filter_map(|value| {
            let kind = value.get("kind")?.as_str()?;
            if kind != "tool-result" {
                return None;
            }
            let tool_use_id = value
                .get("toolUseId")
                .or_else(|| value.get("tool_use_id"))?
                .as_str()?;
            let replacement = value.get("replacement")?.as_str()?;
            Some(ContentReplacementRecord::tool_result(
                tool_use_id.to_string(),
                replacement.to_string(),
            ))
        })
        .collect()
}

/// Maps to CC `utils/toolResultStorage.ts`
/// `reconstructContentReplacementState(...)`.
pub fn reconstruct_content_replacement_state(
    messages: &[crate::types::message::Message],
    records: &[ContentReplacementRecord],
    inherited_replacements: Option<&std::collections::HashMap<String, String>>,
) -> ContentReplacementState {
    let mut state = ContentReplacementState::new();
    let candidate_ids = collect_tool_result_groups(messages)
        .into_iter()
        .flatten()
        .map(|candidate| candidate.tool_use_id)
        .collect::<std::collections::HashSet<_>>();

    for id in &candidate_ids {
        state.seen_ids.insert(id.clone());
    }
    for record in records {
        if candidate_ids.contains(&record.tool_use_id) {
            state
                .replacements
                .insert(record.tool_use_id.clone(), record.replacement.clone());
        }
    }
    if let Some(inherited) = inherited_replacements {
        for (id, replacement) in inherited {
            if candidate_ids.contains(id) && !state.replacements.contains_key(id) {
                state.replacements.insert(id.clone(), replacement.clone());
            }
        }
    }
    state
}

/// Maps to CC `utils/toolResultStorage.ts:1001-1012`
/// `reconstructForSubagentResume(...)`.
///
/// The AgentTool-resume variant of the reconstruction: it encapsulates the
/// feature-flag gate (an absent parent state means the feature is off, so the
/// resumed agent gets no state either) plus the parent gap-fill that covers a
/// fork's inherited `mustReapply` replacements, which the sidechain has applied
/// but never recorded.
pub fn reconstruct_for_subagent_resume(
    parent_state: Option<&ContentReplacementState>,
    resumed_messages: &[crate::types::message::Message],
    sidechain_records: &[ContentReplacementRecord],
) -> Option<ContentReplacementState> {
    let parent_state = parent_state?;
    Some(reconstruct_content_replacement_state(
        resumed_messages,
        sidechain_records,
        Some(&parent_state.replacements),
    ))
}

/// Maps to CC `utils/toolResultStorage.ts`
/// `provisionContentReplacementState(...)`.
///
/// The official feature gate returns `undefined` when disabled. Rust mirrors
/// that with `None`; callers skip `applyToolResultBudget(...)` when the state is
/// absent but keep this helper at the official query/REPL provisioning seam.
pub fn provision_content_replacement_state(
    initial_messages: Option<&[crate::types::message::Message]>,
    initial_content_replacements: &[ContentReplacementRecord],
) -> Option<ContentReplacementState> {
    provision_content_replacement_state_with_enabled(
        is_content_replacement_enabled(),
        initial_messages,
        initial_content_replacements,
    )
}

fn provision_content_replacement_state_with_enabled(
    enabled: bool,
    initial_messages: Option<&[crate::types::message::Message]>,
    initial_content_replacements: &[ContentReplacementRecord],
) -> Option<ContentReplacementState> {
    if !enabled {
        return None;
    }
    Some(if let Some(messages) = initial_messages {
        reconstruct_content_replacement_state(messages, initial_content_replacements, None)
    } else {
        ContentReplacementState::new()
    })
}

/// Maps to CC `getFeatureValue_CACHED_MAY_BE_STALE('tengu_hawthorn_steeple', false)`
/// as consumed by `provisionContentReplacementState(...)`.
///
/// Cometix resolves the gate from the source-controlled switch table instead of
/// GrowthBook.
pub fn is_content_replacement_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ToolResultContentReplacement,
    )
}

/// Maps to CC `utils/toolResultStorage.ts` `getPerMessageBudgetLimit()`.
///
/// Cometix resolves the `tengu_hawthorn_window` override from the
/// source-controlled switch table instead of GrowthBook.
pub fn get_per_message_budget_limit() -> usize {
    let override_value = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::PerMessageBudgetLimitOverride,
    )
    .then(serde_json::Value::default);
    per_message_budget_limit_from_feature_value(override_value.as_ref())
}

/// Maps to CC `getPersistenceThreshold(toolName, declaredMaxResultSizeChars)`.
///
/// Cometix resolves the `tengu_satin_quoll` override map from the
/// source-controlled switch table instead of GrowthBook.
pub fn get_persistence_threshold(tool_name: &str, declared_max: usize) -> usize {
    if declared_max == usize::MAX {
        return usize::MAX;
    }
    let overrides = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::PersistThresholdOverrides,
    )
    .then(serde_json::Map::new);
    if let Some(override_value) = overrides
        .as_ref()
        .and_then(|values| values.get(tool_name))
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        return override_value as usize;
    }
    declared_max.min(crate::constants::tool_limits::DEFAULT_MAX_RESULT_SIZE_CHARS)
}

fn per_message_budget_limit_from_feature_value(value: Option<&serde_json::Value>) -> usize {
    let Some(number) = value.and_then(|value| value.as_f64()) else {
        return MAX_TOOL_RESULTS_PER_MESSAGE_CHARS;
    };
    if number.is_finite() && number > 0.0 {
        number as usize
    } else {
        MAX_TOOL_RESULTS_PER_MESSAGE_CHARS
    }
}

/// Maps to CC `utils/toolResultStorage.ts` `isToolResultContentEmpty(...)`.
pub fn is_tool_result_content_empty(
    content: &str,
    content_blocks: &[crate::types::message::ToolResultContentBlock],
) -> bool {
    content_blocks.is_empty() && content.trim().is_empty()
}

/// Maps to CC `utils/toolResultStorage.ts` `isContentAlreadyCompacted(...)`.
pub fn is_content_already_compacted(content: &str) -> bool {
    // Official budget-produced previews always start with this tag; use
    // `starts_with` to avoid false positives when a tool result merely includes
    // the tag later in ordinary output.
    content.starts_with(PERSISTED_OUTPUT_TAG)
}

/// Maps to CC `utils/toolResultStorage.ts` empty-content marker in
/// `maybePersistLargeToolResult(...)`.
pub fn empty_tool_result_content_marker(tool_name: &str) -> String {
    format!("({tool_name} completed with no output)")
}

/// Maps to CC `utils/toolResultStorage.ts` `getToolResultsDir()`.
pub fn get_tool_results_dir() -> std::path::PathBuf {
    crate::utils::session_storage::get_project_dir(
        &crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string(),
    )
    .join(crate::bootstrap::state::get_session_id())
    .join(TOOL_RESULTS_SUBDIR)
}

/// Maps to CC `utils/toolResultStorage.ts` `getToolResultPath(...)`.
pub fn get_tool_result_path(id: &str, is_json: bool) -> std::path::PathBuf {
    let ext = if is_json { "json" } else { "txt" };
    get_tool_results_dir().join(format!("{id}.{ext}"))
}

/// Maps to CC `utils/toolResultStorage.ts` `ensureToolResultsDir()`.
pub fn ensure_tool_results_dir() {
    // Official code intentionally ignores mkdir errors because the directory may
    // already exist; callers handle any later write error with file-specific
    // context.
    let _ = std::fs::create_dir_all(get_tool_results_dir());
}

/// Maps to CC `utils/toolResultStorage.ts` `PersistedToolResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedToolResult {
    pub filepath: std::path::PathBuf,
    pub original_size: usize,
    pub is_json: bool,
    pub preview: String,
    pub has_more: bool,
}

/// Maps to CC `utils/toolResultStorage.ts` `PersistToolResultError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistToolResultError {
    pub error: String,
}

/// Maps to CC `utils/toolResultStorage.ts` `persistToolResult(...)` string arm.
pub fn persist_tool_result_text(
    content: &str,
    tool_use_id: &str,
) -> Result<PersistedToolResult, PersistToolResultError> {
    ensure_tool_results_dir();
    let filepath = get_tool_result_path(tool_use_id, false);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&filepath)
    {
        Ok(mut file) => {
            if let Err(error) = std::io::Write::write_all(&mut file, content.as_bytes()) {
                tracing::error!(?error, ?filepath, "failed to persist tool result");
                return Err(PersistToolResultError {
                    error: error.to_string(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            tracing::error!(?error, ?filepath, "failed to persist tool result");
            return Err(PersistToolResultError {
                error: error.to_string(),
            });
        }
    }

    let (preview, has_more) = generate_preview(content, PREVIEW_SIZE_BYTES);
    Ok(PersistedToolResult {
        filepath,
        original_size: content.encode_utf16().count(),
        is_json: false,
        preview,
        has_more,
    })
}

/// Maps to CC `utils/toolResultStorage.ts` `buildLargeToolResultMessage(...)`.
pub fn build_large_tool_result_message(result: &PersistedToolResult) -> String {
    use crate::utils::format::format_file_size;
    let mut message = String::new();
    message.push_str(PERSISTED_OUTPUT_TAG);
    message.push('\n');
    message.push_str(&format!(
        "Output too large ({}). Full output saved to: {}\n\n",
        format_file_size(result.original_size as u64),
        result.filepath.display()
    ));
    message.push_str(&format!(
        "Preview (first {}):\n",
        format_file_size(PREVIEW_SIZE_BYTES as u64)
    ));
    message.push_str(&result.preview);
    if result.has_more {
        message.push_str("\n...\n");
    } else {
        message.push('\n');
    }
    message.push_str(PERSISTED_OUTPUT_CLOSING_TAG);
    message
}

fn byte_index_after_utf16_units(content: &str, max_units: usize) -> usize {
    let mut units = 0usize;
    let mut end = 0usize;
    for (index, character) in content.char_indices() {
        let character_units = character.len_utf16();
        if units.saturating_add(character_units) > max_units {
            break;
        }
        units += character_units;
        end = index + character.len_utf8();
    }
    end
}

/// Maps to CC `utils/toolResultStorage.ts` `generatePreview(...)`.
pub fn generate_preview(content: &str, preview_size_units: usize) -> (String, bool) {
    let total_units = content.encode_utf16().count();
    if total_units <= preview_size_units {
        return (content.to_string(), false);
    }
    let end = byte_index_after_utf16_units(content, preview_size_units);
    // Prefer a newline cut in the latter half of the UTF-16 window.
    let truncated = &content[..end];
    let cut = truncated
        .rfind('\n')
        .filter(|&index| truncated[..index].encode_utf16().count() > preview_size_units / 2)
        .unwrap_or(end);
    // SAFETY DEVIATION: JS can split a surrogate pair at the exact code-unit
    // boundary. Rust strings cannot represent a lone surrogate, so keep the
    // preceding complete scalar instead.
    (content[..cut].to_string(), true)
}

/// Source-shaped per-tool result persistence for already-mapped text output.
pub fn maybe_persist_large_tool_result_text(
    content: &str,
    tool_name: &str,
    tool_use_id: &str,
    declared_max: usize,
) -> String {
    let threshold = get_persistence_threshold(tool_name, declared_max);
    if content.encode_utf16().count() <= threshold {
        return content.to_string();
    }
    persist_tool_result_text(content, tool_use_id)
        .map(|result| build_large_tool_result_message(&result))
        .unwrap_or_else(|_| content.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyToolResultBudgetResult {
    pub messages: Vec<crate::types::message::Message>,
    /// Maps to CC `utils/toolResultStorage.ts` `newlyReplaced` records passed
    /// to `query.ts`'s optional `recordContentReplacement(...)` callback.
    pub newly_replaced: Vec<ContentReplacementRecord>,
}

/// Maps to: CC `utils/toolResultStorage.ts` `applyToolResultBudget(...)`.
pub fn apply_tool_result_budget(
    messages: Vec<crate::types::message::Message>,
) -> Vec<crate::types::message::Message> {
    let mut state = ContentReplacementState::new();
    apply_tool_result_budget_with_state(messages, &mut state)
}

/// Maps to CC `utils/toolResultStorage.ts` `enforceToolResultBudget(...)` plus
/// `applyToolResultBudget(...)` with a provisioned `ContentReplacementState`.
pub fn apply_tool_result_budget_with_state(
    messages: Vec<crate::types::message::Message>,
    state: &mut ContentReplacementState,
) -> Vec<crate::types::message::Message> {
    apply_tool_result_budget_with_state_and_skip_tool_names(
        messages,
        state,
        &std::collections::HashSet::new(),
    )
}

/// Maps to CC `utils/toolResultStorage.ts` `applyToolResultBudget(...)`
/// `skipToolNames` / `enforceToolResultBudget(...)` skip handling.
pub fn apply_tool_result_budget_with_state_and_skip_tool_names(
    messages: Vec<crate::types::message::Message>,
    state: &mut ContentReplacementState,
    skip_tool_names: &std::collections::HashSet<String>,
) -> Vec<crate::types::message::Message> {
    apply_tool_result_budget_with_state_and_skip_tool_names_result(messages, state, skip_tool_names)
        .messages
}

/// Maps to CC `utils/toolResultStorage.ts` `enforceToolResultBudget(...)`
/// return value `{ messages, newlyReplaced }` consumed by
/// `applyToolResultBudget(...)`.
pub fn apply_tool_result_budget_with_state_and_skip_tool_names_result(
    mut messages: Vec<crate::types::message::Message>,
    state: &mut ContentReplacementState,
    skip_tool_names: &std::collections::HashSet<String>,
) -> ApplyToolResultBudgetResult {
    let groups = collect_tool_result_groups(&messages);
    let limit = get_per_message_budget_limit();
    let tool_name_by_use_id = if skip_tool_names.is_empty() {
        std::collections::HashMap::new()
    } else {
        build_tool_name_map(&messages)
    };
    let mut replacement_map = std::collections::HashMap::<String, String>::new();
    let mut newly_replaced = Vec::<ContentReplacementRecord>::new();

    for group in groups {
        let mut must_reapply = Vec::<ToolResultCandidate>::new();
        let mut frozen = Vec::<ToolResultCandidate>::new();
        let mut fresh = Vec::<ToolResultCandidate>::new();

        for candidate in group {
            if state.replacements.contains_key(&candidate.tool_use_id) {
                must_reapply.push(candidate);
            } else if state.seen_ids.contains(&candidate.tool_use_id) {
                frozen.push(candidate);
            } else {
                fresh.push(candidate);
            }
        }

        for candidate in must_reapply {
            if let Some(replacement) = state.replacements.get(&candidate.tool_use_id) {
                replacement_map.insert(candidate.tool_use_id, replacement.clone());
            }
        }

        if fresh.is_empty() {
            for candidate in frozen {
                state.seen_ids.insert(candidate.tool_use_id);
            }
            continue;
        }

        let mut skipped_fresh = Vec::<ToolResultCandidate>::new();
        let mut eligible_fresh = Vec::<ToolResultCandidate>::new();
        for candidate in fresh {
            let should_skip = tool_name_by_use_id
                .get(&candidate.tool_use_id)
                .is_some_and(|tool_name| skip_tool_names.contains(tool_name));
            if should_skip {
                skipped_fresh.push(candidate);
            } else {
                eligible_fresh.push(candidate);
            }
        }

        let frozen_size = frozen.iter().map(|candidate| candidate.len).sum::<usize>();
        let fresh_size = eligible_fresh
            .iter()
            .map(|candidate| candidate.len)
            .sum::<usize>();
        let mut selected = Vec::<ToolResultCandidate>::new();
        if frozen_size + fresh_size > limit {
            let mut remaining = frozen_size + fresh_size;
            let mut clearable = eligible_fresh
                .iter()
                .cloned()
                .filter(|candidate| candidate.clearable)
                .collect::<Vec<_>>();
            clearable.sort_by(|left, right| right.len.cmp(&left.len));
            for candidate in clearable {
                if remaining <= limit {
                    break;
                }
                remaining = remaining.saturating_sub(candidate.len);
                selected.push(candidate);
            }
        }

        let selected_ids = selected
            .iter()
            .map(|candidate| candidate.tool_use_id.clone())
            .collect::<std::collections::HashSet<_>>();
        for candidate in frozen
            .into_iter()
            .chain(skipped_fresh.into_iter())
            .chain(eligible_fresh.into_iter())
            .filter(|candidate| !selected_ids.contains(&candidate.tool_use_id))
        {
            state.seen_ids.insert(candidate.tool_use_id);
        }

        for candidate in selected {
            state.seen_ids.insert(candidate.tool_use_id.clone());
            state.replacements.insert(
                candidate.tool_use_id.clone(),
                TOOL_RESULT_CLEARED_MESSAGE.to_string(),
            );
            replacement_map.insert(
                candidate.tool_use_id.clone(),
                TOOL_RESULT_CLEARED_MESSAGE.to_string(),
            );
            newly_replaced.push(ContentReplacementRecord::tool_result(
                candidate.tool_use_id,
                TOOL_RESULT_CLEARED_MESSAGE.to_string(),
            ));
        }
    }

    replace_tool_result_contents(&mut messages, &replacement_map);
    ApplyToolResultBudgetResult {
        messages,
        newly_replaced,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolResultCandidate {
    message_index: usize,
    content_index: usize,
    tool_use_id: String,
    len: usize,
    clearable: bool,
}

/// Maps to CC `utils/toolResultStorage.ts` `buildToolNameMap(...)`.
fn build_tool_name_map(
    messages: &[crate::types::message::Message],
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for message in messages {
        let crate::types::message::Message::Assistant(assistant) = message else {
            continue;
        };
        for content in &assistant.content {
            let crate::types::message::AssistantContent::ToolUse(tool_use) = content else {
                continue;
            };
            map.insert(tool_use.id.0.clone(), tool_use.name.clone());
        }
    }
    map
}

/// Maps to CC `utils/toolResultStorage.ts` `collectCandidatesByMessage(...)`.
///
/// `normalizeMessagesForAPI(...)` merges adjacent user-like messages until an
/// assistant boundary, so aggregate tool-result budgeting must evaluate the
/// same API-level group rather than each stored `UserMessage` independently.
/// Rust messages do not yet carry assistant fragment ids, so same-id assistant
/// rejoin handling is intentionally left to the future transcript-id port.
fn collect_tool_result_groups(
    messages: &[crate::types::message::Message],
) -> Vec<Vec<ToolResultCandidate>> {
    let mut groups = Vec::<Vec<ToolResultCandidate>>::new();
    let mut current = Vec::<ToolResultCandidate>::new();

    let flush = |groups: &mut Vec<Vec<ToolResultCandidate>>,
                 current: &mut Vec<ToolResultCandidate>| {
        if !current.is_empty() {
            groups.push(std::mem::take(current));
        }
    };

    for (message_index, message) in messages.iter().enumerate() {
        match message {
            crate::types::message::Message::User(user) => {
                for (content_index, content) in user.content.iter().enumerate() {
                    let crate::types::message::UserContent::ToolResult(result) = content else {
                        continue;
                    };
                    let has_api_content_blocks = !result.content_blocks.is_empty();
                    if !has_api_content_blocks && result.content.is_empty() {
                        continue;
                    }
                    if is_content_already_compacted(&result.content) {
                        continue;
                    }
                    let len = if has_api_content_blocks {
                        // CC `contentSize(...)` only counts text blocks for
                        // array content. Rust currently only models
                        // non-text `tool_reference` blocks, so their
                        // aggregate budget contribution is zero.
                        0
                    } else {
                        result.content.chars().count()
                    };
                    current.push(ToolResultCandidate {
                        message_index,
                        content_index,
                        tool_use_id: result.tool_use_id.0.clone(),
                        len,
                        clearable: !has_api_content_blocks
                            && !result.content.is_empty()
                            && result.content != TOOL_RESULT_CLEARED_MESSAGE,
                    });
                }
            }
            crate::types::message::Message::Assistant(_) => flush(&mut groups, &mut current),
            crate::types::message::Message::System(_)
            | crate::types::message::Message::Attachment(_)
            | crate::types::message::Message::Progress(_)
            | crate::types::message::Message::HookResult(_) => {}
        }
    }
    flush(&mut groups, &mut current);

    groups
}

/// Maps to CC `utils/toolResultStorage.ts` `replaceToolResultContents(...)`.
fn replace_tool_result_contents(
    messages: &mut [crate::types::message::Message],
    replacement_map: &std::collections::HashMap<String, String>,
) {
    if replacement_map.is_empty() {
        return;
    }

    for message in messages {
        let crate::types::message::Message::User(user) = message else {
            continue;
        };
        for content in &mut user.content {
            let crate::types::message::UserContent::ToolResult(result) = content else {
                continue;
            };
            if let Some(replacement) = replacement_map.get(&result.tool_use_id.0) {
                result.content = replacement.clone();
                result.content_blocks.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn tool_result_message(id: &str, content: String) -> crate::types::message::Message {
        crate::types::message::Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::ToolResult(
                crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId(id.to_string()),
                    content,
                    is_error: false,
                    content_blocks: Vec::new(),
                    tool_use_result: None,
                },
            )],
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

    fn assistant_boundary() -> crate::types::message::Message {
        crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Text(
                "ok".to_string(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })
    }

    fn assistant_tool_use(id: &str, name: &str) -> crate::types::message::Message {
        crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId(id.to_string()),
                    name: name.to_string(),
                    input: serde_json::json!({}),
                },
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        })
    }

    fn first_tool_result_content(message: &crate::types::message::Message) -> Option<&str> {
        let crate::types::message::Message::User(user) = message else {
            return None;
        };
        user.content.iter().find_map(|content| match content {
            crate::types::message::UserContent::ToolResult(result) => Some(result.content.as_str()),
            _ => None,
        })
    }

    #[test]
    fn preview_counts_javascript_utf16_units_without_splitting_rust_scalars() {
        assert_eq!(generate_preview("a😀b", 3), ("a😀".to_string(), true));
        assert_eq!(generate_preview("a😀b", 4), ("a😀b".to_string(), false));
    }

    #[test]
    fn apply_tool_result_budget_clears_largest_aggregate_tool_result() {
        let messages = vec![crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_large_a".to_string()),
                            content: "a".repeat(150_000),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    ),
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_large_b".to_string()),
                            content: "b".repeat(75_000),
                            is_error: true,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    ),
                ],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            },
        )];

        let budgeted = apply_tool_result_budget(messages);
        let crate::types::message::Message::User(user) = &budgeted[0] else {
            panic!("expected user message")
        };
        let cleared = user
            .content
            .iter()
            .filter(|content| {
                matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.content == TOOL_RESULT_CLEARED_MESSAGE
                )
            })
            .count();
        assert_eq!(cleared, 1);
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_large_b" && result.is_error
        )));
    }

    #[test]
    fn apply_tool_result_budget_groups_adjacent_user_messages_like_api_normalization() {
        let messages = vec![
            tool_result_message("toolu_large_a", "a".repeat(150_000)),
            crate::types::message::Message::System(
                crate::types::message::SystemMessage::informational(
                    "progress rows do not split API user groups",
                    crate::types::message::SystemMessageLevel::Info,
                ),
            ),
            tool_result_message("toolu_large_b", "b".repeat(75_000)),
        ];

        let budgeted = apply_tool_result_budget(messages);
        let cleared = budgeted
            .iter()
            .filter_map(|message| match message {
                crate::types::message::Message::User(user) => Some(user),
                _ => None,
            })
            .flat_map(|user| user.content.iter())
            .filter(|content| {
                matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.content == TOOL_RESULT_CLEARED_MESSAGE
                )
            })
            .count();

        assert_eq!(cleared, 1);
    }

    #[test]
    fn apply_tool_result_budget_result_returns_official_newly_replaced_records() {
        let mut state = ContentReplacementState::new();
        let messages = vec![
            tool_result_message("toolu_large_a", "a".repeat(150_000)),
            tool_result_message("toolu_large_b", "b".repeat(75_000)),
        ];

        let result = apply_tool_result_budget_with_state_and_skip_tool_names_result(
            messages,
            &mut state,
            &std::collections::HashSet::new(),
        );

        assert_eq!(result.newly_replaced.len(), 1);
        assert_eq!(result.newly_replaced[0].kind, "tool-result");
        assert_eq!(result.newly_replaced[0].tool_use_id, "toolu_large_a");
        assert_eq!(
            result.newly_replaced[0].replacement,
            TOOL_RESULT_CLEARED_MESSAGE
        );
        assert_eq!(
            serde_json::to_value(&result.newly_replaced[0]).unwrap(),
            serde_json::json!({
                "kind": "tool-result",
                "toolUseId": "toolu_large_a",
                "replacement": TOOL_RESULT_CLEARED_MESSAGE,
            })
        );
        assert!(result.messages.iter().any(|message| match message {
            crate::types::message::Message::User(user) => user.content.iter().any(|content| {
                matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_large_a"
                            && result.content == TOOL_RESULT_CLEARED_MESSAGE
                )
            }),
            _ => false,
        }));
    }

    #[test]
    fn apply_tool_result_budget_with_state_freezes_seen_unreplaced_results() {
        let mut state = ContentReplacementState::new();

        let first_pass = vec![tool_result_message("toolu_old", "a".repeat(150_000))];
        let first_budgeted = apply_tool_result_budget_with_state(first_pass, &mut state);
        assert!(first_budgeted.iter().all(|message| match message {
            crate::types::message::Message::User(user) => user.content.iter().all(|content| {
                !matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.content == TOOL_RESULT_CLEARED_MESSAGE
                )
            }),
            _ => true,
        }));
        assert!(state.seen_ids.contains("toolu_old"));
        assert!(!state.replacements.contains_key("toolu_old"));

        // Same API-level user group is now over budget. Official state freezes
        // the old under-budget decision and replaces only the fresh result.
        let second_pass = vec![
            tool_result_message("toolu_old", "a".repeat(150_000)),
            tool_result_message("toolu_new", "b".repeat(75_000)),
        ];
        let second_budgeted = apply_tool_result_budget_with_state(second_pass, &mut state);
        let mut old_cleared = false;
        let mut new_cleared = false;
        for message in second_budgeted {
            let crate::types::message::Message::User(user) = message else {
                continue;
            };
            for content in user.content {
                let crate::types::message::UserContent::ToolResult(result) = content else {
                    continue;
                };
                if result.tool_use_id.0 == "toolu_old" {
                    old_cleared = result.content == TOOL_RESULT_CLEARED_MESSAGE;
                }
                if result.tool_use_id.0 == "toolu_new" {
                    new_cleared = result.content == TOOL_RESULT_CLEARED_MESSAGE;
                }
            }
        }

        assert!(!old_cleared);
        assert!(new_cleared);
        assert!(!state.replacements.contains_key("toolu_old"));
        assert_eq!(
            state.replacements.get("toolu_new").map(String::as_str),
            Some(TOOL_RESULT_CLEARED_MESSAGE)
        );
    }

    #[test]
    fn apply_tool_result_budget_skips_empty_string_tool_results_like_official_candidates() {
        let messages = vec![tool_result_message("toolu_empty", String::new())];
        let mut state = ContentReplacementState::new();

        let output = apply_tool_result_budget_with_state(messages, &mut state);

        assert_eq!(first_tool_result_content(&output[0]), Some(""));
        assert!(!state.seen_ids.contains("toolu_empty"));
        assert!(state.replacements.is_empty());
    }

    #[test]
    fn apply_tool_result_budget_skips_persisted_output_previews_like_official() {
        let persisted_preview = format!(
            "{}\nOutput too large. Full output saved to: /tmp/toolu_persisted.txt\n{}{}",
            PERSISTED_OUTPUT_TAG,
            "p".repeat(MAX_TOOL_RESULTS_PER_MESSAGE_CHARS + 1),
            PERSISTED_OUTPUT_CLOSING_TAG
        );
        let messages = vec![
            tool_result_message("toolu_persisted", persisted_preview.clone()),
            tool_result_message("toolu_small", "ok".to_string()),
        ];
        let mut state = ContentReplacementState::new();

        let output = apply_tool_result_budget_with_state(messages, &mut state);

        assert_eq!(
            first_tool_result_content(&output[0]),
            Some(persisted_preview.as_str())
        );
        assert_eq!(first_tool_result_content(&output[1]), Some("ok"));
        assert!(!state.seen_ids.contains("toolu_persisted"));
        assert!(state.seen_ids.contains("toolu_small"));
        assert!(state.replacements.is_empty());
    }

    #[test]
    fn apply_tool_result_budget_with_state_reapplies_prior_replacement() {
        let mut state = ContentReplacementState::new();
        let original = vec![
            tool_result_message("toolu_large_a", "a".repeat(150_000)),
            tool_result_message("toolu_large_b", "b".repeat(75_000)),
        ];
        let first_budgeted = apply_tool_result_budget_with_state(original.clone(), &mut state);
        assert!(first_budgeted.iter().any(|message| match message {
            crate::types::message::Message::User(user) => user.content.iter().any(|content| {
                matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_large_a"
                            && result.content == TOOL_RESULT_CLEARED_MESSAGE
                )
            }),
            _ => false,
        }));

        // Replaying the original full-content messages must choose the same
        // replacement instead of reconsidering and clearing a different result.
        let second_budgeted = apply_tool_result_budget_with_state(original, &mut state);
        let cleared_ids = second_budgeted
            .iter()
            .filter_map(|message| match message {
                crate::types::message::Message::User(user) => Some(user),
                _ => None,
            })
            .flat_map(|user| user.content.iter())
            .filter_map(|content| match content {
                crate::types::message::UserContent::ToolResult(result)
                    if result.content == TOOL_RESULT_CLEARED_MESSAGE =>
                {
                    Some(result.tool_use_id.0.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(cleared_ids, vec!["toolu_large_a"]);
    }

    #[test]
    fn provision_content_replacement_state_returns_none_when_gate_disabled_like_official() {
        let loaded = vec![tool_result_message("toolu_old", "a".repeat(150_000))];

        assert!(
            provision_content_replacement_state_with_enabled(false, Some(&loaded), &[]).is_none()
        );
    }

    #[test]
    fn content_replacement_gate_reads_switch_table_and_ignores_growthbook_delivery() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-content-replacement-gate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("create temp config dir");
        std::fs::write(
            root.join(".claude.json"),
            r#"{"cachedGrowthBookFeatures":{"tengu_hawthorn_steeple":true},"growthBookOverrides":{"tengu_hawthorn_steeple":true}}"#,
        )
        .expect("write temp global config");

        crate::utils::process_env::set("CLAUDE_CONFIG_DIR", &root);
        crate::utils::process_env::set(
            "CLAUDE_INTERNAL_FC_OVERRIDES",
            r#"{"tengu_hawthorn_steeple":true}"#,
        );
        assert_eq!(
            is_content_replacement_enabled(),
            crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::ToolResultContentReplacement,
            )
        );
        assert!(!is_content_replacement_enabled());

        crate::utils::process_env::remove("CLAUDE_CONFIG_DIR");
        crate::utils::process_env::remove("CLAUDE_INTERNAL_FC_OVERRIDES");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn per_message_budget_limit_ignores_invalid_values_and_growthbook_delivery() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        assert_eq!(
            per_message_budget_limit_from_feature_value(Some(&serde_json::json!(1234))),
            1234
        );
        assert_eq!(
            per_message_budget_limit_from_feature_value(Some(&serde_json::json!(0))),
            MAX_TOOL_RESULTS_PER_MESSAGE_CHARS
        );
        assert_eq!(
            per_message_budget_limit_from_feature_value(Some(&serde_json::json!("large"))),
            MAX_TOOL_RESULTS_PER_MESSAGE_CHARS
        );

        let root = std::env::temp_dir().join(format!(
            "cometix-tool-result-window-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("create temp config dir");
        std::fs::write(
            root.join(".claude.json"),
            r#"{"cachedGrowthBookFeatures":{"tengu_hawthorn_window":12345},"growthBookOverrides":{"tengu_hawthorn_window":12345}}"#,
        )
        .expect("write temp global config");

        crate::utils::process_env::set("CLAUDE_CONFIG_DIR", &root);
        crate::utils::process_env::set(
            "CLAUDE_INTERNAL_FC_OVERRIDES",
            r#"{"tengu_hawthorn_window":12345}"#,
        );
        assert_eq!(
            get_per_message_budget_limit(),
            MAX_TOOL_RESULTS_PER_MESSAGE_CHARS
        );

        crate::utils::process_env::remove("CLAUDE_CONFIG_DIR");
        crate::utils::process_env::remove("CLAUDE_INTERNAL_FC_OVERRIDES");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconstruct_content_replacement_state_freezes_loaded_unreplaced_results() {
        let loaded = vec![tool_result_message("toolu_old", "a".repeat(150_000))];
        let mut state = provision_content_replacement_state_with_enabled(true, Some(&loaded), &[])
            .expect("enabled gate should provision state");

        assert!(state.seen_ids.contains("toolu_old"));
        assert!(!state.replacements.contains_key("toolu_old"));

        let budgeted = apply_tool_result_budget_with_state(
            vec![
                tool_result_message("toolu_old", "a".repeat(150_000)),
                tool_result_message("toolu_new", "b".repeat(75_000)),
            ],
            &mut state,
        );
        let mut old_cleared = false;
        let mut new_cleared = false;
        for message in budgeted {
            let crate::types::message::Message::User(user) = message else {
                continue;
            };
            for content in user.content {
                let crate::types::message::UserContent::ToolResult(result) = content else {
                    continue;
                };
                if result.tool_use_id.0 == "toolu_old" {
                    old_cleared = result.content == TOOL_RESULT_CLEARED_MESSAGE;
                }
                if result.tool_use_id.0 == "toolu_new" {
                    new_cleared = result.content == TOOL_RESULT_CLEARED_MESSAGE;
                }
            }
        }

        assert!(!old_cleared);
        assert!(new_cleared);
    }

    #[test]
    fn reconstruct_content_replacement_state_reapplies_transcript_records() {
        let loaded = vec![tool_result_message("toolu_old", "original".to_string())];
        let records = vec![ContentReplacementRecord::tool_result(
            "toolu_old".to_string(),
            "stored preview".to_string(),
        )];
        let mut state =
            provision_content_replacement_state_with_enabled(true, Some(&loaded), &records)
                .expect("enabled gate should provision state");

        assert_eq!(
            state.replacements.get("toolu_old").map(String::as_str),
            Some("stored preview")
        );
        let budgeted = apply_tool_result_budget_with_state(loaded, &mut state);
        let crate::types::message::Message::User(user) = &budgeted[0] else {
            panic!("expected user message")
        };
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_old" && result.content == "stored preview"
        )));
    }

    /// CC `toolResultStorage.ts:1001-1012`: an absent parent state means the
    /// feature is off, so the resumed subagent gets no state either; otherwise
    /// the sidechain records rebuild it and the parent's live replacements
    /// gap-fill the fork-inherited `mustReapply` entries the sidechain never
    /// recorded — but only for ids the resumed messages actually contain.
    #[test]
    fn reconstruct_for_subagent_resume_matches_official_gate_and_gap_fill() {
        let resumed = vec![
            tool_result_message("toolu_recorded", "original".to_string()),
            tool_result_message("toolu_inherited", "original".to_string()),
            tool_result_message("toolu_untouched", "original".to_string()),
        ];
        let records = vec![ContentReplacementRecord::tool_result(
            "toolu_recorded".to_string(),
            "sidechain preview".to_string(),
        )];
        let mut parent = ContentReplacementState::new();
        parent
            .replacements
            .insert("toolu_inherited".to_string(), "parent preview".to_string());
        parent.replacements.insert(
            "toolu_recorded".to_string(),
            "parent must not win".to_string(),
        );
        parent
            .replacements
            .insert("toolu_absent".to_string(), "not in messages".to_string());

        assert!(reconstruct_for_subagent_resume(None, &resumed, &records).is_none());

        let state = reconstruct_for_subagent_resume(Some(&parent), &resumed, &records)
            .expect("a present parent state reconstructs one for the resumed agent");
        assert_eq!(
            state.replacements.get("toolu_recorded").map(String::as_str),
            Some("sidechain preview")
        );
        assert_eq!(
            state
                .replacements
                .get("toolu_inherited")
                .map(String::as_str),
            Some("parent preview")
        );
        assert!(!state.replacements.contains_key("toolu_absent"));
        assert!(!state.replacements.contains_key("toolu_untouched"));
        // Every candidate in the transcript was sent to the model once, so it is
        // frozen against future replacement.
        assert!(state.seen_ids.contains("toolu_untouched"));
        assert!(!state.seen_ids.contains("toolu_absent"));
    }

    #[test]
    fn apply_tool_result_budget_skips_unbounded_tool_names_like_official() {
        let mut state = ContentReplacementState::new();
        let mut skip_tool_names = std::collections::HashSet::new();
        skip_tool_names.insert("Read".to_string());
        let messages = vec![
            assistant_tool_use("toolu_large_read", "Read"),
            tool_result_message("toolu_large_read", "r".repeat(500_000)),
            tool_result_message("toolu_text", "a".repeat(150_000)),
        ];

        let budgeted = apply_tool_result_budget_with_state_and_skip_tool_names(
            messages,
            &mut state,
            &skip_tool_names,
        );
        let cleared = budgeted
            .iter()
            .filter_map(|message| match message {
                crate::types::message::Message::User(user) => Some(user),
                _ => None,
            })
            .flat_map(|user| user.content.iter())
            .filter(|content| {
                matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.content == TOOL_RESULT_CLEARED_MESSAGE
                )
            })
            .count();

        assert_eq!(cleared, 0);
        assert!(state.seen_ids.contains("toolu_large_read"));
        assert!(!state.replacements.contains_key("toolu_large_read"));
    }

    #[test]
    fn apply_tool_result_budget_does_not_count_non_text_tool_reference_blocks() {
        let messages = vec![crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_refs".to_string()),
                            content: "x".repeat(500_000),
                            is_error: false,
                            content_blocks: vec![
                                crate::types::message::ToolResultContentBlock::ToolReference {
                                    tool_name: "Read".to_string(),
                                },
                            ],
                            tool_use_result: None,
                        },
                    ),
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_text".to_string()),
                            content: "a".repeat(150_000),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    ),
                ],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            },
        )];

        let budgeted = apply_tool_result_budget(messages);
        let crate::types::message::Message::User(user) = &budgeted[0] else {
            panic!("expected user message")
        };

        assert!(user.content.iter().all(|content| !matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.content == TOOL_RESULT_CLEARED_MESSAGE
        )));
    }

    #[test]
    fn apply_tool_result_budget_keeps_assistant_separated_groups_independent() {
        let messages = vec![
            tool_result_message("toolu_large_a", "a".repeat(150_000)),
            assistant_boundary(),
            tool_result_message("toolu_large_b", "b".repeat(150_000)),
        ];

        let budgeted = apply_tool_result_budget(messages);
        let cleared = budgeted
            .iter()
            .filter_map(|message| match message {
                crate::types::message::Message::User(user) => Some(user),
                _ => None,
            })
            .flat_map(|user| user.content.iter())
            .filter(|content| {
                matches!(
                    content,
                    crate::types::message::UserContent::ToolResult(result)
                        if result.content == TOOL_RESULT_CLEARED_MESSAGE
                )
            })
            .count();

        assert_eq!(cleared, 0);
    }
}
