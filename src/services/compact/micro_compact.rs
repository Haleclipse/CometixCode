//! Microcompact service seam.
//! Maps to CC `services/compact/microCompact.ts` `microcompactMessages(...)`.
//!
//! The upstream implementation performs cache/content microcompaction before
//! the main model call and may yield a microcompact boundary message. Cometix
//! keeps the service in the official file boundary, preserves the same disabled
//! default, and implements the time-based content-clearing path without moving
//! compact logic into `query/deps.rs`.

use crate::constants::query_source::QuerySource;
use crate::services::compact::time_based_mc_config::{
    TimeBasedMicrocompactConfig, time_based_microcompact_config_from_env,
};
use crate::tool::ToolUseContext;
use crate::types::message::{AssistantContent, Message, UserContent};
#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// Maps to CC `services/compact/microCompact.ts` `TIME_BASED_MC_CLEARED_MESSAGE`.
pub const TIME_BASED_MC_CLEARED_MESSAGE: &str =
    crate::utils::tool_result_storage::TOOL_RESULT_CLEARED_MESSAGE;

/// Maps to CC `services/compact/microCompact.ts` `PendingCacheEdits`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCacheEdits {
    pub trigger: String,
    pub deleted_tool_ids: Vec<String>,
    pub baseline_cache_deleted_tokens: u64,
}

impl PendingCacheEdits {
    pub fn auto(deleted_tool_ids: Vec<String>, baseline_cache_deleted_tokens: u64) -> Self {
        Self {
            trigger: "auto".to_string(),
            deleted_tool_ids,
            baseline_cache_deleted_tokens,
        }
    }
}

/// Maps to CC `services/compact/microCompact.ts`
/// `MicrocompactResult.compactionInfo`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicrocompactCompactionInfo {
    pub pending_cache_edits: Option<PendingCacheEdits>,
}

/// Maps to CC `cachedMicrocompact.ts#CacheEditsBlock` as consumed through
/// `microCompact.ts` `pendingCacheEdits` and API `cache_edits` insertion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheEditsBlock {
    pub deleted_tool_ids: Vec<String>,
}

impl CacheEditsBlock {
    pub fn delete_refs(deleted_tool_ids: Vec<String>) -> Self {
        Self { deleted_tool_ids }
    }
}

/// Maps to CC `microCompact.ts#PinnedCacheEdits`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedCacheEdits {
    pub user_message_index: usize,
    pub block: CacheEditsBlock,
}

/// Maps to CC `cachedMicrocompact.ts#getCachedMCConfig()` fields used by
/// `microCompact.ts#cachedMicrocompactPath`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedMicrocompactConfig {
    pub enabled: bool,
    pub trigger_threshold: usize,
    pub keep_recent: usize,
}

impl Default for CachedMicrocompactConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger_threshold: 20,
            keep_recent: 5,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachedMicrocompactState {
    pub registered_tools: HashSet<String>,
    pub tool_order: Vec<String>,
    pub deleted_refs: HashSet<String>,
    pub tool_messages: Vec<Vec<String>>,
    pub pinned_edits: Vec<PinnedCacheEdits>,
    pub pending_cache_edits: Option<CacheEditsBlock>,
    pub sent_tools: HashSet<String>,
}

static CACHED_MC_STATE: LazyLock<Mutex<CachedMicrocompactState>> =
    LazyLock::new(|| Mutex::new(CachedMicrocompactState::default()));

#[cfg(test)]
pub static TEST_CACHED_MC_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

#[derive(Debug, Clone, Default)]
pub struct MicrocompactResult {
    /// Maps to: CC `microcompactMessages(...).messages`.
    pub messages: Vec<Message>,
    /// Maps to: CC microcompact boundary/system messages yielded by `query.ts`
    /// (`query.ts:884` `yield createMicrocompactBoundaryMessage(...)`) — whole
    /// `Message` values, like every other query yield.
    pub boundary_messages: Vec<Message>,
    /// Maps to CC cached microcompact cache-edit metadata. The cached-MC path
    /// leaves message content unchanged and defers the boundary row until after
    /// the API returns authoritative `cache_deleted_input_tokens`.
    pub compaction_info: Option<MicrocompactCompactionInfo>,
}

