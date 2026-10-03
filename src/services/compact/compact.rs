//! Conversation compaction service boundary.
//!
//! Maps to CC `services/compact/compact.ts`:
//! - `CompactionResult`
//! - `buildPostCompactMessages(...)`
//! - `compactConversation(...)`
//!
//! Summarization uses CC's one-turn cache-sharing fork, standalone streaming
//! fallback, abortable pre/post compact hooks, and SessionStart(compact).
//! Successful compaction rebuilds Read state and regenerates file, plan,
//! plan-mode, invoked-skill, async-agent, deferred-tool, agent-list, and MCP
//! instruction attachments before returning the canonical replacement segment.

use crate::types::message::{
    AssistantContent, AttachmentMessage, Message, SystemMessage, UserContent, UserMessage,
};
use chrono::Utc;

pub const ERROR_MESSAGE_NOT_ENOUGH_MESSAGES: &str = "Not enough messages to compact.";
pub const ERROR_MESSAGE_USER_ABORT: &str = "API Error: Request was aborted.";
pub const ERROR_MESSAGE_PROMPT_TOO_LONG: &str =
    "Conversation too long. Press esc twice to go up a few messages and try again.";
pub const ERROR_MESSAGE_INCOMPLETE_RESPONSE: &str =
    "Compaction interrupted · This may be due to network issues — please try again.";

/// Maps to CC `services/compact/compact.ts:122-130`.
pub const POST_COMPACT_MAX_FILES_TO_RESTORE: usize = 5;
pub const POST_COMPACT_TOKEN_BUDGET: i64 = 50_000;
pub const POST_COMPACT_MAX_TOKENS_PER_FILE: usize = 5_000;
pub const POST_COMPACT_MAX_TOKENS_PER_SKILL: i64 = 5_000;
pub const POST_COMPACT_SKILLS_TOKEN_BUDGET: i64 = 25_000;
const SKILL_TRUNCATION_MARKER: &str = "\n\n[... skill content truncated for compaction; use Read on the skill path if you need the full text]";

/// Maps to CC `stripImagesFromMessages(...)` for the standalone fallback.
pub fn strip_images_from_messages(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .cloned()
        .map(|message| match message {
            Message::User(mut user) => {
                user.content = user
                    .content
                    .into_iter()
                    .map(|block| match block {
                        UserContent::Image { .. } => UserContent::Text("[image]".to_string()),
                        UserContent::MetaImage { .. } => {
                            UserContent::MetaText("[image]".to_string())
                        }
                        UserContent::MetaDocument { .. } => {
                            UserContent::MetaText("[document]".to_string())
                        }
                        UserContent::RawImage { is_meta, .. } => {
                            if is_meta {
                                UserContent::MetaText("[image]".to_string())
                            } else {
                                UserContent::Text("[image]".to_string())
                            }
                        }
                        UserContent::Document { .. } => UserContent::Text("[document]".to_string()),
                        UserContent::ToolResult(mut result) => {
                            result.content_blocks = result
                                .content_blocks
                                .into_iter()
                                .map(|block| match block {
                                    crate::types::message::ToolResultContentBlock::RawImage(_)
                                    | crate::types::message::ToolResultContentBlock::Image {
                                        ..
                                    } => crate::types::message::ToolResultContentBlock::text(
                                        "[image]",
                                    ),
                                    crate::types::message::ToolResultContentBlock::Document {
                                        ..
                                    } => crate::types::message::ToolResultContentBlock::text(
                                        "[document]",
                                    ),
                                    block => block,
                                })
                                .collect();
                            UserContent::ToolResult(result)
                        }
                        block => block,
                    })
                    .collect();
                Message::User(user)
            }
            message => message,
        })
        .collect()
}

/// Maps to CC `stripReinjectedAttachments(...)`.
pub fn strip_reinjected_attachments(messages: Vec<Message>) -> Vec<Message> {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    ) {
        return messages;
    }
    messages
        .into_iter()
        .filter(|message| {
            !matches!(
                message,
                Message::Attachment(attachment)
                    if matches!(attachment.attachment_type(), "skill_discovery" | "skill_listing")
            )
        })
        .collect()
}

const PTL_RETRY_MARKER: &str = "[earlier conversation truncated for compaction retry]";

/// Maps to CC `truncateHeadForPTLRetry(...)`.
pub fn truncate_head_for_ptl_retry(messages: &[Message]) -> Option<Vec<Message>> {
    truncate_head_for_ptl_retry_with_gap(messages, None)
}

fn truncate_head_for_ptl_retry_with_gap(
    messages: &[Message],
    token_gap: Option<u64>,
) -> Option<Vec<Message>> {
    let input = match messages.first() {
        Some(Message::User(user)) if matches!(user.content.as_slice(), [UserContent::MetaText(text)] if text == PTL_RETRY_MARKER) => {
            &messages[1..]
        }
        _ => messages,
    };
    let groups = crate::services::compact::grouping::group_messages_by_api_round(input);
    if groups.len() < 2 {
        return None;
    }
    let drop_count = if let Some(token_gap) = token_gap {
        let mut accumulated = 0u64;
        let mut count = 0usize;
        for group in &groups {
            accumulated = accumulated.saturating_add(
                u64::try_from(
                    crate::services::token_estimation::rough_token_count_estimation_for_messages(
                        group,
                    ),
                )
                .unwrap_or_default(),
            );
            count = count.saturating_add(1);
            if accumulated >= token_gap {
                break;
            }
        }
        count
    } else {
        std::cmp::max(1, groups.len().saturating_mul(20) / 100)
    };
    let drop_count = std::cmp::min(groups.len() - 1, drop_count);
    let mut sliced = groups
        .into_iter()
        .skip(drop_count)
        .flatten()
        .collect::<Vec<_>>();
    if matches!(sliced.first(), Some(Message::Assistant(_))) {
        sliced.insert(
            0,
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![UserContent::MetaText(PTL_RETRY_MARKER.to_string())],
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
        );
    }
    Some(sliced)
}

/// Maps to CC `services/compact/compact.ts` `CompactionResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub boundary_marker: SystemMessage,
    pub summary_messages: Vec<UserMessage>,
    pub attachments: Vec<AttachmentMessage>,
    pub hook_results: Vec<Message>,
    pub messages_to_keep: Option<Vec<Message>>,
    pub user_display_message: Option<String>,
    pub pre_compact_token_count: Option<i64>,
    pub post_compact_token_count: Option<i64>,
    pub true_post_compact_token_count: Option<i64>,
    pub compaction_usage: Option<crate::types::message::TokenUsage>,
    /// Rust transport for CC's in-place `context.readFileState.clear()` plus
    /// FileRead repopulation during post-compact attachment generation.
    pub rebuilt_read_file_state: Vec<crate::utils::query_helpers::ReadFileStateEntry>,
}

/// Maps to CC `services/compact/compact.ts` `buildPostCompactMessages(...)`.
pub fn build_post_compact_messages(result: &CompactionResult) -> Vec<Message> {
    let mut messages = Vec::with_capacity(
        1 + result.summary_messages.len()
            + result
                .messages_to_keep
                .as_ref()
                .map_or(0, std::vec::Vec::len)
            + result.attachments.len()
            + result.hook_results.len(),
    );
    messages.push(Message::System(result.boundary_marker.clone()));
    messages.extend(result.summary_messages.iter().cloned().map(Message::User));
    if let Some(messages_to_keep) = &result.messages_to_keep {
        messages.extend(messages_to_keep.iter().cloned());
    }
    messages.extend(result.attachments.iter().cloned().map(Message::Attachment));
    messages.extend(result.hook_results.iter().cloned());
    messages
}

fn normalized_compact_path(
    path: &str,
    context: &crate::tool::ToolUseContext,
) -> std::path::PathBuf {
    let base = context
        .cwd_override
        .as_deref()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    crate::utils::path::expand_path(path, Some(&base)).unwrap_or_else(|_| base.join(path.trim()))
}

