//! Agent tool UI helpers.
//!
//! Maps to: CC `tools/AgentTool/UI.tsx`.
//!
//! The transcript display blocks in this file mount the GENERIC message
//! components (`Markdown`, `AssistantToolUseMessage`, `UserToolResultMessage`).
//! That dependency direction is upstream's own, not an inversion: CC's
//! `tools/AgentTool/UI.tsx:1-52` import block pulls in `components/Markdown.js`,
//! `components/Message.js` (as `MessageComponent`), `components/MessageResponse.js`
//! and `components/AgentProgressLine.js` — the tool UI owns its own rows and
//! reaches into the shared components, never the reverse.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::ctrl_o_to_expand::{CtrlOToExpand, SubAgentProvider, ctrl_o_to_expand_hint};
use crate::components::fallback_tool_use_error_message::FallbackToolUseErrorMessage;
use crate::components::fallback_tool_use_rejected_message::FallbackToolUseRejectedMessage;
use crate::components::markdown::Markdown;
use crate::components::message_response::MessageResponse;
use crate::components::messages::user_tool_result_message::UserToolResultMessage;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderSegment, ToolRenderTone,
};
use crate::components::messages::{AssistantTextMessage, AssistantToolUseMessage};
use crate::types::message::{ToolResultStatus, ToolUseProgressMessage, ToolUseStatus};
use crate::utils::format::{format_duration, format_number};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Rust progress-carrier projection for one user result retained by CC
/// `tools/AgentTool/UI.tsx:271-320` `VerboseAgentTranscript`.
pub(crate) struct VerboseAgentToolResultProjection {
    pub(crate) status: crate::types::message::ToolResultStatus,
}

/// Maps to CC `tools/AgentTool/UI.tsx:288-299` — the `VerboseAgentTranscript`
/// row filter, plus the success/error split `UserToolResultMessage` makes from
/// the block's `is_error`.
///
/// Ordinary nested result rows without `toolUseResult` are omitted for both
/// success and error; preserved sidechain rows carry that raw `toolUseResult`
/// and continue through the normal result renderer. Tool-specific parsing
/// remains in `UserToolResultMessage` and each Tool UI owner.
///
/// This runs at RENDER time, per row. It used to run at the REPL converter,
/// which dropped the whole progress message — a divergence, since CC keeps
/// every forwarded message in `progressMessages` for `extractLastToolInfo`
/// (`:1053-1060`) and `calculateAgentStats` (`:795-804`).
pub(crate) fn verbose_agent_transcript_tool_result_projection(
    tool_name: &str,
    success: bool,
    content: &str,
    tool_use_result: Option<&serde_json::Value>,
) -> Option<VerboseAgentToolResultProjection> {
    let tool_use_result = tool_use_result?;
    let status = if success {
        crate::types::message::ToolResultStatus::Success
    } else {
        crate::types::message::ToolResultStatus::Error
    };
    Some(VerboseAgentToolResultProjection { status })
}

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): the sync/async union
/// (`AgentTool.tsx:271-294`), plus the teammate_spawned and remote_launched
/// shapes CC adds dynamically at runtime when multi-agent features are
/// enabled (`AgentTool.tsx:270` note, `:300-315`).
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<super::AgentOutput> {
    let map = value.as_object()?;
    let string = |key: &str| -> Option<String> {
        map.get(key)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    };
    let optional_string = |key: &str| -> Option<Option<String>> {
        match map.get(key) {
            None => Some(None),
            Some(serde_json::Value::String(value)) => Some(Some(value.clone())),
            Some(_) => None,
        }
    };
    let mut output = super::AgentOutput {
        status: String::new(),
        agent_id: None,
        agent_type: None,
        description: None,
        prompt: None,
        output_file: None,
        can_read_output_file: None,
        content: Vec::new(),
        total_tool_use_count: None,
        total_duration_ms: None,
        total_tokens: None,
        teammate_id: None,
        model: None,
        name: None,
        color: None,
        tmux_session_name: None,
        tmux_window_name: None,
        tmux_pane_id: None,
        team_name: None,
        is_splitpane: None,
        plan_mode_required: None,
        worktree_path: None,
        worktree_branch: None,
        usage: None,
        task_id: None,
        session_url: None,
    };
    match map.get("status")?.as_str()? {
        "completed" => {
            output.status = "completed".to_string();
            output.prompt = Some(string("prompt")?);
            output.agent_id = Some(string("agentId")?);
            output.agent_type = optional_string("agentType")?;
            // content: z.array({type: 'text', text}).
            let content = map.get("content")?.as_array()?;
            output.content = content
                .iter()
                .map(|block| {
                    let block = block.as_object()?;
                    if block.get("type")?.as_str()? != "text" {
                        return None;
                    }
                    Some(block.get("text")?.as_str()?.to_string())
                })
                .collect::<Option<Vec<_>>>()?;
            output.total_tool_use_count =
                Some(usize::try_from(map.get("totalToolUseCount")?.as_u64()?).ok()?);
            output.total_duration_ms = Some(map.get("totalDurationMs")?.as_u64()?);
            output.total_tokens = Some(map.get("totalTokens")?.as_u64()?);
            output.usage = Some(parse_usage(map.get("usage")?)?);
            output.worktree_path = optional_string("worktreePath")?;
            output.worktree_branch = optional_string("worktreeBranch")?;
        }
        "async_launched" => {
            output.status = "async_launched".to_string();
            output.agent_id = Some(string("agentId")?);
            output.description = Some(string("description")?);
            output.prompt = Some(string("prompt")?);
            output.output_file = Some(string("outputFile")?);
            output.can_read_output_file = match map.get("canReadOutputFile") {
                None => None,
                Some(serde_json::Value::Bool(value)) => Some(*value),
                Some(_) => return None,
            };
        }
        "teammate_spawned" => {
            output.status = "teammate_spawned".to_string();
            output.prompt = Some(string("prompt")?);
            output.teammate_id = Some(string("teammate_id")?);
            output.agent_id = Some(string("agent_id")?);
            output.agent_type = optional_string("agent_type")?;
            output.model = optional_string("model")?;
            output.name = Some(string("name")?);
            output.color = optional_string("color")?;
            output.tmux_session_name = Some(string("tmux_session_name")?);
            output.tmux_window_name = Some(string("tmux_window_name")?);
            output.tmux_pane_id = Some(string("tmux_pane_id")?);
            output.team_name = optional_string("team_name")?;
            output.is_splitpane = map.get("is_splitpane").and_then(serde_json::Value::as_bool);
            output.plan_mode_required = map
                .get("plan_mode_required")
                .and_then(serde_json::Value::as_bool);
        }
        "remote_launched" => {
            output.status = "remote_launched".to_string();
            output.task_id = optional_string("taskId")?;
            output.session_url = optional_string("sessionUrl")?;
        }
        _ => return None,
    }
    Some(output)
}

/// Validates the `agentToolResultSchema` usage object and narrows it to the
/// Rust [`crate::types::message::TokenUsage`] aggregate. All seven keys must
/// be present; the five nullable legs accept null.
fn parse_usage(value: &serde_json::Value) -> Option<crate::types::message::TokenUsage> {
    let map = value.as_object()?;
    let required_number = |key: &str| map.get(key)?.as_u64();
    let nullable_number = |key: &str| -> Option<u64> {
        match map.get(key) {
            Some(serde_json::Value::Null) => Some(0),
            Some(serde_json::Value::Number(number)) => number.as_u64(),
            _ => None,
        }
    };
    // The remaining nullable legs must exist (schema-required keys).
    for key in ["server_tool_use", "service_tier", "cache_creation"] {
        map.get(key)?;
    }
    Some(crate::types::message::TokenUsage {
        input_tokens: required_number("input_tokens")?,
        output_tokens: required_number("output_tokens")?,
        cache_creation_input_tokens: nullable_number("cache_creation_input_tokens")?,
        cache_read_input_tokens: nullable_number("cache_read_input_tokens")?,
        cache_deleted_input_tokens: 0,
    })
}

// ─── Raw `toolUseResult` wire projection ─────────────────────────────────

/// Serializes [`super::AgentOutput`] to CC's exact `toolUseResult` wire
/// shape, per status branch:
/// - completed: `{status, prompt, ...agentResult, ...worktreeResult}`
///   (`AgentTool.tsx:1660-1666`) with `content` as text blocks and the
///   `agentToolResultSchema` usage object;
/// - async_launched: the async schema (`AgentTool.tsx:277-291`);
/// - teammate_spawned: the private `TeammateSpawnedOutput` declaration
///   order (`AgentTool.tsx:300-315`).
///
/// An unrecognized status projects nothing.
pub(crate) fn output_to_value(output: &super::AgentOutput) -> Option<serde_json::Value> {
    let mut map = serde_json::Map::new();
    match output.status.as_str() {
        "completed" => {
            map.insert("status".to_string(), serde_json::json!("completed"));
            map.insert(
                "prompt".to_string(),
                serde_json::json!(output.prompt.as_deref().unwrap_or_default()),
            );
            map.insert(
                "agentId".to_string(),
                serde_json::json!(output.agent_id.as_deref().unwrap_or_default()),
            );
            if let Some(agent_type) = output.agent_type.as_deref() {
                map.insert("agentType".to_string(), serde_json::json!(agent_type));
            }
            map.insert(
                "content".to_string(),
                serde_json::Value::Array(
                    output
                        .content
                        .iter()
                        .map(|text| serde_json::json!({"type": "text", "text": text}))
                        .collect(),
                ),
            );
            map.insert(
                "totalToolUseCount".to_string(),
                serde_json::json!(output.total_tool_use_count.unwrap_or(0)),
            );
            map.insert(
                "totalDurationMs".to_string(),
                serde_json::json!(output.total_duration_ms.unwrap_or(0)),
            );
            map.insert(
                "totalTokens".to_string(),
                serde_json::json!(output.total_tokens.unwrap_or(0)),
            );
            // usage is required by the schema; a missing aggregate omits the
            // key, so a replayed row fails safeParse and renders nothing —
            // the same fate CC gives pre-usage transcripts.
            if let Some(usage) = output.usage.as_ref() {
                map.insert("usage".to_string(), usage_to_wire(usage));
            }
            if let Some(worktree_path) = output.worktree_path.as_deref() {
                map.insert("worktreePath".to_string(), serde_json::json!(worktree_path));
            }
            if let Some(worktree_branch) = output.worktree_branch.as_deref() {
                map.insert(
                    "worktreeBranch".to_string(),
                    serde_json::json!(worktree_branch),
                );
            }
        }
        "async_launched" => {
            // CC AgentTool.tsx:1033-1043/:1399-1409 — both async_launched
            // data literals lead with `isAsync: true as const`; that literal
            // IS the persisted toolUseResult (only completed/teammate/remote
            // omit the key).
            map.insert("isAsync".to_string(), serde_json::Value::Bool(true));
            map.insert("status".to_string(), serde_json::json!("async_launched"));
            map.insert(
                "agentId".to_string(),
                serde_json::json!(output.agent_id.as_deref().unwrap_or_default()),
            );
            map.insert(
                "description".to_string(),
                serde_json::json!(output.description.as_deref().unwrap_or_default()),
            );
            map.insert(
                "prompt".to_string(),
                serde_json::json!(output.prompt.as_deref().unwrap_or_default()),
            );
            map.insert(
                "outputFile".to_string(),
                serde_json::json!(output.output_file.as_deref().unwrap_or_default()),
            );
            if let Some(can_read) = output.can_read_output_file {
                map.insert("canReadOutputFile".to_string(), serde_json::json!(can_read));
            }
        }
        "teammate_spawned" => {
            map.insert("status".to_string(), serde_json::json!("teammate_spawned"));
            map.insert(
                "prompt".to_string(),
                serde_json::json!(output.prompt.as_deref().unwrap_or_default()),
            );
            map.insert(
                "teammate_id".to_string(),
                serde_json::json!(output.teammate_id.as_deref().unwrap_or_default()),
            );
            map.insert(
                "agent_id".to_string(),
                serde_json::json!(output.agent_id.as_deref().unwrap_or_default()),
            );
            if let Some(agent_type) = output.agent_type.as_deref() {
                map.insert("agent_type".to_string(), serde_json::json!(agent_type));
            }
            if let Some(model) = output.model.as_deref() {
                map.insert("model".to_string(), serde_json::json!(model));
            }
            map.insert(
                "name".to_string(),
                serde_json::json!(output.name.as_deref().unwrap_or_default()),
            );
            if let Some(color) = output.color.as_deref() {
                map.insert("color".to_string(), serde_json::json!(color));
            }
            map.insert(
                "tmux_session_name".to_string(),
                serde_json::json!(output.tmux_session_name.as_deref().unwrap_or_default()),
            );
            map.insert(
                "tmux_window_name".to_string(),
                serde_json::json!(output.tmux_window_name.as_deref().unwrap_or_default()),
            );
            map.insert(
                "tmux_pane_id".to_string(),
                serde_json::json!(output.tmux_pane_id.as_deref().unwrap_or_default()),
            );
            if let Some(team_name) = output.team_name.as_deref() {
                map.insert("team_name".to_string(), serde_json::json!(team_name));
            }
            if let Some(is_splitpane) = output.is_splitpane {
                map.insert("is_splitpane".to_string(), serde_json::json!(is_splitpane));
            }
            if let Some(plan_mode_required) = output.plan_mode_required {
                map.insert(
                    "plan_mode_required".to_string(),
                    serde_json::json!(plan_mode_required),
                );
            }
        }
        _ => return None,
    }
    Some(serde_json::Value::Object(map))
}

/// Projects the narrowed [`crate::types::message::TokenUsage`] aggregate to
/// the `agentToolResultSchema` usage shape (agentToolUtils.ts:238-256). The
/// aggregate does not track server_tool_use/service_tier/cache_creation yet,
/// so those nullable legs are null.
fn usage_to_wire(usage: &crate::types::message::TokenUsage) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
        "cache_read_input_tokens": usage.cache_read_input_tokens,
        "server_tool_use": serde_json::Value::Null,
        "service_tier": serde_json::Value::Null,
        "cache_creation": serde_json::Value::Null,
    })
}

// ─── Subagent progress row derivations ───────────────────────────────────
// CC forwards the whole normalized message on `agent_progress` and derives
// every display row here, at the renderer. These helpers are the shared half
// of that derivation; the row assembly itself lives with each of CC's two
// consumers ([`AgentToolUseProgressMessage`] and
// [`AgentVerboseTranscriptBlock`], both below).

/// The two `buildSubagentLookups` legs the row derivations need
/// (`utils/messages.ts:1383-1397`): `toolUseByToolUseID` and
/// `toolResultByToolUseID`, plus the `inProgressToolUseIDs` complement
/// (`:1400-1405`).
///
/// CC hands the full `MessageLookups` to `MessageComponent`
/// (`UI.tsx:276-282`, `:653-660`). The Rust rows mount
/// `AssistantToolUseMessage` directly rather than a `Message` dispatcher, and
/// that component takes a per-row `status` instead of the id sets, so these
/// two maps plus [`SubagentProgressLookups::tool_use_status`] and
/// [`SubagentProgressLookups::in_progress_count`] cover every leg the mounts
/// read. The remaining `MessageLookups` fields (`siblingToolUseIDs`,
/// `progressMessagesByToolUseID`, the hook counts) are `EMPTY_LOOKUPS`
/// defaults in CC's builder too (`utils/messages.ts:1407-1414`).
pub(crate) struct SubagentProgressLookups<'a> {
    tool_use_by_tool_use_id:
        std::collections::HashMap<&'a str, &'a crate::types::message::ToolUseBlock>,
    tool_result_by_tool_use_id:
        std::collections::HashMap<&'a str, &'a crate::types::message::ToolResult>,
}

impl<'a> SubagentProgressLookups<'a> {
    pub(crate) fn tool_use(&self, id: &str) -> Option<&'a crate::types::message::ToolUseBlock> {
        self.tool_use_by_tool_use_id.get(id).copied()
    }

    pub(crate) fn tool_result(&self, id: &str) -> Option<&'a crate::types::message::ToolResult> {
        self.tool_result_by_tool_use_id.get(id).copied()
    }

    /// CC `inProgressToolUseIDs` (`utils/messages.ts:1400-1405`): a tool_use
    /// whose id never received a tool_result is still running.
    ///
    /// Resolved is unconditionally `Succeeded`, never `Failed`: this lookup
    /// family is `buildSubagentLookups`, whose return spreads `EMPTY_LOOKUPS`
    /// and overrides only `toolUseByToolUseID` / `resolvedToolUseIDs` /
    /// `toolResultByToolUseID` (`utils/messages.ts:1407-1414`), so
    /// `erroredToolUseIDs` stays the empty set `EMPTY_LOOKUPS` declares
    /// (`:1352`). Its one tool-use-row consumer,
    /// `AssistantToolUseMessage.tsx:178` `isError={lookups.erroredToolUseIDs.has(param.id)}`,
    /// is therefore false for EVERY subagent progress row. The sibling builder
    /// `buildMessageLookups` does fill that set (`:1243-1244`), but the Agent
    /// progress path does not use it.
    pub(crate) fn tool_use_status(&self, id: &str) -> crate::types::message::ToolUseStatus {
        match self.tool_result(id) {
            None => crate::types::message::ToolUseStatus::Running,
            Some(_) => crate::types::message::ToolUseStatus::Succeeded,
        }
    }

    /// CC `inProgressToolUseIDs.size` — what `Message.tsx:129` hands each
    /// mounted `AssistantMessageBlock` as `inProgressToolCallCount`
    /// (`AssistantToolUseMessage.tsx:36`), which the nested tool's own progress
    /// renderer uses for its condensed-line estimate. The AgentTool progress
    /// mount passes `collapsedInProgressIDs` (`UI.tsx:653-660`, `:700`), so the
    /// nested count is over the SUBAGENT's tool uses, not the main loop's.
    pub(crate) fn in_progress_count(&self) -> usize {
        self.tool_use_by_tool_use_id
            .keys()
            .filter(|id| !self.tool_result_by_tool_use_id.contains_key(*id))
            .count()
    }
}

/// Maps to: CC `utils/messages.ts:1373-1416#buildSubagentLookups` (the two id
/// legs) — the same walk `UI.tsx` runs over `progressMessages.map(pm => pm.data)`
/// before rendering, and the inline rebuild at `UI.tsx:1031-1043`.
pub(crate) fn subagent_progress_lookups(
    progress_messages: &[crate::types::message::ToolUseProgressMessage],
) -> SubagentProgressLookups<'_> {
    use crate::types::message::{AssistantContent, RenderableMessageKind, UserContent};

    let mut lookups = SubagentProgressLookups {
        tool_use_by_tool_use_id: std::collections::HashMap::new(),
        tool_result_by_tool_use_id: std::collections::HashMap::new(),
    };
    for progress in progress_messages {
        // CC `:1033-1035` / `hasProgressMessage` — payloads from other
        // progress producers carry no message and are skipped.
        let Some(message) = progress.subagent_progress_message() else {
            continue;
        };
        match &message.kind {
            RenderableMessageKind::Assistant { message } => {
                for block in &message.content {
                    if let AssistantContent::ToolUse(tool_use) = block {
                        lookups
                            .tool_use_by_tool_use_id
                            .insert(tool_use.id.0.as_str(), tool_use);
                    }
                }
            }
            RenderableMessageKind::User { message } => {
                for block in &message.content {
                    if let UserContent::ToolResult(result) = block {
                        lookups
                            .tool_result_by_tool_use_id
                            .insert(result.tool_use_id.0.as_str(), result);
                    }
                }
            }
            _ => {}
        }
    }
    lookups
}