/// Maps to CC `cachedMicrocompact.ts#isCachedMicrocompactEnabled` and
/// `getCachedMCConfig` for Cometix's local, source-controlled runtime seam.
pub fn cached_microcompact_config_from_env() -> CachedMicrocompactConfig {
    let mut config = CachedMicrocompactConfig::default();
    config.enabled = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("COMETIX_CACHED_MICROCOMPACT").as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_CACHED_MICROCOMPACT")
            .as_deref(),
    );
    if let Some(value) = crate::utils::process_env::var("COMETIX_CACHED_MC_TRIGGER_THRESHOLD") {
        if let Ok(parsed) = value.parse::<usize>() {
            config.trigger_threshold = parsed.max(1);
        }
    }
    if let Some(value) = crate::utils::process_env::var("COMETIX_CACHED_MC_KEEP_RECENT") {
        if let Ok(parsed) = value.parse::<usize>() {
            config.keep_recent = parsed.max(1);
        }
    }
    config
}

/// Maps to CC `cachedMicrocompact.ts#isModelSupportedForCacheEditing`.
pub fn is_model_supported_for_cache_editing(model: &str) -> bool {
    // The sourcemap snapshot contains only the `microCompact.ts` callsite and a
    // generated stub for `cachedMicrocompact.ts`; without the official supported
    // model table, Cometix treats every non-empty local model as supported when
    // the explicit cached-MC gate is enabled.
    !model.trim().is_empty()
}

/// Maps to CC `microCompact.ts#consumePendingCacheEdits`.
pub fn consume_pending_cache_edits() -> Option<CacheEditsBlock> {
    CACHED_MC_STATE.lock().unwrap().pending_cache_edits.take()
}

/// Maps to CC `microCompact.ts#getPinnedCacheEdits`.
pub fn get_pinned_cache_edits() -> Vec<crate::services::api::claude::CachedMcPinnedEdit> {
    CACHED_MC_STATE
        .lock()
        .unwrap()
        .pinned_edits
        .iter()
        .map(|pinned| crate::services::api::claude::CachedMcPinnedEdit {
            user_message_index: pinned.user_message_index,
            block: crate::services::api::claude::CachedMcEditsBlock::delete_refs(
                pinned.block.deleted_tool_ids.clone(),
            ),
        })
        .collect()
}

/// Maps to CC `microCompact.ts#pinCacheEdits`.
pub fn pin_cache_edits(user_message_index: usize, block: CacheEditsBlock) {
    let mut state = CACHED_MC_STATE.lock().unwrap();
    state.pinned_edits.push(PinnedCacheEdits {
        user_message_index,
        block,
    });
}

/// Maps to CC `microCompact.ts#markToolsSentToAPIState`.
pub fn mark_tools_sent_to_api_state() {
    let mut state = CACHED_MC_STATE.lock().unwrap();
    state.sent_tools = state.registered_tools.clone();
}

/// Maps to CC `services/compact/microCompact.ts` `resetMicrocompactState()`.
pub fn reset_microcompact_state() {
    *CACHED_MC_STATE.lock().unwrap() = CachedMicrocompactState::default();
}

#[cfg(test)]
pub fn cached_microcompact_state_for_test() -> CachedMicrocompactState {
    CACHED_MC_STATE.lock().unwrap().clone()
}

pub fn microcompact_messages(
    messages_for_query: Vec<Message>,
    tool_use_context: &ToolUseContext,
    query_source: &QuerySource,
) -> MicrocompactResult {
    microcompact_messages_with_configs(
        messages_for_query,
        tool_use_context,
        query_source,
        time_based_microcompact_config_from_env(),
        cached_microcompact_config_from_env(),
    )
}

/// Testable implementation of CC `maybeTimeBasedMicrocompact(...)`.
///
/// The official config comes from GrowthBook (`tengu_slate_heron`) and defaults
/// disabled. Cometix reads the hardcoded `utils::feature_flags` switch instead
/// of GrowthBook cache/config, with local env override seams kept for tests.
pub fn microcompact_messages_with_time_config(
    messages_for_query: Vec<Message>,
    query_source: &QuerySource,
    config: TimeBasedMicrocompactConfig,
) -> MicrocompactResult {
    microcompact_messages_with_configs(
        messages_for_query,
        &ToolUseContext::default(),
        query_source,
        config,
        CachedMicrocompactConfig::default(),
    )
}