/// Maps to CC `services/compact/compact.ts:1587-1641`
/// `collectReadToolFilePaths(...)`.
fn collect_read_tool_file_paths(
    messages: &[Message],
    context: &crate::tool::ToolUseContext,
) -> std::collections::HashSet<std::path::PathBuf> {
    let stub_ids = messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(&user.content),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            UserContent::ToolResult(result)
                if result
                    .content
                    .starts_with(crate::tools::file_read_tool::prompt::FILE_UNCHANGED_STUB) =>
            {
                Some(result.tool_use_id.0.clone())
            }
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();

    messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(&assistant.content),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            AssistantContent::ToolUse(tool_use)
                if tool_use.name == crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME
                    && !stub_ids.contains(&tool_use.id.0) =>
            {
                tool_use
                    .input
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
            }
            _ => None,
        })
        .map(|path| normalized_compact_path(path, context))
        .collect()
}

fn should_exclude_from_post_compact_restore(
    filename: &str,
    context: &crate::tool::ToolUseContext,
) -> bool {
    let normalized = normalized_compact_path(filename, context);
    if normalized
        == normalized_compact_path(
            &crate::utils::plans::get_plan_file_path(context.agent_id.as_deref())
                .display()
                .to_string(),
            context,
        )
    {
        return true;
    }
    ["Managed", "User", "Project", "Local", "AutoMem", "TeamMem"]
        .into_iter()
        .map(crate::utils::config::get_memory_path)
        .map(|path| normalized_compact_path(&path.display().to_string(), context))
        .any(|path| path == normalized)
}

/// Maps to CC `services/compact/compact.ts:1396-1465`
/// `createPostCompactFileAttachments(...)`.
pub async fn create_post_compact_file_attachments(
    read_file_state: &[crate::utils::query_helpers::ReadFileStateEntry],
    tool_use_context: &mut crate::tool::ToolUseContext,
    max_files: usize,
    preserved_messages: &[Message],
) -> Vec<AttachmentMessage> {
    let preserved_paths = collect_read_tool_file_paths(preserved_messages, tool_use_context);
    let mut recent_files = read_file_state
        .iter()
        .filter(|entry| {
            !should_exclude_from_post_compact_restore(&entry.path, tool_use_context)
                && !preserved_paths
                    .contains(&normalized_compact_path(&entry.path, tool_use_context))
        })
        .collect::<Vec<_>>();
    recent_files.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp_ms.unwrap_or_default()));
    recent_files.truncate(max_files);

    struct RestoreResult {
        index: usize,
        attachment: Option<AttachmentMessage>,
    }

    let mut workers = futures::stream::FuturesUnordered::new();
    for (index, entry) in recent_files.into_iter().enumerate() {
        let mut worker_context = tool_use_context.clone();
        let path = entry.path.clone();
        workers.push(async move {
            let limits = crate::tools::file_read_tool::limits::FileReadingLimits {
                max_tokens: POST_COMPACT_MAX_TOKENS_PER_FILE as f64,
                ..crate::tools::file_read_tool::limits::get_default_file_reading_limits()
            };
            let attachment = crate::utils::attachments::generate_file_attachment(
                &path,
                &mut worker_context,
                crate::utils::attachments::FileAttachmentMode::Compact,
                crate::utils::attachments::GenerateFileAttachmentOptions::default(),
                limits,
            )
            .await;
            RestoreResult { index, attachment }
        });
    }

    use futures::StreamExt as _;
    let mut generated = Vec::new();
    while let Some(result) = workers.next().await {
        generated.push((result.index, result.attachment));
    }
    generated.sort_by_key(|(index, _)| *index);

    // Promise.all preserves recent-file order; Read effects already commit
    // through the shared context identities as each worker settles.
    let mut used_tokens = 0i64;
    generated
        .into_iter()
        .filter_map(|(_, attachment)| attachment)
        .filter(|attachment| {
            let tokens = crate::services::token_estimation::rough_token_count_estimation(
                &serde_json::to_string(attachment).unwrap_or_default(),
            );
            if used_tokens.saturating_add(tokens) > POST_COMPACT_TOKEN_BUDGET {
                return false;
            }
            used_tokens = used_tokens.saturating_add(tokens);
            true
        })
        .collect()
}

/// Maps to CC `services/compact/compact.ts:1470-1485`
/// `createPlanAttachmentIfNeeded(...)`.
pub fn create_plan_attachment_if_needed(agent_id: Option<&str>) -> Option<AttachmentMessage> {
    let plan_content = crate::utils::plans::get_plan(agent_id)?;
    (!plan_content.is_empty()).then(|| {
        AttachmentMessage::new(serde_json::json!({
            "type": "plan_file_reference",
            "planFilePath": crate::utils::plans::get_plan_file_path(agent_id),
            "planContent": plan_content,
        }))
    })
}

fn truncate_skill_to_tokens(content: &str, max_tokens: i64) -> String {
    if crate::services::token_estimation::rough_token_count_estimation(content) <= max_tokens {
        return content.to_string();
    }
    let char_budget = usize::try_from(max_tokens.saturating_mul(4))
        .unwrap_or(usize::MAX)
        .saturating_sub(SKILL_TRUNCATION_MARKER.len());
    let mut utf16_units = 0usize;
    let end = content
        .char_indices()
        .take_while(|(_, character)| {
            let next = utf16_units.saturating_add(character.len_utf16());
            if next > char_budget {
                return false;
            }
            utf16_units = next;
            true
        })
        .last()
        .map(|(index, character)| index + character.len_utf8())
        .unwrap_or(0);
    format!("{}{SKILL_TRUNCATION_MARKER}", &content[..end])
}

/// Maps to CC `services/compact/compact.ts:1487-1534`
/// `createSkillAttachmentIfNeeded(...)`.
pub fn create_skill_attachment_if_needed(agent_id: Option<&str>) -> Option<AttachmentMessage> {
    let mut invoked = crate::bootstrap::state::get_invoked_skills_for_agent(agent_id);
    invoked.sort_by_key(|skill| std::cmp::Reverse(skill.invoked_at_ms));
    let mut used_tokens = 0i64;
    let skills = invoked
        .into_iter()
        .filter_map(|skill| {
            let content =
                truncate_skill_to_tokens(&skill.content, POST_COMPACT_MAX_TOKENS_PER_SKILL);
            let tokens = crate::services::token_estimation::rough_token_count_estimation(&content);
            if used_tokens.saturating_add(tokens) > POST_COMPACT_SKILLS_TOKEN_BUDGET {
                return None;
            }
            used_tokens = used_tokens.saturating_add(tokens);
            Some(serde_json::json!({
                "name": skill.skill_name,
                "path": skill.skill_path,
                "content": content,
            }))
        })
        .collect::<Vec<_>>();
    (!skills.is_empty()).then(|| {
        AttachmentMessage::new(serde_json::json!({
            "type": "invoked_skills",
            "skills": skills,
        }))
    })
}

/// Maps to CC `services/compact/compact.ts:1536-1560`
/// `createPlanModeAttachmentIfNeeded(...)`.
pub fn create_plan_mode_attachment_if_needed(
    context: &crate::tool::ToolUseContext,
) -> Option<AttachmentMessage> {
    let mode = context
        .get_app_state()
        .map(|state| state.tool_permission_context.mode)
        .unwrap_or(context.tool_permission_context.mode);
    (mode == crate::types::permissions::PermissionMode::Plan).then(|| {
        let plan_file_path = crate::utils::plans::get_plan_file_path(context.agent_id.as_deref());
        AttachmentMessage::new(serde_json::json!({
            "type": "plan_mode",
            "reminderType": "full",
            "isSubAgent": context.agent_id.is_some(),
            "planFilePath": plan_file_path,
            "planExists": crate::utils::plans::get_plan(context.agent_id.as_deref()).is_some(),
        }))
    })
}

