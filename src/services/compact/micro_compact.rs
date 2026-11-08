//! Microcompact service seam.
//! Maps to CC `services/compact/microCompact.ts` `microcompactMessages(...)`.
//!
//! The upstream implementation performs cache/content microcompaction before
//! the main model call and may yield a microcompact boundary message. Nyan
//! keeps the service in the official file boundary and preserves the cached
//! microcompact path without moving compact logic into `query/deps.rs`.
//!
//! @nyan: CC's time-based content clearing is removed (elapsed time must never
//! invalidate a prompt-cache prefix). nyan's masking lives here too
//! (`mask_tool_results_if_needed`), but it is threshold-driven and records
//! frozen decisions in `ContentReplacementState` instead of rewriting messages.

use crate::constants::query_source::QuerySource;
use crate::tool::ToolUseContext;
use crate::types::message::{AssistantContent, Message, UserContent};
#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

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
    /// Maps to CC microcompact boundary/system messages yielded by `query.ts`
    /// (`query.ts:884` `yield createMicrocompactBoundaryMessage(...)`) — whole
    /// `Message` values, like every other query yield.
    pub boundary_messages: Vec<Message>,
    /// Maps to CC cached microcompact cache-edit metadata. The cached-MC path
    /// leaves message content unchanged and defers the boundary row until after
    /// the API returns authoritative `cache_deleted_input_tokens`.
    pub compaction_info: Option<MicrocompactCompactionInfo>,
}

#[derive(Debug, Default)]
pub struct MaskResult {
    pub messages: Vec<Message>,
    pub newly_replaced: Vec<crate::utils::tool_result_storage::ContentReplacementRecord>,
    pub masked_count: usize,
    pub masked_tokens: i64,
    /// Net tokens removed from content that the last API `usage` already
    /// counted. `token_count_with_estimation` cannot see this reduction, so the
    /// caller threads it through the `snip_tokens_freed` channel.
    pub anchor_tokens_freed: i64,
}

/// Maps to CC `cachedMicrocompact.ts#isCachedMicrocompactEnabled` and
/// `getCachedMCConfig` for Nyan's local, source-controlled runtime seam.
pub fn cached_microcompact_config_from_env() -> CachedMicrocompactConfig {
    let mut config = CachedMicrocompactConfig::default();
    config.enabled = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_CACHED_MICROCOMPACT").as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_CACHED_MICROCOMPACT").as_deref(),
    );
    if let Some(value) = crate::utils::process_env::var("NYAN_CACHED_MC_TRIGGER_THRESHOLD")
        && let Ok(parsed) = value.parse::<usize>()
    {
        config.trigger_threshold = parsed.max(1);
    }
    if let Some(value) = crate::utils::process_env::var("NYAN_CACHED_MC_KEEP_RECENT")
        && let Ok(parsed) = value.parse::<usize>()
    {
        config.keep_recent = parsed.max(1);
    }
    config
}