/// Maps to: CC `tools/AgentTool/UI.tsx:1026-1126#extractLastToolInfo`.
///
/// Two outcomes, in CC's order:
/// 1. Roll up TRAILING consecutive search/read operations once at least two
///    have accumulated (`:1067-1069`, through the shared
///    `getSearchReadSummaryText(searchCount, readCount, true)`). Only
///    tool_result rows increment the counters — a tool_use and its result
///    would double-count (`:1053-1060`) — but a non-collapsible row of EITHER
///    kind ends the backwards scan.
/// 2. Otherwise describe the last tool_result: `userFacingName` alone, or
///    `"{userFacingName}: {getToolUseSummary(input)}"` when that tool
///    implements the member and returns a truthy summary (`:1101-1117`). Both
///    reads are fed the tool's own `inputSchema.safeParse` result, never the
///    raw input (`:1102-1113`), and BOTH are gated on the tool still being in
///    `tools` — CC returns the raw wire name when `findToolByName` misses
///    (`:1096-1098`).
///
/// `None` when neither applies — the caller then shows `Initializing…`.
///
/// `tools` is CC's second parameter (`:1028`), the live main-loop pool threaded
/// REPL.tsx:5821/6162 → Messages.tsx:251 → MessageRow.tsx:201 →
/// Message.tsx:250 → GroupedToolUseContent.tsx:16 → `:69` →
/// `renderGroupedAgentToolUse`'s options (`UI.tsx:835`) → `:847`.
pub(crate) fn extract_last_tool_info(
    progress_messages: &[crate::types::message::ToolUseProgressMessage],
    tools: &[crate::types::tools::Tool],
) -> Option<String> {
    use crate::types::message::{AssistantContent, RenderableMessageKind, UserContent};

    // CC `:1032-1043`: index every nested tool_use so the reverse scan can
    // resolve a tool_result back to the block that produced it.
    let lookups = subagent_progress_lookups(progress_messages);

    // CC `:1046-1065`: walk backwards while the row is a search/read op.
    let mut search_count = 0usize;
    let mut read_count = 0usize;
    for progress in progress_messages.iter().rev() {
        // CC `:1050-1052`: a payload from another progress producer is
        // SKIPPED, not a scan terminator.
        let Some(message) = progress.subagent_progress_message() else {
            continue;
        };
        // CC `getSearchOrReadInfo` (`:76-102`) reads `content[0]` on both the
        // assistant and the user branch; normalization guarantees one block.
        let resolved = match &message.kind {
            RenderableMessageKind::Assistant { message } => match message.first_content_block() {
                Some(AssistantContent::ToolUse(tool_use)) => Some((tool_use, false)),
                _ => None,
            },
            RenderableMessageKind::User { message } => match message.first_content_block() {
                Some(UserContent::ToolResult(result)) => lookups
                    .tool_use(result.tool_use_id.0.as_str())
                    .map(|tool_use| (tool_use, true)),
                _ => None,
            },
            _ => None,
        };
        // CC `:1062-1064`: anything the classifier cannot call a search/read
        // ends the scan.
        let Some((tool_use, is_result)) = resolved else {
            break;
        };
        // Seam, unchanged by the pool threading: CC's classifier takes the pool
        // too — `getSearchOrReadFromContent(content, tools)`
        // (`collapseReadSearch.ts:244-273`) → `getToolSearchOrReadInfo(name,
        // input, tools)` (`:143-206`), which resolves the tool through
        // `findToolByName(tools, name) ?? findToolByName(getReplPrimitiveTools(),
        // name)` and reads `tool.isSearchOrReadCommand`. The port's classifier
        // is by-name and takes no pool; that carrier belongs to
        // `utils/collapse_read_search.rs`, whose other callers
        // (`collapse_read_search_groups`, `has_content_after_index`) share the
        // same gap.
        let Some(info) = crate::utils::collapse_read_search::get_search_or_read_from_content(
            tool_use.name.as_str(),
            Some(&tool_use.input),
        ) else {
            break;
        };
        if !(info.is_search() || info.is_read()) {
            break;
        }
        // CC: "Only count tool_result messages to avoid double counting".
        if is_result {
            if info.is_search() {
                search_count += 1;
            } else if info.is_read() {
                read_count += 1;
            }
        }
    }

    if search_count + read_count >= 2 {
        return Some(
            crate::utils::collapse_read_search::get_search_read_summary_text(
                search_count,
                read_count,
                true,
                0,
                None,
                0,
            ),
        );
    }

    // CC `:1071-1085`: the last progress message that is a user row carrying a
    // tool_result block.
    let last_result = progress_messages.iter().rev().find_map(|progress| {
        let message = progress.subagent_progress_message()?;
        let RenderableMessageKind::User { message } = &message.kind else {
            return None;
        };
        message.content.iter().find_map(|block| match block {
            UserContent::ToolResult(result) => Some(result),
            _ => None,
        })
    })?;
    // CC `:1092-1096`: an unresolvable id falls through to the final
    // `return null` (the `if (toolUseBlock)` guard).
    let tool_use = lookups.tool_use(last_result.tool_use_id.0.as_str())?;
    // CC `:1096-1099`:
    //   const tool = findToolByName(tools, toolUseBlock.name)
    //   if (!tool) { return toolUseBlock.name } // Fallback to raw name
    // The pool decides. A tool the CURRENT pool does not carry has no members
    // to read, so the RAW wire name is the whole answer — a Grep removed from
    // the pool reads as "Grep", not as the global facing name "Search" — and
    // no summary is derived.
    let Some(tool) = crate::types::tools::find_tool_by_name(tools, tool_use.name.as_str()) else {
        return Some(tool_use.name.clone());
    };
    // CC `:1102-1103` `tool.inputSchema.safeParse(input)` — the FOUND tool's
    // own schema. Both consumers below read the SAME result:
    // `parsedInput.success ? parsedInput.data : undefined` (`:1105-1107` for
    // the facing name, `:1111-1113` for the summary), so a malformed input
    // shows the bare name and no summary. The gate is the shared
    // `assistant_tool_use_message` owner, the same one the ungrouped row
    // (`AssistantToolUseMessage.tsx:88`) and the subagent progress rows use.
    let parsed_input =
        crate::components::messages::assistant_tool_use_message::assistant_tool_use_parsed_input(
            tool.name.as_str(),
            &tool_use.input,
        );
    // CC's `Tool` object carries the pool metadata and the render-time members
    // together; this port splits it into `types::tools::Tool` (the pool entry)
    // plus a process-wide `ToolCall` behaviour singleton, so `tool.<member>`
    // reads resolve through the singleton the FOUND pool entry names.
    let behavior = crate::services::tools::tool_execution::find_tool_call(tool.name.as_str());
    // CC `:1105-1107` `tool.userFacingName(...)`. A pool entry with no
    // registered behaviour (an MCP tool) falls back to its own name, which is
    // the default `buildTool` installs for the member (`Tool.ts:789`
    // `userFacingName: () => def.name`).
    let user_facing_name = behavior.map_or_else(
        || tool.name.clone(),
        |behavior| behavior.user_facing_name(parsed_input.as_ref()),
    );
    // CC `:1109-1117`: the summary is used only when the tool implements the
    // member AND returns a truthy value; otherwise the bare facing name.
    //
    // `ToolCall::get_tool_use_summary` takes `&Value` where CC's parameter is
    // `Partial<z.infer<Input>> | undefined` (Tool.ts:539), so a failed
    // `safeParse` skips the call instead of passing `undefined`. Same answer:
    // every CC implementer opens with `if (!input?.<field>) return null`
    // (BashTool.tsx:720-723, GlobTool/UI.tsx:58-63, FileReadTool/UI.tsx:189-194,
    // NotebookEditTool/UI.tsx:17-22 …).
    let summary = behavior
        .zip(parsed_input.as_ref())
        .and_then(|(behavior, parsed)| behavior.get_tool_use_summary(parsed));
    match summary {
        Some(summary) if !summary.is_empty() => Some(format!("{user_facing_name}: {summary}")),
        _ => Some(user_facing_name),
    }
}

// ─── Grouped agent row derivations ───────────────────────────────────────

/// Maps to: CC `tools/AgentTool/UI.tsx:791-822#calculateAgentStats`.
///
/// `tokens` is `None` — CC's `null`, not `0` — until the agent has produced an
/// assistant message; `AgentProgressLine` then omits the token segment
/// entirely (`AgentProgressLine.tsx:92` `tokens !== null &&`).
pub(crate) struct AgentStats {
    pub(crate) tool_use_count: usize,
    pub(crate) tokens: Option<u64>,
}

/// Maps to: CC `tools/AgentTool/UI.tsx:791-822#calculateAgentStats`.
///
/// Both legs read the forwarded progress messages only. A resumed session has
/// none (progress is never persisted — see [`crate::types::message::ToolUseProgressMessage`]),
/// so a replayed group reads `0 tool uses` with no token segment, exactly as
/// CC's `lookups.progressMessagesByToolUseID` miss does.
///
/// The `usage` read here is the one the progress row was EMITTED with — the
/// `message_start` usage — in CC as well as here; the final `output_tokens`
/// lands only on the retained history copy. The full CC derivation lives on
/// [`crate::tools::agent_tool::run_agent`]'s
/// `forward_subagent_progress_from_message`; this is not a staleness bug to
/// "fix" by re-emitting the row (`REPL.tsx:3478-3481` forbids replacing
/// `agent_progress`).
pub(crate) fn calculate_agent_stats(
    progress_messages: &[crate::types::message::ToolUseProgressMessage],
) -> AgentStats {
    use crate::types::message::{RenderableMessageKind, UserContent};

    // CC `:795-804`: one per forwarded USER row that carries a tool_result —
    // the assistant tool_use row it answers is not counted.
    let tool_use_count = progress_messages
        .iter()
        .filter(|progress| {
            let Some(message) = progress.subagent_progress_message() else {
                return false;
            };
            let RenderableMessageKind::User { message } = &message.kind else {
                return false;
            };
            message
                .content
                .iter()
                .any(|block| matches!(block, UserContent::ToolResult(_)))
        })
        .count();

    // CC `:806-819` uses `findLast`, so the NEWEST assistant row's usage wins
    // outright; the legs are never accumulated across rows.
    let tokens = progress_messages
        .iter()
        .rev()
        .find_map(
            |progress| match &progress.subagent_progress_message()?.kind {
                RenderableMessageKind::Assistant { message } => Some(message.usage.as_ref()),
                _ => None,
            },
        )
        .map(|usage| {
            // CC's `usage` is the SDK's required `Usage`; only the two cache
            // legs are nullable and only they take `?? 0` (`:815-816`). Rust's
            // aggregate coerces those nulls at the parse boundary, and the
            // aggregate itself is optional here — a forwarded row without one
            // contributes 0 rather than suppressing the segment, because CC's
            // `tokens` is null only when there is no assistant row at all.
            usage.map_or(0, |usage| {
                usage.cache_creation_input_tokens
                    + usage.cache_read_input_tokens
                    + usage.input_tokens
                    + usage.output_tokens
            })
        });

    AgentStats {
        tool_use_count,
        tokens,
    }
}

/// Maps to: CC `tools/AgentTool/UI.tsx:844-914` — the per-member entry
/// `renderGroupedAgentToolUse` derives for each grouped Agent tool use, minus
/// the three pass-throughs (`id` / `isResolved` / `isError`) the caller
/// already holds.
pub(crate) struct GroupedAgentStat {
    /// CC `agentType` — `@name` on a teammate spawn, else `userFacingName`.
    pub(crate) agent_type: String,
    /// CC `description`. `Some("")` is meaningful: `AgentProgressLine` reads it
    /// through `??` for the collapsed title but `&&` for the parenthesised
    /// suffix, so an empty string titles the row yet renders no ` ()`.
    pub(crate) description: Option<String>,
    pub(crate) tool_use_count: usize,
    pub(crate) tokens: Option<u64>,
    pub(crate) color: Option<crate::utils::theme::ThemeColorKey>,
    pub(crate) description_color: Option<crate::utils::theme::ThemeColorKey>,
    pub(crate) last_tool_info: Option<String>,
    pub(crate) task_description: Option<String>,
    /// CC `name` — the RAW input name, without the `@` the teammate branch
    /// prefixes onto `agentType`.
    pub(crate) name: Option<String>,
    pub(crate) is_async: bool,
}

/// Maps to: CC `tools/AgentTool/UI.tsx:989-1011#userFacingName`, hung on the
/// Tool object at `AgentTool.tsx:130`/`:1687`. Both of CC's callers reach it
/// through the same value: the grouped renderer (`UI.tsx:874`) and the
/// ungrouped row (`AssistantToolUseMessage.tsx:93`, `tool.userFacingName(data)`
/// where `data` is `undefined` on a failed `safeParse`) — hence the `Option`,
/// which is CC's `input: Partial<…> | undefined`.
pub(crate) fn user_facing_name(parsed: Option<&serde_json::Value>) -> String {
    match parsed
        .and_then(|parsed| parsed.get("subagent_type"))
        .and_then(|value| value.as_str())
    {
        // CC's `input?.subagent_type &&` is JS-truthy: an empty string falls to
        // the trailing `return 'Agent'`.
        Some(subagent_type) if !subagent_type.is_empty() && subagent_type != "general-purpose" => {
            // CC `:1004-1007`: worker agents display as "Agent" for a cleaner UI.
            if subagent_type == "worker" {
                "Agent".to_string()
            } else {
                subagent_type.to_string()
            }
        }
        _ => "Agent".to_string(),
    }
}

/// Maps to: CC `tools/AgentTool/UI.tsx:1013-1024#userFacingNameBackgroundColor`
/// — every subagent type, `worker` included, goes through `getAgentColor`;
/// only `general-purpose` is exempt, and that exemption lives in
/// `agentColorManager.ts:36-39`.
///
/// CC's `if (!input?.subagent_type) return undefined` is one JS-truthy guard
/// over both the absent input and the empty string.
pub(crate) fn user_facing_name_background_color(
    parsed: Option<&serde_json::Value>,
) -> Option<crate::utils::theme::ThemeColorKey> {
    let subagent_type = parsed
        .and_then(|parsed| parsed.get("subagent_type"))
        .and_then(|value| value.as_str())?;
    if subagent_type.is_empty() {
        return None;
    }
    super::agent_color_manager::get_agent_color(subagent_type)
}

/// Maps to: CC `tools/AgentTool/UI.tsx:844-914`.
///
/// `output_status` is CC's `result?.output?.status`, where `output` is the
/// result row's raw `toolUseResult` (`GroupedToolUseContent.tsx:42-46`).
///
/// `tools` is the pool half of `renderGroupedAgentToolUse`'s `options`
/// (`UI.tsx:835` `const { shouldAnimate, tools } = options`), read only by
/// `extractLastToolInfo(progressMessages, tools)` (`:847`).
pub(crate) fn grouped_agent_stat(
    input: &serde_json::Value,
    output_status: Option<&str>,
    progress_messages: &[crate::types::message::ToolUseProgressMessage],
    tools: &[crate::types::tools::Tool],
) -> GroupedAgentStat {
    let stats = calculate_agent_stats(progress_messages);
    let last_tool_info = extract_last_tool_info(progress_messages, tools);
    // CC `:848` `inputSchema().safeParse(param.input)` — the same gated schema
    // the tool advertises, so the `run_in_background` omit below is the real
    // one (`AgentTool.tsx:252-254`).
    let parsed = crate::utils::zod::safe_parse(super::input_schema(), input).ok();
    let field = |key: &str| {
        parsed
            .as_ref()
            .and_then(|parsed| parsed.get(key))
            .and_then(|value| value.as_str())
    };

    // CC `:850-853`: `teammate_spawned` is not in the exported Output union,
    // so the check is a string comparison on the raw status.
    let is_teammate_spawn = output_status == Some("teammate_spawned");

    let agent_type;
    let description;
    let mut color = None;
    let mut description_color = None;
    let task_description;
    // CC `:861` `isTeammateSpawn && parsedInput.success && parsedInput.data.name`
    // — the name guard is JS-truthy, so an empty name takes the else branch.
    match field("name").filter(|name| is_teammate_spawn && !name.is_empty()) {
        Some(name) => {
            agent_type = format!("@{name}");
            let custom = field("subagent_type").filter(|value| is_custom_subagent_type(value));
            // CC `:864-866`: only a CUSTOM type is shown beside the name.
            description = custom.map(ToOwned::to_owned);
            task_description = field("description").map(ToOwned::to_owned);
            // CC `:869-871`: the custom agent definition's colour lands on the
            // TYPE, not on the name.
            description_color = custom.and_then(super::agent_color_manager::get_agent_color);
        }
        None => {
            // CC `:872-881`: both reads are gated on `parsedInput.success`, and
            // the failure arm spells out the same `'Agent'` / `undefined` the
            // functions themselves return for an absent input.
            agent_type = match parsed.as_ref() {
                Some(data) => user_facing_name(Some(data)),
                None => "Agent".to_string(),
            };
            description = field("description").map(ToOwned::to_owned);
            color = match parsed.as_ref() {
                Some(data) => user_facing_name_background_color(Some(data)),
                None => None,
            };
            task_description = None;
        }
    }

    // CC `:886-895`. `'run_in_background' in parsedInput.data` is a presence
    // check on the STRIPPED data, so the schema's gated omit decides it too.
    let launched_as_async = parsed
        .as_ref()
        .and_then(|parsed| parsed.get("run_in_background"))
        == Some(&serde_json::Value::Bool(true));
    let backgrounded_mid_execution =
        matches!(output_status, Some("async_launched" | "remote_launched"));

    GroupedAgentStat {
        agent_type,
        description,
        tool_use_count: stats.tool_use_count,
        tokens: stats.tokens,
        color,
        description_color,
        last_tool_info,
        task_description,
        // CC `:897`: the raw name, independent of the teammate branch.
        name: field("name").map(ToOwned::to_owned),
        is_async: launched_as_async || backgrounded_mid_execution || is_teammate_spawn,
    }
}

/// Maps to: CC `tools/AgentTool/UI.tsx:1128-1136#isCustomSubagentType` —
/// `!!subagentType && !== 'general-purpose' && !== 'worker'`. The first leg is
/// JS-truthy, so an empty string is NOT custom. Read only by the grouped
/// teammate-spawn branch (`:864-871`).
pub(crate) fn is_custom_subagent_type(agent_type: &str) -> bool {
    !agent_type.is_empty() && !matches!(agent_type, "general-purpose" | "worker")
}

// ─── Ungrouped tool-use row ──────────────────────────────────────────────
// Maps to: CC `tools/AgentTool/UI.tsx:472-512` — the two renderers
// `AssistantToolUseMessage` reads off the Tool object for a single (ungrouped)
// Agent row.