pub fn microcompact_messages_with_configs(
    messages_for_query: Vec<Message>,
    tool_use_context: &ToolUseContext,
    query_source: &QuerySource,
    time_config: TimeBasedMicrocompactConfig,
    cached_config: CachedMicrocompactConfig,
) -> MicrocompactResult {
    if let Some(messages) =
        maybe_time_based_microcompact(messages_for_query.clone(), query_source, &time_config)
    {
        return MicrocompactResult {
            messages,
            boundary_messages: Vec::new(),
            compaction_info: None,
        };
    }

    let model = tool_use_context
        .main_loop_model
        .as_deref()
        .unwrap_or("default");
    if cached_config.enabled
        && is_model_supported_for_cache_editing(model)
        && is_main_thread_source(query_source)
    {
        return cached_microcompact_path(messages_for_query, &cached_config);
    }

    MicrocompactResult {
        messages: messages_for_query,
        boundary_messages: Vec::new(),
        compaction_info: None,
    }
}

fn cached_microcompact_path(
    messages: Vec<Message>,
    config: &CachedMicrocompactConfig,
) -> MicrocompactResult {
    let compactable_tool_ids = collect_compactable_tool_ids(&messages)
        .into_iter()
        .collect::<HashSet<_>>();

    {
        let mut state = CACHED_MC_STATE.lock().unwrap();
        for message in &messages {
            let Message::User(user) = message else {
                continue;
            };
            let mut group_ids = Vec::new();
            for block in &user.content {
                let UserContent::ToolResult(tool_result) = block else {
                    continue;
                };
                let tool_use_id = &tool_result.tool_use_id.0;
                if compactable_tool_ids.contains(tool_use_id)
                    && !state.registered_tools.contains(tool_use_id)
                {
                    register_tool_result(&mut state, tool_use_id.clone());
                    group_ids.push(tool_use_id.clone());
                }
            }
            register_tool_message(&mut state, group_ids);
        }
    }

    let tools_to_delete = {
        let state = CACHED_MC_STATE.lock().unwrap();
        get_tool_results_to_delete(&state, config)
    };

    if tools_to_delete.is_empty() {
        return MicrocompactResult {
            messages,
            boundary_messages: Vec::new(),
            compaction_info: None,
        };
    }

    let cache_edits = create_cache_edits_block(&tools_to_delete);
    let baseline = messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(assistant) => assistant
                .usage
                .as_ref()
                .map(|usage| usage.cache_deleted_input_tokens),
            _ => None,
        })
        .unwrap_or(0);

    MicrocompactResult {
        messages,
        boundary_messages: Vec::new(),
        compaction_info: Some(MicrocompactCompactionInfo {
            pending_cache_edits: Some(PendingCacheEdits::auto(
                cache_edits.deleted_tool_ids,
                baseline,
            )),
        }),
    }
}

/// Maps to CC `cachedMicrocompact.ts#registerToolResult`.
fn register_tool_result(state: &mut CachedMicrocompactState, tool_use_id: String) {
    if state.registered_tools.insert(tool_use_id.clone()) {
        state.tool_order.push(tool_use_id);
    }
}

/// Maps to CC `cachedMicrocompact.ts#registerToolMessage`.
fn register_tool_message(state: &mut CachedMicrocompactState, group_ids: Vec<String>) {
    if !group_ids.is_empty() {
        state.tool_messages.push(group_ids);
    }
}

/// Maps to CC `cachedMicrocompact.ts#getToolResultsToDelete`.
fn get_tool_results_to_delete(
    state: &CachedMicrocompactState,
    config: &CachedMicrocompactConfig,
) -> Vec<String> {
    let active = state
        .tool_order
        .iter()
        .filter(|id| !state.deleted_refs.contains(*id))
        .cloned()
        .collect::<Vec<_>>();
    if active.len() <= config.trigger_threshold {
        return Vec::new();
    }
    let keep_recent = config.keep_recent.max(1);
    let delete_count = active.len().saturating_sub(keep_recent);
    active.into_iter().take(delete_count).collect()
}