/// Maps to CC `cachedMicrocompact.ts#isModelSupportedForCacheEditing`.
pub fn is_model_supported_for_cache_editing(model: &str) -> bool {
    // The sourcemap snapshot contains only the `microCompact.ts` callsite and a
    // generated stub for `cachedMicrocompact.ts`; without the official supported
    // model table, Nyan treats every non-empty local model as supported when
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

fn compaction_settings() -> crate::utils::settings::nyan::CompactionSettings {
    crate::utils::settings::get_initial_settings()
        .nyan
        .map(|settings| settings.compaction())
        .unwrap_or_default()
}

pub fn content_mask_enabled() -> bool {
    compaction_settings().mask()
}

#[derive(Clone)]
struct MaskCandidate {
    message_index: usize,
    tool_use_id: String,
    tool_name: String,
    tokens: i64,
    bytes: usize,
    preview: String,
}

/// @nyan: deterministic pre-compaction masking. It is deliberately separate
/// from cached microcompact: this changes the model projection and therefore
/// owns the persistent ContentReplacementState decision.
pub fn mask_tool_results_if_needed(
    messages: Vec<Message>,
    state: &mut crate::utils::tool_result_storage::ContentReplacementState,
    context: &ToolUseContext,
    query_source: &QuerySource,
) -> MaskResult {
    let measured_before = crate::utils::tokens::token_count_with_estimation(&messages);
    let mut result = MaskResult {
        messages: messages.clone(),
        ..Default::default()
    };
    if !content_mask_enabled() || crate::utils::forked_agent::is_cache_sharing_fork(query_source) {
        result.messages =
            crate::utils::tool_result_storage::apply_tool_result_replacements_only(messages, state);
        return result;
    }

    let model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let effective_window =
        crate::services::compact::auto_compact::get_effective_context_window_size(&model).max(1);
    let threshold =
        crate::services::compact::auto_compact::get_auto_compact_threshold(&model).max(0);
    if measured_before <= threshold {
        result.messages =
            crate::utils::tool_result_storage::apply_tool_result_replacements_only(messages, state);
        return result;
    }

    let settings = compaction_settings();
    let minimum_tokens =
        ((effective_window as f64) * settings.mask_min_fraction().clamp(0.0, 1.0)).ceil() as i64;
    let headroom =
        ((effective_window as f64) * settings.mask_headroom().clamp(0.0, 1.0)).round() as i64;
    let target = threshold.saturating_sub(headroom);
    let current_round_boundary = messages
        .iter()
        .rposition(|message| {
            matches!(
                message,
                Message::User(user)
                    if user.content.iter().any(|content| {
                        !matches!(content, UserContent::ToolResult(_))
                    })
            )
        })
        .unwrap_or(messages.len());

    let mut tool_names = HashMap::<String, String>::new();
    for message in &messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for content in &assistant.content {
            if let AssistantContent::ToolUse(tool_use) = content {
                tool_names.insert(tool_use.id.0.clone(), tool_use.name.clone());
            }
        }
    }

    let mut candidates = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        if message_index >= current_round_boundary {
            continue;
        }
        let Message::User(user) = message else {
            continue;
        };
        for content in &user.content {
            let UserContent::ToolResult(tool_result) = content else {
                continue;
            };
            let Some(tool_name) = tool_names.get(&tool_result.tool_use_id.0) else {
                continue;
            };
            if !is_compactable_tool_name(tool_name)
                || state.replacements.contains_key(&tool_result.tool_use_id.0)
                || crate::utils::tool_result_storage::is_tool_result_cleared(&tool_result.content)
            {
                continue;
            }
            let tokens = calculate_tool_result_tokens(tool_result);
            if tokens < minimum_tokens || tokens <= 0 {
                continue;
            }
            let preview = tool_result.content.chars().take(1500).collect::<String>();
            candidates.push(MaskCandidate {
                message_index,
                tool_use_id: tool_result.tool_use_id.0.clone(),
                tool_name: tool_name.clone(),
                tokens,
                bytes: tool_result.content.len(),
                preview,
            });
        }
    }

    if candidates.is_empty() {
        result.messages =
            crate::utils::tool_result_storage::apply_tool_result_replacements_only(messages, state);
        return result;
    }

    // @nyan: compaction.keep shadow mode (C14, decision-provider 4.4).
    // Evaluate keep-ordering vs actual-ordering and append comparison to
    // $NYAN_CONFIG_DIR/decision/compaction.keep.jsonl. The actual compression
    // outcome below remains 100% unchanged.
    if crate::services::compact::decision::is_keep_shadow_enabled() {
        let candidate_infos: Vec<_> = candidates
            .iter()
            .map(|c| crate::services::compact::decision::MaskCandidateInfo {
                message_index: c.message_index,
                tool_use_id: c.tool_use_id.clone(),
                tool_name: c.tool_name.clone(),
                tokens: c.tokens,
                bytes: c.bytes,
                preview: c.preview.clone(),
            })
            .collect();
        crate::services::compact::decision::spawn_compaction_keep_shadow(
            messages.clone(),
            candidate_infos,
            target,
            measured_before,
            threshold,
            context.clone(),
        );
    }

    // C2: accumulate oldest-first until `target` (threshold minus headroom) is
    // reachable. `measured > threshold >= target` means at least one candidate
    // is always picked, so the earliest cleared position is the oldest
    // candidate; the prompt-cache prefix is lost from there anyway, hence every
    // later candidate is cleared in the same pass.
    let required = measured_before.saturating_sub(target).max(1);
    let mut saved = 0_i64;
    let mut first_selected = 0usize;
    for (index, candidate) in candidates.iter().enumerate() {
        if saved >= required {
            break;
        }
        saved = saved.saturating_add(candidate.tokens);
        first_selected = index;
    }
    let anchor = crate::utils::tokens::usage_anchor_index(&messages);
    for candidate in candidates.into_iter().skip(first_selected) {
        let replacement = crate::utils::tool_result_storage::cleared_tool_result_message(
            &candidate.tool_name,
            candidate.bytes,
            &candidate.tool_use_id,
        );
        let placeholder_tokens =
            crate::services::token_estimation::rough_token_count_estimation(&replacement);
        if anchor.is_some_and(|anchor| candidate.message_index <= anchor) {
            result.anchor_tokens_freed = result
                .anchor_tokens_freed
                .saturating_add((candidate.tokens - placeholder_tokens).max(0));
        }
        state.seen_ids.insert(candidate.tool_use_id.clone());
        state
            .replacements
            .insert(candidate.tool_use_id.clone(), replacement.clone());
        result.masked_count += 1;
        result.masked_tokens = result.masked_tokens.saturating_add(candidate.tokens);
        result.newly_replaced.push(
            crate::utils::tool_result_storage::ContentReplacementRecord::tool_result(
                candidate.tool_use_id,
                replacement,
            ),
        );
    }
    result.messages =
        crate::utils::tool_result_storage::apply_tool_result_replacements_only(messages, state);
    result
}