/// Maps to: CC `tools/AgentTool/UI.tsx:472-483` `renderToolUseMessage` —
/// `if (!description || !prompt) return null; return description`. Both
/// guards are JS-truthy, so an absent OR empty value on either field hides
/// the whole tool-use row (AssistantToolUseMessage.tsx:145-150).
///
/// The row's bold name and its background colour come from
/// [`user_facing_name`] / [`user_facing_name_background_color`], and the model
/// from [`render_tool_use_tag`] — the three separate members CC hangs on the
/// Tool object. This function contributes only the parenthesised text.
///
/// Seam: CC hides the whole row when `inputSchema.safeParse(param.input)`
/// fails (`AssistantToolUseMessage.tsx:88-89`, `:145-150`); this port reads the
/// two required fields off the raw input, which agrees with CC for every shape
/// that differs in `description`/`prompt` and diverges only for an input that
/// carries both yet violates another field's type (e.g. `model: "gpt"`).
pub(crate) fn render_tool_use_message(input: &serde_json::Value) -> Option<String> {
    let description = crate::components::messages::user_tool_result_message::utils::first_string(
        input,
        &["description"],
    )
    .filter(|description| !description.is_empty())?;
    crate::components::messages::user_tool_result_message::utils::first_string(input, &["prompt"])
        .filter(|prompt| !prompt.is_empty())?;
    Some(description)
}

/// Maps to: CC `tools/AgentTool/UI.tsx:485-512#renderToolUseTag`.
///
/// One tag today: the agent's model, shown only when it resolves to something
/// other than the main-loop model. `input.model` is JS-truthy-gated, so an
/// empty string renders nothing. The dim ` ` + text chrome CC builds with
/// `marginLeft={1}` lives at the shared tag slot in
/// `AssistantToolUseMessage`, the same slot `FileReadTool` already uses.
pub(crate) fn render_tool_use_tag(input: &serde_json::Value) -> Option<String> {
    let model = crate::components::messages::user_tool_result_message::utils::first_string(
        input,
        &["model"],
    )
    .filter(|model| !model.is_empty())?;
    let main_model = crate::utils::model::model::get_main_loop_model();
    let agent_model = crate::utils::model::model::parse_user_specified_model(&model);
    if agent_model == main_model {
        return None;
    }
    Some(crate::utils::model::model::render_model_name(&agent_model))
}

// ─── Transcript display blocks ───────────────────────────────────────────
// CC declares these in this very file (`tools/AgentTool/UI.tsx`), reaching up
// into the generic message components for the rows themselves. They lived in
// `components/messages/user_tool_result_message/mod.rs` until the previous
// batch, which restored CC's ownership; their only consumer,
// [`render_tool_result_message`], now sits in this file too.

#[derive(Default, Props)]
pub(crate) struct AgentTranscriptMarkdownBlockProps {
    pub title: String,
    pub blocks: Vec<String>,
}

/// Maps to: CC `tools/AgentTool/UI.tsx:225-263` — `AgentPromptDisplay` (`:225`)
/// and `AgentResponseDisplay` (`:245`). One parameterized block covers both,
/// because upstream's two components differ only by the `Text color="success"
/// bold` title ("Prompt:" vs "Response:"): both render each block through
/// `Markdown` inside a `paddingLeft={2}` box, and both space the second and
/// later blocks with `marginTop={index === 0 ? 0 : 1}`.
///
/// Blocks are rendered verbatim: `AgentResponseDisplay` maps
/// `content.map((block, index) => <Box … marginTop={index === 0 ? 0 : 1}>
/// <Markdown>{block.text}</Markdown></Box>)` (`:256-259`) with no trim and no
/// empty-block filter, so `["", "hello"]` renders an empty box whose successor
/// still carries its `marginTop` — a 1-row gap. This block used to trim and
/// drop empty entries, collapsing that gap.
#[component]
pub(crate) fn AgentTranscriptMarkdownBlock(
    props: &AgentTranscriptMarkdownBlockProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let blocks = props.blocks.clone();

    element! {
        View(flex_direction: FlexDirection::Row) {
            Text(content: "  ⎿ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
            View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                Text(content: props.title.clone(), color: theme.success, weight: Weight::Bold)
                #(blocks.into_iter().enumerate().map(|(idx, block)| {
                    element! {
                        View(
                            flex_direction: FlexDirection::Column,
                            padding_left: 2u32,
                            margin_top: if idx == 0 { 0u32 } else { 1u32 },
                        ) {
                            Markdown(content: block)
                        }
                    }
                }))
            }
        }
    }
}

#[derive(Default, Props)]
pub(crate) struct AgentVerboseTranscriptBlockProps {
    pub progress_messages: Vec<ToolUseProgressMessage>,
    /// CC `verbose` (`tools/AgentTool/UI.tsx:268`) — threaded from
    /// `renderToolResultMessage`'s own `verbose` option into every mounted row
    /// (`:311`, `MessageComponent verbose={verbose}`). The upstream source is
    /// the message list's `verbose` prop, which `UserToolSuccessMessage`
    /// receives and forwards.
    pub verbose: bool,
}

/// Maps to: CC `tools/AgentTool/UI.tsx:271-323#VerboseAgentTranscript` — the
/// rendering half. The row derivation is [`agent_verbose_progress_rows`].
///
/// Every row is wrapped in `<MessageResponse height={1}>` (`:304`), and
/// `MessageResponse` clamps with `overflowY="hidden"`
/// (`components/MessageResponse.tsx:18`), so a multi-line row shows exactly
/// its first line. iocraft's `overflow: Hidden` clips both axes where CC clips
/// only Y; the X axis is irrelevant here because the overwide case wraps into
/// the clipped second row in both trees.
#[component]
pub(crate) fn AgentVerboseTranscriptBlock(
    props: &AgentVerboseTranscriptBlockProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let rows = agent_verbose_progress_rows(&props.progress_messages);
    let verbose = props.verbose;

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(rows.into_iter().map(|row| match row {
                AgentVerboseProgressRow::Text(text) => element! {
                    View(
                        flex_direction: FlexDirection::Row,
                        height: 1u32,
                        overflow: Overflow::Hidden,
                    ) {
                        Text(content: "  ⎿ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                            // CC `Message.tsx:433-442` mounts
                            // `AssistantTextMessage`, whose default arm is the
                            // `<Markdown>` this used to call directly
                            // (`AssistantTextMessage.tsx:203-207`). Mounting it
                            // also picks up the error/rate-limit arms above
                            // that default, which a bare Markdown skipped.
                            AssistantTextMessage(
                                content: text,
                                add_margin: false,
                                should_show_dot: false,
                                verbose: verbose,
                            )
                        }
                    }
                }.into_any(),
                AgentVerboseProgressRow::ToolUse { tool_name, input, status } => element! {
                    View(
                        flex_direction: FlexDirection::Row,
                        height: 1u32,
                        overflow: Overflow::Hidden,
                    ) {
                        Text(content: "  ⎿ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                            AssistantToolUseMessage(
                                tool_name: tool_name,
                                // `input` present => the row renders through the
                                // owning tool's `renderToolUseMessage`, which is
                                // what CC's mounted `MessageComponent` does.
                                input: Some(input),
                                description: String::new(),
                                status: Some(status),
                                add_margin: false,
                                can_animate: false,
                                should_show_dot: Some(false),
                                verbose: verbose,
                                is_transcript_mode: false,
                            )
                        }
                    }
                }.into_any(),
                AgentVerboseProgressRow::ToolResult { tool_name, status, content, tool_use_result } => element! {
                    View(height: 1u32, overflow: Overflow::Hidden) {
                        UserToolResultMessage(
                            tool_name: tool_name,
                            // Progress-carried status folds onto the wire flag;
                            // sentinel-less progress content cannot hit the
                            // cancel/reject leaves anyway.
                            is_error: status != ToolResultStatus::Success,
                            content: content,
                            tool_use_result: tool_use_result,
                            verbose: verbose,
                            is_transcript_mode: false,
                        )
                    }
                }.into_any(),
            }))
        }
    }
}

/// One `VerboseAgentTranscript` row (CC `UI.tsx:303-320` mounts
/// `MessageComponent` per message; Cometix derives a typed row per content
/// block and mounts the same leaf component `Message.tsx` would dispatch to —
/// see [`AgentVerboseTranscriptBlock`]).
///
/// The old `RenderableMessage` arm retired with the `SubagentTranscriptMessage`
/// payload variant: `AgentProgress` now carries the whole `RenderableMessage`,
/// so the replay seam and the live path are the same rows. Its Rust-only
/// `response_gutter: false` escape hatch went with it — CC wraps EVERY
/// transcript row in `<MessageResponse height={1}>` (`:304`), so the gutter is
/// unconditional, and so is the one-line clamp that wrapper carries
/// ([`AgentVerboseTranscriptBlock`] ports both).
#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentVerboseProgressRow {
    Text(String),
    ToolUse {
        tool_name: String,
        /// CC `param.input`. `VerboseAgentTranscript` (`tools/AgentTool/UI.tsx:299-320`)
        /// hands each progress message to the full `MessageComponent` with
        /// `tools`, so a nested tool use resolves its own renderer exactly as a
        /// top-level one does. Carrying the input keeps that possible; a
        /// pre-rendered string here could only be produced by guessing.
        input: serde_json::Value,
        status: ToolUseStatus,
    },
    ToolResult {
        tool_name: String,
        status: ToolResultStatus,
        content: String,
        /// CC `message.toolUseResult` — the raw the by-tool-name renderer
        /// consumes.
        tool_use_result: Option<serde_json::Value>,
    },
}

/// Maps to: CC `tools/AgentTool/UI.tsx:271-320` `VerboseAgentTranscript` — the
/// filter at `:288-299` plus the per-message `MessageComponent` mount at
/// `:303-320`, projected onto Cometix's row enum.
///
/// CC's filter drops a user progress row whose MESSAGE has no `toolUseResult`
/// ("Subagent progress messages don't carry the parsed tool output, so
/// UserToolSuccessMessage returns null and MessageResponse renders a bare ⎿").
/// Rust carries `tool_use_result` on the `tool_result` block instead of the
/// message envelope, so the same predicate reads it there; a user row with no
/// tool_result block has no `toolUseResult` either and drops the same way.
fn agent_verbose_progress_rows(
    progress_messages: &[ToolUseProgressMessage],
) -> Vec<AgentVerboseProgressRow> {
    use crate::types::message::{AssistantContent, RenderableMessageKind, UserContent};

    // CC `:276-282` runs `buildSubagentLookups` over the same list first.
    let lookups = subagent_progress_lookups(progress_messages);
    progress_messages
        .iter()
        .filter_map(|progress| {
            // CC `hasProgressMessage` (`:289-291`).
            let message = progress.subagent_progress_message()?;
            match &message.kind {
                RenderableMessageKind::Assistant { message } => {
                    match message.first_content_block()? {
                        AssistantContent::ToolUse(tool_use) => {
                            let tool_name = tool_use.name.trim();
                            if tool_name.is_empty() {
                                return None;
                            }
                            Some(AgentVerboseProgressRow::ToolUse {
                                tool_name: tool_name.to_string(),
                                input: tool_use.input.clone(),
                                // CC `inProgressToolUseIDs`
                                // (`utils/messages.ts:1400-1405`) is what the
                                // mounted `MessageComponent` reads for this.
                                status: lookups.tool_use_status(tool_use.id.0.as_str()),
                            })
                        }
                        AssistantContent::Text(text) => {
                            let text = text.trim();
                            (!text.is_empty())
                                .then(|| AgentVerboseProgressRow::Text(text.to_string()))
                        }
                        _ => None,
                    }
                }
                RenderableMessageKind::User { message } => {
                    let UserContent::ToolResult(result) = message.first_content_block()? else {
                        // CC `:293-296`: a user row without `toolUseResult`
                        // drops; a non-tool_result user row never has one.
                        return None;
                    };
                    let tool_use = lookups.tool_use(result.tool_use_id.0.as_str())?;
                    let projection = verbose_agent_transcript_tool_result_projection(
                        tool_use.name.as_str(),
                        !result.is_error,
                        result.content.as_str(),
                        result.tool_use_result.as_ref(),
                    )?;
                    Some(AgentVerboseProgressRow::ToolResult {
                        tool_name: tool_use.name.trim().to_string(),
                        status: projection.status,
                        content: result.content.trim().to_string(),
                        tool_use_result: result.tool_use_result.clone(),
                    })
                }
                _ => None,
            }
        })
        .collect()
}

// ─── Tool result renderer ────────────────────────────────────────────────
// Maps to: CC `tools/AgentTool/UI.tsx:325+#renderToolResultMessage` — ONE
// upstream renderer that Cometix splits into two halves, because this port has
// two result pipelines where CC has only React nodes:
//   * [`render_tool_result_message`] — the element half, mounted by the
//     generic success leaf (`UserToolSuccessMessage.tsx:86-99`
//     `tool.renderToolResultMessage?.()`).
//   * [`render_tool_result_lines`] — the line half, resolved by the by-name
//     dispatch table that stands in for that same dynamic dispatch.
// Neither half is invented: they are two projections of the one CC function.
// `file_edit_tool`, `file_write_tool`, `file_read_tool` and
// `notebook_edit_tool` already carry the same `render_tool_result_message` /
// `render_tool_result_lines` pair.

/// Maps to: CC `tools/AgentTool/UI.tsx:325-470#renderToolResultMessage` — the
/// ELEMENT half (see the section note above; [`render_tool_result_lines`] is
/// the line half of the same upstream renderer).
///
/// Covers all three success statuses in both modes, as the one CC function
/// does: remote_launched (`:343-356`), async_launched (`:357-390`) and
/// completed (`:392-469`). The completed branch's non-transcript hint is the
/// real [`CtrlOToExpand`] component (`:462-467`), so a nested mount inside a
/// [`SubAgentProvider`] self-suppresses exactly as CC's `SubAgentContext`
/// gate does (`components/CtrlOToExpand.tsx:24-34`). The async_launched hint
/// is NOT `CtrlOToExpand` upstream — it is `KeyboardShortcutHint` +
/// `ConfigurableShortcutHint` inside a plain `<Text dimColor>` (`:364-380`) —
/// so it stays unconditional here too.
pub(crate) fn render_tool_result_message(
    tool_name: &str,
    tool_use_result: Option<&serde_json::Value>,
    progress_messages: &[ToolUseProgressMessage],
    status: ToolResultStatus,
    verbose: bool,
    is_transcript_mode: bool,
) -> Option<AnyElement<'static>> {
    // Agent (legacy alias Task) renders from the raw `toolUseResult` on the
    // row, resolved by tool name; progress arrives through the component
    // props channel (CC `progressMessagesForMessage`).
    if !(tool_name.eq_ignore_ascii_case("Agent") || tool_name.eq_ignore_ascii_case("Task")) {
        return None;
    }
    if status != ToolResultStatus::Success {
        return None;
    }
    let output = tool_use_result.and_then(parse_output)?;
    let (agent_status, task_id, session_url, prompt, content) = (
        &output.status,
        &output.task_id,
        &output.session_url,
        &output.prompt,
        &output.content,
    );
    let (total_tool_use_count, total_duration_ms, total_tokens) = (
        &output.total_tool_use_count,
        &output.total_duration_ms,
        &output.total_tokens,
    );

    match agent_status.as_str() {
        "remote_launched" => {
            // CC `:346-353`: `Remote agent launched ` in normal text, then a
            // dim `· {taskId} · {sessionUrl}` tail. Cometix keeps its existing
            // defensive per-field omission where CC interpolates the required
            // fields unconditionally.
            let mut detail = String::new();
            if let Some(task_id) = task_id.as_deref().filter(|value| !value.is_empty()) {
                detail.push_str("· ");
                detail.push_str(task_id);
            }
            if let Some(session_url) = session_url.as_deref().filter(|value| !value.is_empty()) {
                if !detail.is_empty() {
                    detail.push(' ');
                }
                detail.push_str("· ");
                detail.push_str(session_url);
            }
            let head = if detail.is_empty() {
                "Remote agent launched".to_string()
            } else {
                "Remote agent launched ".to_string()
            };
            let dim_tail = (!detail.is_empty()).then_some(detail);
            Some(
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        MessageResponse {
                            Text(content: head, wrap: TextWrap::NoWrap)
                            #(dim_tail.map(|tail| element! {
                                Text(content: tail, dim: true, wrap: TextWrap::NoWrap)
                            }))
                        }
                    }
                }
                .into_any(),
            )
        }
        "async_launched" => {
            // CC `:364-380`: the ` (↓ to manage · ctrl+o to expand)`
            // parenthetical is dim and renders only outside transcript mode;
            // the expand leg needs a truthy prompt.
            let hint = (!is_transcript_mode).then(|| {
                let mut hints = vec!["↓ to manage".to_string()];
                if prompt.as_deref().is_some_and(|prompt| !prompt.is_empty()) {
                    hints.push(ctrl_o_to_expand_hint().trim_matches(['(', ')']).to_string());
                }
                format!(" ({})", hints.join(" · "))
            });
            // CC `:383-387`: transcript mode swaps the hint for the prompt
            // block, gated on the same truthy prompt (no trim upstream).
            let prompt_block = (is_transcript_mode)
                .then(|| prompt.clone())
                .flatten()
                .filter(|value| !value.is_empty());
            Some(
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        MessageResponse {
                            Text(content: "Backgrounded agent".to_string(), wrap: TextWrap::NoWrap)
                            #(hint.map(|hint| element! {
                                Text(content: hint, dim: true, wrap: TextWrap::NoWrap)
                            }))
                        }
                        #(prompt_block.map(|prompt| element! {
                            AgentTranscriptMarkdownBlock(
                                title: "Prompt:".to_string(),
                                blocks: vec![prompt],
                            )
                        }))
                    }
                }
                .into_any(),
            )
        }
        "completed" => {
            // CC `:427-431`: `isTranscriptMode && prompt &&` — a JS-truthy
            // gate, no trim.
            let prompt_block = (is_transcript_mode)
                .then(|| prompt.clone())
                .flatten()
                .filter(|value| !value.is_empty());
            // CC `:441`: `content && content.length > 0` gates on the RAW
            // array; empty strings inside it still render their gap slot
            // (see `AgentTranscriptMarkdownBlock`).
            let response_blocks =
                (is_transcript_mode && !content.is_empty()).then(|| content.clone());
            let completion = format!(
                "Done ({} · {} tokens · {})",
                plural_tool_uses(total_tool_use_count.unwrap_or(0)),
                format_number(total_tokens.unwrap_or(0)),
                format_duration(total_duration_ms.unwrap_or(0))
            );
            Some(
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        #(prompt_block.map(|prompt| element! {
                            AgentTranscriptMarkdownBlock(
                                title: "Prompt:".to_string(),
                                blocks: vec![prompt],
                            )
                        }))
                        // CC `:432-440`: the verbose transcript renders inside
                        // a SubAgentProvider, which is what suppresses nested
                        // expand hints. The `is_empty` guard is render-neutral
                        // (an empty list renders an empty column upstream too).
                        #(if !is_transcript_mode || progress_messages.is_empty() {
                            None
                        } else {
                            Some(element! {
                                SubAgentProvider {
                                    AgentVerboseTranscriptBlock(
                                        progress_messages: progress_messages.to_vec(),
                                        verbose: verbose,
                                    )
                                }
                            })
                        })
                        #(response_blocks.map(|blocks| element! {
                            AgentTranscriptMarkdownBlock(
                                title: "Response:".to_string(),
                                blocks: blocks,
                            )
                        }))
                        MessageResponse(content: completion)
                        // CC `:462-467`: `{!isTranscriptMode && <Text dimColor>
                        // {'  '}<CtrlOToExpand /></Text>}` — the component
                        // self-suppresses inside SubAgentProvider.
                        #((!is_transcript_mode).then(|| element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "  ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                CtrlOToExpand
                            }
                        }))
                    }
                }
                .into_any(),
            )
        }
        _ => None,
    }
}