/// Maps to CC `services/compact/compact.ts:1562-1585`
/// `createAsyncAgentAttachmentsIfNeeded(...)`.
pub fn create_async_agent_attachments_if_needed(
    context: &crate::tool::ToolUseContext,
) -> Vec<AttachmentMessage> {
    let mut agents = crate::tasks::local_agent_task::local_agent_tasks_snapshot();
    agents.sort_by(|left, right| {
        left.start_time_ms
            .cmp(&right.start_time_ms)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    agents
        .into_iter()
        .filter(|agent| {
            !agent.retrieved
                && agent.status != "pending"
                && context.agent_id.as_deref() != Some(agent.agent_id.as_str())
        })
        .map(|agent| {
            let delta_summary = if agent.status == "running" {
                agent.progress.and_then(|progress| progress.summary)
            } else {
                agent.error
            };
            AttachmentMessage::new(serde_json::json!({
                "type": "task_status",
                "taskId": agent.agent_id,
                "taskType": "local_agent",
                "description": agent.description,
                "status": agent.status,
                "deltaSummary": delta_summary,
                "outputFilePath": crate::utils::task::disk_output::get_task_output_path(&agent.task_id),
            }))
        })
        .collect()
}

async fn create_post_compact_attachments(
    read_file_state: &[crate::utils::query_helpers::ReadFileStateEntry],
    context: &mut crate::tool::ToolUseContext,
    preserved_messages: &[Message],
) -> Vec<AttachmentMessage> {
    let mut attachments = create_post_compact_file_attachments(
        read_file_state,
        context,
        POST_COMPACT_MAX_FILES_TO_RESTORE,
        preserved_messages,
    )
    .await;
    attachments.extend(create_async_agent_attachments_if_needed(context));
    attachments.extend(create_plan_attachment_if_needed(
        context.agent_id.as_deref(),
    ));
    attachments.extend(create_plan_mode_attachment_if_needed(context));
    attachments.extend(create_skill_attachment_if_needed(
        context.agent_id.as_deref(),
    ));

    let model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    attachments.extend(
        crate::utils::attachments::get_deferred_tools_delta_attachment(
            &context.tools,
            &model,
            preserved_messages,
        ),
    );
    attachments.extend(
        crate::utils::attachments::get_agent_listing_delta_attachment(context, preserved_messages),
    );
    attachments.extend(
        crate::utils::attachments::get_mcp_instructions_delta_attachment(
            context,
            preserved_messages,
        ),
    );
    attachments
}

fn compact_hook_context(
    context: &crate::tool::ToolUseContext,
) -> crate::services::hooks::HookContext {
    let original_cwd = crate::bootstrap::state::get_original_cwd();
    let permission_mode = context
        .get_app_state()
        .map(|state| state.tool_permission_context.mode)
        .unwrap_or(context.tool_permission_context.mode);
    crate::services::hooks::HookContext {
        session_id: crate::bootstrap::state::get_session_id(),
        transcript_path: crate::utils::session_storage::get_transcript_path(
            context.agent_id.as_deref(),
        )
        .display()
        .to_string(),
        cwd: context
            .cwd_override
            .clone()
            .unwrap_or(original_cwd)
            .display()
            .to_string(),
        permission_mode: Some(
            crate::utils::permissions::permission_mode::permission_mode_internal_name(
                permission_mode,
            )
            .to_string(),
        ),
        agent_id: context.agent_id.clone(),
        agent_type: context.agent_type.clone(),
    }
}

/// Maps to CC `services/compact/compact.ts` `compactConversation(...)`.
///
/// Unlike the former deterministic seam, this runs the official one-turn
/// cache-sharing fork and falls back to a standalone compact model stream.
pub async fn compact_conversation(
    messages: Vec<Message>,
    context: &crate::tool::ToolUseContext,
    cache_safe_params: &crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
    suppress_follow_up_questions: bool,
    custom_instructions: Option<&str>,
    is_auto_compact: bool,
) -> Result<CompactionResult, String> {
    if messages.is_empty() {
        return Err(ERROR_MESSAGE_NOT_ENOUGH_MESSAGES.to_string());
    }
    if context.abort_controller.is_aborted() {
        return Err(ERROR_MESSAGE_USER_ABORT.to_string());
    }

    let pre_compact_token_count = crate::utils::tokens::token_count_with_estimation(&messages);
    let trigger = if is_auto_compact { "auto" } else { "manual" };
    let hooks_config = crate::services::hooks::load_hooks_config().config;
    let hook_context = compact_hook_context(context);
    let pre_hook = crate::services::hooks::compaction::execute_pre_compact_hooks(
        &hooks_config,
        trigger,
        custom_instructions,
        &hook_context,
        &context.abort_controller,
    )
    .await;
    let merged_instructions = merge_hook_instructions(
        custom_instructions,
        pre_hook.new_custom_instructions.as_deref(),
    );
    let compact_prompt =
        crate::services::compact::prompt::get_compact_prompt(merged_instructions.as_deref());
    let summary_request = UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        content: vec![UserContent::Text(compact_prompt)],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    };

    let mut messages_to_summarize = messages.clone();
    let mut retry_cache_safe_params = cache_safe_params.clone();
    let mut ptl_attempts = 0usize;
    let (summary_response, summary) = loop {
        let response = stream_compact_summary(
            &messages_to_summarize,
            summary_request.clone(),
            context,
            &retry_cache_safe_params,
        )
        .await?;
        if context.abort_controller.is_aborted() {
            return Err(ERROR_MESSAGE_USER_ABORT.to_string());
        }
        let summary = crate::utils::messages::get_assistant_message_text(&Message::Assistant(
            response.assistant.clone(),
        ))
        .ok_or_else(|| {
            "Failed to generate conversation summary - response did not contain valid text content"
                .to_string()
        })?;
        if !summary.starts_with(crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE) {
            break (response, summary);
        }
        ptl_attempts += 1;
        let (actual_tokens, token_limit) =
            crate::services::api::errors::parse_prompt_too_long_token_counts(&summary);
        let token_gap = actual_tokens
            .zip(token_limit)
            .and_then(|(actual, limit)| actual.checked_sub(limit))
            .filter(|gap| *gap > 0);
        let Some(truncated) = (ptl_attempts <= 3)
            .then(|| truncate_head_for_ptl_retry_with_gap(&messages_to_summarize, token_gap))
            .flatten()
        else {
            return Err(ERROR_MESSAGE_PROMPT_TOO_LONG.to_string());
        };
        retry_cache_safe_params.fork_context_messages = truncated.clone();
        messages_to_summarize = truncated;
    };
    if summary.trim_start().starts_with("API Error:") {
        return Err(summary);
    }

    // Store current read state before the compact boundary replaces history,
    // then regenerate model-visible restoration attachments from fresh data.
    let pre_compact_read_file_state = context.read_file_state.snapshot();
    let mut post_compact_context = context.clone();
    if let Some(state) = context.get_app_state() {
        post_compact_context.tool_permission_context = (*state.tool_permission_context).clone();
    }
    post_compact_context.read_file_state.clear();
    post_compact_context.loaded_nested_memory_paths.clear();
    let post_compact_attachments = create_post_compact_attachments(
        &pre_compact_read_file_state,
        &mut post_compact_context,
        &[],
    )
    .await;
    let rebuilt_read_file_state = post_compact_context.read_file_state.snapshot();

    let hook_messages = crate::utils::session_start::process_session_start_hooks_async(
        "compact",
        None,
        None,
        context.main_loop_model.as_deref(),
    )
    .await
    .into_iter()
    .map(Message::HookResult)
    .collect::<Vec<_>>();

    // Maps to CC `compact.ts` `reAppendSessionMetadata()` after a successful
    // compact so custom title/tag stay inside the resume tail window.
    if let Err(error) = crate::utils::session_storage::re_append_session_metadata() {
        crate::utils::debug::log_for_debugging(&format!(
            "Failed to re-append session metadata after compact: {error}"
        ));
    }

    let post_hook = crate::services::hooks::compaction::execute_post_compact_hooks(
        &hooks_config,
        trigger,
        &summary,
        &hook_context,
        &context.abort_controller,
    )
    .await;
    let user_display_message = [
        pre_hook.user_display_message,
        post_hook.user_display_message,
    ]
    .into_iter()
    .flatten()
    .filter(|message| !message.is_empty())
    .collect::<Vec<_>>()
    .join("\n");

    let transcript_path = hook_context.transcript_path;
    Ok(finalize_compaction_result(
        &messages,
        &summary,
        suppress_follow_up_questions,
        trigger,
        Some(&transcript_path),
        pre_compact_token_count,
        summary_response.compaction_call_tokens,
        summary_response.assistant.usage.clone(),
        post_compact_attachments,
        rebuilt_read_file_state,
        hook_messages,
        (!user_display_message.is_empty()).then_some(user_display_message),
    ))
}

/// Maps to CC compact.ts#annotateBoundaryWithPreservedSegment.
fn annotate_boundary_with_preserved_segment(
    mut boundary: SystemMessage,
    anchor_uuid: &str,
    kept: &[Message],
) -> SystemMessage {
    if let (Some(head), Some(tail)) = (kept.first(), kept.last()) {
        if let SystemMessage::CompactBoundary {
            compact_metadata, ..
        } = &mut boundary
        {
            compact_metadata.get_or_insert_default().preserved_segment = Some(serde_json::json!({
                "headUuid": head.uuid(), "tailUuid": tail.uuid(), "anchorUuid": anchor_uuid,
            }));
        }
    }
    boundary
}

/// Maps to CC compact.ts:772-1109#partialCompactConversation.
pub async fn partial_compact_conversation(
    all_messages: Vec<Message>,
    pivot_index: usize,
    context: &crate::tool::ToolUseContext,
    cache_safe_params: &crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
    user_feedback: Option<&str>,
    direction: crate::types::message::PartialCompactDirection,
) -> Result<CompactionResult, String> {
    use crate::types::message::{CompactMetadata, PartialCompactDirection};
    let pivot = pivot_index.min(all_messages.len());
    let (summarized, kept) = match direction {
        PartialCompactDirection::From => (&all_messages[pivot..], &all_messages[..pivot]),
        PartialCompactDirection::UpTo => (&all_messages[..pivot], &all_messages[pivot..]),
    };
    let messages_to_keep: Vec<Message> = kept
        .iter()
        .filter(|message| {
            !matches!(message, Message::Progress(_))
                && (direction == PartialCompactDirection::From
                    || !matches!(
                        message,
                        Message::System(SystemMessage::CompactBoundary { .. })
                    ) && !matches!(message, Message::User(user) if user.is_compact_summary))
        })
        .cloned()
        .collect();
    if summarized.is_empty() {
        return Err(match direction {
            PartialCompactDirection::From => "Nothing to summarize after the selected message.",
            PartialCompactDirection::UpTo => "Nothing to summarize before the selected message.",
        }
        .to_string());
    }
    let pre_compact_token_count = crate::utils::tokens::token_count_with_estimation(&all_messages);
    let hooks_config = crate::services::hooks::load_hooks_config().config;
    let hook_context = compact_hook_context(context);
    let pre_hook = crate::services::hooks::compaction::execute_pre_compact_hooks(
        &hooks_config,
        "manual",
        None,
        &hook_context,
        &context.abort_controller,
    )
    .await;
    let instructions = match (
        pre_hook.new_custom_instructions.filter(|s| !s.is_empty()),
        user_feedback.filter(|s| !s.is_empty()),
    ) {
        (Some(hook), Some(feedback)) => Some(format!("{hook}\n\nUser context: {feedback}")),
        (Some(hook), None) => Some(hook),
        (None, Some(feedback)) => Some(format!("User context: {feedback}")),
        (None, None) => None,
    };
    let summary_request = crate::utils::messages::create_user_message(
        crate::services::compact::prompt::get_partial_compact_prompt(
            instructions.as_deref(),
            direction,
        ),
    );
    let api_messages = if direction == PartialCompactDirection::UpTo {
        summarized.to_vec()
    } else {
        all_messages.clone()
    };
    let mut messages_to_summarize = api_messages;
    let mut retry_cache_safe_params = cache_safe_params.clone();
    retry_cache_safe_params.fork_context_messages = messages_to_summarize.clone();
    let mut ptl_attempts = 0usize;
    let (summary_response, summary) = loop {
        let response = stream_compact_summary(
            &messages_to_summarize,
            summary_request.clone(),
            context,
            &retry_cache_safe_params,
        )
        .await?;
        if context.abort_controller.is_aborted() {
            return Err(ERROR_MESSAGE_USER_ABORT.to_string());
        }
        let summary = crate::utils::messages::get_assistant_message_text(&Message::Assistant(
            response.assistant.clone(),
        ))
        .ok_or_else(|| {
            "Failed to generate conversation summary - response did not contain valid text content"
                .to_string()
        })?;
        if !summary.starts_with(crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE) {
            break (response, summary);
        }
        ptl_attempts += 1;
        let (actual_tokens, token_limit) =
            crate::services::api::errors::parse_prompt_too_long_token_counts(&summary);
        let token_gap = actual_tokens
            .zip(token_limit)
            .and_then(|(actual, limit)| actual.checked_sub(limit))
            .filter(|gap| *gap > 0);
        let Some(truncated) = (ptl_attempts <= 3)
            .then(|| truncate_head_for_ptl_retry_with_gap(&messages_to_summarize, token_gap))
            .flatten()
        else {
            return Err(ERROR_MESSAGE_PROMPT_TOO_LONG.to_string());
        };
        retry_cache_safe_params.fork_context_messages = truncated.clone();
        messages_to_summarize = truncated;
    };
    if crate::services::api::errors::starts_with_api_error_prefix(&summary) {
        return Err(summary);
    }

    let read_state = context.read_file_state.snapshot();
    let mut post_context = context.clone();
    if let Some(state) = context.get_app_state() {
        post_context.tool_permission_context = (*state.tool_permission_context).clone();
    }
    post_context.read_file_state.clear();
    post_context.loaded_nested_memory_paths.clear();
    let attachments =
        create_post_compact_attachments(&read_state, &mut post_context, &messages_to_keep).await;
    let hook_results = crate::utils::session_start::process_session_start_hooks_async(
        "compact",
        None,
        None,
        context.main_loop_model.as_deref(),
    )
    .await
    .into_iter()
    .map(Message::HookResult)
    .collect();
    let parent = if direction == PartialCompactDirection::UpTo {
        all_messages[..pivot]
            .iter()
            .rev()
            .find(|m| !matches!(m, Message::Progress(_)))
    } else {
        messages_to_keep.last()
    };
    let mut discovered = crate::utils::tool_search::extract_discovered_tool_names(&all_messages)
        .into_iter()
        .collect::<Vec<_>>();
    discovered.sort();
    let mut boundary = SystemMessage::compact_boundary(Some(CompactMetadata {
        trigger: Some("manual".to_string()),
        pre_tokens: Some(pre_compact_token_count),
        user_context: user_feedback.map(str::to_string),
        messages_summarized: Some(summarized.len()),
        pre_compact_discovered_tools: discovered,
        ..Default::default()
    }));
    if let SystemMessage::CompactBoundary {
        logical_parent_uuid,
        ..
    } = &mut boundary
    {
        *logical_parent_uuid = parent.map(|message| message.uuid().to_string());
    }
    let mut summary_message = crate::utils::messages::create_user_message(
        crate::services::compact::prompt::get_compact_user_summary_message(
            &summary,
            false,
            Some(&hook_context.transcript_path),
            false,
        ),
    );
    summary_message.is_compact_summary = true;
    if messages_to_keep.is_empty() {
        summary_message.is_visible_in_transcript_only = true;
    } else {
        summary_message.summarize_metadata = Some(CompactMetadata {
            messages_summarized: Some(summarized.len()),
            user_context: user_feedback.map(str::to_string),
            direction: Some(direction.as_str().to_string()),
            ..Default::default()
        });
    }
    // CC notifyCompaction/markPostCompaction feed API telemetry only (PORTING.md: n-a).
    if let Err(error) = crate::utils::session_storage::re_append_session_metadata() {
        crate::utils::debug::log_for_debugging(&format!(
            "Failed to re-append session metadata after compact: {error}"
        ));
    }
    let post_hook = crate::services::hooks::compaction::execute_post_compact_hooks(
        &hooks_config,
        "manual",
        &summary,
        &hook_context,
        &context.abort_controller,
    )
    .await;
    let anchor = if direction == PartialCompactDirection::UpTo {
        summary_message.uuid.clone()
    } else {
        boundary.base().uuid.clone()
    };
    Ok(CompactionResult {
        boundary_marker: annotate_boundary_with_preserved_segment(
            boundary,
            &anchor,
            &messages_to_keep,
        ),
        summary_messages: vec![summary_message],
        messages_to_keep: Some(messages_to_keep),
        attachments,
        hook_results,
        user_display_message: post_hook.user_display_message,
        pre_compact_token_count: Some(pre_compact_token_count),
        post_compact_token_count: Some(
            summary_response
                .assistant
                .usage
                .as_ref()
                .map(crate::utils::tokens::get_token_count_from_usage)
                .unwrap_or(0),
        ),
        true_post_compact_token_count: None,
        compaction_usage: summary_response.assistant.usage,
        rebuilt_read_file_state: post_context.read_file_state.snapshot(),
    })
}

#[derive(Debug)]
struct CompactSummaryResponse {
    assistant: crate::types::message::AssistantMessage,
    compaction_call_tokens: i64,
}

fn compact_fork_params(
    summary_request: UserMessage,
    context: &crate::tool::ToolUseContext,
    cache_safe_params: &crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
) -> crate::utils::forked_agent::ForkedAgentParams {
    // Maps to: CC `services/compact/compact.ts:1125-1133#createCompactCanUseTool`
    // — `{ behavior: 'deny', message: 'Tool use is not allowed during
    // compaction', decisionReason: { type: 'other', reason: 'compaction agent
    // should only produce text summary' } }`.
    let deny_tools = crate::tool::CanUseToolCallback::new(
        |_tool, _input, _context, _assistant, _tool_use_id, _force| {
            crate::types::permissions::PermissionDecision::Deny {
                message: "Tool use is not allowed during compaction".to_string(),
                decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                    reason: "compaction agent should only produce text summary".to_string(),
                },
                tool_use_id: None,
            }
        },
    );
    crate::utils::forked_agent::ForkedAgentParams {
        prompt_messages: vec![Message::User(summary_request)],
        cache_safe_params: crate::utils::forked_agent::CacheSafeParams {
            system_prompt: cache_safe_params.system_prompt.clone(),
            user_context: cache_safe_params.user_context.clone(),
            system_context: cache_safe_params.system_context.clone(),
            tool_use_context: context.clone(),
            fork_context_messages: std::sync::Arc::new(
                cache_safe_params.fork_context_messages.clone(),
            ),
        },
        can_use_tool: deny_tools,
        query_source: crate::constants::query_source::QuerySource::Compact,
        fork_label: "compact".to_string(),
        max_turns: Some(1),
        max_output_tokens: None,
        skip_cache_write: true,
        overrides: Some(crate::utils::forked_agent::SubagentContextOverrides {
            abort_controller: Some(context.abort_controller.clone()),
            ..Default::default()
        }),
        on_message: None,
        on_progress: None,
        skip_transcript: false,
    }
}

