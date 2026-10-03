//! Maps to: CC `components/messages/GroupedToolUseContent.tsx`.

use crate::components::agent_progress_line::AgentProgressLine;
use crate::components::ctrl_o_to_expand::CtrlOToExpand;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderTone,
};
use crate::components::tool_use_loader::ToolUseLoader;
use crate::tools::agent_tool::ui::grouped_agent_stat;
use crate::types::message::{
    GroupedToolUseMessage, RenderableMessage, ToolResultStatus, ToolUseProgressMessage,
    ToolUseStatus,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

fn is_agent_like_group(tool_name: &str) -> bool {
    matches!(tool_name.to_ascii_lowercase().as_str(), "agent" | "task")
}

/// CC `GroupedToolUseContent.tsx:50-51` — `msg.message.content[0]` is the
/// member's tool_use block: the row's first non-identity block.
fn row_tool_use_block(row: &RenderableMessage) -> Option<&crate::types::message::ToolUseBlock> {
    match &row.kind {
        crate::types::message::RenderableMessageKind::Assistant { message } => {
            match message.first_content_block() {
                Some(crate::types::message::AssistantContent::ToolUse(tool_use)) => Some(tool_use),
                _ => None,
            }
        }
        _ => None,
    }
}

/// CC `GroupedToolUseContent.tsx:39-47` — a result message's tool_result
/// blocks. One block per row (normalize-guaranteed), so at most one.
fn row_tool_result_block(row: &RenderableMessage) -> Option<&crate::types::message::ToolResult> {
    match &row.kind {
        crate::types::message::RenderableMessageKind::User { message } => {
            match message.first_content_block() {
                Some(crate::types::message::UserContent::ToolResult(tool_result)) => {
                    Some(tool_result)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Render-time per-member data — CC `GroupedToolUseContent.tsx:50-63`
/// `toolUsesData`: derived from `message.messages` + `message.results` on
/// every render, never stored on the message (the old `GroupedToolUseItem`
/// row storage is gone).
struct GroupedToolUseData {
    /// CC `param.id`, carried onto `agentStats[].id` (`AgentTool/UI.tsx:900`)
    /// for the React `key` (`:967`). iocraft keys children positionally, so
    /// this has no render-side reader — it is the id the member's own state
    /// was already resolved from.
    #[allow(dead_code)]
    tool_use_id: String,
    /// CC `param.name` — kept beside `input` so the generic fallback can call
    /// the owning tool's renderer without re-walking the member rows.
    tool_name: String,
    /// CC `param.input` — the grouped renderer re-parses it through the tool's
    /// own `inputSchema`, so the block's input must survive to render time.
    input: serde_json::Value,
    /// CC `result` presence; the status is the result block's
    /// `derived_status()` (cancel/reject sentinels, then `is_error`).
    result_status: Option<ToolResultStatus>,
    result_content: Option<String>,
    /// CC `result.output.status`, where `output` is the result row's raw
    /// `toolUseResult` (`GroupedToolUseContent.tsx:42-46`).
    output_status: Option<String>,
    /// CC `filterToolProgressMessages(lookups.progressMessagesByToolUseID.get(id) ?? [])`
    /// (`:58-60`). CC's filter drops `hook_progress`; the Rust progress union
    /// has no such member (the hook seam is an actor event), so the lookup is
    /// the whole filter.
    progress_messages: Vec<ToolUseProgressMessage>,
    /// CC `isResolved` / `isError` / `isInProgress` (`:55-57`).
    is_resolved: bool,
    is_error: bool,
    is_in_progress: bool,
}

/// CC `GroupedToolUseContent.tsx:35-63` — build `resultsByToolUseId` from the
/// group's result rows, then map each member row to its derived data. Members
/// without a (non-empty) tool_use id never group in the producer; skipping
/// them here preserves that invariant for hand-built groups.
fn grouped_tool_use_data(
    message: &GroupedToolUseMessage,
    in_progress_tool_use_ids: &std::collections::HashSet<String>,
    lookups: Option<&crate::components::messages_list::MessageLookups>,
) -> Vec<GroupedToolUseData> {
    let mut results_by_tool_use_id: std::collections::HashMap<
        &str,
        &crate::types::message::ToolResult,
    > = std::collections::HashMap::new();
    for result_row in &message.results {
        if let Some(tool_result) = row_tool_result_block(result_row) {
            if !tool_result.tool_use_id.0.is_empty() {
                results_by_tool_use_id.insert(tool_result.tool_use_id.0.as_str(), tool_result);
            }
        }
    }

    message
        .messages
        .iter()
        .filter_map(|member| {
            let tool_use =
                row_tool_use_block(member).filter(|tool_use| !tool_use.id.0.is_empty())?;
            let result = results_by_tool_use_id.get(tool_use.id.0.as_str());
            let result_status = result.map(|tool_result| tool_result.derived_status());
            // CC reads `isResolved`/`isError` straight off `lookups`. A group
            // member's own result row is the closer authority when the mount
            // has no lookups at all (`GroupedToolUseMessage.results` is what
            // `resolvedToolUseIDs` would have said), so it takes priority and
            // the two sets answer only for a member without one.
            let status = match result_status {
                Some(ToolResultStatus::Success) => ToolUseStatus::Succeeded,
                Some(_) => ToolUseStatus::Failed,
                None => super::assistant_tool_use_message::derive_tool_use_status(
                    Some(tool_use.id.0.as_str()),
                    in_progress_tool_use_ids,
                    lookups,
                ),
            };
            Some(GroupedToolUseData {
                tool_use_id: tool_use.id.0.clone(),
                tool_name: tool_use.name.clone(),
                input: tool_use.input.clone(),
                result_status,
                result_content: result.map(|tool_result| tool_result.content.clone()),
                output_status: result.and_then(|tool_result| {
                    tool_result
                        .tool_use_result
                        .as_ref()?
                        .get("status")?
                        .as_str()
                        .map(ToOwned::to_owned)
                }),
                progress_messages: lookups
                    .and_then(|lookups| {
                        lookups
                            .progress_messages_by_tool_use_id
                            .get(tool_use.id.0.as_str())
                    })
                    .cloned()
                    .unwrap_or_default(),
                is_resolved: matches!(status, ToolUseStatus::Succeeded | ToolUseStatus::Failed),
                is_error: status == ToolUseStatus::Failed,
                is_in_progress: status == ToolUseStatus::Running,
            })
        })
        .collect()
}

/// Each member's summary, re-rendered from its tool_use block through the
/// owning tool renderer — the call the producer used to pre-bake.
///
/// Only the generic (non-Agent) fallback consumes these; CC's Agent renderer
/// derives every row from `input`, `result.output` and `progressMessages`
/// instead. So this runs once per fallback render, not once per member of
/// every render — the Agent group is the only group `apply_grouping` can
/// actually produce (`groupToolUses.ts:28`), and it read none of it.
fn grouped_tool_use_summaries(items: &[GroupedToolUseData]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            super::assistant_tool_use_message::render_tool_use_message(
                item.tool_name.as_str(),
                &item.input,
                ToolRenderOptions::default(),
            )
            .unwrap_or_default()
        })
        .collect()
}

/// The generic fallback headline's summary — the first member whose block
/// renders a non-empty summary.
fn grouped_tool_use_summary(summaries: &[String]) -> String {
    summaries
        .iter()
        .find(|summary| !summary.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

/// `group_status` is derived by the caller, not stored on the message: CC's
/// `GroupedToolUseMessage` (`types/message.ts:140-144`) carries only `messages`
/// and `results`, and `groupToolUses(messages)` takes only messages.
/// `count` is `messages.len()` — CC has no count field either.
///
/// Official `GroupedToolUseContent.tsx:29-32` returns null when the tool has no
/// `renderGroupedToolUse`, and `groupToolUses.ts:28` only groups tools that
/// have one — so AgentTool is the only group CC can produce. This is the
/// generic main-screen fallback until each tool-specific grouped renderer is
/// ported; the Agent branch lives in [`render_grouped_agent_tool_use`].
fn render_grouped_tool_use_lines(
    tool_name: &str,
    count: usize,
    summary: &str,
    group_status: ToolUseStatus,
) -> Vec<ToolRenderLine> {
    let label = if summary.trim().is_empty() {
        format!("{tool_name} ×{count}")
    } else {
        format!("{tool_name} ×{count} — {}", summary.trim())
    };
    vec![ToolRenderLine::new(
        label,
        grouped_tool_use_tone(group_status),
    )]
}

fn grouped_tool_use_tone(status: ToolUseStatus) -> ToolRenderTone {
    // Maps to: CC `renderGroupedAgentToolUse` — finished summaries use default
    // `<Text>`, not `color="success"`.
    match status {
        ToolUseStatus::Succeeded => ToolRenderTone::Normal,
        ToolUseStatus::Failed => ToolRenderTone::Error,
        ToolUseStatus::Running => ToolRenderTone::Normal,
        ToolUseStatus::Queued => ToolRenderTone::Inactive,
    }
}

#[derive(Default, Props)]
pub struct GroupedToolUseContentProps {
    pub message: Option<GroupedToolUseMessage>,
    pub add_margin: bool,
    /// Maps to: CC `GroupedToolUseContent.tsx` receiving `inProgressToolUseIDs`
    /// and `lookups`. CC's `GroupedToolUseMessage` (types/message.ts:140-144)
    /// carries no status — `groupToolUses(messages)` takes only messages — so
    /// each member's state is derived here, exactly as for an ungrouped row.
    pub in_progress_tool_use_ids: std::sync::Arc<std::collections::HashSet<String>>,
    pub lookups: Option<std::sync::Arc<crate::components::messages_list::MessageLookups>>,
    /// Maps to: CC `GroupedToolUseContent.tsx:19` `shouldAnimate` — the value
    /// `MessageRow.tsx:174-179` already narrowed to "some member is in the live
    /// set"; CC narrows it once more here (`:68`) and again in the tool's own
    /// renderer (`AgentTool/UI.tsx:937`).
    pub should_animate: bool,
    /// Maps to: CC `GroupedToolUseContent.tsx:16` `tools: Tools` — the live
    /// main-loop pool, forwarded from `Message.tsx:250` and handed straight
    /// back out at `:67-70` as the tool renderer's `options.tools`.
    ///
    /// Seam: CC's `:29-32` also resolves the GROUP's own tool through
    /// `findToolByName(tools, message.toolName)` and renders null when the pool
    /// has no such tool or the tool has no `renderGroupedToolUse`. This port
    /// keeps the pre-existing name gate ([`is_agent_like_group`]) for that
    /// decision — a mount that has not been threaded the pool would otherwise
    /// drop the whole group instead of degrading one line — so the pool is read
    /// only where `extractLastToolInfo` reads it.
    pub tools: std::sync::Arc<Vec<crate::types::tools::Tool>>,
}

/// Fold of the members' derived states, in the order CC's renderer cares about:
/// any failure colours the whole group, then any live member, then any member
/// still waiting; only an all-succeeded group reads as done.
fn derive_grouped_tool_use_status(statuses: &[ToolUseStatus]) -> ToolUseStatus {
    let mut has_running = false;
    let mut has_queued = false;
    for status in statuses {
        match status {
            ToolUseStatus::Failed => return ToolUseStatus::Failed,
            ToolUseStatus::Running => has_running = true,
            ToolUseStatus::Queued => has_queued = true,
            ToolUseStatus::Succeeded => {}
        }
    }
    if has_running {
        ToolUseStatus::Running
    } else if has_queued {
        ToolUseStatus::Queued
    } else {
        ToolUseStatus::Succeeded
    }
}

/// Maps to: CC `tools/AgentTool/UI.tsx:824-987#renderGroupedAgentToolUse`.
///
/// The per-member derivation lives with the tool
/// ([`crate::tools::agent_tool::ui::grouped_agent_stat`]); this is the chrome
/// around it. Everything the row shows now comes from the member's `input`,
/// its result's raw `toolUseResult`, and its forwarded progress messages — the
/// three inputs CC reads. The previous implementation reverse-parsed the
/// rendered summary string and the result CONTENT (`<usage>` tags, prose
/// sniffing for "async agent launched successfully"); CC has no such source
/// and neither does a real payload.
fn render_grouped_agent_tool_use(
    items: &[GroupedToolUseData],
    should_animate: bool,
    tools: &[crate::types::tools::Tool],
    theme: &Theme,
) -> AnyElement<'static> {
    // CC `:844-915`.
    let agent_stats = items
        .iter()
        .map(|item| {
            grouped_agent_stat(
                &item.input,
                item.output_status.as_deref(),
                &item.progress_messages,
                tools,
            )
        })
        .collect::<Vec<_>>();

    // CC `:917-919`.
    let any_unresolved = items.iter().any(|item| !item.is_resolved);
    let any_error = items.iter().any(|item| item.is_error);
    let all_complete = !any_unresolved;

    // CC `:922-928` — the shared type is suppressed when it is literally
    // 'Agent', so an untyped group keeps the bare "agents" noun.
    let all_same_type = !agent_stats.is_empty()
        && agent_stats
            .iter()
            .all(|stat| stat.agent_type == agent_stats[0].agent_type);
    let common_type = agent_stats
        .first()
        .map(|stat| stat.agent_type.as_str())
        .filter(|agent_type| all_same_type && *agent_type != "Agent");

    // CC `:931` — `Array.prototype.every`, so an empty group is all-async.
    let all_async = agent_stats.iter().all(|stat| stat.is_async);
    // CC `:945`/`:952`/`:958` read `toolUses.length`, which is
    // `message.messages.map(...)` with NO filter (`GroupedToolUseContent.tsx:49`)
    // — CC really does keep a member whose block carries no id: `content.id` is
    // `undefined`, so it misses `resultsByToolUseId` and all three lookup Sets,
    // and the Agent renderer still emits a row for it. Rust's
    // `grouped_tool_use_data` filter therefore makes this count POST-filter
    // where CC's is pre-filter. The two agree for every group `apply_grouping`
    // can build: it only groups messages whose `content[0].type === 'tool_use'`
    // (`groupToolUses.ts:37-38`), and a real block's id is never empty. Keep the
    // count on `agent_stats` so it stays consistent with the member rows below,
    // which `is_last` indexes against.
    let count = agent_stats.len();

    // CC `:941-962` — one default-coloured `<Text>` in which ONLY the count is
    // bold. The noun is always plural: `${commonType} agents` / 'agents'.
    let noun = match common_type {
        Some(common_type) => format!("{common_type} agents"),
        None => "agents".to_string(),
    };
    let (prefix, suffix) = match (all_complete, all_async) {
        (true, true) => (String::new(), " background agents launched ".to_string()),
        (true, false) => (String::new(), format!(" {noun} finished")),
        (false, _) => ("Running ".to_string(), format!(" {noun}…")),
    };

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                ToolUseLoader(
                    should_animate: should_animate && any_unresolved,
                    is_unresolved: any_unresolved,
                    is_error: any_error,
                )
                #(if prefix.is_empty() {
                    None
                } else {
                    Some(element! { Text(content: prefix, wrap: TextWrap::NoWrap) })
                })
                Text(content: count.to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(content: suffix, wrap: TextWrap::NoWrap)
                #(if all_complete && all_async {
                    Some(element! {
                        crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint(
                            shortcut: "↓".to_string(),
                            action: "manage".to_string(),
                            parens: true,
                            dim: true,
                        )
                    })
                } else {
                    None
                })
                // CC `:961` — the trailing `{' '}` inside the `<Text>`, before
                // the expand hint.
                Text(content: " ".to_string(), wrap: TextWrap::NoWrap)
                #(if all_async {
                    None
                } else {
                    Some(element! { CtrlOToExpand })
                })
            }
            #(agent_stats.iter().enumerate().map(|(index, stat)| {
                let item = &items[index];
                element! {
                    AgentProgressLine(
                        agent_type: stat.agent_type.clone(),
                        description: stat.description.clone(),
                        description_color: stat.description_color.map(|key| theme.color(key)),
                        task_description: stat.task_description.clone(),
                        tool_use_count: stat.tool_use_count,
                        tokens: stat.tokens,
                        color: stat.color.map(|key| theme.color(key)),
                        is_last: index + 1 == count,
                        is_resolved: item.is_resolved,
                        is_error: item.is_error,
                        is_async: stat.is_async,
                        should_animate: should_animate,
                        last_tool_info: stat.last_tool_info.clone(),
                        hide_type: all_same_type,
                        name: stat.name.clone(),
                    )
                }
            }))
        }
    }
    .into_any()
}