/// Maps to: CC `tools/AgentTool/UI.tsx:325+#renderToolResultMessage` — the LINE
/// half (see the section note above; [`render_tool_result_message`] is the
/// element half of the same upstream renderer). Called from the by-tool-name
/// dispatch table in `user_tool_result_message/mod.rs`, which stands in for
/// CC's `tool.renderToolResultMessage?.()` dynamic dispatch.
pub(crate) fn render_tool_result_lines(
    status: &str,
    _agent_id: Option<&str>,
    task_id: Option<&str>,
    session_url: Option<&str>,
    prompt: Option<&str>,
    content: &[String],
    total_tool_use_count: Option<usize>,
    total_duration_ms: Option<u64>,
    total_tokens: Option<u64>,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    match status {
        "remote_launched" => {
            // CC `UI.tsx:346-353` dims the `· {taskId} · {sessionUrl}` tail
            // inside the otherwise-normal line. `text` keeps the full joined
            // copy for plain-text consumers; the styled split rides
            // `segments`, which the component renderer prefers.
            let mut detail = String::new();
            if let Some(task_id) = task_id.filter(|value| !value.is_empty()) {
                detail.push_str(" · ");
                detail.push_str(task_id);
            }
            if let Some(session_url) = session_url.filter(|value| !value.is_empty()) {
                detail.push_str(" · ");
                detail.push_str(session_url);
            }
            let mut line = ToolRenderLine::new(
                format!("Remote agent launched{detail}"),
                ToolRenderTone::Normal,
            );
            if !detail.is_empty() {
                line = line.with_segments(vec![
                    ToolRenderSegment::new("Remote agent launched "),
                    ToolRenderSegment::new(detail.trim_start().to_string()).with_dim(true),
                ]);
            }
            vec![line]
        }
        "async_launched" => {
            // CC `UI.tsx:362-381` dims the whole ` (↓ to manage · ctrl+o to
            // expand)` parenthetical (`<Text dimColor>{' ('}…{')'}</Text>`).
            let mut text = "Backgrounded agent".to_string();
            let mut hint = None;
            if !options.is_transcript_mode {
                let mut hints = vec!["↓ to manage".to_string()];
                if prompt.is_some_and(|prompt| !prompt.is_empty()) {
                    hints.push(ctrl_o_to_expand_hint().trim_matches(['(', ')']).to_string());
                }
                let parenthetical = format!(" ({})", hints.join(" · "));
                text.push_str(&parenthetical);
                hint = Some(parenthetical);
            }
            let mut line = ToolRenderLine::new(text, ToolRenderTone::Normal);
            if let Some(hint) = hint {
                line = line.with_segments(vec![
                    ToolRenderSegment::new("Backgrounded agent"),
                    ToolRenderSegment::new(hint).with_dim(true),
                ]);
            }
            let mut lines = vec![line];
            if options.is_transcript_mode {
                push_agent_prompt_lines(&mut lines, prompt);
            }
            lines
        }
        "completed" => {
            let tool_uses = total_tool_use_count.unwrap_or(0);
            let tokens = total_tokens.unwrap_or(0);
            let duration = total_duration_ms.unwrap_or(0);
            let text = format!(
                "Done ({} · {} tokens · {})",
                plural_tool_uses(tool_uses),
                format_number(tokens),
                format_duration(duration)
            );
            let mut lines = Vec::new();
            if options.is_transcript_mode {
                push_agent_prompt_lines(&mut lines, prompt);
                push_agent_response_lines(&mut lines, content);
            }
            lines.push(ToolRenderLine::new(text, ToolRenderTone::Normal));
            // CC `UI.tsx:462-467` renders this hint through `CtrlOToExpand`,
            // which self-suppresses inside a `SubAgentProvider`. The component
            // path takes [`render_tool_result_message`], where the real
            // component carries that gate; this line half serves only
            // contexts with no component tree (top-level semantics), so the
            // hint is unconditional here.
            if !options.is_transcript_mode {
                lines.push(ToolRenderLine::new(
                    format!("  {}", ctrl_o_to_expand_hint()),
                    ToolRenderTone::Inactive,
                ));
            }
            lines
        }
        _ => Vec::new(),
    }
}

// ─── Error / rejected renderers ──────────────────────────────────────────
// Maps to: CC `tools/AgentTool/UI.tsx:723-789` — `renderToolUseRejectedMessage`
// and `renderToolUseErrorMessage`. Both replay the progress transcript through
// `renderToolUseProgressMessage` (`:755-759`, `:780-785`) and then render the
// generic fallback. The dispatch stays with the generic error/reject leaves
// (`UserToolErrorMessage.tsx:83-94`, `UserToolRejectMessage.tsx:45-58`); only
// the bodies live here, per the same ownership split as
// [`render_tool_result_message`].

/// CC `INITIALIZING_TEXT` (`tools/AgentTool/UI.tsx:514`).
const INITIALIZING_TEXT: &str = "Initializing…";

/// CC `ESTIMATED_LINES_PER_TOOL` (`tools/AgentTool/UI.tsx:220`).
const ESTIMATED_LINES_PER_TOOL: usize = 9;

/// CC `TERMINAL_BUFFER_LINES` (`tools/AgentTool/UI.tsx:221`).
const TERMINAL_BUFFER_LINES: usize = 7;

/// One row of CC's `displayedMessages.map(…)` mount (`UI.tsx:673-710`).
///
/// CC mounts a whole `<MessageComponent … style="condensed">` per processed
/// message (`:692-709`). `Message.tsx:109-131` maps the message's content
/// blocks through `AssistantMessageBlock`, and the producer normalizes each
/// forwarded progress message down to a single block
/// (`AgentTool.tsx:1483-1506`), so exactly one of that switch's arms runs per
/// row: `tool_use` → `AssistantToolUseMessage` (`Message.tsx:406-432`), `text`
/// → `AssistantTextMessage` (`:433-442`). `style="condensed"` reaches only the
/// USER branch (`Message.tsx:170-177`), which the external build's row filter
/// has already removed, so it carries nothing here.
///
/// This enum carries what each mount needs and nothing pre-rendered: until
/// this batch the derivation collapsed every row to a `String`, which cost the
/// tool row its bold facing name, its agent background color, its dim tag and
/// its path hyperlinks, and cost a text row its Markdown entirely.
///
/// Seam (unchanged here): a `thinking` / `redacted_thinking` / image block
/// produces NO row, where CC keeps the message in `processedMessages` — it
/// passes the assistant-only filter (`:129-135`) — and mounts a component that
/// renders null outside verbose/transcript (`Message.tsx:444-463`). The
/// difference is one slot in the 3-row display window and in the hidden count.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentProgressRow {
    /// CC `Message.tsx:433-442` → `AssistantTextMessage`.
    Text(String),
    /// CC `Message.tsx:406-432` → `AssistantToolUseMessage`.
    ToolUse {
        tool_name: String,
        /// CC `param.input` — the mounted row runs the owning tool's own
        /// `renderToolUseMessage`, so a pre-rendered string here could only be
        /// guessed.
        input: serde_json::Value,
        /// CC `inProgressToolUseIDs` (`utils/messages.ts:1400-1405`), which
        /// the mounted component turns into `isResolved`/`isQueued`
        /// (`AssistantToolUseMessage.tsx:120-121`).
        status: ToolUseStatus,
    },
}

impl AgentProgressRow {
    /// CC `:624-635` counts HIDDEN TOOL USES, not hidden messages: the
    /// predicate is `content.some(c => c.type === 'tool_use')`.
    fn hidden_weight(&self) -> usize {
        match self {
            AgentProgressRow::ToolUse { .. } => 1,
            AgentProgressRow::Text(_) => 0,
        }
    }
}

/// The projection-neutral carrier for CC
/// `tools/AgentTool/UI.tsx:516-721#renderToolUseProgressMessage` — ONE CC
/// derivation with three call sites: the collapsed in-flight view
/// (`AssistantToolUseMessage.tsx:203-231`), the rejected replay (`UI.tsx:755`)
/// and the error replay (`:781`). All three now render through the one
/// component [`AgentToolUseProgressMessage`], as CC's one function does.
struct AgentProgressRows {
    /// Visible rows after the display window. A row whose mounted component
    /// renders null (`UI.tsx:687-691`) still occupies a slot here — the mount
    /// itself decides, exactly as upstream.
    visible: Vec<AgentProgressRow>,
    /// CC `hiddenToolUseCount` (`UI.tsx:618-635`).
    hidden_tool_use_count: usize,
    /// CC `prompt` — `progressMessages[0]?.data.prompt`, JS-truthy
    /// (`UI.tsx:637-639`).
    prompt: Option<String>,
    /// CC `displayedMessages.length === 0` (`UI.tsx:645`) — counted BEFORE
    /// the empty-string drop, like upstream's null-rendering rows.
    displayed_is_empty: bool,
}

impl AgentProgressRows {
    /// CC's two `Initializing…` fallbacks collapsed into one predicate: the
    /// no-progress return (`UI.tsx:532-538` — an empty list yields empty rows,
    /// so `displayed_is_empty` holds and no prompt exists) and the
    /// all-filtered window without a transcript prompt (`:645-651`).
    ///
    /// NOT the all-gated case: a window whose every row renders null keeps
    /// `displayed_is_empty` false, and CC then mounts the outer
    /// `MessageResponse` around null children — a bare `⎿` gutter row
    /// (`:664-719`, `:687-691`), never `Initializing…`.
    fn falls_back_to_initializing(&self, is_transcript_mode: bool) -> bool {
        self.displayed_is_empty && !(is_transcript_mode && self.prompt.is_some())
    }
}

/// Maps to: CC `tools/AgentTool/UI.tsx:516-721#renderToolUseProgressMessage` —
/// the shared row derivation behind all three CC call sites. The condensed
/// branch (`:540-599`) is chrome rather than rows and stays with the renderer
/// ([`AgentToolUseProgressMessage`]); the replay re-entries pass no
/// `terminalSize` / `inProgressToolCallCount` (`UI.tsx:755-759`, `:781-785`),
/// so it is dead for those two call sites.
///
/// The external build of `processProgressMessages` keeps ONLY assistant rows
/// (`:129-136`, `m.data.message.type !== 'user'`), so there are no summary
/// groups and no user rows. Nothing is rendered here: each row carries what
/// its CC component mount needs (see [`AgentProgressRow`]), and the four
/// `AssistantToolUseMessage` gates (`AssistantToolUseMessage.tsx:141-150`) are
/// applied by that component itself — the row still occupies its slot in the
/// display window and the hidden count either way (`UI.tsx:687-691`).
fn agent_progress_rows(
    progress_messages: &[ToolUseProgressMessage],
    is_transcript_mode: bool,
) -> AgentProgressRows {
    use crate::types::message::{AssistantContent, RenderableMessageKind};

    // CC `:653-660` builds the lookups over the WHOLE progress list before
    // rendering any row, so a result that arrives after the display window
    // still resolves the tool use inside it.
    let lookups = subagent_progress_lookups(progress_messages);

    // CC `:129-136`: external keeps assistant rows only.
    let rows: Vec<AgentProgressRow> = progress_messages
        .iter()
        .filter_map(|progress| {
            let message = progress.subagent_progress_message()?;
            let RenderableMessageKind::Assistant { message } = &message.kind else {
                return None;
            };
            match message.first_content_block()? {
                AssistantContent::ToolUse(tool_use) => Some(AgentProgressRow::ToolUse {
                    tool_name: tool_use.name.trim().to_string(),
                    input: tool_use.input.clone(),
                    status: lookups.tool_use_status(tool_use.id.0.as_str()),
                }),
                AssistantContent::Text(text) => Some(AgentProgressRow::Text(text.clone())),
                _ => None,
            }
        })
        .collect();

    // CC `:610-623`: transcript shows everything; the main screen shows the
    // last MAX_PROGRESS_MESSAGES_TO_SHOW (3) processed messages.
    let hidden_rows = if is_transcript_mode {
        0
    } else {
        rows.len().saturating_sub(3)
    };
    let hidden_tool_use_count = rows
        .iter()
        .take(hidden_rows)
        .map(AgentProgressRow::hidden_weight)
        .sum();

    // CC `:637-639` — `firstData.prompt`, guarded by `hasProgressMessage`;
    // both gates below are JS-truthy.
    let prompt = progress_messages
        .first()
        .and_then(|progress| match progress {
            ToolUseProgressMessage::AgentProgress { prompt, .. }
            | ToolUseProgressMessage::SkillProgress { prompt, .. } => Some(prompt.clone()),
            _ => None,
        })
        .filter(|prompt| !prompt.is_empty());

    let displayed_is_empty = rows.len() == hidden_rows;
    let visible = rows.into_iter().skip(hidden_rows).collect();

    AgentProgressRows {
        visible,
        hidden_tool_use_count,
        prompt,
        displayed_is_empty,
    }
}

/// Maps to: CC `tools/AgentTool/UI.tsx:551-578#getProgressStats` — the
/// condensed-mode stats closure.
///
/// NOT [`calculate_agent_stats`] (`:791-822`): that one counts USER rows
/// carrying a `tool_result` block; this one counts messages whose content
/// carries a `tool_use` block (`:552-560` — the predicate has no
/// `type === 'assistant'` narrowing, but only assistant content can hold
/// one). The token leg is the same `findLast`-assistant derivation both CC
/// functions inline verbatim (`:562-575` vs `:806-819`), duplicated here
/// because CC duplicates it.
struct CondensedProgressStats {
    tool_use_count: usize,
    /// CC `tokens` — `null`, not `0`, without an assistant row
    /// (`UI.tsx:567-575`).
    tokens: Option<u64>,
}

/// Maps to: CC `tools/AgentTool/UI.tsx:551-578#getProgressStats`.
fn get_progress_stats(progress_messages: &[ToolUseProgressMessage]) -> CondensedProgressStats {
    use crate::types::message::{AssistantContent, RenderableMessageKind};

    // CC `:552-560`: one per progress message whose content holds a
    // `tool_use` block — the assistant half of a nested call, unlike
    // `calculateAgentStats`' tool_result count.
    let tool_use_count = progress_messages
        .iter()
        .filter(|progress| {
            let Some(message) = progress.subagent_progress_message() else {
                return false;
            };
            match &message.kind {
                RenderableMessageKind::Assistant { message } => message
                    .content
                    .iter()
                    .any(|block| matches!(block, AssistantContent::ToolUse(_))),
                _ => false,
            }
        })
        .count();

    // CC `:562-575` — `findLast`, so the NEWEST assistant row's usage wins
    // outright; the two cache legs are `?? 0` while input/output are
    // required. Rust's aggregate coerces the nullable legs at the parse
    // boundary and is itself optional — a forwarded row without one
    // contributes 0 rather than suppressing the segment, because CC's
    // `tokens` is null only when there is no assistant row at all (the same
    // holding as [`calculate_agent_stats`]).
    let tokens = progress_messages
        .iter()
        .rev()
        .find_map(
            |progress| match &progress.subagent_progress_message()?.kind {
                RenderableMessageKind::Assistant { message } => Some(message.usage.as_ref()),
                _ => None,
            },
        )
        .map(|usage| {
            usage.map_or(0, |usage| {
                usage.cache_creation_input_tokens
                    + usage.cache_read_input_tokens
                    + usage.input_tokens
                    + usage.output_tokens
            })
        });

    CondensedProgressStats {
        tool_use_count,
        tokens,
    }
}

#[derive(Default, Props)]
pub(crate) struct AgentToolUseProgressMessageProps {
    pub progress_messages: Vec<ToolUseProgressMessage>,
    pub verbose: bool,
    pub is_transcript_mode: bool,
    /// CC `terminalSize?.rows` (`UI.tsx:521`, `:547-549`). `None` mirrors a
    /// caller that passes no `terminalSize` at all — the error and rejected
    /// replays (`:755-759`, `:781-785`) — which disables condensed mode.
    pub terminal_rows: Option<u16>,
    /// CC `inProgressToolCallCount` (`:520`), read as `?? 1` (`:543`).
    pub in_progress_tool_call_count: Option<usize>,
}