async fn stream_compact_summary(
    messages: &[Message],
    summary_request: UserMessage,
    context: &crate::tool::ToolUseContext,
    cache_safe_params: &crate::services::compact::auto_compact::AutoCompactCacheSafeParams,
) -> Result<CompactSummaryResponse, String> {
    let options = compact_fork_params(summary_request.clone(), context, cache_safe_params);
    match crate::utils::forked_agent::run_forked_agent(options).await {
        Ok(result) if result.api_errors.is_empty() => {
            if let Some(assistant) =
                result
                    .messages
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        Message::Assistant(assistant)
                            if crate::utils::messages::get_assistant_message_text(message)
                                .is_some() =>
                        {
                            Some(assistant.clone())
                        }
                        _ => None,
                    })
            {
                let usage = &result.total_usage;
                let total = usage
                    .input_tokens
                    .saturating_add(usage.output_tokens)
                    .saturating_add(usage.cache_creation_input_tokens)
                    .saturating_add(usage.cache_read_input_tokens);
                return Ok(CompactSummaryResponse {
                    assistant,
                    compaction_call_tokens: i64::try_from(total).unwrap_or(i64::MAX),
                });
            }
        }
        Ok(_) => {}
        Err(error) if crate::utils::errors::is_abort_error(&error) => {
            return Err(ERROR_MESSAGE_USER_ABORT.to_string());
        }
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Compact cache-sharing fork failed, using streaming fallback: {error}"
            ));
        }
    }

    stream_compact_summary_fallback(messages, summary_request, context).await
}