/// Maps to CC `cachedMicrocompact.ts#createCacheEditsBlock`.
fn create_cache_edits_block(tools_to_delete: &[String]) -> CacheEditsBlock {
    let block = CacheEditsBlock::delete_refs(tools_to_delete.to_vec());
    let mut state = CACHED_MC_STATE.lock().unwrap();
    for tool in tools_to_delete {
        state.deleted_refs.insert(tool.clone());
    }
    state.pending_cache_edits = Some(block.clone());
    block
}

fn maybe_time_based_microcompact(
    messages: Vec<Message>,
    query_source: &QuerySource,
    config: &TimeBasedMicrocompactConfig,
) -> Option<Vec<Message>> {
    if !evaluate_time_based_trigger(&messages, query_source, config) {
        return None;
    }

    let compactable_ids = collect_compactable_tool_ids(&messages);
    let keep_recent = config.keep_recent.max(1);
    let keep_set = compactable_ids
        .iter()
        .rev()
        .take(keep_recent)
        .cloned()
        .collect::<HashSet<_>>();
    let clear_set = compactable_ids
        .into_iter()
        .filter(|id| !keep_set.contains(id))
        .collect::<HashSet<_>>();
    if clear_set.is_empty() {
        return None;
    }

    let mut tokens_saved = 0_i64;
    let mut result = messages;
    for message in &mut result {
        let Message::User(user) = message else {
            continue;
        };
        for content in &mut user.content {
            let UserContent::ToolResult(tool_result) = content else {
                continue;
            };
            if clear_set.contains(&tool_result.tool_use_id.0)
                && tool_result.content != TIME_BASED_MC_CLEARED_MESSAGE
            {
                // Official `maybeTimeBasedMicrocompact(...)` replaces every
                // clear-set block before checking the aggregate `tokensSaved`.
                // Zero-token blocks are therefore cleared when any sibling
                // saves tokens, but the whole projection is discarded when the
                // total saved count is zero.
                let estimate = calculate_tool_result_tokens(tool_result);
                tokens_saved = tokens_saved.saturating_add(estimate);
                tool_result.content = TIME_BASED_MC_CLEARED_MESSAGE.to_string();
            }
        }
    }

    if tokens_saved == 0 {
        return None;
    }

    // Maps to CC `microCompact.ts#maybeTimeBasedMicrocompact`, which resets
    // cached-MC state after mutating prompt content because server cache edits
    // can no longer target the previously-cached tool results.
    reset_microcompact_state();
    Some(result)
}

/// Maps to CC `microCompact.ts` `calculateToolResultTokens(...)` for Rust's
/// current typed `ToolResultBlockParam` subset. Tool-reference-only content
/// blocks do not count toward saved prompt tokens, matching CC's fallthrough
/// for non-text/image/document array entries.
fn calculate_tool_result_tokens(tool_result: &crate::types::message::ToolResult) -> i64 {
    if !tool_result.content_blocks.is_empty() {
        return tool_result
            .content_blocks
            .iter()
            .map(|block| match block {
                crate::types::message::ToolResultContentBlock::ToolReference { .. } => 0,
                crate::types::message::ToolResultContentBlock::Text { text } => {
                    crate::services::token_estimation::rough_token_count_estimation(text)
                }
                // Match CC IMAGE_MAX_TOKEN_SIZE stand-in for media blocks.
                crate::types::message::ToolResultContentBlock::Image { .. }
                | crate::types::message::ToolResultContentBlock::Document { .. }
                | crate::types::message::ToolResultContentBlock::RawImage(_) => 2_000,
            })
            .sum();
    }
    if tool_result.content.is_empty() {
        return 0;
    }
    crate::services::token_estimation::rough_token_count_estimation(&tool_result.content)
}

fn evaluate_time_based_trigger(
    messages: &[Message],
    query_source: &QuerySource,
    config: &TimeBasedMicrocompactConfig,
) -> bool {
    if !config.enabled || !is_main_thread_source(query_source) {
        return false;
    }
    let Some(last_assistant_timestamp) = messages.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => Some(assistant.timestamp),
        _ => None,
    }) else {
        return false;
    };
    let gap_minutes = (chrono::Utc::now() - last_assistant_timestamp).num_seconds() as f64 / 60.0;
    gap_minutes.is_finite() && gap_minutes >= config.gap_threshold_minutes
}