/// Maps to: CC `tools/AgentTool/UI.tsx:516-721#renderToolUseProgressMessage` —
/// the WHOLE function, as one component, for all three of its call sites: the
/// collapsed in-flight view (`AssistantToolUseMessage.tsx:203-231`), the
/// rejected replay (`UI.tsx:755`) and the error replay (`:781`).
///
/// Until this batch the in-flight call site went through a `Vec<String>`
/// projection, which flattened everything CC styles here: the dim
/// `Initializing…` row, the dim condensed line with its bold count, the
/// success-bold `Prompt:` title over a Markdown body, the dim `+N more tool
/// uses` line, and — the whole point of the mount — each row's own
/// `MessageComponent` (bold tool name, the agent background color, the dim
/// tag, path hyperlinks, Markdown for a text row).
///
/// Structure per `:664-720`: the `MessageResponse` gutter, then a column
/// holding a `SubAgentProvider` around the prompt block and the visible rows,
/// then the `+N more tool uses <CtrlOToExpand/>` line OUTSIDE the provider
/// (`:711-717`) so only a NESTED mount suppresses its hint.
#[component]
pub(crate) fn AgentToolUseProgressMessage(
    props: &AgentToolUseProgressMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();

    // CC `:532-538` — no progress at all.
    if props.progress_messages.is_empty() {
        return initializing_row();
    }

    // CC `:540-549` — the condensed one-liner guard. Every leg is JS-truthy:
    // `terminalSize && terminalSize.rows &&` skips both a missing size and a
    // zero row count (the head-less test-render default).
    let estimated_lines = props.in_progress_tool_call_count.unwrap_or(1) * ESTIMATED_LINES_PER_TOOL
        + TERMINAL_BUFFER_LINES;
    let condensed = !props.is_transcript_mode
        && props
            .terminal_rows
            .map(usize::from)
            .is_some_and(|rows| rows > 0 && rows < estimated_lines);
    if condensed {
        // CC `:583-597`: one `<Text dimColor>` run whose tool-use COUNT is
        // `<Text bold>` — dim and bold together, because Ink's nested Text
        // inherits the parent's dimColor.
        let stats = get_progress_stats(&props.progress_messages);
        let mut contents = vec![
            MixedTextContent::new("In progress… · ").dim(),
            MixedTextContent::new(stats.tool_use_count.to_string())
                .dim()
                .weight(Weight::Bold),
            MixedTextContent::new(format!(
                " tool {}",
                if stats.tool_use_count == 1 {
                    "use"
                } else {
                    "uses"
                }
            ))
            .dim(),
        ];
        // CC `:588` `{tokens && ` · …`}` — null omits the segment, but React
        // renders a literal number `0` as a child, so `tokens === 0` in CC
        // would emit a glued "0" after the tool-use count (a bug-shaped
        // texture, not an omission). Deliberate deviation: this port omits at
        // 0 as well. Practically unreachable — progress rows are real API
        // assistant replies whose usage legs sum above zero.
        if let Some(tokens) = stats.tokens.filter(|tokens| *tokens > 0) {
            contents
                .push(MixedTextContent::new(format!(" · {} tokens", format_number(tokens))).dim());
        }
        contents.push(MixedTextContent::new(" · ").dim());
        return element! {
            MessageResponse(height: Some(1u32)) {
                View(flex_direction: FlexDirection::Row) {
                    MixedText(contents: contents, wrap: TextWrap::NoWrap)
                    // CC `:589-595`. This hint is `ConfigurableShortcutHint`,
                    // NOT `CtrlOToExpand`, so no SubAgent suppression applies
                    // upstream either.
                    ConfigurableShortcutHint(
                        action: "app:toggleTranscript".to_string(),
                        context: "Global".to_string(),
                        fallback: "ctrl+o".to_string(),
                        description: "expand".to_string(),
                        parens: true,
                        dim: true,
                    )
                }
            }
        }
        .into_any();
    }

    let rows = agent_progress_rows(&props.progress_messages, props.is_transcript_mode);
    // CC `:645-651` — an all-filtered window without a transcript prompt.
    if rows.falls_back_to_initializing(props.is_transcript_mode) {
        return initializing_row();
    }

    let prompt_block = (props.is_transcript_mode)
        .then(|| rows.prompt.clone())
        .flatten();
    // CC `:700` hands each mounted row `collapsedInProgressIDs`, whose `.size`
    // becomes the nested row's own `inProgressToolCallCount`
    // (`Message.tsx:129`).
    let nested_in_progress_count =
        subagent_progress_lookups(&props.progress_messages).in_progress_count();
    let verbose = props.verbose;
    let visible = rows.visible;
    let hidden_tool_use_count = rows.hidden_tool_use_count;
    let hidden_line = (hidden_tool_use_count > 0).then(|| {
        format!(
            "+{hidden_tool_use_count} more tool {} ",
            if hidden_tool_use_count == 1 {
                "use"
            } else {
                "uses"
            }
        )
    });

    element! {
        MessageResponse {
            View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                SubAgentProvider {
                    // CC `:668-672`: the transcript prompt heading inside the
                    // provider, spaced with `marginBottom={1}`. The body is
                    // `AgentPromptDisplay` (`:225-243`) — a success-bold title
                    // over `<Markdown>` behind `paddingLeft={2}`.
                    #(prompt_block.map(|prompt| element! {
                        View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                            Text(content: "Prompt:".to_string(), color: theme.success, weight: Weight::Bold)
                            View(flex_direction: FlexDirection::Column, padding_left: 2u32) {
                                Markdown(content: prompt)
                            }
                        }
                    }))
                    // CC `:692-709`: the `MessageComponent` mount per row. A
                    // row whose component renders null contributes no line —
                    // upstream's `:687-691` note — because the mount itself
                    // returns an empty element, not because anything filtered
                    // it here.
                    #(visible.into_iter().map(|row| match row {
                        AgentProgressRow::ToolUse { tool_name, input, status } => element! {
                            AssistantToolUseMessage(
                                tool_name: tool_name,
                                input: Some(input),
                                description: String::new(),
                                status: Some(status),
                                add_margin: false,
                                can_animate: false,
                                should_show_dot: Some(false),
                                verbose: verbose,
                                is_transcript_mode: false,
                                in_progress_tool_call_count: Some(nested_in_progress_count),
                            )
                        }.into_any(),
                        AgentProgressRow::Text(text) => element! {
                            AssistantTextMessage(
                                content: text,
                                add_margin: false,
                                should_show_dot: false,
                                verbose: verbose,
                            )
                        }.into_any(),
                    }))
                }
                #(hidden_line.map(|line| element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: line, dim: true, wrap: TextWrap::NoWrap)
                        CtrlOToExpand
                    }
                }))
            }
        }
    }
    .into_any()
}

/// CC's two `Initializing…` returns (`UI.tsx:532-538`, `:645-651`) — the same
/// `<MessageResponse height={1}><Text dimColor>` in both.
fn initializing_row() -> AnyElement<'static> {
    element! {
        MessageResponse(height: Some(1u32)) {
            Text(content: INITIALIZING_TEXT.to_string(), dim: true, wrap: TextWrap::NoWrap)
        }
    }
    .into_any()
}

/// Maps to: CC `tools/AgentTool/UI.tsx:723-763#renderToolUseRejectedMessage` —
/// replay the progress transcript, then `FallbackToolUseRejectedMessage`. The
/// ant-only `[ANT-ONLY] API calls:` block (`:748-754`) does not render on
/// external builds. CC ignores the parsed input entirely on this path.
pub(crate) fn render_tool_use_rejected_message_element(
    progress_messages: &[ToolUseProgressMessage],
    verbose: bool,
    is_transcript_mode: bool,
) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column) {
            AgentToolUseProgressMessage(
                progress_messages: progress_messages.to_vec(),
                verbose: verbose,
                is_transcript_mode: is_transcript_mode,
            )
            FallbackToolUseRejectedMessage
        }
    }
    .into_any()
}

/// Maps to: CC `tools/AgentTool/UI.tsx:765-789#renderToolUseErrorMessage` —
/// replay the progress transcript, then `FallbackToolUseErrorMessage` with the
/// row's result content and the same `verbose`.
pub(crate) fn render_tool_use_error_message_element(
    result: &str,
    progress_messages: &[ToolUseProgressMessage],
    verbose: bool,
    is_transcript_mode: bool,
) -> AnyElement<'static> {
    let result = result.to_string();
    element! {
        View(flex_direction: FlexDirection::Column) {
            AgentToolUseProgressMessage(
                progress_messages: progress_messages.to_vec(),
                verbose: verbose,
                is_transcript_mode: is_transcript_mode,
            )
            FallbackToolUseErrorMessage(
                result: Some(result),
                verbose: verbose,
            )
        }
    }
    .into_any()
}

/// The `Prompt:` label emitter for the line half of CC
/// `tools/AgentTool/UI.tsx:325+#renderToolResultMessage`. The label carries
/// `ToolRenderTone::Success` because upstream's `AgentPromptDisplay` title is
/// `<Text color="success" bold>` (`UI.tsx:225-243`) — the sole reason the
/// generic `status_tone` keeps a `Success` tone at all.
fn push_agent_prompt_lines(lines: &mut Vec<ToolRenderLine>, prompt: Option<&str>) {
    let Some(prompt) = prompt.filter(|prompt| !prompt.trim().is_empty()) else {
        return;
    };
    lines.push(ToolRenderLine::new("Prompt:", ToolRenderTone::Success));
    push_indented_nonempty_lines(lines, prompt);
}

/// The `Response:` label emitter for the line half of CC
/// `tools/AgentTool/UI.tsx:325+#renderToolResultMessage`; the `success`-toned
/// title is `AgentResponseDisplay`'s (`UI.tsx:245-263`).
fn push_agent_response_lines(lines: &mut Vec<ToolRenderLine>, content: &[String]) {
    let nonempty_content = content
        .iter()
        .filter(|block| !block.trim().is_empty())
        .collect::<Vec<_>>();
    if nonempty_content.is_empty() {
        return;
    }
    lines.push(ToolRenderLine::new("Response:", ToolRenderTone::Success));
    for (idx, block) in nonempty_content.iter().enumerate() {
        if idx > 0 {
            lines.push(ToolRenderLine::new(String::new(), ToolRenderTone::Normal));
        }
        push_indented_nonempty_lines(lines, block);
    }
}

/// The line-half stand-in for the `paddingLeft={2}` box both display blocks
/// wrap their Markdown in (CC `tools/AgentTool/UI.tsx:225-263`).
fn push_indented_nonempty_lines(lines: &mut Vec<ToolRenderLine>, text: &str) {
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        lines.push(ToolRenderLine::new(
            format!("  {}", line.trim_end()),
            ToolRenderTone::Normal,
        ));
    }
}

/// The `${totalToolUseCount} tool use(s)` leg of the completion summary both
/// halves of CC `tools/AgentTool/UI.tsx:325+#renderToolResultMessage` build.
fn plural_tool_uses(count: usize) -> String {
    if count == 1 {
        "1 tool use".to_string()
    } else {
        format!("{count} tool uses")
    }
}

#[cfg(test)]
mod collapsed_progress_tests {
    use super::*;
    use crate::types::message::ToolUseProgressMessage as Progress;
    use crate::types::message::{
        AssistantContent, AssistantMessage, RenderableMessage, RenderableMessageKind, TokenUsage,
    };
    use crate::utils::test_env::EnvVarGuard;

    /// One `agent_progress` payload the way the producer emits it: a
    /// normalized single-block message (AgentTool.tsx:1483-1506).
    fn agent_progress(message: RenderableMessage) -> Progress {
        Progress::AgentProgress {
            message: Box::new(message),
            // The loop literal's empty prompt (AgentTool.tsx:1500-1502).
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        }
    }