async fn stream_compact_summary_fallback(
    messages: &[Message],
    summary_request: UserMessage,
    context: &crate::tool::ToolUseContext,
) -> Result<CompactSummaryResponse, String> {
    let after_boundary = crate::utils::messages::get_messages_after_compact_boundary(messages);
    let mut api_messages =
        strip_reinjected_attachments(strip_images_from_messages(&after_boundary));
    api_messages.push(Message::User(summary_request));
    let app_state = context.get_app_state();
    let model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let mut options = crate::services::api::claude::Options::new(
        model.clone(),
        crate::constants::query_source::QuerySource::Compact
            .as_api_source()
            .to_string(),
    );
    options.is_non_interactive_session = context.is_non_interactive_session;
    options.has_append_system_prompt = context.append_system_prompt.is_some();
    options.max_output_tokens_override = Some(
        (crate::utils::context::COMPACT_MAX_OUTPUT_TOKENS as u32)
            .min(crate::services::api::claude::get_max_output_tokens_for_model(&model)),
    );
    options.effort_value = app_state.and_then(|state| state.effort_value.clone());
    // API request descriptors are narrower than ToolUseContext agent
    // definitions. The text-only compact fallback exposes no agent tools.
    options.agents = Vec::new();
    options.abort_signal = Some(context.abort_controller.signal());
    let mut stream = crate::services::api::claude::query_model_with_streaming(
        &api_messages,
        &vec!["You are a helpful AI assistant tasked with summarizing conversations.".to_string()],
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[crate::tools::file_read_tool::file_read_tool_schema()],
        &options,
    )
    .await
    .map_err(|error| {
        if crate::utils::errors::is_abort_error(&error) {
            ERROR_MESSAGE_USER_ABORT.to_string()
        } else {
            error.to_string()
        }
    })?;

    let mut response = None;
    while let Some(item) = stream.recv().await {
        match item {
            crate::services::api::claude::QueryModelStreamItem::Stream(
                crate::types::message::StreamEvent::ApiEvent { event, .. },
            ) => {
                if event.get("type").and_then(serde_json::Value::as_str)
                    == Some("content_block_delta")
                    && event
                        .get("delta")
                        .and_then(|delta| delta.get("type"))
                        .and_then(serde_json::Value::as_str)
                        == Some("text_delta")
                {
                    let streamed = event
                        .get("delta")
                        .and_then(|delta| delta.get("text"))
                        .and_then(serde_json::Value::as_str)
                        .map(|text| text.encode_utf16().count())
                        .unwrap_or(0);
                    context.response_length_sink.add(streamed);
                }
            }
            crate::services::api::claude::QueryModelStreamItem::Assistant(assistant) => {
                response = Some(assistant);
            }
            crate::services::api::claude::QueryModelStreamItem::AssistantDelta {
                stop_reason,
                usage,
            } => {
                if let Some(assistant) = response.as_mut() {
                    assistant.stop_reason = stop_reason;
                    assistant.usage = usage;
                }
            }
            crate::services::api::claude::QueryModelStreamItem::SystemError(error) => {
                return Err(format!("API Error: {}", error.content));
            }
            // Retry heartbeats are non-terminal: withRetry is still working
            // toward an assistant response, so the compact loop keeps waiting
            // (CC's consumer ignores the SystemAPIErrorMessage yields).
            crate::services::api::claude::QueryModelStreamItem::SystemApiError(_)
            | crate::services::api::claude::QueryModelStreamItem::ModelFallback { .. }
            | crate::services::api::claude::QueryModelStreamItem::Content(_)
            | crate::services::api::claude::QueryModelStreamItem::CompletedContent(_)
            | crate::services::api::claude::QueryModelStreamItem::StreamingFallback => {}
        }
    }
    let assistant = response.ok_or_else(|| ERROR_MESSAGE_INCOMPLETE_RESPONSE.to_string())?;
    let total = assistant.usage.as_ref().map_or(0, |usage| {
        usage
            .input_tokens
            .saturating_add(usage.output_tokens)
            .saturating_add(usage.cache_creation_input_tokens)
            .saturating_add(usage.cache_read_input_tokens)
    });
    Ok(CompactSummaryResponse {
        assistant,
        compaction_call_tokens: i64::try_from(total).unwrap_or(i64::MAX),
    })
}