fn is_main_thread_source(query_source: &QuerySource) -> bool {
    // `QuerySource::Prompt` is Cometix's current main REPL/user prompt source.
    matches!(query_source, QuerySource::Prompt)
}

fn collect_compactable_tool_ids(messages: &[Message]) -> Vec<String> {
    let mut ids = Vec::new();
    for message in messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for content in &assistant.content {
            if let AssistantContent::ToolUse(tool_use) = content {
                if is_compactable_tool_name(&tool_use.name) {
                    ids.push(tool_use.id.0.clone());
                }
            }
        }
    }
    ids
}

fn is_compactable_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "Read"
            | "Bash"
            | "PowerShell"
            | "Grep"
            | "Glob"
            | "WebSearch"
            | "WebFetch"
            | "Edit"
            | "FileEdit"
            | "Write"
            | "FileWrite"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_cached_mc_state_for_test() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_CACHED_MC_LOCK.lock().unwrap();
        reset_microcompact_state();
        guard
    }

    fn assistant_tool_message(ids: &[&str], cache_deleted_input_tokens: u64) -> Message {
        Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::minutes(120),
            content: ids
                .iter()
                .map(|id| {
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId((*id).to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": format!("echo {id}")}),
                        },
                    )
                })
                .collect(),
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: Some(crate::types::message::TokenUsage {
                cache_deleted_input_tokens,
                ..Default::default()
            }),
        })
    }

    fn user_tool_result_message(ids: &[&str]) -> Message {
        Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: ids
                .iter()
                .map(|id| {
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId((*id).to_string()),
                            content: format!("output for {id}"),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    )
                })
                .collect(),
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
    fn microcompact_messages_is_safe_noop_when_all_microcompact_gates_disabled() {
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "before microcompact".to_string(),
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
        })];
        let context = ToolUseContext::default();

        let result = microcompact_messages(messages.clone(), &context, &QuerySource::Prompt);

        assert_eq!(result.messages, messages);
        assert!(result.boundary_messages.is_empty());
    }

    #[test]
    fn time_based_microcompact_skips_zero_token_tool_results_like_official() {
        let _state_guard = lock_cached_mc_state_for_test();
        let old_timestamp = chrono::Utc::now() - chrono::Duration::minutes(120);
        let messages = vec![
            Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: old_timestamp,
                content: vec![
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_empty".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "true"}),
                        },
                    ),
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            name: "Read".to_string(),
                            input: serde_json::json!({"file_path": "Cargo.toml"}),
                        },
                    ),
                ],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_empty".to_string()),
                            content: String::new(),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    ),
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            content: "recent output".to_string(),
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
            }),
        ];

        let result = microcompact_messages_with_time_config(
            messages.clone(),
            &QuerySource::Prompt,
            TimeBasedMicrocompactConfig {
                enabled: true,
                gap_threshold_minutes: 60.0,
                keep_recent: 1,
            },
        );

        assert_eq!(result.messages, messages);
    }

    #[test]
    fn time_based_microcompact_keeps_tool_reference_only_results_when_projection_saves_zero_tokens()
    {
        let _state_guard = lock_cached_mc_state_for_test();
        let old_timestamp = chrono::Utc::now() - chrono::Duration::minutes(120);
        let original_ref_content = "tool references are stored out of band".repeat(100);
        let messages = vec![
            Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: old_timestamp,
                content: vec![
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_ref".to_string()),
                            name: "Read".to_string(),
                            input: serde_json::json!({"file_path": "Cargo.toml"}),
                        },
                    ),
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            name: "Read".to_string(),
                            input: serde_json::json!({"file_path": "src/main.rs"}),
                        },
                    ),
                ],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_ref".to_string()),
                            content: original_ref_content.clone(),
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
                            tool_use_id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            content: "recent output".to_string(),
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
            }),
        ];

        let result = microcompact_messages_with_time_config(
            messages,
            &QuerySource::Prompt,
            TimeBasedMicrocompactConfig {
                enabled: true,
                gap_threshold_minutes: 60.0,
                keep_recent: 1,
            },
        );

        let Message::User(user) = &result.messages[1] else {
            panic!("expected user message")
        };
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_ref"
                    && result.content == original_ref_content
                    && matches!(
                        result.content_blocks.as_slice(),
                        [crate::types::message::ToolResultContentBlock::ToolReference { tool_name }]
                            if tool_name == "Read"
                    )
        )));
    }

    #[test]
    fn time_based_microcompact_clears_zero_token_siblings_when_any_tokens_saved_like_official() {
        let _state_guard = lock_cached_mc_state_for_test();
        let old_timestamp = chrono::Utc::now() - chrono::Duration::minutes(120);
        let messages = vec![
            Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: old_timestamp,
                content: vec![
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_empty".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "true"}),
                        },
                    ),
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_old".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "echo old"}),
                        },
                    ),
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            name: "Read".to_string(),
                            input: serde_json::json!({"file_path": "Cargo.toml"}),
                        },
                    ),
                ],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_empty".to_string()),
                            content: String::new(),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    ),
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_old".to_string()),
                            content: "old output".repeat(100),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    ),
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            content: "recent output".to_string(),
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
            }),
        ];

        let result = microcompact_messages_with_time_config(
            messages,
            &QuerySource::Prompt,
            TimeBasedMicrocompactConfig {
                enabled: true,
                gap_threshold_minutes: 60.0,
                keep_recent: 1,
            },
        );

        let Message::User(user) = &result.messages[1] else {
            panic!("expected user message")
        };
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_empty"
                    && result.content == crate::utils::tool_result_storage::TOOL_RESULT_CLEARED_MESSAGE
        )));
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_old"
                    && result.content == crate::utils::tool_result_storage::TOOL_RESULT_CLEARED_MESSAGE
        )));
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_recent" && result.content == "recent output"
        )));
    }

    #[test]
    fn time_based_microcompact_clears_old_compactable_tool_results_and_keeps_recent() {
        let _state_guard = lock_cached_mc_state_for_test();
        let old_timestamp = chrono::Utc::now() - chrono::Duration::minutes(120);
        let messages = vec![
            Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: old_timestamp,
                content: vec![
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_old".to_string()),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "echo old"}),
                        },
                    ),
                    crate::types::message::AssistantContent::ToolUse(
                        crate::types::message::ToolUseBlock {
                            id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            name: "Read".to_string(),
                            input: serde_json::json!({"file_path": "Cargo.toml"}),
                        },
                    ),
                ],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_old".to_string()),
                            content: "old output".repeat(100),
                            is_error: false,
                            content_blocks: Vec::new(),
                            tool_use_result: None,
                        },
                    ),
                    crate::types::message::UserContent::ToolResult(
                        crate::types::message::ToolResult {
                            tool_use_id: crate::types::ids::ToolUseId("toolu_recent".to_string()),
                            content: "recent output".to_string(),
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
            }),
        ];

        let result = microcompact_messages_with_time_config(
            messages,
            &QuerySource::Prompt,
            TimeBasedMicrocompactConfig {
                enabled: true,
                gap_threshold_minutes: 60.0,
                keep_recent: 1,
            },
        );

        let Message::User(user) = &result.messages[1] else {
            panic!("expected user message")
        };
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_old"
                    && result.content == crate::utils::tool_result_storage::TOOL_RESULT_CLEARED_MESSAGE
        )));
        assert!(user.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::ToolResult(result)
                if result.tool_use_id.0 == "toolu_recent" && result.content == "recent output"
        )));
    }

    #[test]
    fn cached_microcompact_queues_deletes_and_preserves_messages_like_official() {
        let _state_guard = lock_cached_mc_state_for_test();
        let messages = vec![
            assistant_tool_message(&["toolu_old_1", "toolu_old_2", "toolu_recent"], 17),
            user_tool_result_message(&["toolu_old_1", "toolu_old_2", "toolu_recent"]),
        ];
        let context = ToolUseContext {
            main_loop_model: Some("claude-sonnet-4".to_string()),
            ..Default::default()
        };

        let result = microcompact_messages_with_configs(
            messages.clone(),
            &context,
            &QuerySource::Prompt,
            TimeBasedMicrocompactConfig {
                enabled: false,
                gap_threshold_minutes: 60.0,
                keep_recent: 1,
            },
            CachedMicrocompactConfig {
                enabled: true,
                trigger_threshold: 2,
                keep_recent: 1,
            },
        );

        assert_eq!(result.messages, messages);
        let pending = result
            .compaction_info
            .as_ref()
            .and_then(|info| info.pending_cache_edits.as_ref())
            .expect("cached MC should queue cache edits");
        assert_eq!(pending.trigger, "auto");
        assert_eq!(pending.baseline_cache_deleted_tokens, 17);
        assert_eq!(
            pending.deleted_tool_ids,
            vec!["toolu_old_1".to_string(), "toolu_old_2".to_string()]
        );

        let state = cached_microcompact_state_for_test();
        assert_eq!(
            state.tool_order,
            vec![
                "toolu_old_1".to_string(),
                "toolu_old_2".to_string(),
                "toolu_recent".to_string()
            ]
        );
        assert_eq!(
            state.tool_messages,
            vec![vec![
                "toolu_old_1".to_string(),
                "toolu_old_2".to_string(),
                "toolu_recent".to_string()
            ]]
        );
        assert!(state.deleted_refs.contains("toolu_old_1"));
        assert!(state.deleted_refs.contains("toolu_old_2"));
        assert_eq!(
            state
                .pending_cache_edits
                .map(|block| block.deleted_tool_ids),
            Some(vec!["toolu_old_1".to_string(), "toolu_old_2".to_string()])
        );
    }

    #[test]
    fn cached_microcompact_pin_consume_mark_and_reset_state_like_official() {
        let _state_guard = lock_cached_mc_state_for_test();
        pin_cache_edits(
            3,
            CacheEditsBlock::delete_refs(vec!["toolu_deleted".to_string()]),
        );
        {
            let mut state = CACHED_MC_STATE.lock().unwrap();
            state.registered_tools.insert("toolu_deleted".to_string());
            state.pending_cache_edits = Some(CacheEditsBlock::delete_refs(vec![
                "toolu_pending".to_string(),
            ]));
        }

        let pinned = get_pinned_cache_edits();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].user_message_index, 3);
        assert_eq!(pinned[0].block.edits[0].cache_reference, "toolu_deleted");

        mark_tools_sent_to_api_state();
        assert!(
            cached_microcompact_state_for_test()
                .sent_tools
                .contains("toolu_deleted")
        );

        let consumed = consume_pending_cache_edits().expect("pending edits should be consumed");
        assert_eq!(consumed.deleted_tool_ids, vec!["toolu_pending".to_string()]);
        assert!(consume_pending_cache_edits().is_none());

        reset_microcompact_state();
        let state = cached_microcompact_state_for_test();
        assert!(state.pinned_edits.is_empty());
        assert!(state.registered_tools.is_empty());
        assert!(state.sent_tools.is_empty());
    }

    #[test]
    fn time_based_microcompact_resets_cached_state_after_prompt_mutation_like_official() {
        let _state_guard = lock_cached_mc_state_for_test();
        pin_cache_edits(
            1,
            CacheEditsBlock::delete_refs(vec!["toolu_stale".to_string()]),
        );
        {
            let mut state = CACHED_MC_STATE.lock().unwrap();
            state.registered_tools.insert("toolu_stale".to_string());
            state.deleted_refs.insert("toolu_stale".to_string());
        }
        let messages = vec![
            assistant_tool_message(&["toolu_stale", "toolu_recent"], 0),
            user_tool_result_message(&["toolu_stale", "toolu_recent"]),
        ];

        let result = microcompact_messages_with_time_config(
            messages,
            &QuerySource::Prompt,
            TimeBasedMicrocompactConfig {
                enabled: true,
                gap_threshold_minutes: 60.0,
                keep_recent: 1,
            },
        );

        assert!(matches!(
            &result.messages[1],
            Message::User(user) if user.content.iter().any(|content| matches!(
                content,
                crate::types::message::UserContent::ToolResult(result)
                    if result.tool_use_id.0 == "toolu_stale"
                        && result.content == crate::utils::tool_result_storage::TOOL_RESULT_CLEARED_MESSAGE
            ))
        ));
        assert_eq!(
            cached_microcompact_state_for_test(),
            CachedMicrocompactState::default()
        );
    }
}