pub fn project_content_replacements(
    messages: Vec<Message>,
    state: Option<&crate::utils::tool_result_storage::ContentReplacementState>,
) -> Vec<Message> {
    match state {
        Some(state) => {
            crate::utils::tool_result_storage::apply_tool_result_replacements_only(messages, state)
        }
        None => messages,
    }
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
        cached_microcompact_config_from_env(),
    )
}

pub fn microcompact_messages_with_configs(
    messages_for_query: Vec<Message>,
    tool_use_context: &ToolUseContext,
    query_source: &QuerySource,
    cached_config: CachedMicrocompactConfig,
) -> MicrocompactResult {
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

fn is_main_thread_source(query_source: &QuerySource) -> bool {
    // `QuerySource::Prompt` is Nyan's current main REPL/user prompt source.
    matches!(query_source, QuerySource::Prompt)
}

fn collect_compactable_tool_ids(messages: &[Message]) -> Vec<String> {
    let mut ids = Vec::new();
    for message in messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for content in &assistant.content {
            if let AssistantContent::ToolUse(tool_use) = content
                && is_compactable_tool_name(&tool_use.name)
            {
                ids.push(tool_use.id.0.clone());
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
            | "CodeSearch"
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
    use crate::utils::test_env::TEST_ENV_LOCK;

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

    #[tokio::test]
    async fn threshold_mask_clears_old_results_but_not_the_current_user_round() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "50000");
        let mut messages = vec![
            assistant_tool_message(&["toolu_old"], 0),
            user_tool_result_message(&["toolu_old"]),
            Message::User(crate::utils::messages::create_user_message(
                "current task".to_string(),
            )),
        ];
        if let Message::User(user) = &mut messages[1]
            && let Some(UserContent::ToolResult(result)) = user.content.first_mut()
        {
            result.content = "x".repeat(100_000);
        }
        let context = ToolUseContext {
            main_loop_model: Some("claude-sonnet-4-6".to_string()),
            ..Default::default()
        };
        let mut state = crate::utils::tool_result_storage::ContentReplacementState::new();
        let result =
            mask_tool_results_if_needed(messages, &mut state, &context, &QuerySource::Prompt);
        assert_eq!(result.masked_count, 1);
        assert!(result.masked_tokens > 0);
        assert!(state.replacements.contains_key("toolu_old"));
        assert!(matches!(
            result.messages.last(),
            Some(Message::User(user))
                if user.content.iter().any(|content| matches!(
                    content,
                    UserContent::Text(text) if text == "current task"
                ))
        ));
        assert!(matches!(
            &result.messages[1],
            Message::User(user) if user.content.iter().any(|content| matches!(
                content,
                UserContent::ToolResult(result)
                    if crate::utils::tool_result_storage::is_tool_result_cleared(&result.content)
            ))
        ));
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
    }

    #[test]
    fn idle_time_never_clears_tool_results() {
        let _state_guard = lock_cached_mc_state_for_test();
        // `assistant_tool_message` stamps its message two hours in the past, so
        // this is a long idle gap by any threshold CC's time-based path used.
        let messages = vec![
            assistant_tool_message(&["toolu_old", "toolu_recent"], 0),
            user_tool_result_message(&["toolu_old", "toolu_recent"]),
        ];
        let result = microcompact_messages_with_configs(
            messages.clone(),
            &ToolUseContext::default(),
            &QuerySource::Prompt,
            CachedMicrocompactConfig::default(),
        );
        assert_eq!(result.messages, messages);
        assert!(result.boundary_messages.is_empty());
    }

    #[tokio::test]
    async fn cache_sharing_forks_replay_but_never_decide() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "50000");
        let mut messages = vec![
            assistant_tool_message(&["toolu_old"], 0),
            user_tool_result_message(&["toolu_old"]),
            Message::User(crate::utils::messages::create_user_message(
                "current task".to_string(),
            )),
        ];
        if let Message::User(user) = &mut messages[1]
            && let Some(UserContent::ToolResult(result)) = user.content.first_mut()
        {
            result.content = "x".repeat(100_000);
        }
        let context = ToolUseContext {
            main_loop_model: Some("claude-sonnet-4-6".to_string()),
            ..Default::default()
        };
        let mut state = crate::utils::tool_result_storage::ContentReplacementState::new();
        let result =
            mask_tool_results_if_needed(messages, &mut state, &context, &QuerySource::Compact);
        assert_eq!(result.masked_count, 0);
        assert!(result.newly_replaced.is_empty());
        assert!(state.replacements.is_empty());
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
    }

    #[tokio::test]
    async fn keep_shadow_records_comparison_when_enabled_without_changing_actual_outcome() {
        use crate::services::decision::{
            Answer, BackendFuture, DecisionBackend, DecisionOutcome, DecisionProvider,
            DecisionRequest, Receipt,
        };
        use std::collections::BTreeMap;
        use std::sync::Arc;

        struct MockKeepBackend;
        impl DecisionBackend for MockKeepBackend {
            fn decide<'a>(
                &'a self,
                req: &'a DecisionRequest,
                _deadline: std::time::Duration,
                _abort: &'a crate::tool::AbortController,
            ) -> BackendFuture<'a> {
                let mut answers = BTreeMap::new();
                for q in req.questions.keys() {
                    // Say keep = true (0.95 prob) for toolu_keep, false (0.1) for toolu_drop
                    let prob = if q.contains("keep") { 0.95 } else { 0.1 };
                    answers.insert(q.clone(), Answer::Noul { probability: prob });
                }
                Box::pin(async move {
                    DecisionOutcome::Answered {
                        receipt: Receipt {
                            point: "compaction.keep".into(),
                            model: "typesafe/jev-1.14.0".into(),
                            backend: "mock".into(),
                            answers: answers.clone(),
                            ..Default::default()
                        },
                        answers,
                    }
                })
            }
            fn name(&self) -> &'static str {
                "mock"
            }
        }

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "50000");

        // Temporary config dir so shadow log writes to an isolated file
        let temp_dir =
            std::env::temp_dir().join(format!("nyan-keep-shadow-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        crate::utils::process_env::set("NYAN_CONFIG_DIR", temp_dir.to_str().unwrap());

        // Install mock decision provider
        let mut decision_settings = crate::utils::settings::nyan::DecisionSettings::default();
        decision_settings.enabled = Some(true);
        let mock_provider = Arc::new(DecisionProvider::with_backend(
            decision_settings,
            Arc::new(MockKeepBackend),
        ));
        crate::services::compact::decision::test_override::set(mock_provider);

        let mut messages = vec![
            assistant_tool_message(&["toolu_drop", "toolu_keep"], 0),
            user_tool_result_message(&["toolu_drop", "toolu_keep"]),
            Message::User(crate::utils::messages::create_user_message(
                "current task".to_string(),
            )),
        ];
        if let Message::User(user) = &mut messages[1] {
            if let Some(UserContent::ToolResult(r1)) = user.content.get_mut(0) {
                r1.content = "x".repeat(60_000);
            }
            if let Some(UserContent::ToolResult(r2)) = user.content.get_mut(1) {
                r2.content = "y".repeat(60_000);
            }
        }
        let context = ToolUseContext {
            main_loop_model: Some("claude-sonnet-4-6".to_string()),
            ..Default::default()
        };
        let mut state = crate::utils::tool_result_storage::ContentReplacementState::new();
        crate::services::compact::decision::reset_shadow_task_counters_for_test();
        let result =
            mask_tool_results_if_needed(messages, &mut state, &context, &QuerySource::Prompt);

        // Actual outcome should still have cleared candidates according to C2 status quo
        assert!(result.masked_count >= 1);

        // Poll briefly until the background shadow task writes a full line:
        // the file is created before the record is written into it.
        let log_file = temp_dir.join("decision/compaction.keep.jsonl");
        let start = std::time::Instant::now();
        while !std::fs::read_to_string(&log_file).is_ok_and(|content| content.ends_with('\n'))
            && start.elapsed() < std::time::Duration::from_secs(3)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            log_file.exists(),
            "shadow log file must be written by background task"
        );
        let content = std::fs::read_to_string(&log_file).unwrap();
        assert!(!content.trim().is_empty());
        let record: crate::services::compact::decision::CompactionKeepShadowRecord =
            serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(record.point, "compaction.keep");
        assert_eq!(record.mode, "shadow");
        assert_eq!(record.outcome, "answered");
        assert_eq!(record.fallback_reason, Some("shadow".to_string()));
        assert_eq!(record.timeout_ms, 800);
        assert_eq!(record.candidates_count, 2);

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
        crate::services::compact::decision::test_override::clear();
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
        crate::utils::process_env::remove("NYAN_CONFIG_DIR");
    }
}