fn merge_hook_instructions(user: Option<&str>, hook: Option<&str>) -> Option<String> {
    match (
        user.filter(|value| !value.is_empty()),
        hook.filter(|value| !value.is_empty()),
    ) {
        (Some(user), Some(hook)) => Some(format!("{user}\n\n{hook}")),
        (Some(user), None) => Some(user.to_string()),
        (None, Some(hook)) => Some(hook.to_string()),
        (None, None) => None,
    }
}

fn finalize_compaction_result(
    messages: &[Message],
    summary: &str,
    suppress_follow_up_questions: bool,
    trigger: &str,
    transcript_path: Option<&str>,
    pre_compact_token_count: i64,
    compaction_call_tokens: i64,
    compaction_usage: Option<crate::types::message::TokenUsage>,
    attachments: Vec<AttachmentMessage>,
    rebuilt_read_file_state: Vec<crate::utils::query_helpers::ReadFileStateEntry>,
    hook_results: Vec<Message>,
    user_display_message: Option<String>,
) -> CompactionResult {
    let mut pre_compact_discovered_tools =
        crate::utils::tool_search::extract_discovered_tool_names(messages)
            .into_iter()
            .collect::<Vec<_>>();
    pre_compact_discovered_tools.sort();
    let summary_message = UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        content: vec![UserContent::Text(
            crate::services::compact::prompt::get_compact_user_summary_message(
                summary,
                suppress_follow_up_questions,
                transcript_path,
                false,
            ),
        )],
        is_compact_summary: true,
        plan_content: None,
        image_paste_ids: None,
        // CC full-compact summary (`services/compact/compact.ts:614-623`):
        // `isCompactSummary: true, isVisibleInTranscriptOnly: true` — the
        // summary text renders only in ctrl+o transcript mode; the default
        // view shows the compact boundary instead. The partial-compact
        // variant (`compact.ts:1031-1044`, `summarizeMetadata` when
        // messagesToKeep is non-empty) is handled by partial_compact_conversation;
        // this finalizer produces `messages_to_keep: None`, i.e. CC's full path.
        is_visible_in_transcript_only: true,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    };
    // Maps to: CC `createCompactBoundaryMessage` (`utils/messages.ts:4530-
    // 4555`); the constant content/level live on the wire adapter.
    let boundary_marker =
        SystemMessage::compact_boundary(Some(crate::types::message::CompactMetadata {
            trigger: Some(trigger.to_string()),
            pre_tokens: Some(pre_compact_token_count),
            pre_compact_discovered_tools,
            ..Default::default()
        }));
    let mut result = CompactionResult {
        boundary_marker,
        summary_messages: vec![summary_message],
        attachments,
        hook_results,
        messages_to_keep: None,
        user_display_message,
        pre_compact_token_count: Some(pre_compact_token_count),
        post_compact_token_count: Some(compaction_call_tokens),
        true_post_compact_token_count: None,
        compaction_usage,
        rebuilt_read_file_state,
    };
    result.true_post_compact_token_count = Some(crate::utils::tokens::token_count_with_estimation(
        &build_post_compact_messages(&result),
    ));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantMessage, UserMessage};
    use crate::utils::test_env::{EnvVarGuard, IsolatedProjectSettings, TEST_ENV_LOCK};

    #[test]
    fn strip_media_matches_official_meta_for_simple_and_raw_images() {
        // CC compact.ts:157-164 replaces all image/document blocks without
        // changing the user envelope's isMeta, irrespective of source fields.
        let mut user = crate::utils::messages::create_user_message("unused".into());
        user.content = vec![
            UserContent::MetaImage {
                media_type: "image/png".into(),
                data: "eA==".into(),
            },
            UserContent::RawImage {
                block: serde_json::json!({"type":"image","source":{"type":"url","url":"https://example.invalid/a"}}),
                is_meta: true,
            },
            UserContent::MetaDocument {
                media_type: "application/pdf".into(),
                data: "eA==".into(),
            },
        ];
        let messages = strip_images_from_messages(&[Message::User(user)]);
        let Message::User(user) = &messages[0] else {
            panic!("user")
        };
        assert_eq!(
            user.content,
            vec![
                UserContent::MetaText("[image]".into()),
                UserContent::MetaText("[image]".into()),
                UserContent::MetaText("[document]".into())
            ]
        );
    }

    #[test]
    fn prompt_too_long_retry_drops_oldest_round_and_repairs_assistant_first() {
        let user = |text: &str| {
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
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
        };
        let assistant = |api_message_id: &str, text: &str| {
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![
                    AssistantContent::Text(text.to_string()),
                    AssistantContent::MessageIdentity(
                        crate::types::message::AssistantMessageIdentity {
                            request_id: Some("req-compact".to_string()),
                            api_message_id: Some(api_message_id.to_string()),
                            ..Default::default()
                        },
                    ),
                ],
                model: None,
                stop_reason: None,
                usage: None,
            })
        };
        let messages = vec![
            user("old"),
            assistant("msg-round-1", "round one"),
            user("result"),
            assistant("msg-round-2", "round two"),
        ];

        let truncated = truncate_head_for_ptl_retry(&messages).expect("multiple API rounds");

        assert!(matches!(
            truncated.first(),
            Some(Message::User(UserMessage { content, .. }))
                if matches!(content.as_slice(), [UserContent::MetaText(text)] if text == PTL_RETRY_MARKER)
        ));
        assert!(matches!(truncated.get(1), Some(Message::Assistant(_))));
    }

    #[test]
    fn compact_fallback_strips_direct_and_tool_result_media() {
        let message = Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![
                UserContent::Image {
                    media_type: "image/png".to_string(),
                    data: "large".to_string(),
                },
                UserContent::ToolResult(crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("toolu-media".to_string()),
                    content: String::new(),
                    is_error: false,
                    content_blocks: vec![
                        crate::types::message::ToolResultContentBlock::image_base64(
                            "image/png",
                            "large",
                        ),
                    ],
                    tool_use_result: None,
                }),
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
        });

        let stripped = strip_images_from_messages(&[message]);
        let Message::User(user) = &stripped[0] else {
            panic!("expected user message");
        };
        assert!(matches!(&user.content[0], UserContent::Text(text) if text == "[image]"));
        let UserContent::ToolResult(result) = &user.content[1] else {
            panic!("expected tool result");
        };
        assert!(matches!(
            &result.content_blocks[0],
            crate::types::message::ToolResultContentBlock::Text { text } if text == "[image]"
        ));
    }

    #[test]
    fn compact_fork_params_use_one_turn_cache_sharing_and_hard_deny() {
        let context = crate::tool::ToolUseContext::default();
        let summary_request = UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![UserContent::Text("summarize".to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        };
        let cache = crate::services::compact::auto_compact::AutoCompactCacheSafeParams {
            system_prompt: vec!["system".to_string()],
            fork_context_messages: vec![Message::User(summary_request.clone())],
            ..Default::default()
        };

        let params = compact_fork_params(summary_request, &context, &cache);

        assert_eq!(
            params.query_source,
            crate::constants::query_source::QuerySource::Compact
        );
        assert_eq!(params.fork_label, "compact");
        assert_eq!(params.max_turns, Some(1));
        assert_eq!(params.max_output_tokens, None);
        assert!(params.skip_cache_write);
        assert!(!params.skip_transcript);
        assert_eq!(params.cache_safe_params.system_prompt, vec!["system"]);
        assert_eq!(params.cache_safe_params.fork_context_messages.len(), 1);
        assert_eq!(params.prompt_messages.len(), 1);

        let tool = crate::tools::file_read_tool::file_read_tool_schema();
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let decision = params
            .can_use_tool
            .decide(
                &tool,
                &serde_json::json!({"file_path":"/tmp/example"}),
                &context,
                &assistant,
                "toolu-compact",
                None,
            )
            .expect("permission callback should not abort")
            .expect("compact hard-deny callback");
        // CC compact.ts:1125-1133: the deny carries the model-facing message
        // and the 'other' decisionReason.
        assert!(matches!(
            decision,
            crate::types::permissions::PermissionDecision::Deny { ref message, .. }
                if message == "Tool use is not allowed during compaction"
        ));
    }

    #[test]
    fn build_post_compact_messages_preserves_official_order() {
        let boundary = SystemMessage::compact_boundary(None);
        let summary = UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![UserContent::Text("summary".to_string())],
            is_compact_summary: true,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        };
        let keep = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::Text("keep".to_string())],
            model: None,
            stop_reason: None,
            usage: None,
        });
        let attachment = AttachmentMessage::new(serde_json::json!({
            "type": "file",
            "displayPath": "src/lib.rs"
        }));
        let hook = Message::System(SystemMessage::informational(
            "hook",
            crate::types::message::SystemMessageLevel::Info,
        ));
        let result = CompactionResult {
            boundary_marker: boundary,
            summary_messages: vec![summary],
            attachments: vec![attachment],
            hook_results: vec![hook],
            messages_to_keep: Some(vec![keep]),
            user_display_message: None,
            pre_compact_token_count: None,
            post_compact_token_count: None,
            true_post_compact_token_count: None,
            compaction_usage: None,
            rebuilt_read_file_state: Vec::new(),
        };

        let messages = build_post_compact_messages(&result);
        assert!(matches!(messages[0], Message::System(_)));
        assert!(matches!(messages[1], Message::User(_)));
        assert!(matches!(messages[2], Message::Assistant(_)));
        assert!(matches!(messages[3], Message::Attachment(_)));
        assert!(matches!(messages[4], Message::System(_)));
    }

    #[test]
    fn compact_conversation_carries_tool_search_discovered_tools_on_boundary() {
        let mut messages = (0..5)
            .map(|i| {
                Message::User(UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    content: vec![UserContent::Text(format!("message {i}"))],
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
            })
            .collect::<Vec<_>>();
        messages.insert(
            1,
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![UserContent::ToolResult(crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("toolu_search".to_string()),
                    content: "{}".to_string(),
                    is_error: false,
                    content_blocks: vec![
                        crate::types::message::ToolResultContentBlock::ToolReference {
                            tool_name: "WebFetch".to_string(),
                        },
                    ],
                    tool_use_result: None,
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
            }),
        );

        let pre_compact_tokens = crate::utils::tokens::token_count_with_estimation(&messages);
        let result = finalize_compaction_result(
            &messages,
            "<summary>summary</summary>",
            true,
            "manual",
            None,
            pre_compact_tokens,
            12,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
        );

        assert_eq!(
            result
                .boundary_marker
                .compact_metadata()
                .expect("compact metadata")
                .pre_compact_discovered_tools,
            vec!["WebFetch".to_string()]
        );
    }

    #[test]
    fn finalized_model_summary_builds_boundary_and_replaces_full_history() {
        let messages = (0..6)
            .map(|i| {
                Message::User(UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    content: vec![UserContent::Text(format!("message {i}"))],
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
            })
            .collect::<Vec<_>>();
        let pre_compact_tokens = crate::utils::tokens::token_count_with_estimation(&messages);
        let result = finalize_compaction_result(
            &messages,
            "<analysis>draft</analysis><summary>model summary</summary>",
            true,
            "manual",
            None,
            pre_compact_tokens,
            42,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
        );
        let post = build_post_compact_messages(&result);

        assert!(matches!(
            post.first(),
            Some(Message::System(SystemMessage::CompactBoundary { .. }))
        ));
        assert!(matches!(
            post.get(1),
            Some(Message::User(UserMessage {
                is_compact_summary: true,
                ..
            }))
        ));
        assert_eq!(post.len(), 2);
        assert_eq!(result.post_compact_token_count, Some(42));
        let metadata = result
            .boundary_marker
            .compact_metadata()
            .expect("compact metadata");
        assert_eq!(metadata.trigger.as_deref(), Some("manual"));
        assert_eq!(metadata.pre_tokens, Some(pre_compact_tokens));
    }

    struct CompactTestDir(std::path::PathBuf);

    impl CompactTestDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("cometix-post-compact-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for CompactTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn post_compact_files_skip_preserved_reads_and_rebuild_read_state() {
        let temp = CompactTestDir::new();
        let paths = ["old.rs", "middle.rs", "new.rs"].map(|name| temp.0.join(name));
        for (index, path) in paths.iter().enumerate() {
            std::fs::write(path, format!("content {index}\n")).unwrap();
        }
        let read_state = paths
            .iter()
            .enumerate()
            .map(
                |(index, path)| crate::utils::query_helpers::ReadFileStateEntry {
                    path: path.display().to_string(),
                    content: Some(format!("stale {index}")),
                    timestamp_ms: Some(i64::try_from(index).unwrap()),
                    offset: Some(serde_json::json!(1)),
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Read,
                },
            )
            .collect::<Vec<_>>();
        let preserved = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu-new".to_string()),
                    name: crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME.to_string(),
                    input: serde_json::json!({"file_path": paths[2]}),
                },
            )],
            model: None,
            stop_reason: None,
            usage: None,
        })];
        let mut context =
            crate::tool::ToolUseContext::default().with_cwd_override(Some(temp.0.clone()));
        let authoritative_cache = context.read_file_state.clone();
        let authoritative_nested = context
            .nested_memory_attachment_triggers
            .as_ref()
            .unwrap()
            .clone();
        let authoritative_dynamic = context.dynamic_skill_dir_triggers.as_ref().unwrap().clone();
        context.read_file_state.clear();

        let attachments = create_post_compact_file_attachments(
            &read_state,
            &mut context,
            POST_COMPACT_MAX_FILES_TO_RESTORE,
            &preserved,
        )
        .await;

        assert_eq!(attachments.len(), 2);
        assert!(attachments.iter().all(|attachment| {
            attachment
                .attachment
                .to_wire()
                .get("filename")
                .and_then(serde_json::Value::as_str)
                != Some(paths[2].to_string_lossy().as_ref())
        }));
        assert_eq!(context.read_file_state.len(), 2);
        assert!(context.read_file_state.same_identity(&authoritative_cache));
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .same_identity(&authoritative_nested)
        );
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .same_identity(&authoritative_dynamic)
        );
        assert!(context.read_file_state.snapshot().into_iter().all(|entry| {
            entry
                .content
                .as_deref()
                .is_some_and(|content| content.starts_with("content "))
                && entry.offset == Some(serde_json::json!(1))
                && entry.source == crate::utils::query_helpers::ReadFileStateSource::Read
        }));
    }

    #[test]
    fn invoked_skill_attachment_is_agent_scoped_and_truncated_to_official_budget() {
        let _lock = crate::bootstrap::state::TEST_INVOKED_SKILLS_LOCK
            .lock()
            .unwrap();
        crate::bootstrap::state::clear_invoked_skills(None);
        crate::bootstrap::state::add_invoked_skill(
            "large-main",
            "/skills/large-main/SKILL.md",
            "x".repeat(30_000),
            None,
        );
        crate::bootstrap::state::add_invoked_skill(
            "child-only",
            "/skills/child/SKILL.md",
            "child instructions",
            Some("agent-child"),
        );

        let attachment = create_skill_attachment_if_needed(None).expect("main skill");
        let wire = attachment.attachment.to_wire();
        let skills = wire
            .get("skills")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].get("name").and_then(serde_json::Value::as_str),
            Some("large-main")
        );
        assert!(
            skills[0]
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| content.ends_with(SKILL_TRUNCATION_MARKER))
        );
        assert_eq!(
            create_skill_attachment_if_needed(Some("agent-child"))
                .unwrap()
                .attachment
                .to_wire()
                .get("skills")
                .and_then(serde_json::Value::as_array)
                .unwrap()
                .len(),
            1
        );
        crate::bootstrap::state::clear_invoked_skills(None);
    }

    #[test]
    fn plan_mode_and_unretrieved_agent_status_are_restored_after_compact() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect compact state".to_string(),
                prompt: "inspect".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
        );
        let mut permission = crate::tool::ToolPermissionContext::default();
        permission.mode = crate::types::permissions::PermissionMode::Plan;
        let context = crate::tool::ToolUseContext::with_permission_context(permission);

        let plan = create_plan_mode_attachment_if_needed(&context).expect("plan mode");
        assert_eq!(plan.attachment_type(), "plan_mode");
        assert_eq!(
            plan.attachment
                .to_wire()
                .get("reminderType")
                .and_then(serde_json::Value::as_str),
            Some("full")
        );
        let tasks = create_async_agent_attachments_if_needed(&context);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].attachment_type(), "task_status");
        assert_eq!(
            tasks[0]
                .attachment
                .to_wire()
                .get("taskId")
                .and_then(serde_json::Value::as_str),
            Some(task_id.as_str())
        );

        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
    }
    #[tokio::test]
    async fn partial_compact_empty_ranges_match_official_before_any_model_call() {
        use crate::types::message::PartialCompactDirection;
        let messages = vec![Message::User(crate::utils::messages::create_user_message(
            "kept".into(),
        ))];
        let cache = crate::services::compact::auto_compact::AutoCompactCacheSafeParams {
            system_prompt: vec![],
            user_context: Default::default(),
            system_context: Default::default(),
            fork_context_messages: messages.clone(),
        };
        // CC compact.ts:801-807: direction-specific empty-range errors precede hooks/API.
        for (pivot, direction, expected) in [
            (
                0,
                PartialCompactDirection::UpTo,
                "Nothing to summarize before the selected message.",
            ),
            (
                1,
                PartialCompactDirection::From,
                "Nothing to summarize after the selected message.",
            ),
        ] {
            assert_eq!(
                partial_compact_conversation(
                    messages.clone(),
                    pivot,
                    &crate::tool::ToolUseContext::default(),
                    &cache,
                    None,
                    direction
                )
                .await
                .unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn preserved_segment_matches_official_direction_anchor() {
        let mut head = crate::utils::messages::create_user_message("head".into());
        head.uuid = "head".into();
        let mut tail = crate::utils::messages::create_user_message("tail".into());
        tail.uuid = "tail".into();
        let kept = vec![Message::User(head), Message::User(tail)];
        // CC compact.ts:349-370: head/tail are original message UUIDs; caller selects anchor.
        for anchor in ["boundary", "last-summary"] {
            let boundary = annotate_boundary_with_preserved_segment(
                SystemMessage::compact_boundary(None),
                anchor,
                &kept,
            );
            assert_eq!(
                boundary.compact_metadata().unwrap().preserved_segment,
                Some(serde_json::json!({"headUuid":"head","tailUuid":"tail","anchorUuid":anchor}))
            );
        }
        assert_eq!(
            annotate_boundary_with_preserved_segment(
                SystemMessage::compact_boundary(None),
                "anchor",
                &[]
            )
            .compact_metadata(),
            None
        );
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn partial_compact_matches_official_model_wire_and_preserved_history() {
        use crate::types::message::PartialCompactDirection;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::tls_provider::install_crypto_provider();
        let _project = IsolatedProjectSettings::pin();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _url = EnvVarGuard::set(
            "ANTHROPIC_BASE_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
        let _key = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ant-test-partial-compact");
        let _memory = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
        let _auto = EnvVarGuard::set("DISABLE_AUTO_COMPACT", "1");
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = requests.clone();
        let server = tokio::spawn(async move {
            for response_text in [
                "<summary>FROM_SUMMARY</summary>",
                "<summary>UP_TO_SUMMARY</summary>",
                "\nPlease run /login · API Error: invalid credentials\n",
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 4096];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                while request.len() < header_end + length {
                    let mut buffer = [0; 4096];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                captured.lock().unwrap().push(
                    serde_json::from_slice(&request[header_end..header_end + length]).unwrap(),
                );
                let events = [
                    serde_json::json!({"type":"message_start","message":{"id":"msg_partial","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}),
                    serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":response_text}}),
                    serde_json::json!({"type":"content_block_stop","index":0}),
                    serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
                    serde_json::json!({"type":"message_stop"}),
                ];
                let response = events
                    .iter()
                    .map(|event| {
                        format!(
                            "event: {}\ndata: {}\n\n",
                            event["type"].as_str().unwrap(),
                            event
                        )
                    })
                    .collect::<String>();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            }
        });
        let mut prefix = crate::utils::messages::create_user_message("PREFIX_SENTINEL".into());
        prefix.uuid = "prefix".into();
        let mut pivot = crate::utils::messages::create_user_message("PIVOT_SENTINEL".into());
        pivot.uuid = "pivot".into();
        let all = vec![
            Message::User(prefix),
            Message::Assistant(crate::utils::messages::create_assistant_message(
                "earlier reply".into(),
            )),
            Message::User(pivot),
        ];
        let cache = crate::services::compact::auto_compact::AutoCompactCacheSafeParams {
            system_prompt: vec!["Test system prompt".into()],
            user_context: Default::default(),
            system_context: Default::default(),
            fork_context_messages: all.clone(),
        };
        let context = crate::tool::ToolUseContext {
            main_loop_model: Some("claude-sonnet-4-6".into()),
            ..Default::default()
        };
        // CC compact.ts:847-850,1007-1044,1078-1098: cache prefix, kept order, boundary and summary metadata.
        for direction in [PartialCompactDirection::From, PartialCompactDirection::UpTo] {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                partial_compact_conversation(
                    all.clone(),
                    2,
                    &context,
                    &cache,
                    Some("KEEP_CONTEXT"),
                    direction,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            let kept = result.messages_to_keep.as_ref().unwrap();
            assert_eq!(
                kept.len(),
                if direction == PartialCompactDirection::From {
                    2
                } else {
                    1
                }
            );
            assert_eq!(
                result.summary_messages[0]
                    .summarize_metadata
                    .as_ref()
                    .unwrap()
                    .direction
                    .as_deref(),
                Some(direction.as_str())
            );
            assert!(!result.summary_messages[0].is_visible_in_transcript_only);
            let metadata = result.boundary_marker.compact_metadata().unwrap();
            let segment = metadata.preserved_segment.as_ref().unwrap();
            let expected_anchor = if direction == PartialCompactDirection::From {
                &result.boundary_marker.base().uuid
            } else {
                &result.summary_messages[0].uuid
            };
            assert_eq!(
                segment["anchorUuid"].as_str(),
                Some(expected_anchor.as_str())
            );
            assert_eq!(
                metadata.messages_summarized,
                Some(if direction == PartialCompactDirection::From {
                    1
                } else {
                    2
                })
            );
            assert_eq!(result.post_compact_token_count, Some(12));
        }
        // The real model-response path must reject trimmed authentication text before returning a boundary.
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            partial_compact_conversation(
                all,
                2,
                &context,
                &cache,
                None,
                PartialCompactDirection::From,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error, "Please run /login · API Error: invalid credentials");
        server.await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[0]["messages"]
                .to_string()
                .contains("PIVOT_SENTINEL")
        );
        assert!(
            !requests[1]["messages"]
                .to_string()
                .contains("PIVOT_SENTINEL")
        );
        for request in &requests[..2] {
            assert!(
                request["messages"]
                    .to_string()
                    .contains("User context: KEEP_CONTEXT")
            );
        }
    }
}