    /// The sync-launch payload variant that DOES carry the prompt —
    /// AgentTool.tsx:1084-1092 forwards it ONCE, on the first normalized user
    /// message; the loop path (:1502) always sends `prompt: ''`.
    fn agent_progress_with_prompt(message: RenderableMessage, prompt: &str) -> Progress {
        Progress::AgentProgress {
            message: Box::new(message),
            prompt: prompt.to_string(),
            agent_id: "agent-1".to_string(),
        }
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Progress {
        agent_progress(RenderableMessage::assistant_block(
            format!("assistant-{id}"),
            AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(id.to_string()),
                name: name.to_string(),
                input,
            }),
        ))
    }

    fn tool_result(id: &str) -> Progress {
        agent_progress(RenderableMessage::user_tool_result(
            format!("user-{id}"),
            id,
            "ok",
            false,
        ))
    }

    fn assistant_with_usage(uuid: &str, usage: Option<TokenUsage>) -> Progress {
        agent_progress(RenderableMessage {
            uuid: uuid.to_string(),
            kind: RenderableMessageKind::Assistant {
                message: AssistantMessage {
                    uuid: uuid.to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![AssistantContent::Text("working".to_string())],
                    model: None,
                    stop_reason: None,
                    usage,
                },
            },
        })
    }

    fn greps(count: usize) -> Vec<Progress> {
        (0..count)
            .map(|index| {
                tool_use(
                    &format!("t{index}"),
                    "Grep",
                    serde_json::json!({ "pattern": format!("p{index}") }),
                )
            })
            .collect()
    }

    /// The SGR pairs that open and close a bold / dim run. Asserting on these
    /// is the point: this renderer used to be a `Vec<String>` projection, and
    /// a string-equality test cannot see that CC dims `Initializing…`, bolds
    /// the condensed count, or gives each row its own component chrome.
    const BOLD_ON: &str = "\u{1b}[1m";
    const DIM_ON: &str = "\u{1b}[2m";

    /// Renders CC's `renderToolUseProgressMessage` through the real component
    /// and returns `(plain_lines, ansi)`.
    fn render(
        progress: &[Progress],
        verbose: bool,
        is_transcript_mode: bool,
        terminal_rows: Option<u16>,
        in_progress_tool_call_count: Option<usize>,
    ) -> (Vec<String>, String) {
        // Tests of emitted style spans require a color-capable terminal;
        // standalone nextest processes otherwise expose a non-TTY pipe.
        let _force = EnvVarGuard::set("FORCE_COLOR", "3");
        let _term = EnvVarGuard::set("TERM", "dumb");
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentToolUseProgressMessage(
                    progress_messages: progress.to_vec(),
                    verbose: verbose,
                    is_transcript_mode: is_transcript_mode,
                    terminal_rows: terminal_rows,
                    in_progress_tool_call_count: in_progress_tool_call_count,
                )
            }
        }
        .render(Some(80));
        let plain = canvas
            .to_string()
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        canvas.write_ansi(&mut bytes).expect("canvas ansi");
        (plain, String::from_utf8(bytes).expect("ansi is utf-8"))
    }

    fn rows(progress: &[Progress], verbose: bool, is_transcript_mode: bool) -> Vec<String> {
        render(progress, verbose, is_transcript_mode, None, None).0
    }

    /// G12 — CC `UI.tsx:610-612`: the display window slices on
    /// `isTranscriptMode`, NOT on `verbose`. The old auxiliary port used
    /// `verbose` as the axis; this pins the corrected one on both sides.
    #[test]
    fn the_window_axis_is_transcript_mode_not_verbose() {
        let progress = greps(5);

        // verbose does not widen the main-screen window: still the last 3
        // processed rows plus the hidden tool-use line (`:610-635`).
        let verbose_main = rows(&progress, true, false);
        assert_eq!(verbose_main.len(), 4, "rows={verbose_main:?}");
        assert!(
            verbose_main.iter().any(|row| row.contains("p4"))
                && !verbose_main.iter().any(|row| row.contains("p0")),
            "rows={verbose_main:?}"
        );
        assert!(
            verbose_main
                .last()
                .is_some_and(|row| row.contains("+2 more tool uses (ctrl+o to expand)")),
            "rows={verbose_main:?}"
        );

        // Transcript mode shows every processed row and hides nothing
        // (`:610-612`, `:618-620`).
        let transcript = rows(&progress, false, true);
        assert_eq!(transcript.len(), 5, "rows={transcript:?}");
        assert!(
            transcript.iter().any(|row| row.contains("p0")),
            "rows={transcript:?}"
        );
        assert!(
            !transcript.iter().any(|row| row.contains("more tool")),
            "rows={transcript:?}"
        );
    }

    /// K4-G1, the mount itself. CC `UI.tsx:692-709` mounts a whole
    /// `MessageComponent` per row, so a tool_use row is
    /// `AssistantToolUseMessage` (`Message.tsx:406-432`) with its BOLD facing
    /// name and, for AgentTool, its `userFacingNameBackgroundColor`
    /// (`AssistantToolUseMessage.tsx:94`, `:180-190`); a text row is
    /// `AssistantTextMessage` (`Message.tsx:433-442`), i.e. `<Markdown>`
    /// (`AssistantTextMessage.tsx:203-207`).
    ///
    /// The retired `Vec<String>` projection emitted one unstyled `Text` per
    /// row, so the emitted stream carried NO `ESC[1m` anywhere, printed
    /// `**auth.rs**` verbatim, and never reached a nested tool's own progress
    /// renderer. Every assertion here fails on that shape.
    ///
    /// Observed emission after the mount (80 columns, default theme):
    ///
    /// ```text
    /// ESC[0m ESC[38;2;153;153;153m "  ⎿ " ESC[39m ESC[1m "reviewer" ESC[22m "(audit parity)" … CRLF
    ///     ESC[2m "Initializing…" ESC[0m … CRLF
    ///     "Checked " ESC[1m "auth.rs" ESC[0m … CRLF
    /// ```
    #[test]
    fn each_row_mounts_its_component_instead_of_flattening_to_a_string() {
        let progress = vec![
            // A nested Agent tool use, so the mount is observable twice: the
            // row's own chrome AND the nested tool's progress renderer, which
            // only a real component mount can reach.
            tool_use(
                "t0",
                "Agent",
                serde_json::json!({
                    "description": "audit parity",
                    "prompt": "go",
                    "subagent_type": "reviewer",
                }),
            ),
            agent_progress(RenderableMessage::assistant_block(
                "assistant-md",
                AssistantContent::Text("Checked **auth.rs**".to_string()),
            )),
        ];
        let (plain, ansi) = render(&progress, false, false, None, None);

        // The first bold run opens on the facing name — CC `:186-190`
        // `<Text bold …>{userFacingToolName}</Text>`, from
        // `userFacingName(data)` (`UI.tsx:1000-1010`), not the wire name.
        let bold_at = ansi
            .find(BOLD_ON)
            .expect("the mounted row must bold its tool name");
        assert!(
            ansi[bold_at + BOLD_ON.len()..].starts_with("reviewer"),
            "the bold run must open on the facing name: {:?}",
            &ansi[bold_at..(bold_at + 60).min(ansi.len())]
        );

        // The nested Agent row runs its OWN progress renderer with the empty
        // `progressMessagesForMessage` CC hands it (`UI.tsx:698`), i.e. the dim
        // `Initializing…` (`:532-538`) — and inside the outer response, so it
        // draws no second `⎿` (`MessageResponse.tsx:12-15`).
        assert_eq!(plain[1], "    Initializing…", "rows={plain:?}");
        assert_eq!(
            plain.iter().filter(|row| row.contains('⎿')).count(),
            1,
            "a nested response must not add a gutter: {plain:?}"
        );
        let dim_at = ansi
            .find(DIM_ON)
            .expect("the nested Initializing… row must be dim");
        assert!(
            ansi[dim_at + DIM_ON.len()..].starts_with(INITIALIZING_TEXT),
            "the dim run must open on the nested text: {:?}",
            &ansi[dim_at..(dim_at + 40).min(ansi.len())]
        );

        // The text row went through Markdown: `**auth.rs**` is emphasis, not
        // literal asterisks. The string projection printed the raw source.
        assert!(
            plain.iter().any(|row| row.contains("auth.rs"))
                && !plain.iter().any(|row| row.contains("**")),
            "the text row must render as Markdown: {plain:?}"
        );
        let auth_bold = ansi
            .rfind(BOLD_ON)
            .expect("Markdown strong emphasis must be bold");
        assert!(
            ansi[auth_bold + BOLD_ON.len()..].starts_with("auth.rs"),
            "the second bold run is the Markdown emphasis: {:?}",
            &ansi[auth_bold..(auth_bold + 40).min(ansi.len())]
        );

        // CC `:702` `shouldShowDot={false}` — no `⏺`/`●` on a progress row.
        assert!(
            !plain
                .iter()
                .any(|row| row.contains('⏺') || row.contains('●')),
            "progress rows are mounted with shouldShowDot=false: {plain:?}"
        );
    }

    /// G12 — CC `UI.tsx:637-639` + `:668-672`: transcript mode renders the
    /// first payload's prompt as the `AgentPromptDisplay` block (title,
    /// indented body, `marginBottom={1}`) ahead of the rows; the main screen
    /// never does.
    #[test]
    fn the_transcript_prompt_block_renders_before_the_rows() {
        let progress = vec![
            agent_progress_with_prompt(
                RenderableMessage::assistant_block(
                    "assistant-t0",
                    AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("t0".to_string()),
                        name: "Grep".to_string(),
                        input: serde_json::json!({ "pattern": "needle" }),
                    }),
                ),
                "Inspect the auth flow",
            ),
            tool_use("t1", "Grep", serde_json::json!({ "pattern": "haystack" })),
        ];

        let (transcript, ansi) = render(&progress, false, true, None, None);
        assert_eq!(
            &transcript[..3],
            ["  ⎿ Prompt:", "      Inspect the auth flow", "",],
            "rows={transcript:?}"
        );
        assert!(
            transcript[3..].iter().any(|row| row.contains("needle")),
            "rows={transcript:?}"
        );
        // CC `AgentPromptDisplay` (`:235-237`) titles the block with
        // `<Text color="success" bold>`. The old `Vec<String>` projection could
        // only emit the word — this fails on that shape.
        let bold_at = ansi.find(BOLD_ON).expect("the Prompt: title must be bold");
        assert!(
            ansi[bold_at + BOLD_ON.len()..].starts_with("Prompt:"),
            "the bold run must open on the title: {:?}",
            &ansi[bold_at..bold_at + 40.min(ansi.len() - bold_at)]
        );

        let main = rows(&progress, false, false);
        assert!(
            !main.iter().any(|row| row.contains("Prompt:")),
            "rows={main:?}"
        );

        // CC `:637-639` reads `progressMessages[0]?.data.prompt` — only the
        // FIRST payload's prompt counts. JS-truthy: the loop literal's empty
        // prompt renders no block.
        let empty_prompt = rows(&greps(1), false, true);
        assert!(
            !empty_prompt.iter().any(|row| row.contains("Prompt:")),
            "rows={empty_prompt:?}"
        );
    }

    /// G12 — the all-gated edge. CC `:645-651` falls back to `Initializing…`
    /// only when `displayedMessages` is EMPTY; a window whose every mounted
    /// row renders null keeps the outer `MessageResponse`, i.e. a bare `⎿`
    /// gutter row (`:664-719`, `:687-691`). The old auxiliary port collapsed
    /// both cases into `Initializing…`.
    #[test]
    fn an_all_gated_window_renders_a_bare_gutter_row_not_initializing() {
        // ToolSearch is CC `userFacingName: () => ''` — the mounted component
        // renders null while the row still occupies the window.
        let gated = (0..3)
            .map(|index| {
                tool_use(
                    &format!("t{index}"),
                    "ToolSearch",
                    serde_json::json!({ "query": "x" }),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows(&gated, false, false),
            vec!["  ⎿".to_string()],
            "an all-gated window is a bare gutter, never Initializing…"
        );

        // An EMPTY window does fall back: user-only rows are dropped by the
        // external `processProgressMessages` filter (`:129-136`), leaving
        // `displayedMessages.length === 0` (`:645`).
        let user_only = vec![tool_result("t9")];
        let (initializing, ansi) = render(&user_only, false, false, None, None);
        assert_eq!(initializing, vec![format!("  ⎿ {INITIALIZING_TEXT}")]);
        // CC `:535` / `:648` `<Text dimColor>{INITIALIZING_TEXT}</Text>`. The
        // string projection emitted the bare word, so the dim run stopped at
        // the gutter and this assertion fails on that shape.
        let dim_at = ansi
            .rfind(DIM_ON)
            .expect("Initializing… must be dim, like CC's <Text dimColor>");
        assert!(
            ansi[dim_at + DIM_ON.len()..].starts_with(INITIALIZING_TEXT),
            "the dim run must open on the text: {:?}",
            &ansi[dim_at..]
        );

        // …and so does no progress at all (`:532-538`).
        assert_eq!(
            rows(&[], false, false),
            vec![format!("  ⎿ {INITIALIZING_TEXT}")]
        );

        // CC `:645` excepts `isTranscriptMode && prompt`: the empty window
        // still renders the prompt block instead of Initializing….
        let user_only_with_prompt = vec![agent_progress_with_prompt(
            RenderableMessage::user_tool_result("user-t9", "t9", "ok", false),
            "go",
        )];
        // The trailing blank row is the prompt Box's `marginBottom={1}`
        // (`UI.tsx:669`), which CC keeps even with no rows after it.
        assert_eq!(
            rows(&user_only_with_prompt, false, true),
            vec![
                "  ⎿ Prompt:".to_string(),
                "      go".to_string(),
                String::new()
            ]
        );
    }

    /// G5 — CC `UI.tsx:540-549`: condensed mode needs a live terminal whose
    /// row count is BELOW `(inProgressToolCallCount ?? 1) * 9 + 7`, and never
    /// runs in transcript mode. Every guard leg is JS-truthy, so `rows: 0`
    /// (the headless test default) disables it.
    #[test]
    fn condensed_mode_needs_a_small_live_terminal_outside_transcript() {
        let progress = greps(1);
        let condensed = |terminal_rows: Option<u16>, count: Option<usize>, transcript: bool| {
            render(&progress, false, transcript, terminal_rows, count).0[0].contains("In progress…")
        };

        // count None → `?? 1` → estimate 1*9+7 = 16.
        assert!(condensed(Some(15), None, false));
        assert!(!condensed(Some(16), None, false), "strictly `<`");
        assert!(!condensed(Some(0), None, false), "rows 0 is falsy");
        assert!(!condensed(None, None, false), "no terminalSize");
        assert!(!condensed(Some(5), None, true), "never in transcript mode");

        // count 3 → estimate 3*9+7 = 34.
        assert!(condensed(Some(33), Some(3), false));
        assert!(!condensed(Some(34), Some(3), false));
    }

    /// G5 — CC `UI.tsx:580-598`: the exact condensed copy. Singular/plural on
    /// the count, the tokens segment omitted when `tokens` is null OR 0
    /// (`:588` `{tokens && …}` is JS-truthy), and the configured expand hint
    /// with parens.
    #[test]
    fn condensed_line_matches_the_cc_copy() {
        // One pending tool use; its assistant row has no usage → tokens 0 →
        // segment omitted, singular "use".
        let (single, single_ansi) = render(&greps(1), false, false, Some(10), None);
        assert_eq!(
            single,
            vec!["  ⎿ In progress… · 1 tool use · (ctrl+o to expand)".to_string()]
        );
        // CC `:586` wraps ONLY the count in `<Text bold>`, inside the row's
        // `<Text dimColor>` — so the count is dim AND bold while its
        // neighbours are dim only. The old projection produced one flat
        // string: no `ESC[1m` anywhere, and this fails at the first assert.
        let bold_at = single_ansi
            .find(BOLD_ON)
            .expect("the tool-use count must be bold");
        assert!(
            single_ansi[bold_at + BOLD_ON.len()..].starts_with('1'),
            "the bold run must cover the count and nothing else: {:?}",
            &single_ansi[bold_at..]
        );
        assert!(
            single_ansi[..bold_at].contains(DIM_ON),
            "the row opens dim before the bold count: {single_ansi:?}"
        );

        // Two tool uses; the newest assistant row's four usage legs sum to
        // 1500 → formatNumber's `1.5k`.
        let mut progress = greps(2);
        progress.push(assistant_with_usage(
            "a1",
            Some(TokenUsage {
                input_tokens: 1_000,
                output_tokens: 400,
                cache_creation_input_tokens: 60,
                cache_read_input_tokens: 40,
                cache_deleted_input_tokens: 0,
            }),
        ));
        assert_eq!(
            render(&progress, false, false, Some(10), None).0,
            vec!["  ⎿ In progress… · 2 tool uses · 1.5k tokens · (ctrl+o to expand)".to_string()]
        );
    }

    /// The scout-verified trap: `getProgressStats` (`UI.tsx:551-578`) counts
    /// content carrying a `tool_use` block — the ASSISTANT half — while
    /// `calculateAgentStats` (`:791-822`) counts USER rows carrying a
    /// `tool_result`. The two disagree on pending calls and orphan results,
    /// so reusing one for the other over/under-counts.
    #[test]
    fn condensed_stats_count_assistant_tool_use_rows_not_results() {
        // A pending call: tool_use forwarded, result not yet.
        let pending = greps(1);
        assert_eq!(get_progress_stats(&pending).tool_use_count, 1);
        assert_eq!(calculate_agent_stats(&pending).tool_use_count, 0);

        // An orphan result: the inverse split.
        let orphan = vec![tool_result("t9")];
        assert_eq!(get_progress_stats(&orphan).tool_use_count, 0);
        assert_eq!(calculate_agent_stats(&orphan).tool_use_count, 1);

        // CC `:567-575`: tokens are null — not 0 — without an assistant row,
        // and `findLast` makes the newest assistant's usage win outright.
        assert_eq!(get_progress_stats(&orphan).tokens, None);
        let two_assistants = vec![
            assistant_with_usage(
                "a1",
                Some(TokenUsage {
                    input_tokens: 9_000,
                    output_tokens: 9_000,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    cache_deleted_input_tokens: 0,
                }),
            ),
            assistant_with_usage(
                "a2",
                Some(TokenUsage {
                    input_tokens: 100,
                    output_tokens: 27,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    cache_deleted_input_tokens: 0,
                }),
            ),
        ];
        assert_eq!(get_progress_stats(&two_assistants).tokens, Some(127));
    }
}

#[cfg(test)]
mod extract_last_tool_info_tests {
    use super::*;
    use crate::types::message::ToolUseProgressMessage as Progress;

    /// One `agent_progress` payload the way the producer emits it: a
    /// normalized single-block message (AgentTool.tsx:1483-1506).
    fn agent_progress(message: crate::types::message::RenderableMessage) -> Progress {
        Progress::AgentProgress {
            message: Box::new(message),
            // The loop literal's empty prompt (AgentTool.tsx:1500-1502).
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        }
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Progress {
        agent_progress(crate::types::message::RenderableMessage::assistant_block(
            format!("assistant-{id}"),
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(id.to_string()),
                name: name.to_string(),
                input,
            }),
        ))
    }

    fn tool_result(id: &str, _name: &str) -> Progress {
        agent_progress(crate::types::message::RenderableMessage::user_tool_result(
            format!("user-{id}"),
            id,
            "ok",
            false,
        ))
    }

    fn read(id: &str, path: &str) -> [Progress; 2] {
        [
            tool_use(id, "Read", serde_json::json!({ "file_path": path })),
            tool_result(id, "Read"),
        ]
    }

    fn grep(id: &str, pattern: &str) -> [Progress; 2] {
        [
            tool_use(id, "Grep", serde_json::json!({ "pattern": pattern })),
            tool_result(id, "Grep"),
        ]
    }

    /// CC's `tools` is the live main-loop pool (`REPL.tsx:1216`), and
    /// `findToolByName` reads `name` / `aliases` only (`Tool.ts:348-360`), so a
    /// fixture pool needs no more than the entries the row must match.
    fn pool(names: &[&str]) -> Vec<crate::types::tools::Tool> {
        names
            .iter()
            .map(|name| crate::types::tools::Tool {
                name: (*name).to_string(),
                ..Default::default()
            })
            .collect()
    }

    /// The pool every fixture below renders against unless it is testing the
    /// membership check itself.
    fn full_pool() -> Vec<crate::types::tools::Tool> {
        pool(&["Read", "Grep", "Bash", "TodoWrite"])
    }

    /// CC `:1067-1069`: the rollup needs `searchCount + readCount >= 2`, and
    /// `:1053-1060` counts ONLY tool_result rows.
    #[test]
    fn trailing_search_read_rollup_needs_two_results() {
        // One completed read is below the threshold — falls through to the
        // last-tool_result description instead of a rollup.
        let one = read("t1", "src/main.rs").to_vec();
        assert_eq!(
            extract_last_tool_info(&one, &full_pool()).as_deref(),
            Some("Read: src/main.rs")
        );

        // Two completed ops roll up, present tense with the trailing ellipsis.
        let mut two = grep("t1", "needle").to_vec();
        two.extend(read("t2", "src/main.rs"));
        assert_eq!(
            extract_last_tool_info(&two, &full_pool()).as_deref(),
            Some("Searching for 1 pattern, reading 1 file…")
        );

        // A trailing tool_use WITHOUT its result does not count (it is still
        // collapsible, so the scan continues past it, but only results
        // increment) — two results are still required.
        let mut one_and_pending = read("t1", "src/main.rs").to_vec();
        one_and_pending.push(tool_use(
            "t2",
            "Grep",
            serde_json::json!({ "pattern": "needle" }),
        ));
        assert_eq!(
            extract_last_tool_info(&one_and_pending, &full_pool()).as_deref(),
            Some("Read: src/main.rs")
        );
    }

    /// CC `:1062-1064`: a non-collapsible row ends the backwards scan, so
    /// earlier search/read ops are NOT folded in.
    #[test]
    fn a_non_collapsible_row_breaks_the_trailing_scan() {
        let mut messages = grep("t1", "needle").to_vec();
        messages.extend(read("t2", "src/main.rs"));
        messages.extend([
            tool_use("t3", "Bash", serde_json::json!({ "command": "cargo test" })),
            tool_result("t3", "Bash"),
        ]);
        // The Bash row breaks the scan; the description path runs instead.
        // BashTool DOES implement `getToolUseSummary` (BashTool.tsx:720-730),
        // so the row carries the truncated command. This assertion previously
        // pinned a bare `Bash`, from a grep that matched only the
        // object-literal shorthand form of the member.
        assert_eq!(
            extract_last_tool_info(&messages, &full_pool()).as_deref(),
            Some("Bash: cargo test")
        );

        // CC `BashTool.tsx:723-727`: a truthy `description` wins over the
        // command, and is NOT truncated.
        let described = vec![
            tool_use(
                "t1",
                "Bash",
                serde_json::json!({ "command": "cargo test", "description": "Run the suite" }),
            ),
            tool_result("t1", "Bash"),
        ];
        assert_eq!(
            extract_last_tool_info(&described, &full_pool()).as_deref(),
            Some("Bash: Run the suite")
        );
    }

    /// CC `:1109-1117`: `"{userFacingName}: {summary}"` when the tool
    /// implements `getToolUseSummary`, bare `userFacingName` otherwise.
    #[test]
    fn last_result_is_described_by_its_own_tool() {
        // Grep implements the member → name + summary. Note the name is CC's
        // user-facing "Search", not the wire name.
        let messages = grep("t1", "needle").to_vec();
        assert_eq!(
            extract_last_tool_info(&messages, &full_pool()).as_deref(),
            Some("Search: needle")
        );

        // TodoWrite implements neither the member nor a user-facing name
        // (CC `userFacingName: () => ''`), so CC yields the empty string here
        // rather than null — the caller renders an empty status, not
        // `Initializing…`.
        let messages = vec![
            tool_use("t1", "TodoWrite", serde_json::json!({ "todos": [] })),
            tool_result("t1", "TodoWrite"),
        ];
        assert_eq!(
            extract_last_tool_info(&messages, &full_pool()).as_deref(),
            Some("")
        );
    }

    /// CC `:1096-1098`:
    /// `const tool = findToolByName(tools, toolUseBlock.name)` /
    /// `if (!tool) { return toolUseBlock.name }`.
    ///
    /// The pool — not a global name table — decides. A Grep the current pool no
    /// longer carries renders as the RAW wire name "Grep" with no summary; the
    /// global facing name "Search" and the pattern summary are what the tool's
    /// own members would have produced, and they are unreachable without it.
    #[test]
    fn a_tool_missing_from_the_pool_falls_back_to_the_raw_wire_name() {
        let messages = grep("t1", "needle").to_vec();

        // Grep in the pool: the found tool's members drive both halves.
        assert_eq!(
            extract_last_tool_info(&messages, &pool(&["Grep"])).as_deref(),
            Some("Search: needle")
        );

        // Grep narrowed out of the pool: raw name, no summary. An empty pool
        // (a mount that was never threaded one) takes the same branch.
        assert_eq!(
            extract_last_tool_info(&messages, &pool(&["Read", "Bash"])).as_deref(),
            Some("Grep")
        );
        assert_eq!(
            extract_last_tool_info(&messages, &[]).as_deref(),
            Some("Grep")
        );
    }

    /// CC `toolMatchesName` (`Tool.ts:348-353`): `findToolByName` matches the
    /// primary name OR an alias, so a row recorded under a renamed tool's old
    /// wire name still resolves to the pool entry — and then reads that
    /// entry's members, not the alias's.
    #[test]
    fn the_pool_lookup_matches_aliases_like_official() {
        let messages = vec![
            tool_use("t1", "Search", serde_json::json!({ "pattern": "needle" })),
            tool_result("t1", "Search"),
        ];
        let aliased = vec![crate::types::tools::Tool {
            name: "Grep".to_string(),
            aliases: vec!["Search".to_string()],
            ..Default::default()
        }];
        assert_eq!(
            extract_last_tool_info(&messages, &aliased).as_deref(),
            Some("Search: needle")
        );
        // Without the alias the same row is a pool miss → raw wire name.
        assert_eq!(
            extract_last_tool_info(&messages, &pool(&["Grep"])).as_deref(),
            Some("Search")
        );
    }

    /// CC's `Tool` object carries the members; this port splits them onto a
    /// `ToolCall` singleton resolved by the FOUND pool entry's name. A pool
    /// entry with no registered behaviour — every MCP tool — therefore shows
    /// its own name and no summary, which is the default `buildTool` installs
    /// (`Tool.ts:789` `userFacingName: () => def.name`).
    #[test]
    fn a_pool_entry_without_registered_behavior_shows_its_own_name() {
        let messages = vec![
            tool_use(
                "t1",
                "mcp__server__lookup",
                serde_json::json!({ "query": "needle" }),
            ),
            tool_result("t1", "mcp__server__lookup"),
        ];
        assert_eq!(
            extract_last_tool_info(&messages, &pool(&["mcp__server__lookup"])).as_deref(),
            Some("mcp__server__lookup")
        );
    }

    /// CC `:1102-1114`: BOTH reads take `parsedInput.success ?
    /// parsedInput.data : undefined`, so an input the tool's own `inputSchema`
    /// rejects renders the bare `userFacingName` — CC never derives a summary
    /// from a payload it could not parse.
    #[test]
    fn malformed_input_drops_the_summary_and_keeps_the_facing_name() {
        // `output_mode` is a three-value enum (`GrepTool.ts` inputSchema), so
        // `bogus` fails safeParse even though `pattern` alone would summarise.
        let malformed = vec![
            tool_use(
                "t1",
                "Grep",
                serde_json::json!({ "pattern": "needle", "output_mode": "bogus" }),
            ),
            tool_result("t1", "Grep"),
        ];
        assert_eq!(
            extract_last_tool_info(&malformed, &full_pool()).as_deref(),
            Some("Search")
        );

        // Control: the same row without the rejected key keeps its summary.
        let well_formed = grep("t1", "needle").to_vec();
        assert_eq!(
            extract_last_tool_info(&well_formed, &full_pool()).as_deref(),
            Some("Search: needle")
        );
    }

    /// CC `:1092-1096`: the `if (toolUseBlock)` guard falls through to the
    /// final `return null` when the result's id resolves to nothing.
    #[test]
    fn unresolvable_result_id_yields_nothing() {
        let messages = vec![tool_result("orphan", "Read")];
        assert_eq!(extract_last_tool_info(&messages, &full_pool()), None);
        assert_eq!(extract_last_tool_info(&[], &full_pool()), None);
    }
}