#[component]
pub fn GroupedToolUseContent(
    props: &GroupedToolUseContentProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let message = props.message.clone().unwrap_or(GroupedToolUseMessage {
        tool_name: "Tool".to_string(),
        messages: Vec::new(),
        results: Vec::new(),
    });
    // CC `GroupedToolUseContent.tsx:35-63`: per-member data (input, result
    // pairing, liveness, progress) is derived from the grouped rows + result
    // rows + lookups on every render.
    let items = grouped_tool_use_data(
        &message,
        &props.in_progress_tool_use_ids,
        props.lookups.as_deref(),
    );

    if is_agent_like_group(&message.tool_name) {
        // CC `:65-70` — `shouldAnimate && anyInProgress`, then the tool's own
        // renderer narrows it again by `anyUnresolved`. `tools` is the second
        // half of the same `options` object (`:69`).
        let any_in_progress = items.iter().any(|item| item.is_in_progress);
        return render_grouped_agent_tool_use(
            &items,
            props.should_animate && any_in_progress,
            &props.tools,
            &theme,
        );
    }

    let count = message.messages.len();
    let summaries = grouped_tool_use_summaries(&items);
    let summary = grouped_tool_use_summary(&summaries);
    let group_status = derive_grouped_tool_use_status(
        &items
            .iter()
            .map(
                |item| match (item.is_resolved, item.is_error, item.is_in_progress) {
                    (true, true, _) => ToolUseStatus::Failed,
                    (true, false, _) => ToolUseStatus::Succeeded,
                    (false, _, true) => ToolUseStatus::Running,
                    (false, _, false) => ToolUseStatus::Queued,
                },
            )
            .collect::<Vec<_>>(),
    );
    let lines = render_grouped_tool_use_lines(&message.tool_name, count, &summary, group_status);

    element! {
        View(
            flex_direction: FlexDirection::Column,
            margin_top: if props.add_margin { 1u32 } else { 0u32 },
        ) {
            #(lines.into_iter().map(|line| {
                let color = match line.tone {
                    ToolRenderTone::Normal => theme.claude,
                    ToolRenderTone::Success => theme.success,
                    ToolRenderTone::Warning => theme.warning,
                    ToolRenderTone::Error => theme.error,
                    ToolRenderTone::Inactive => theme.inactive,
                };
                element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "  ⎿ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        Text(content: line.text, color: color, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    }
                }
            }))
            #(items.iter().zip(summaries.iter()).map(|(item, summary)| {
                let color = match item.result_status {
                    Some(ToolResultStatus::Error) => theme.error,
                    Some(ToolResultStatus::Rejected) => theme.warning,
                    Some(ToolResultStatus::Canceled) => theme.inactive,
                    _ if item.is_error => theme.error,
                    _ if item.is_resolved => theme.success,
                    _ if item.is_in_progress => theme.claude,
                    _ => theme.inactive,
                };
                let result_suffix = item.result_content
                    .as_ref()
                    .map(|content| content.trim())
                    .filter(|content| !content.is_empty())
                    .map(|content| format!(" — {content}"))
                    .unwrap_or_default();
                element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "    • ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        Text(content: format!("{summary}{result_suffix}"), color: color, wrap: TextWrap::NoWrap)
                    }
                }
            }))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::messages_list::MessageLookups;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::sync::Arc;

    fn canvas_lines(canvas: &Canvas) -> Vec<String> {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn find_text_cell(canvas: &Canvas, needle: &str) -> Option<(usize, usize)> {
        canvas_lines(canvas)
            .iter()
            .enumerate()
            .find_map(|(row, line)| line.find(needle).map(|column| (column, row)))
    }

    /// Fixtures build real tool_use inputs — the grouped Agent renderer parses
    /// `param.input` through the tool's own `inputSchema`
    /// (`AgentTool/UI.tsx:848`), and a display summary is not invertible back
    /// into an input.
    fn tool_use_row(
        uuid: &str,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
    ) -> RenderableMessage {
        RenderableMessage::assistant_block(
            uuid,
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(tool_use_id.to_string()),
                name: tool_name.to_string(),
                input,
            }),
        )
    }

    fn result_row(tool_use_id: &str, content: &str) -> RenderableMessage {
        RenderableMessage::user_tool_result(
            format!("result-{tool_use_id}"),
            tool_use_id,
            content,
            false,
        )
    }

    /// CC's `result.output` is the result row's raw `toolUseResult`
    /// (`GroupedToolUseContent.tsx:42-46`), not the model-facing content string
    /// — a backgrounded member is recognised by `output.status`, never by
    /// sniffing prose.
    fn result_row_with_output(
        tool_use_id: &str,
        content: &str,
        output: serde_json::Value,
    ) -> RenderableMessage {
        result_row(tool_use_id, content).with_tool_use_result(Some(output))
    }

    /// CC `lookups.progressMessagesByToolUseID` — the only source
    /// `calculateAgentStats` and `extractLastToolInfo` read.
    fn lookups_with_progress(
        entries: Vec<(&str, Vec<ToolUseProgressMessage>)>,
    ) -> Arc<MessageLookups> {
        let mut lookups = MessageLookups::default();
        for (tool_use_id, progress) in entries {
            lookups
                .progress_messages_by_tool_use_id
                .insert(tool_use_id.to_string(), progress);
        }
        Arc::new(lookups)
    }

    fn agent_progress(message: RenderableMessage) -> ToolUseProgressMessage {
        ToolUseProgressMessage::AgentProgress {
            message: Box::new(message),
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        }
    }

    /// One nested Read: the assistant tool_use plus the user tool_result CC
    /// counts (`UI.tsx:800-803`).
    fn nested_read(tool_use_id: &str, path: &str) -> [ToolUseProgressMessage; 2] {
        [
            agent_progress(tool_use_row(
                &format!("nested-{tool_use_id}"),
                tool_use_id,
                "Read",
                serde_json::json!({ "file_path": path }),
            )),
            agent_progress(result_row(tool_use_id, "ok")),
        ]
    }

    /// A nested assistant row carrying `usage` — the only token source
    /// (`UI.tsx:812-818`, `findLast`).
    fn nested_assistant_usage(uuid: &str, input_tokens: u64) -> ToolUseProgressMessage {
        agent_progress(RenderableMessage {
            uuid: uuid.to_string(),
            kind: crate::types::message::RenderableMessageKind::Assistant {
                message: crate::types::message::AssistantMessage {
                    uuid: uuid.to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::AssistantContent::Text(
                        "working".to_string(),
                    )],
                    model: None,
                    stop_reason: None,
                    usage: Some(crate::types::message::TokenUsage {
                        input_tokens,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        cache_deleted_input_tokens: 0,
                    }),
                },
            },
        })
    }

    // `prompt` rides every fixture because it rides every real Agent
    // tool_use: CC hides the whole row when `description` or `prompt` is
    // missing (AgentTool/UI.tsx:472-483), so an input without it would test
    // the hidden-row path rather than grouping.
    fn agent_input(description: &str) -> serde_json::Value {
        serde_json::json!({ "description": description, "prompt": "do the work" })
    }

    fn typed_agent_input(agent_type: &str, description: &str) -> serde_json::Value {
        serde_json::json!({
            "description": description,
            "prompt": "do the work",
            "subagent_type": agent_type,
        })
    }

    fn named_agent_input(name: &str, agent_type: &str, description: &str) -> serde_json::Value {
        serde_json::json!({
            "description": description,
            "prompt": "do the work",
            "subagent_type": agent_type,
            "name": name,
        })
    }

    /// CC's `tools` prop is the live main-loop pool (`REPL.tsx:1216`), and
    /// `findToolByName` reads `name` / `aliases` only (`Tool.ts:348-360`), so a
    /// fixture pool needs no more than the entries the rows must match.
    fn tool_pool(names: &[&str]) -> Arc<Vec<crate::types::tools::Tool>> {
        Arc::new(
            names
                .iter()
                .map(|name| crate::types::tools::Tool {
                    name: (*name).to_string(),
                    ..Default::default()
                })
                .collect(),
        )
    }

    /// The pool the fixtures render against: the Agent rows themselves plus the
    /// nested tools their progress messages carry.
    fn default_tool_pool() -> Arc<Vec<crate::types::tools::Tool>> {
        tool_pool(&["Agent", "Read", "Grep", "Bash"])
    }

    fn render_group(message: GroupedToolUseMessage) -> Canvas {
        render_group_with_lookups(message, None)
    }

    fn render_group_with_lookups(
        message: GroupedToolUseMessage,
        lookups: Option<Arc<MessageLookups>>,
    ) -> Canvas {
        render_group_with_lookups_and_tools(message, lookups, default_tool_pool())
    }

    fn render_group_with_lookups_and_tools(
        message: GroupedToolUseMessage,
        lookups: Option<Arc<MessageLookups>>,
        tools: Arc<Vec<crate::types::tools::Tool>>,
    ) -> Canvas {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                GroupedToolUseContent(
                    message: Some(message),
                    add_margin: false,
                    lookups: lookups,
                    tools: tools,
                )
            }
        }
        .render(None)
    }

    #[test]
    fn grouped_tool_use_data_pairs_results_and_derives_status_from_blocks() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    agent_input("Inspect auth"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    agent_input("Inspect storage"),
                ),
            ],
            results: vec![RenderableMessage::user_tool_result(
                "result-1",
                "toolu_agent_1",
                crate::utils::messages::CANCEL_MESSAGE,
                true,
            )],
        };

        let items = grouped_tool_use_data(&message, &Default::default(), None);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].tool_use_id, "toolu_agent_1");
        assert_eq!(items[0].tool_name, "Agent");
        assert_eq!(items[0].input, agent_input("Inspect auth"));
        // The summary is fallback-only work now, so it is derived on demand
        // rather than stored on every member of every render.
        assert_eq!(
            grouped_tool_use_summaries(&items),
            vec!["Inspect auth".to_string(), "Inspect storage".to_string()]
        );
        assert_eq!(items[0].result_status, Some(ToolResultStatus::Canceled));
        assert!(items[0].is_resolved);
        assert!(items[0].is_error);
        // No raw `toolUseResult` on the row → CC's `result?.output?.status` is
        // undefined.
        assert_eq!(items[0].output_status, None);
        assert_eq!(items[1].tool_use_id, "toolu_agent_2");
        assert_eq!(items[1].result_status, None);
        assert_eq!(items[1].result_content, None);
        assert!(!items[1].is_resolved);
        assert!(!items[1].is_in_progress);
    }

    #[test]
    fn grouped_tool_use_uses_local_fallback_until_tool_specific_grouping_exists() {
        let lines = render_grouped_tool_use_lines(
            "Read",
            5,
            "loaded related message components",
            derive_grouped_tool_use_status(&[]),
        );
        assert_eq!(lines[0].text, "Read ×5 — loaded related message components");
        assert_eq!(lines[0].tone, ToolRenderTone::Normal);
    }

    #[test]
    fn grouped_agent_tool_use_uses_official_shaped_summary_and_tree_rows() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    agent_input("Inspect auth"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    agent_input("Inspect storage"),
                ),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        // CC `UI.tsx:952-953`: the noun is ALWAYS plural, and `commonType` is
        // null here because the shared type is literally 'Agent' (`:926`).
        assert!(text.contains("2 agents finished"), "canvas=\n{text}");
        assert!(text.contains("ctrl+o to expand"), "canvas=\n{text}");
        // `hideType` (all the same type) collapses the row to
        // `name ?? description ?? agentType` (AgentProgressLine.tsx:62).
        // With no progress messages the stats read 0 uses / null tokens.
        assert!(
            text.contains("├─ Inspect auth · 0 tool uses"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("└─ Inspect storage · 0 tool uses"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("tokens"), "canvas=\n{text}");
        assert!(text.contains("Done"), "canvas=\n{text}");
        assert!(!text.contains("Agent ×2"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_legacy_task_tool_use_uses_agent_noun_like_official_alias() {
        let message = GroupedToolUseMessage {
            tool_name: "Task".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Task",
                    agent_input("Run checks"),
                ),
                tool_use_row("agent-2", "toolu_agent_2", "Task", agent_input("Run lint")),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        assert!(text.contains("2 agents finished"), "canvas=\n{text}");
        assert!(!text.contains("tasks"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_agent_tool_use_uses_common_subagent_type_in_headline_and_hides_repeated_type() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    serde_json::json!({
                        "description": "Inspect auth",
                        "prompt": "do the work",
                        "subagent_type": "reviewer",
                        "model": "opus",
                    }),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    typed_agent_input("reviewer", "Inspect storage"),
                ),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        assert!(
            text.contains("2 reviewer agents finished"),
            "canvas=\n{text}"
        );
        assert!(text.contains("├─ Inspect auth"), "canvas=\n{text}");
        assert!(text.contains("└─ Inspect storage"), "canvas=\n{text}");
        assert!(!text.contains("reviewer: Inspect auth"), "canvas=\n{text}");
        // The model rides `input.model` and reaches the row through CC's
        // separate `renderToolUseTag`, never the grouped renderer.
        assert!(!text.contains("model"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_agent_tool_use_collapses_named_members_to_name_and_description() {
        let message = GroupedToolUseMessage {
            tool_name: "Task".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Task",
                    named_agent_input("runner", "reviewer", "Run tests"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Task",
                    named_agent_input("auditor", "reviewer", "Check logs"),
                ),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        assert!(
            text.contains("2 reviewer agents finished"),
            "canvas=\n{text}"
        );
        // Not a teammate spawn (no `teammate_spawned` output), so `agentType`
        // is `userFacingName` = the subagent type and `name` stays raw. With
        // `hideType`, CC renders `name` + `: description`
        // (AgentProgressLine.tsx:62-63) — the `@` prefix only exists on the
        // teammate-spawn branch's `agentType` (`UI.tsx:862`).
        assert!(text.contains("├─ runner: Run tests"), "canvas=\n{text}");
        assert!(text.contains("└─ auditor: Check logs"), "canvas=\n{text}");
        assert!(!text.contains("@runner"), "canvas=\n{text}");
        assert!(!text.contains("(reviewer)"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_agent_tool_use_keeps_type_visible_for_mixed_subagent_types() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    typed_agent_input("reviewer", "Inspect auth"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    typed_agent_input("planner", "Plan migration"),
                ),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        assert!(text.contains("2 agents finished"), "canvas=\n{text}");
        assert!(
            text.contains("├─ reviewer (Inspect auth)"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("└─ planner (Plan migration)"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("reviewer: Inspect auth"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_agent_tool_use_applies_official_agent_color_context_to_visible_type() {
        let theme = *crate::utils::theme::current();
        crate::tools::agent_tool::agent_color_manager::set_agent_color(
            "reviewer",
            Some(crate::tools::agent_tool::agent_color_manager::AgentColorName::Cyan),
        );
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    typed_agent_input("reviewer", "Inspect auth"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    typed_agent_input("planner", "Plan migration"),
                ),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let canvas = render_group(message);
        crate::tools::agent_tool::agent_color_manager::set_agent_color("reviewer", None);
        let text = canvas.to_string();
        let (column, row) = find_text_cell(&canvas, "reviewer").expect("reviewer cell");
        let first_cell = canvas.cell(column, row).expect("reviewer first cell");

        assert!(
            text.contains("├─ reviewer (Inspect auth)"),
            "canvas=\n{text}"
        );
        assert_eq!(first_cell.background_color, Some(theme.agent_cyan));
    }

    #[test]
    fn grouped_agent_tool_use_shows_agent_type_for_default_agent_in_mixed_groups() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    typed_agent_input("reviewer", "Inspect auth"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    agent_input("Inspect storage"),
                ),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        assert!(
            text.contains("├─ reviewer (Inspect auth)"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("└─ Agent (Inspect storage)"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn grouped_named_agent_tool_use_uses_type_not_name_for_mixed_subagent_types() {
        let message = GroupedToolUseMessage {
            tool_name: "Task".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Task",
                    named_agent_input("runner", "reviewer", "Run tests"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Task",
                    named_agent_input("planner", "planner", "Plan fix"),
                ),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        assert!(text.contains("├─ reviewer (Run tests)"), "canvas=\n{text}");
        assert!(text.contains("└─ planner (Plan fix)"), "canvas=\n{text}");
        assert!(!text.contains("@runner"), "canvas=\n{text}");
        assert!(!text.contains("@planner"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_agent_tool_use_unresolved_rows_use_official_initializing_fallback() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![tool_use_row(
                "agent-1",
                "toolu_agent_1",
                "Agent",
                agent_input("Inspect auth"),
            )],
            results: Vec::new(),
        };

        let text = render_group(message).to_string();

        // CC has no singular noun anywhere in this renderer (`UI.tsx:958-959`).
        assert!(text.contains("Running 1 agents…"), "canvas=\n{text}");
        assert!(text.contains("Initializing…"), "canvas=\n{text}");
        assert!(!text.contains("Running…"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_agent_live_rows_read_stats_and_status_from_forwarded_progress() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    agent_input("Inspect auth"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    agent_input("Inspect storage"),
                ),
            ],
            results: Vec::new(),
        };
        let mut progress = Vec::new();
        progress.extend(nested_read("nested_1", "src/a.rs"));
        progress.extend(nested_read("nested_2", "src/b.rs"));
        progress.extend(nested_read("nested_3", "src/c.rs"));
        progress.push(nested_assistant_usage("nested-final", 12_500));

        let text = render_group_with_lookups(
            message,
            Some(lookups_with_progress(vec![("toolu_agent_1", progress)])),
        )
        .to_string();

        // `calculateAgentStats`: three tool_result rows, and the LAST
        // assistant's usage (`UI.tsx:806-818`).
        assert!(
            text.contains("├─ Inspect auth · 3 tool uses · 12.5k tokens"),
            "canvas=\n{text}"
        );
        // The trailing assistant text row breaks `extractLastToolInfo`'s
        // backwards scan, so the status is the last tool_result's own
        // description (`UI.tsx:1086-1120`).
        assert!(text.contains("Read: src/c.rs"), "canvas=\n{text}");
        // The second member has no progress at all.
        assert!(
            text.contains("└─ Inspect storage · 0 tool uses"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Initializing…"), "canvas=\n{text}");
    }

    /// The `tools` prop, end to end through this component:
    /// `GroupedToolUseContent.tsx:16` → `:69` `tool.renderGroupedToolUse(data,
    /// { shouldAnimate, tools })` → `AgentTool/UI.tsx:835` →
    /// `:847 extractLastToolInfo(progressMessages, tools)` → `:1096`
    /// `findToolByName`.
    ///
    /// A nested Read the pool no longer carries renders the RAW wire name with
    /// no summary (`:1097`), where the pooled render shows the tool's own
    /// `userFacingName` + `getToolUseSummary`.
    #[test]
    fn the_pool_prop_decides_how_a_nested_tool_row_is_named() {
        let group = || GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![tool_use_row(
                "agent-1",
                "toolu_agent_1",
                "Agent",
                agent_input("Inspect auth"),
            )],
            results: Vec::new(),
        };
        let progress = || {
            Some(lookups_with_progress(vec![(
                "toolu_agent_1",
                nested_read("nested_1", "src/a.rs").to_vec(),
            )]))
        };

        let pooled =
            render_group_with_lookups_and_tools(group(), progress(), tool_pool(&["Agent", "Read"]))
                .to_string();
        assert!(pooled.contains("Read: src/a.rs"), "canvas=\n{pooled}");

        let unpooled =
            render_group_with_lookups_and_tools(group(), progress(), tool_pool(&["Agent"]))
                .to_string();
        assert!(unpooled.contains("Read"), "canvas=\n{unpooled}");
        assert!(!unpooled.contains("src/a.rs"), "canvas=\n{unpooled}");
    }

    #[test]
    fn grouped_agent_token_stats_use_official_compact_number_boundaries() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![tool_use_row(
                "agent-1",
                "toolu_agent_1",
                "Agent",
                agent_input("Inspect auth"),
            )],
            results: vec![result_row("toolu_agent_1", "done")],
        };
        let mut progress = Vec::new();
        progress.extend(nested_read("nested_1", "src/a.rs"));
        progress.extend(nested_read("nested_2", "src/b.rs"));
        progress.push(nested_assistant_usage("nested-final", 999_999));

        let text = render_group_with_lookups(
            message,
            Some(lookups_with_progress(vec![("toolu_agent_1", progress)])),
        )
        .to_string();

        assert!(
            text.contains("└─ Inspect auth · 2 tool uses · 1.0m tokens"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn grouped_background_custom_agent_keeps_type_description_when_not_teammate_spawn() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Agent",
                    typed_agent_input("reviewer", "Run lint"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    typed_agent_input("planner", "Run tests"),
                ),
            ],
            results: vec![
                result_row_with_output(
                    "toolu_agent_1",
                    "launched",
                    serde_json::json!({"status": "async_launched"}),
                ),
                result_row_with_output(
                    "toolu_agent_2",
                    "launched",
                    serde_json::json!({"status": "async_launched"}),
                ),
            ],
        };

        let text = render_group(message).to_string();

        assert!(
            text.contains("2 background agents launched"),
            "canvas=\n{text}"
        );
        // Not a teammate spawn: the type is `userFacingName` and the
        // description keeps its parens (`UI.tsx:872-883`).
        assert!(text.contains("├─ reviewer (Run lint)"), "canvas=\n{text}");
        assert!(text.contains("└─ planner (Run tests)"), "canvas=\n{text}");
        // `isBackgrounded = isAsync && isResolved` drops both the usage
        // trailer and the status line (AgentProgressLine.tsx:88, :97).
        assert!(!text.contains("tool use"), "canvas=\n{text}");
        assert!(!text.contains("Done"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_background_teammate_spawn_keeps_name_and_type_like_official_agent_line() {
        let message = GroupedToolUseMessage {
            tool_name: "Task".to_string(),
            messages: vec![
                tool_use_row(
                    "agent-1",
                    "toolu_agent_1",
                    "Task",
                    named_agent_input("runner", "reviewer", "Run tests"),
                ),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Task",
                    named_agent_input("auditor", "planner", "Check logs"),
                ),
            ],
            results: vec![
                result_row_with_output(
                    "toolu_agent_1",
                    "spawned",
                    serde_json::json!({"status": "teammate_spawned"}),
                ),
                result_row_with_output(
                    "toolu_agent_2",
                    "spawned",
                    serde_json::json!({"status": "teammate_spawned"}),
                ),
            ],
        };

        let text = render_group(message).to_string();

        assert!(
            text.contains("2 background agents launched"),
            "canvas=\n{text}"
        );
        // CC `:861-871`: `@name` becomes the type and the custom subagent type
        // takes the description slot; the task description moves to
        // `taskDescription`, which a backgrounded row never shows.
        assert!(text.contains("├─ @runner (reviewer)"), "canvas=\n{text}");
        assert!(text.contains("└─ @auditor (planner)"), "canvas=\n{text}");
        assert!(!text.contains("Run tests"), "canvas=\n{text}");
        assert!(!text.contains("Done"), "canvas=\n{text}");
    }

    #[test]
    fn grouped_agent_tool_use_renders_background_launch_summary_without_expand_hint() {
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row("agent-1", "toolu_agent_1", "Agent", agent_input("Run lint")),
                tool_use_row(
                    "agent-2",
                    "toolu_agent_2",
                    "Agent",
                    agent_input("Run tests"),
                ),
            ],
            results: vec![
                result_row_with_output(
                    "toolu_agent_1",
                    "Async agent launched successfully.",
                    serde_json::json!({"status": "async_launched"}),
                ),
                result_row_with_output(
                    "toolu_agent_2",
                    "Remote agent launched in CCR.",
                    serde_json::json!({"status": "remote_launched"}),
                ),
            ],
        };

        let text = render_group(message).to_string();

        assert!(
            text.contains("2 background agents launched"),
            "canvas=\n{text}"
        );
        assert!(text.contains("├─ Run lint"), "canvas=\n{text}");
        assert!(text.contains("└─ Run tests"), "canvas=\n{text}");
        assert!(text.contains("↓ to manage"), "canvas=\n{text}");
        assert!(!text.contains("ctrl+o to expand"), "canvas=\n{text}");
        assert!(!text.contains("Done"), "canvas=\n{text}");
    }

    /// CC `:886-889`: `run_in_background: true` on the INPUT makes the member
    /// async even before any result arrives, and `every` then flips the whole
    /// headline (`:931`, `:943-949`).
    ///
    /// The fork gate is vetoed for the duration (`forkSubagent.ts:35`, the
    /// branch CC takes for every headless session) because `:888` reads
    /// `'run_in_background' in parsedInput.data` — a presence check on the
    /// STRIPPED input, and `AgentTool.tsx:252-254` omits the property from the
    /// schema whenever the fork gate is on. Without the veto zod drops the flag
    /// before this leg can see it and the test would assert the OTHER leg's
    /// absence.
    #[test]
    fn grouped_agent_run_in_background_input_marks_members_async() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _fork_vetoed = crate::tools::agent_tool::fork_subagent::fork_veto_environment();
        let _background = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        let mut first = agent_input("Run lint");
        first["run_in_background"] = serde_json::json!(true);
        let mut second = agent_input("Run tests");
        second["run_in_background"] = serde_json::json!(true);
        let message = GroupedToolUseMessage {
            tool_name: "Agent".to_string(),
            messages: vec![
                tool_use_row("agent-1", "toolu_agent_1", "Agent", first),
                tool_use_row("agent-2", "toolu_agent_2", "Agent", second),
            ],
            results: vec![
                result_row("toolu_agent_1", "done"),
                result_row("toolu_agent_2", "done"),
            ],
        };

        let text = render_group(message).to_string();

        assert!(
            text.contains("2 background agents launched"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("ctrl+o to expand"), "canvas=\n{text}");
    }
}