#[cfg(test)]
mod grouped_agent_stat_tests {
    use super::*;
    use crate::types::message::ToolUseProgressMessage as Progress;
    use crate::types::message::{
        AssistantContent, AssistantMessage, RenderableMessage, RenderableMessageKind, TokenUsage,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn agent_progress(message: RenderableMessage) -> Progress {
        Progress::AgentProgress {
            message: Box::new(message),
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        }
    }

    fn nested_result(tool_use_id: &str) -> Progress {
        agent_progress(RenderableMessage::user_tool_result(
            format!("user-{tool_use_id}"),
            tool_use_id,
            "ok",
            false,
        ))
    }

    fn nested_assistant(uuid: &str, usage: Option<TokenUsage>) -> Progress {
        agent_progress(RenderableMessage {
            uuid: uuid.to_string(),
            kind: RenderableMessageKind::Assistant {
                message: AssistantMessage {
                    uuid: uuid.to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![AssistantContent::Text("working".to_string())],
                    model: None,
                    stop_reason: None,
                    usage,
                },
            },
        })
    }

    fn usage(input: u64, output: u64, cache_creation: u64, cache_read: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
            cache_deleted_input_tokens: 0,
        }
    }

    /// CC `:795-804`: only USER rows carrying a tool_result count, and a
    /// payload from another progress producer is skipped by `hasProgressMessage`.
    #[test]
    fn tool_use_count_counts_only_nested_tool_results() {
        let messages = vec![
            nested_assistant("a1", None),
            nested_result("t1"),
            nested_result("t2"),
            // A forwarded bash_progress has no `message` at all.
            Progress::QueryUpdate {
                query: "needle".to_string(),
            },
        ];
        assert_eq!(calculate_agent_stats(&messages).tool_use_count, 2);
        assert_eq!(calculate_agent_stats(&[]).tool_use_count, 0);
    }

    /// CC `:811-819`: `tokens` is `null` — not `0` — until an assistant row
    /// exists, which is what makes `AgentProgressLine` omit the segment.
    #[test]
    fn tokens_are_null_without_an_assistant_row() {
        assert_eq!(calculate_agent_stats(&[]).tokens, None);
        assert_eq!(
            calculate_agent_stats(&[nested_result("t1")]).tokens,
            None,
            "a tool_result row alone carries no usage"
        );
    }

    /// CC `:806-818` uses `findLast`, so the newest assistant's usage REPLACES
    /// the older one instead of accumulating, and all four legs are summed.
    #[test]
    fn tokens_come_from_the_last_assistant_only() {
        let messages = vec![
            nested_assistant("a1", Some(usage(9_000, 9_000, 9_000, 9_000))),
            nested_result("t1"),
            nested_assistant("a2", Some(usage(100, 20, 3, 4))),
        ];
        assert_eq!(calculate_agent_stats(&messages).tokens, Some(127));

        // CC `:813-818` sums EXACTLY four legs. `cache_deleted_input_tokens`
        // is a cache-editing counter (`claude.ts:2966-2981`), never a term of
        // this total.
        let with_deletions = nested_assistant(
            "a3",
            Some(TokenUsage {
                cache_deleted_input_tokens: 50_000,
                ..usage(100, 20, 3, 4)
            }),
        );
        assert_eq!(
            calculate_agent_stats(&[with_deletions]).tokens,
            Some(127),
            "deleted cache tokens are not one of CC's four legs"
        );
    }

    /// The Rust aggregate is optional where CC's `Usage` is required; a row
    /// without one still yields `Some`, because CC's null case is "no assistant
    /// row", not "no usage object".
    #[test]
    fn a_usage_less_assistant_row_still_yields_a_token_total() {
        assert_eq!(
            calculate_agent_stats(&[nested_assistant("a1", None)]).tokens,
            Some(0)
        );
    }

    /// No progress messages, so the pool half (`extractLastToolInfo`'s only
    /// consumer, `UI.tsx:847`) has nothing to resolve; every assertion below is
    /// on the input-derived legs.
    fn stat(input: serde_json::Value, output_status: Option<&str>) -> GroupedAgentStat {
        grouped_agent_stat(&input, output_status, &[], &[])
    }

    /// CC `:872-883` — the non-teammate branch: `userFacingName` for the type,
    /// the input description, and the type's background colour.
    #[test]
    fn default_branch_uses_user_facing_name_and_description() {
        let plain = stat(
            serde_json::json!({"description": "Inspect auth", "prompt": "go"}),
            None,
        );
        assert_eq!(plain.agent_type, "Agent");
        assert_eq!(plain.description.as_deref(), Some("Inspect auth"));
        assert_eq!(plain.task_description, None);
        assert!(!plain.is_async);

        let typed = stat(
            serde_json::json!({
                "description": "Inspect auth",
                "prompt": "go",
                "subagent_type": "reviewer",
            }),
            None,
        );
        assert_eq!(typed.agent_type, "reviewer");

        // CC `:1002-1007`: general-purpose and worker both display as 'Agent'.
        for subagent_type in ["general-purpose", "worker"] {
            let folded = stat(
                serde_json::json!({
                    "description": "Inspect auth",
                    "prompt": "go",
                    "subagent_type": subagent_type,
                }),
                None,
            );
            assert_eq!(folded.agent_type, "Agent", "subagent_type={subagent_type}");
        }
    }

    /// CC `:848` — `safeParse` fails without the two required fields, and the
    /// whole entry degrades to the untyped 'Agent' shape.
    #[test]
    fn a_malformed_input_degrades_to_the_untyped_agent_entry() {
        let broken = stat(serde_json::json!({"subagent_type": "reviewer"}), None);
        assert_eq!(broken.agent_type, "Agent");
        assert_eq!(broken.description, None);
        assert_eq!(broken.color, None);
        assert_eq!(broken.name, None);
    }

    /// CC `:861-871` — the teammate-spawn branch renames the type to `@name`,
    /// moves the CUSTOM subagent type into the description slot, and parks the
    /// task description on `taskDescription`.
    #[test]
    fn teammate_spawn_branch_uses_at_name_and_custom_type() {
        let spawned = stat(
            serde_json::json!({
                "description": "Run tests",
                "prompt": "go",
                "subagent_type": "reviewer",
                "name": "runner",
            }),
            Some("teammate_spawned"),
        );
        assert_eq!(spawned.agent_type, "@runner");
        assert_eq!(spawned.description.as_deref(), Some("reviewer"));
        assert_eq!(spawned.task_description.as_deref(), Some("Run tests"));
        assert_eq!(spawned.name.as_deref(), Some("runner"));
        assert!(spawned.is_async);

        // A non-custom type leaves the description slot empty (`:864-866`).
        let worker = stat(
            serde_json::json!({
                "description": "Run tests",
                "prompt": "go",
                "subagent_type": "worker",
                "name": "runner",
            }),
            Some("teammate_spawned"),
        );
        assert_eq!(worker.agent_type, "@runner");
        assert_eq!(worker.description, None);

        // CC `:861` guards on a TRUTHY name, so an empty one takes the else
        // branch — where the type is `userFacingName`, not `@`.
        let unnamed = stat(
            serde_json::json!({
                "description": "Run tests",
                "prompt": "go",
                "subagent_type": "reviewer",
                "name": "",
            }),
            Some("teammate_spawned"),
        );
        assert_eq!(unnamed.agent_type, "reviewer");
        assert_eq!(unnamed.description.as_deref(), Some("Run tests"));
        assert!(unnamed.is_async, "the spawn status still forces async");
    }

    /// CC `:1128-1136#isCustomSubagentType` opens with `!!subagentType`, so the
    /// empty string is NOT custom — it must behave like `worker`, not like a
    /// one-character type name. `grouped_agent_stat`'s teammate branch is the
    /// only reader (`:864-871`), where a non-custom type leaves BOTH the
    /// description slot and its colour empty.
    #[test]
    fn an_empty_subagent_type_is_not_a_custom_type() {
        assert!(!is_custom_subagent_type(""));
        assert!(!is_custom_subagent_type("general-purpose"));
        assert!(!is_custom_subagent_type("worker"));
        assert!(is_custom_subagent_type("reviewer"));
        // Case-sensitive, like every `!==` in the source.
        assert!(is_custom_subagent_type("General-Purpose"));
        assert!(is_custom_subagent_type("Worker"));

        let empty_type = stat(
            serde_json::json!({
                "description": "Run tests",
                "prompt": "go",
                "subagent_type": "",
                "name": "runner",
            }),
            Some("teammate_spawned"),
        );
        assert_eq!(empty_type.agent_type, "@runner");
        assert_eq!(empty_type.description, None);
        assert_eq!(empty_type.description_color, None);
    }

    /// CC `:886-895` — three independent legs feed `isAsync`.
    ///
    /// The `run_in_background` leg is dead under the fork gate, and CC wrote it
    /// to be: `'run_in_background' in parsedInput.data` (`:888`) is a PRESENCE
    /// check on the parsed data, and `inputSchema()` omitted the property
    /// (`AgentTool.tsx:252-254`), so zod strips whatever the model sent and the
    /// leg cannot fire. Nothing is lost — `forceAsync` (`:812`) routed that
    /// spawn through the async path anyway, and `async_launched` reports it.
    #[test]
    fn is_async_folds_the_input_flag_and_both_launch_statuses() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _fork_gate = super::super::fork_subagent::fork_gate_environment();
        let base = serde_json::json!({"description": "Run lint", "prompt": "go"});
        assert!(!stat(base.clone(), None).is_async);
        assert!(!stat(base.clone(), Some("completed")).is_async);
        assert!(stat(base.clone(), Some("async_launched")).is_async);
        assert!(stat(base.clone(), Some("remote_launched")).is_async);

        let mut backgrounded = base.clone();
        backgrounded["run_in_background"] = serde_json::json!(true);
        assert!(
            !stat(backgrounded, None).is_async,
            "the omitted property is stripped before `:888` can see it"
        );

        // `=== true` — a truthy non-boolean would fail `safeParse` outright.
        let mut explicit_false = base;
        explicit_false["run_in_background"] = serde_json::json!(false);
        assert!(!stat(explicit_false, None).is_async);
    }

    /// The same leg with the schema's omit lifted, which is the shape CC ships
    /// to every session the fork gate vetoes (`forkSubagent.ts:34-35`). Without
    /// it the assertion above would also hold for a renderer that dropped
    /// `launchedAsAsync` entirely.
    ///
    /// Its own test because `input_schema()` memoizes like CC's `lazySchema`,
    /// so one process observes one side of the gate — which is what nextest
    /// gives each test.
    #[test]
    fn a_vetoed_fork_gate_lets_the_input_flag_report_async() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _fork_vetoed = super::super::fork_subagent::fork_veto_environment();
        let _background = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        let base = serde_json::json!({"description": "Run lint", "prompt": "go"});

        let mut backgrounded = base.clone();
        backgrounded["run_in_background"] = serde_json::json!(true);
        assert!(stat(backgrounded, None).is_async);

        let mut explicit_false = base;
        explicit_false["run_in_background"] = serde_json::json!(false);
        assert!(!stat(explicit_false, None).is_async);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::RenderableMessage;

    /// One `agent_progress` payload the way the producer emits it: a
    /// normalized single-block message (`AgentTool.tsx:1483-1506`).
    fn agent_progress(message: RenderableMessage) -> ToolUseProgressMessage {
        ToolUseProgressMessage::AgentProgress {
            message: Box::new(message),
            // The loop literal's empty prompt (`AgentTool.tsx:1500-1502`).
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        }
    }

    /// The row must carry the tool's INPUT, not a guessed string, for tools the
    /// old four-key sniffer could not describe.
    ///
    /// `subagent_tool_use_description` looked at `description`, `command`,
    /// `file_path` and `query`, then fell back to `serde_json::to_string(input)`.
    /// `Grep` takes `pattern`/`path` and `Skill` takes `skill`/`args`, so both
    /// hit the fallback and the expanded agent transcript printed
    /// `Grep({"pattern":"**/*.md","path":"…"})` — observed in a live run.
    ///
    /// CC never had that failure mode: `VerboseAgentTranscript`
    /// (`tools/AgentTool/UI.tsx:299-320`) mounts the full `MessageComponent`
    /// with `tools`, so the nested row resolves the owning tool's own
    /// `renderToolUseMessage` exactly as a top-level row does.
    ///
    /// The existing `Read` case
    /// (`user_tool_result_renders_agent_verbose_progress_between_prompt_and_response`,
    /// below in this module) could not catch this: `Read` has a `file_path`, so
    /// the sniffer's third branch happened to be right.
    ///
    /// The names here are WIRE names, which is the only kind a `ToolUseBlock`
    /// carries. An earlier version of this test passed `"Search"` — Grep's
    /// user-facing name (`GrepTool.ts:169-171`), which no tool answers to — so
    /// every lookup downstream missed and the test proved only that the enum
    /// carried its argument.
    #[test]
    fn nested_tool_use_row_carries_input_for_tools_the_old_sniffer_missed() {
        for (tool_name, input) in [
            (
                "Grep",
                serde_json::json!({ "pattern": "**/*.md", "path": "/repo" }),
            ),
            (
                "Skill",
                serde_json::json!({ "skill": "audit", "args": "src" }),
            ),
        ] {
            let tool_use = agent_progress(RenderableMessage::assistant_blocks(
                "nested-sniffer-miss",
                vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_sniffer_miss".to_string()),
                        name: tool_name.to_string(),
                        input: input.clone(),
                    },
                )],
            ));
            let rows = agent_verbose_progress_rows(&[tool_use]);
            let [AgentVerboseProgressRow::ToolUse { input: carried, .. }] = rows.as_slice() else {
                panic!("expected one tool-use row for {tool_name}, got {rows:?}");
            };
            assert_eq!(
                carried, &input,
                "{tool_name}: the row must carry CC's `param.input` so the tool's \
                 own renderer can run; a pre-rendered string here can only be guessed"
            );
        }
    }

    /// CC `AgentTool/UI.tsx:288-299` — `VerboseAgentTranscript` drops a user
    /// progress row that carries no `toolUseResult` ("Subagent progress
    /// messages don't carry the parsed tool output, so UserToolSuccessMessage
    /// returns null and MessageResponse renders a bare ⎿"). The message itself
    /// stays in the progress list; only this row filter removes it, which is
    /// why the converter no longer drops it upstream.
    #[test]
    fn verbose_transcript_drops_user_rows_without_a_preserved_tool_use_result() {
        let tool_use = agent_progress(RenderableMessage::assistant_block(
            "nested-auth-tool-use",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_nested_auth".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({ "file_path": "src/auth.rs" }),
            }),
        ));
        let unpreserved = agent_progress(RenderableMessage::user_tool_result(
            "nested-auth-tool-result",
            "toolu_nested_auth",
            "Read 12 lines",
            false,
        ));
        let rows = agent_verbose_progress_rows(&[tool_use.clone(), unpreserved]);
        assert_eq!(
            rows,
            vec![AgentVerboseProgressRow::ToolUse {
                tool_name: "Read".to_string(),
                // The row carries CC's `param.input`, not a pre-rendered string:
                // `VerboseAgentTranscript` (`tools/AgentTool/UI.tsx:299-320`)
                // mounts the full `MessageComponent`, so the nested row resolves
                // the owning tool's own `renderToolUseMessage`. This assertion
                // used to read `description: "src/auth.rs"` — the output of a
                // four-key sniffing chain that happened to have a `file_path`
                // branch. See `nested_tool_use_row_carries_input_for_tools_the_old_sniffer_missed`.
                input: serde_json::json!({ "file_path": "src/auth.rs" }),
                // Unresolved by any tool_result row that survives the filter,
                // but CC's `inProgressToolUseIDs` is computed BEFORE the
                // filter, over the raw list — the result is present there.
                status: ToolUseStatus::Succeeded,
            }]
        );

        let preserved = agent_progress(
            RenderableMessage::user_tool_result(
                "nested-auth-tool-result",
                "toolu_nested_auth",
                "Read 12 lines",
                false,
            )
            .with_tool_use_result(Some(serde_json::json!({
                "type": "text",
                "file": {
                    "filePath": "src/auth.rs",
                    "content": "",
                    "numLines": 12,
                    "startLine": 1,
                    "totalLines": 12
                }
            }))),
        );
        let rows = agent_verbose_progress_rows(&[tool_use, preserved]);
        assert_eq!(rows.len(), 2);
        assert!(matches!(
            &rows[1],
            AgentVerboseProgressRow::ToolResult {
                tool_name,
                status: ToolResultStatus::Success,
                tool_use_result: Some(raw),
                ..
            } if tool_name == "Read" && raw["file"]["numLines"] == serde_json::json!(12)
        ));
    }

    #[test]
    fn verbose_transcript_omits_ordinary_results_without_tool_use_results() {
        assert!(
            verbose_agent_transcript_tool_result_projection("Read", true, "ok", None).is_none()
        );
        assert!(
            verbose_agent_transcript_tool_result_projection("Read", false, "failed", None)
                .is_none()
        );
    }

    #[test]
    fn verbose_transcript_retains_preserved_and_malformed_raw_tool_use_results() {
        let malformed = serde_json::json!({"type": "text", "file": {"filePath": "x"}});
        let read = verbose_agent_transcript_tool_result_projection(
            "Read",
            true,
            "raw nested payload",
            Some(&malformed),
        )
        .expect("preserved Read toolUseResult projects a result row");
        assert_eq!(read.status, ToolResultStatus::Success);

        let raw = serde_json::json!({"stdout": "boom", "stderr": ""});
        let error =
            verbose_agent_transcript_tool_result_projection("Bash", false, "boom", Some(&raw))
                .expect("preserved error toolUseResult projects a result row");
        assert_eq!(error.status, ToolResultStatus::Error);
    }

    // ─── renderToolResultMessage (both halves) ───────────────────────────
    // These drive [`render_tool_result_lines`] and [`render_tool_result_message`]
    // through their real entry points: the by-tool-name dispatch table, and the
    // mounted `UserToolResultMessage` component. They lived in
    // `components/messages/user_tool_result_message/mod.rs` until the renderer
    // bodies moved here.

    fn agent_completed_raw(tool_uses: u64, duration_ms: u64, tokens: u64) -> serde_json::Value {
        serde_json::json!({
            "status": "completed",
            "prompt": "Inspect auth",
            "agentId": "agent-1",
            "content": [{"type": "text", "text": "All good"}],
            "totalToolUseCount": tool_uses,
            "totalDurationMs": duration_ms,
            "totalTokens": tokens,
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": null,
                "cache_read_input_tokens": null,
                "server_tool_use": null,
                "service_tier": null,
                "cache_creation": null,
            },
        })
    }

    #[test]
    fn user_tool_result_renders_agent_result_rows_like_official_tool_ui() {
        use crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result;

        // The raw `toolUseResult` on the row drives the renderer.
        let completed_raw = agent_completed_raw(3, 4_200, 12_500);
        let completed = render_tool_result_lines_for_result(
            "Agent",
            ToolResultStatus::Success,
            "",
            Some(&completed_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(completed[0].text, "Done (3 tool uses · 12.5k tokens · 4s)");
        assert_eq!(completed[1].text, "  (ctrl+o to expand)");

        let transcript_raw = agent_completed_raw(1, 1_500, 900);
        let transcript_completed = render_tool_result_lines_for_result(
            "Agent",
            ToolResultStatus::Success,
            "",
            Some(&transcript_raw),
            None,
            &[],
            ToolRenderOptions {
                is_transcript_mode: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(transcript_completed[0].text, "Prompt:");
        assert_eq!(transcript_completed[0].tone, ToolRenderTone::Success);
        assert_eq!(transcript_completed[1].text, "  Inspect auth");
        assert_eq!(transcript_completed[2].text, "Response:");
        assert_eq!(transcript_completed[2].tone, ToolRenderTone::Success);
        assert_eq!(transcript_completed[3].text, "  All good");
        assert_eq!(
            transcript_completed[4].text,
            "Done (1 tool use · 900 tokens · 1s)"
        );

        // A completed raw without the schema-required usage fails safeParse
        // and renders nothing — the fate CC gives pre-usage transcripts.
        let mut no_usage = agent_completed_raw(1, 1, 1);
        no_usage.as_object_mut().unwrap().remove("usage");
        assert!(
            render_tool_result_lines_for_result(
                "Agent",
                ToolResultStatus::Success,
                "",
                Some(&no_usage),
                None,
                &[],
                ToolRenderOptions::default(),
            )
            .is_empty()
        );

        let async_raw = serde_json::json!({
            "status": "async_launched",
            "agentId": "agent-2",
            "description": "Run tests",
            "prompt": "Run tests",
            "outputFile": "/tmp/agent-2.jsonl",
        });
        let async_launched = render_tool_result_lines_for_result(
            "Task",
            ToolResultStatus::Success,
            "",
            Some(&async_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            async_launched[0].text,
            "Backgrounded agent (↓ to manage · ctrl+o to expand)"
        );
        let async_transcript = render_tool_result_lines_for_result(
            "Agent",
            ToolResultStatus::Success,
            "",
            Some(&async_raw),
            None,
            &[],
            ToolRenderOptions {
                is_transcript_mode: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(async_transcript[0].text, "Backgrounded agent");
        assert_eq!(async_transcript[1].text, "Prompt:");
        assert_eq!(async_transcript[2].text, "  Run tests");

        let remote_raw = serde_json::json!({
            "status": "remote_launched",
            "taskId": "task-remote",
            "sessionUrl": "https://example.test/session",
        });
        let remote = render_tool_result_lines_for_result(
            "Agent",
            ToolResultStatus::Success,
            "",
            Some(&remote_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            remote[0].text,
            "Remote agent launched · task-remote · https://example.test/session"
        );
    }

    #[test]
    fn user_tool_result_renders_agent_transcript_prompt_response_as_markdown_blocks() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Agent".to_string(),
                    is_error: false,
                    content: String::new(),                    // The raw `toolUseResult` drives the transcript.
                    tool_use_result: Some(serde_json::json!({
                        "status": "completed",
                        "prompt": "## Plan\n\n- Inspect auth",
                        "agentId": "agent-1",
                        "content": [{"type": "text", "text": "# Result\n\n**Done**"}],
                        "totalToolUseCount": 1,
                        "totalDurationMs": 1_500,
                        "totalTokens": 900,
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "cache_creation_input_tokens": null,
                            "cache_read_input_tokens": null,
                            "server_tool_use": null,
                            "service_tier": null,
                            "cache_creation": null,
                        },
                    })),
                    verbose: true,
                    is_transcript_mode: true,
                )
            }
        }
        .render(None);
        let text = canvas.to_string();

        assert!(text.contains("⎿ Prompt:"), "canvas=\n{text}");
        assert!(text.contains("Plan"), "canvas=\n{text}");
        assert!(text.contains("Inspect auth"), "canvas=\n{text}");
        assert!(text.contains("⎿ Response:"), "canvas=\n{text}");
        assert!(text.contains("Result"), "canvas=\n{text}");
        assert!(text.contains("Done"), "canvas=\n{text}");
        assert!(
            text.contains("⎿ Done (1 tool use · 900 tokens · 1s)"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("## Plan"), "canvas=\n{text}");
        assert!(!text.contains("# Result"), "canvas=\n{text}");
        assert!(!text.contains("**Done**"), "canvas=\n{text}");
    }

    #[test]
    fn user_tool_result_renders_agent_verbose_progress_between_prompt_and_response() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Agent".to_string(),
                    is_error: false,
                    content: String::new(),                    // The raw `toolUseResult` drives the transcript;
                    // progress arrives through the component props channel.
                    tool_use_result: Some(serde_json::json!({
                        "status": "completed",
                        "prompt": "Inspect auth",
                        "agentId": "agent-1",
                        "content": [{"type": "text", "text": "All good"}],
                        "totalToolUseCount": 2,
                        "totalDurationMs": 2_000,
                        "totalTokens": 1_000,
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "cache_creation_input_tokens": null,
                            "cache_read_input_tokens": null,
                            "server_tool_use": null,
                            "service_tier": null,
                            "cache_creation": null,
                        },
                    })),
                    progress_messages: vec![
                            agent_progress(RenderableMessage::assistant_block(
                                "nested-progress-text",
                                crate::types::message::AssistantContent::Text(
                                    "### Progress\n\nReading auth files".to_string(),
                                ),
                            )),
                            agent_progress(RenderableMessage::assistant_block(
                                "nested-auth-tool-use",
                                crate::types::message::AssistantContent::ToolUse(
                                    crate::types::message::ToolUseBlock {
                                        id: crate::types::ids::ToolUseId(
                                            "toolu_nested_auth".to_string(),
                                        ),
                                        name: "Read".to_string(),
                                        input: serde_json::json!({ "file_path": "src/auth.rs" }),
                                    },
                                ),
                            )),
                            agent_progress(
                                RenderableMessage::user_tool_result(
                                    "nested-auth-tool-result",
                                    "toolu_nested_auth",
                                    "raw nested read payload should not leak",
                                    false,
                                )
                                // The raw rides the message's tool_result block.
                                .with_tool_use_result(Some(serde_json::json!({
                                    "type": "text",
                                    "file": {
                                        "filePath": "src/auth.rs",
                                        "content": "",
                                        "numLines": 12,
                                        "startLine": 1,
                                        "totalLines": 12
                                    }
                                }))),
                            ),
                            // The old `SubagentTranscriptMessage` replay seam:
                            // `AgentProgress` carries the whole
                            // `RenderableMessage` itself, so a recorded row is
                            // just another progress payload.
                            agent_progress(RenderableMessage::assistant_block(
                                "nested-transparent-replay",
                                crate::types::message::AssistantContent::Text(
                                    "Nested VM progress".to_string(),
                                ),
                            )),
                        ],
                    verbose: true,
                    is_transcript_mode: true,
                )
            }
        }
        .render(None);
        let text = canvas.to_string();

        let prompt_idx = text.find("Prompt:").expect("prompt should render");
        let progress_idx = text.find("Progress").expect("progress should render");
        let tool_idx = text
            .find("Read(src/auth.rs)")
            .expect("tool row should render");
        let result_idx = text
            .find("Read 12 lines")
            .expect("tool result should render through the nested display contract");
        let replay_idx = text
            .find("Nested VM progress")
            .expect("recorded subagent row should render");
        let response_idx = text.find("Response:").expect("response should render");
        assert!(prompt_idx < progress_idx, "canvas=\n{text}");
        assert!(progress_idx < tool_idx, "canvas=\n{text}");
        assert!(tool_idx < result_idx, "canvas=\n{text}");
        assert!(result_idx < replay_idx, "canvas=\n{text}");
        assert!(replay_idx < response_idx, "canvas=\n{text}");
        // CC `UI.tsx:304` clamps every transcript row to one line
        // (`MessageResponse height={1}` + `overflowY="hidden"`), so only the
        // first rendered line of the two-paragraph progress text survives.
        // This assertion read `contains("Reading auth files")` before the
        // clamp was ported.
        assert!(!text.contains("Reading auth files"), "canvas=\n{text}");
        assert!(!text.contains("### Progress"), "canvas=\n{text}");
        assert!(
            !text.contains("raw nested read payload should not leak"),
            "nested typed tool-result display should own the visible row; canvas=\n{text}"
        );
        assert!(
            text.contains("⎿ Done (2 tool uses · 1.0k tokens · 2s)"),
            "canvas=\n{text}"
        );
    }

    /// CC `UI.tsx:304` wraps every verbose transcript row in
    /// `<MessageResponse height={1}>`, and `MessageResponse.tsx:18` clamps
    /// with `overflowY="hidden"` — a multi-line row shows exactly one line.
    #[test]
    fn verbose_transcript_rows_clamp_to_one_line() {
        let progress = vec![agent_progress(RenderableMessage::assistant_block(
            "multi-line-text",
            crate::types::message::AssistantContent::Text(
                "first paragraph\n\nsecond paragraph".to_string(),
            ),
        ))];
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentVerboseTranscriptBlock(progress_messages: progress, verbose: true)
            }
        }
        .render(Some(60));
        let text = canvas.to_string();

        assert!(text.contains("first paragraph"), "canvas=\n{text}");
        assert!(
            !text.contains("second paragraph"),
            "the height=1 overflow-hidden clamp must hide every later line; canvas=\n{text}"
        );
        assert_eq!(
            text.trim_end_matches('\n').lines().count(),
            1,
            "canvas=\n{text}"
        );
    }

    /// CC `UI.tsx:311` threads `verbose={verbose}` into every mounted row —
    /// observable through BashTool's `renderToolUseMessage`, whose
    /// `show_full()` gate keeps the whole command only when verbose and
    /// truncates it otherwise (`bash_tool/ui.rs:43-61`).
    #[test]
    fn verbose_transcript_threads_verbose_into_nested_rows() {
        let command = format!("echo {} && echo END_MARKER", "x".repeat(400));
        let progress = || {
            vec![agent_progress(RenderableMessage::assistant_block(
                "nested-bash",
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_verbose_thread".to_string()),
                        name: "Bash".to_string(),
                        input: serde_json::json!({ "command": command.as_str() }),
                    },
                ),
            ))]
        };

        let collapsed = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentVerboseTranscriptBlock(progress_messages: progress(), verbose: false)
            }
        }
        .render(None)
        .to_string();
        assert!(
            !collapsed.contains("END_MARKER"),
            "verbose=false must keep the truncated command; canvas=\n{collapsed}"
        );

        let verbose = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentVerboseTranscriptBlock(progress_messages: progress(), verbose: true)
            }
        }
        .render(None)
        .to_string();
        assert!(
            verbose.contains("END_MARKER"),
            "verbose=true must thread into the nested renderer; canvas=\n{verbose}"
        );
    }

    /// CC `UI.tsx:256-259`: `AgentResponseDisplay` maps every content block —
    /// no trim, no empty filter — so `["", "hello"]` renders an empty box and
    /// the second block's `marginTop={1}` leaves a 1-row gap.
    #[test]
    fn agent_response_display_renders_empty_blocks_as_gap() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentTranscriptMarkdownBlock(
                    title: "Response:".to_string(),
                    blocks: vec![String::new(), "hello".to_string()],
                )
            }
        }
        .render(Some(40));
        let text = canvas.to_string();
        let lines = text.trim_end_matches('\n').lines().collect::<Vec<_>>();

        assert!(lines[0].contains("Response:"), "canvas=\n{text}");
        assert_eq!(lines.len(), 3, "canvas=\n{text}");
        assert!(
            lines[1].trim().is_empty(),
            "the empty block plus the successor's marginTop is a 1-row gap; canvas=\n{text}"
        );
        assert!(lines[2].contains("hello"), "canvas=\n{text}");

        // Without the leading empty block there is no gap row.
        let solo = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentTranscriptMarkdownBlock(
                    title: "Response:".to_string(),
                    blocks: vec!["hello".to_string()],
                )
            }
        }
        .render(Some(40));
        let solo_text = solo.to_string();
        assert_eq!(
            solo_text.trim_end_matches('\n').lines().count(),
            2,
            "canvas=\n{solo_text}"
        );
    }

    /// CC dims the `· {taskId} · {sessionUrl}` tail of remote_launched
    /// (`UI.tsx:348-352`) and the whole ` (↓ to manage · ctrl+o to expand)`
    /// parenthetical of async_launched (`:364-380`). The line half carries the
    /// split on `segments`; `text` keeps the full copy for plain-text readers.
    #[test]
    fn remote_and_async_lines_carry_dim_segments() {
        use crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result;

        let remote_raw = serde_json::json!({
            "status": "remote_launched",
            "taskId": "task-remote",
            "sessionUrl": "https://example.test/session",
        });
        let remote = render_tool_result_lines_for_result(
            "Agent",
            ToolResultStatus::Success,
            "",
            Some(&remote_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            remote[0].text,
            "Remote agent launched · task-remote · https://example.test/session"
        );
        assert_eq!(
            remote[0].segments,
            vec![
                ToolRenderSegment::new("Remote agent launched "),
                ToolRenderSegment::new("· task-remote · https://example.test/session")
                    .with_dim(true),
            ]
        );

        let async_raw = serde_json::json!({
            "status": "async_launched",
            "agentId": "agent-2",
            "description": "Run tests",
            "prompt": "Run tests",
            "outputFile": "/tmp/agent-2.jsonl",
        });
        let async_launched = render_tool_result_lines_for_result(
            "Agent",
            ToolResultStatus::Success,
            "",
            Some(&async_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            async_launched[0].segments,
            vec![
                ToolRenderSegment::new("Backgrounded agent"),
                ToolRenderSegment::new(" (↓ to manage · ctrl+o to expand)").with_dim(true),
            ]
        );

        // Transcript mode renders no parenthetical, so no segments either.
        let async_transcript = render_tool_result_lines_for_result(
            "Agent",
            ToolResultStatus::Success,
            "",
            Some(&async_raw),
            None,
            &[],
            ToolRenderOptions {
                is_transcript_mode: true,
                ..ToolRenderOptions::default()
            },
        );
        assert!(async_transcript[0].segments.is_empty());
    }

    /// CC `UI.tsx:765-789#renderToolUseErrorMessage`: replay the progress
    /// transcript through `renderToolUseProgressMessage`, THEN the fallback.
    /// The window keeps the last 3 processed rows and rolls the earlier tool
    /// uses into `+N more tool uses <CtrlOToExpand/>` (`:610-635`, `:712-717`).
    #[test]
    fn agent_error_result_replays_progress_before_fallback() {
        let read_use = |index: usize| {
            agent_progress(RenderableMessage::assistant_block(
                format!("nested-read-{index}"),
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId(format!("toolu_err_{index}")),
                        name: "Read".to_string(),
                        input: serde_json::json!({ "file_path": format!("src/f{index}.rs") }),
                    },
                ),
            ))
        };
        let progress = (1..=6).map(read_use).collect::<Vec<_>>();

        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Agent".to_string(),
                    is_error: true,
                    content: "<tool_use_error>agent exploded</tool_use_error>".to_string(),
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);
        let text = canvas.to_string();

        // Last three rows visible, earlier ones rolled up. Each replay row is
        // now the mounted `AssistantToolUseMessage` (K4-G1), so the shape is
        // CC's own two adjacent boxes — `<Text bold>{name}</Text>` then
        // `<Text>({summary})</Text>` (`AssistantToolUseMessage.tsx:180-194`) —
        // with NO separating space. The retired string projection interpolated
        // `format!("{display_name} ({description})")`, so this test used to
        // read `Read (src/f4.rs)`: a space CC never emits.
        assert!(text.contains("Read(src/f4.rs)"), "canvas=\n{text}");
        assert!(text.contains("Read(src/f6.rs)"), "canvas=\n{text}");
        assert!(!text.contains("Read(src/f1.rs)"), "canvas=\n{text}");
        assert!(
            text.contains("+3 more tool uses (ctrl+o to expand)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Error: agent exploded"), "canvas=\n{text}");
        let replay_idx = text.find("Read(src/f4.rs)").expect("replay renders");
        let error_idx = text
            .find("Error: agent exploded")
            .expect("fallback renders");
        assert!(
            replay_idx < error_idx,
            "the replay must precede the fallback; canvas=\n{text}"
        );
    }

    /// CC `UI.tsx:723-763#renderToolUseRejectedMessage`: the same replay, then
    /// `FallbackToolUseRejectedMessage`; with no progress at all the replay is
    /// the `Initializing…` row (`:532-538`).
    #[test]
    fn agent_rejected_result_replays_progress_before_interrupt() {
        let progress = vec![
            agent_progress(RenderableMessage::assistant_block(
                "rejected-text",
                crate::types::message::AssistantContent::Text("Scanning auth".to_string()),
            )),
            agent_progress(RenderableMessage::assistant_block(
                "rejected-read",
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_rejected".to_string()),
                        name: "Read".to_string(),
                        input: serde_json::json!({ "file_path": "src/auth.rs" }),
                    },
                ),
            )),
        ];
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Agent".to_string(),
                    is_error: true,
                    content: crate::utils::messages::REJECT_MESSAGE.to_string(),
                    tool_input: Some(serde_json::json!({
                        "description": "Inspect auth",
                        "prompt": "go",
                    })),
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);
        let text = canvas.to_string();

        assert!(text.contains("Scanning auth"), "canvas=\n{text}");
        // No space: the mounted row is CC's own name/summary box pair (see
        // `agent_error_result_replays_progress_before_fallback`).
        assert!(text.contains("Read(src/auth.rs)"), "canvas=\n{text}");
        let replay_idx = text.find("Scanning auth").expect("replay renders");
        let interrupt_idx = text
            .find(crate::components::interrupted_by_user::INTERRUPTED_BY_USER_TEXT)
            .expect("rejected fallback renders");
        assert!(
            replay_idx < interrupt_idx,
            "the replay must precede the rejected fallback; canvas=\n{text}"
        );

        // No progress at all → the Initializing… row (`UI.tsx:532-538`).
        let empty = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Agent".to_string(),
                    is_error: true,
                    content: crate::utils::messages::REJECT_MESSAGE.to_string(),
                    tool_input: Some(serde_json::json!({
                        "description": "Inspect auth",
                        "prompt": "go",
                    })),
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(empty.contains("Initializing…"), "canvas=\n{empty}");
    }

    /// CC `UI.tsx:462-467` renders the completed hint through `CtrlOToExpand`,
    /// which self-suppresses inside a `SubAgentProvider`
    /// (`components/CtrlOToExpand.tsx:24-34`) — the nested Done line shows no
    /// `(ctrl+o to expand)` while the top-level one keeps it.
    #[test]
    fn nested_completed_done_line_suppresses_expand_hint() {
        let raw = agent_completed_raw(2, 2_000, 1_000);
        let top_level = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Agent".to_string(),
                    is_error: false,
                    content: String::new(),
                    tool_use_result: Some(raw.clone()),
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(
            top_level.contains("Done (2 tool uses · 1.0k tokens · 2s)"),
            "canvas=\n{top_level}"
        );
        assert!(
            top_level.contains("(ctrl+o to expand)"),
            "canvas=\n{top_level}"
        );

        let nested = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                SubAgentProvider {
                    UserToolResultMessage(
                        tool_name: "Agent".to_string(),
                        is_error: false,
                        content: String::new(),
                        tool_use_result: Some(raw),
                        verbose: false,
                        is_transcript_mode: false,
                    )
                }
            }
        }
        .render(None)
        .to_string();
        assert!(nested.contains("Done (2 tool uses"), "canvas=\n{nested}");
        assert!(
            !nested.contains("(ctrl+o to expand)"),
            "the SubAgentContext gate must suppress the nested hint; canvas=\n{nested}"
        );
    }
}
