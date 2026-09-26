//! Maps to: CC `components/Message.tsx`.
//! Dispatches typed transcript messages to the concrete renderers under
//! `components/messages/`.

use super::compact_summary::{
    CompactSummary, CompactSummaryDirection, CompactSummaryMetadata, CompactSummaryScreen,
};
use super::messages::*;
use super::messages_list::MessageLookups;
use crate::types::message::{
    RenderableMessage, RenderableMessageKind, SystemMessage, SystemMessageLevel,
    ToolUseProgressMessage, ToolUseStatus,
};
use iocraft::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Default, Props)]
pub struct MessageProps {
    pub message: RenderableMessage,
    pub add_margin: bool,
    pub can_animate: bool,
    pub verbose: bool,
    pub is_transcript_mode: bool,
    /// Cometix extension carriers (user-authorized L2, 2026-07-31; CC verbose
    /// pattern) — prop threading for the two display preferences.
    pub expand_thinking: bool,
    pub expand_collapsed_read_search: bool,
    /// Maps to: CC `Message.tsx` `inProgressToolUseIDs` / `lookups` props, the
    /// two inputs `AssistantToolUseMessage.tsx:120-121` derives a tool use's
    /// queued/running/resolved state from.
    pub in_progress_tool_use_ids: Arc<HashSet<String>>,
    pub lookups: Option<Arc<MessageLookups>>,
    /// Maps to: CC `Message.tsx:62` `tools: Tools` (destructured at `:87`) —
    /// the live main-loop pool arriving from `MessageRow.tsx:201`. Forwarded on
    /// to `GroupedToolUseContent` (`:250`); CC also hands it to `UserMessage`
    /// (`:121`), `AssistantMessage` (`:175`) and `CollapsedReadSearchContent`
    /// (`:275`), which this port's counterparts do not consume yet.
    pub tools: Arc<Vec<crate::types::tools::Tool>>,
}

#[component]
pub fn Message(props: &MessageProps) -> impl Into<AnyElement<'static>> {
    match &props.message.kind {
        RenderableMessageKind::User { message } => render_user(
            message,
            props.add_margin,
            props.verbose,
            props.is_transcript_mode,
            props.lookups.as_deref(),
        ),
        RenderableMessageKind::Assistant { message } => render_assistant(
            &props.message.uuid,
            message,
            props.add_margin,
            props.can_animate,
            props.verbose,
            props.is_transcript_mode,
            props.expand_thinking,
            &props.in_progress_tool_use_ids,
            props.lookups.as_deref(),
        ),
        RenderableMessageKind::System(message) => render_system(
            message,
            props.add_margin,
            props.verbose,
            props.is_transcript_mode,
        ),
        RenderableMessageKind::Attachment(attachment) => element! {
            AttachmentMessage(
                attachment: Some(attachment.clone()),
                add_margin: props.add_margin,
                verbose: props.verbose,
                is_transcript_mode: props.is_transcript_mode,
            )
        }
        .into_any(),
        // Maps to: CC `Messages.tsx:590` — progress messages are filtered out
        // before the render list, so `Message.tsx` has no 'progress' branch at
        // all. This arm exists because Rust's match must be total; a progress
        // message that reaches a direct `Message` mount renders nothing.
        RenderableMessageKind::Progress { .. } => element! { View(height: 0u32) {} }.into_any(),
        RenderableMessageKind::GroupedToolUse(message) => element! {
            GroupedToolUseContent(
                message: Some(message.clone()),
                add_margin: props.add_margin,
                in_progress_tool_use_ids: Arc::clone(&props.in_progress_tool_use_ids),
                lookups: props.lookups.clone(),
                // CC `Message.tsx:253` forwards `MessageRow`'s `shouldAnimate`;
                // `can_animate` IS that value here (`message_row.rs:118-119`).
                should_animate: props.can_animate,
                // CC `Message.tsx:250` `tools={tools}`.
                tools: Arc::clone(&props.tools),
            )
        }
        .into_any(),
        RenderableMessageKind::CollapsedReadSearch(message) => element! {
            CollapsedReadSearchContent(
                message: Some(message.clone()),
                add_margin: props.add_margin,
                can_animate: props.can_animate,
                verbose: props.verbose,
                is_transcript_mode: props.is_transcript_mode,
                expand_by_default: props.expand_collapsed_read_search,
            )
        }
        .into_any(),
    }
}

/// Maps to: CC `Message.tsx:140-193` (`case 'user'`) + the `UserMessage`
/// per-block switch (`Message.tsx:284-352`).
///
/// The row carries the real `UserMessage`; discrimination happens HERE, per
/// content block, exactly where CC performs it: `Message.tsx:170` maps
/// `message.message.content` and this maps `content.iter()`
/// (`normalize_messages` guarantees one block per row, so the map renders the
/// same output as the retired `block_index` single-block path). The
/// content-pattern layer (xml-tag if-chain) lives in `UserTextMessage`
/// (CC `UserTextMessage.tsx:48-189`); the tool-result dispatch
/// (lookups + content-prefix) lives in `UserToolResultMessage`
/// (CC `UserToolResultMessage.tsx:43-90`).
fn render_user(
    message: &crate::types::message::UserMessage,
    add_margin: bool,
    verbose: bool,
    is_transcript_mode: bool,
    lookups: Option<&MessageLookups>,
) -> AnyElement<'static> {
    use crate::types::message::UserContent;
    // CC Message.tsx:141-148: isCompactSummary renders the CompactSummary
    // screen before any block dispatch.
    if message.is_compact_summary {
        let content = crate::utils::messages::get_user_message_text(message).unwrap_or_default();
        return element! {
            CompactSummary(
                text_content: content,
                screen: if is_transcript_mode {
                    CompactSummaryScreen::Transcript
                } else {
                    CompactSummaryScreen::Main
                },
                // L1: project the existing render props from the original message,
                // as CC Message.tsx:143-147 forwards the complete message envelope.
                metadata: message.summarize_metadata.as_ref().map(|metadata| CompactSummaryMetadata {
                    messages_summarized: metadata.messages_summarized.unwrap_or_default(),
                    direction: if metadata.direction.as_deref() == Some("up_to") {
                        CompactSummaryDirection::UpTo
                    } else {
                        CompactSummaryDirection::From
                    },
                    user_context: metadata.user_context.clone().filter(|context| !context.is_empty()),
                }),
            )
        }
        .into_any();
    }
    // CC Message.tsx:149-164 precomputes imageIndices from imagePasteIds by
    // image position (`id ?? imagePosition`, where the fallback is the
    // 1-based position after increment).
    let mut image_indices: Vec<u32> = Vec::with_capacity(message.content.len());
    let mut image_position: u32 = 0;
    for param in &message.content {
        if matches!(
            param,
            UserContent::Image { .. }
                | UserContent::MetaImage { .. }
                | UserContent::RawImage { .. }
        ) {
            let id = message
                .image_paste_ids
                .as_ref()
                .and_then(|ids| ids.get(image_position as usize))
                .copied();
            image_position += 1;
            image_indices.push(id.unwrap_or(image_position));
        } else {
            image_indices.push(image_position);
        }
    }
    // CC Message.tsx:168-187: the per-block map inside a full-width column.
    let blocks: Vec<AnyElement<'static>> = message
        .content
        .iter()
        .enumerate()
        .map(|(index, block)| match block {
            UserContent::Text(text) => element! {
                // CC Message.tsx:321 hands message.planContent to
                // UserTextMessage, which renders it before any tag dispatch
                // (UserTextMessage.tsx:53).
                UserTextMessage(
                    content: text.clone(),
                    plan_content: message.plan_content.clone(),
                    add_margin: add_margin,
                    verbose: verbose,
                    is_transcript_mode: is_transcript_mode,
                )
            }
            .into_any(),
            UserContent::MetaText(_)
            | UserContent::MetaImage { .. }
            | UserContent::RawImage { is_meta: true, .. }
            | UserContent::MetaDocument { .. } => {
                // CC hides isMeta user messages in the render-list filter
                // (Messages.tsx:597 shouldShowUserMessage,
                // utils/messages.ts:4663); rows that still reach the renderer
                // render nothing.
                element! { View(height: 0u32) {} }.into_any()
            }
            UserContent::Image { .. } | UserContent::RawImage { is_meta: false, .. } => {
                let image_id = image_indices[index];
                element! { UserImageMessage(image_id: Some(image_id), add_margin: add_margin) }
                    .into_any()
            }
            UserContent::Document { .. } => {
                // CC `UserMessage`'s switch has no `document` arm — default
                // returns undefined (Message.tsx:349-351).
                element! { View(height: 0u32) {} }.into_any()
            }
            UserContent::ToolResult(tool_result) => {
                // CC UserToolResultMessage.tsx:43 resolves the tool via
                // lookups (useGetToolFromMessages) — the row does not store
                // the tool name.
                let tool_use_row = lookups.and_then(|l| {
                    l.tool_use_by_tool_use_id
                        .get(tool_result.tool_use_id.0.as_str())
                });
                let tool_name = tool_use_row
                    .and_then(|row| crate::components::message_row::assistant_tool_use_name(row))
                    .unwrap_or_default();
                let tool_input = tool_use_row
                    .and_then(|row| crate::components::message_row::assistant_tool_use_input(row));
                // Maps to: CC `progressMessagesForMessage` resolved from the
                // lookups for the paired tool_use.
                let progress_messages = lookups
                    .and_then(|l| {
                        l.progress_messages_by_tool_use_id
                            .get(tool_result.tool_use_id.0.as_str())
                    })
                    .cloned()
                    .unwrap_or_default();
                element! {
                    UserToolResultMessage(
                        tool_name: tool_name,
                        content: tool_result.content.clone(),
                        is_error: tool_result.is_error,
                        tool_use_result: tool_result.tool_use_result.clone(),
                        tool_input: tool_input,
                        progress_messages: progress_messages,
                        verbose: verbose,
                        is_transcript_mode: is_transcript_mode,
                    )
                }
                .into_any()
            }
        })
        .collect();
    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            #(blocks)
        }
    }
    .into_any()
}

/// Maps to: CC `Message.tsx:113-139` (`case 'assistant'`) +
/// `AssistantMessageBlock`'s per-block switch (`:415-489`).
///
/// The row carries the real `AssistantMessage`; discrimination happens HERE,
/// per content block, exactly where CC performs it: `Message.tsx:116` maps
/// `message.message.content` and this maps `content.iter()`
/// (`normalize_messages` guarantees one real block per row, so the map renders
/// the same output as the retired `block_index` single-block path). The
/// `MessageIdentity` sibling is Rust's out-of-band carrier for CC's envelope
/// fields — not a CC content block — and is skipped, exactly as the old
/// single-block path skipped it implicitly.
fn render_assistant(
    row_uuid: &str,
    message: &crate::types::message::AssistantMessage,
    add_margin: bool,
    can_animate: bool,
    verbose: bool,
    is_transcript_mode: bool,
    expand_thinking: bool,
    in_progress_tool_use_ids: &HashSet<String>,
    lookups: Option<&MessageLookups>,
) -> AnyElement<'static> {
    use crate::types::message::AssistantContent;

    // Maps to: CC `Message.tsx` branching on `message.isApiErrorMessage` — a
    // message-level field (`types/message.ts:41`). Cometix carries it as the
    // `MessageIdentity` content block (`AssistantMessageIdentity`), so the
    // renderer derives it from the message it now holds. `AssistantTextMessage`
    // still recognises the `"API Error"` prefix for rows restored from older
    // transcripts, which carry no identity block.
    let is_api_error_message = message.content.iter().any(|block| {
        matches!(
            block,
            AssistantContent::MessageIdentity(identity) if identity.is_api_error_message
        )
    });

    // CC Message.tsx:115-138: the per-block map inside a full-width column.
    let blocks: Vec<AnyElement<'static>> = message
        .content
        .iter()
        .filter(|block| !matches!(block, AssistantContent::MessageIdentity(_)))
        .map(|block| {
            render_assistant_block(
                block,
                row_uuid,
                is_api_error_message,
                add_margin,
                can_animate,
                verbose,
                is_transcript_mode,
                expand_thinking,
                in_progress_tool_use_ids,
                lookups,
            )
        })
        .collect();
    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            #(blocks)
        }
    }
    .into_any()
}

/// Maps to: CC `AssistantMessageBlock` (`Message.tsx:415-489`) — the switch
/// on `param.type`.
#[allow(clippy::too_many_arguments)]
fn render_assistant_block(
    block: &crate::types::message::AssistantContent,
    row_uuid: &str,
    is_api_error_message: bool,
    add_margin: bool,
    can_animate: bool,
    verbose: bool,
    is_transcript_mode: bool,
    expand_thinking: bool,
    in_progress_tool_use_ids: &HashSet<String>,
    lookups: Option<&MessageLookups>,
) -> AnyElement<'static> {
    use crate::types::message::AssistantContent;

    match block {
        AssistantContent::Text(text) => element! {
            AssistantTextMessage(
                content: text.clone(),
                add_margin: add_margin,
                should_show_dot: true,
                verbose: verbose,
                is_api_error_message: is_api_error_message,
            )
        }
        .into_any(),
        AssistantContent::Thinking { text, .. } => element! {
            AssistantThinkingMessage(
                content: text.clone(),
                // The per-row `expanded` seam died with `AssistantMessageKind`
                // — every constructor wrote false. CC drives expansion from
                // verbose/transcript mode.
                expanded: false,
                add_margin: add_margin,
                verbose: verbose,
                is_transcript_mode: is_transcript_mode,
                expand_by_default: expand_thinking,
            )
        }
        .into_any(),
        AssistantContent::RedactedThinking { .. } => element! {
            AssistantRedactedThinkingMessage(add_margin: add_margin)
        }
        .into_any(),
        AssistantContent::ToolUse(tool_use) => {
            let tool_use_id = if tool_use.id.0.is_empty() {
                row_uuid.to_string()
            } else {
                tool_use.id.0.clone()
            };
            // CC's AssistantToolUseMessage receives the raw ToolUseBlockParam
            // and derives everything else; the summary is render-time work.
            let description =
                crate::components::messages::assistant_tool_use_message::render_tool_use_message(
                    tool_use.name.as_str(),
                    &tool_use.input,
                    crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
                )
                .unwrap_or_default();
            // Maps to: CC `AssistantToolUseMessage.tsx:96`
            // `isTransparentWrapper: tool.isTransparentWrapper?.() ?? false` —
            // a render-time tool lookup, not a message type. The lookup happens
            // here rather than in the component because the component receives
            // the display name, not `param.name`.
            let is_transparent_wrapper =
                crate::services::tools::tool_execution::find_tool_call(tool_use.name.as_str())
                    .is_some_and(crate::tool::ToolCall::is_transparent_wrapper);
            // CC's AssistantToolUseMessage receives the raw `param.name` and
            // projects the user-facing name itself
            // (`AssistantToolUseMessage.tsx` `tool.userFacingName(...)`); the
            // component's by-tool-name dispatches (path links, per-tool
            // summaries, nonvisual checks) all match the canonical name, so
            // pre-projecting here made every non-Read file tool miss them and
            // fall back to generic rendering.
            element! {
                AssistantToolUseMessage(
                    tool_use_id: Some(tool_use_id.clone()),
                    tool_name: tool_use.name.clone(),
                    input: Some(tool_use.input.clone()),
                    description: description,
                    // CC `AssistantToolUseMessage.tsx:120-121` — derived from
                    // the live set plus lookups, never read off the row.
                    status: Some(derive_tool_use_status(
                        Some(tool_use_id.as_str()),
                        in_progress_tool_use_ids,
                        lookups,
                    )),
                    add_margin: add_margin,
                    can_animate: can_animate,
                    // Maps to: CC `MessageRow.tsx:154-155`
                    // `getProgressMessagesFromLookup(msg, lookups)`.
                    progress_messages: lookups
                        .and_then(|lookups| {
                            lookups.progress_messages_by_tool_use_id.get(tool_use_id.as_str())
                        })
                        .cloned()
                        .unwrap_or_default(),
                    is_transparent_wrapper: is_transparent_wrapper,
                    verbose: verbose,
                    is_transcript_mode: is_transcript_mode,
                    // Maps to: CC `Message.tsx:129`
                    // `inProgressToolCallCount={inProgressToolUseIDs.size}`.
                    in_progress_tool_call_count: Some(in_progress_tool_use_ids.len()),
                )
            }
            .into_any()
        }
        // CC `Message.tsx:467` dispatches on the content union; only the plain
        // result has text to show, and redacted/error render nothing here.
        AssistantContent::Advisor { content, .. } => match content.text() {
            Some(text) => element! {
                AdvisorMessage(content: text.to_string(), add_margin: add_margin)
            }
            .into_any(),
            None => element! { Fragment }.into_any(),
        },
        // Maps to: CC `AssistantMessageBlock`'s default arm — server tool
        // blocks without an advisor projection render nothing (`Message.tsx:
        // 481-489` logs and returns null). The identity arm is totality only:
        // `render_assistant` filters the out-of-band block before the map.
        AssistantContent::ServerToolUse(_)
        | AssistantContent::WebSearchToolResult { .. }
        | AssistantContent::MessageIdentity(_) => element! { View(height: 0u32) {} }.into_any(),
    }
}

/// Maps to: CC `Message.tsx:194-245` `case 'system'` (compact/microcompact/
/// local_command special cases) composed with `SystemTextMessage.tsx`'s
/// subtype ladder — the renderer discriminates on the model union directly
/// (batch D1).
fn render_system(
    message: &SystemMessage,
    add_margin: bool,
    verbose: bool,
    is_transcript_mode: bool,
) -> AnyElement<'static> {
    match message {
        SystemMessage::Informational { content, level, .. } => element! {
            SystemTextMessage(
                content: content.clone(),
                add_margin: add_margin,
                level: Some(*level),
            )
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:143-145` → `SystemAPIErrorMessage.tsx`:
        // countdown seconds derive from `retryInMs`.
        SystemMessage::ApiError {
            error,
            retry_in_ms,
            retry_attempt,
            max_retries,
            ..
        } => element! {
            SystemApiErrorMessage(
                error: error.clone(),
                retry_attempt: *retry_attempt,
                max_retries: *max_retries,
                retry_in_seconds: (*retry_in_ms / 1000) as u32,
                add_margin: add_margin,
            )
        }
        .into_any(),
        // CC `Message.tsx:195-202`.
        SystemMessage::CompactBoundary { .. } => element! {
            CompactBoundaryMessage(add_margin: add_margin)
        }
        .into_any(),
        // CC `Message.tsx:204-207`: renders null.
        SystemMessage::MicrocompactBoundary { .. } => element! {
            View(height: 0u32) {}
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:62-64` → TurnDurationMessage; the display
        // string derives from `durationMs` at render.
        SystemMessage::TurnDuration {
            duration_ms,
            budget_tokens,
            budget_limit,
            budget_nudges,
            ..
        } => element! {
            TurnDurationMessage(
                duration: crate::utils::format::format_duration(*duration_ms),
                duration_ms: Some(*duration_ms),
                budget_tokens: *budget_tokens,
                budget_limit: *budget_limit,
                budget_nudges: budget_nudges.unwrap_or(0),
                add_margin: add_margin,
            )
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:66-68,403-441`.
        SystemMessage::MemorySaved { written_paths, .. } => element! {
            SystemTextMessage(content: format!("Memory saved: {}", written_paths.join(", ")), add_margin: add_margin, level: Some(SystemMessageLevel::Info))
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:70-84`: dim content row.
        SystemMessage::AwaySummary { content, .. } => element! {
            SystemTextMessage(content: content.clone(), add_margin: add_margin, level: Some(SystemMessageLevel::Info))
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:112-114,483-509`.
        SystemMessage::BridgeStatus { content, .. } => element! {
            SystemTextMessage(content: format!("Bridge: {}", content), add_margin: add_margin, level: Some(SystemMessageLevel::Info))
        }
        .into_any(),
        SystemMessage::StopHookSummary {
            hook_label,
            hook_count,
            hook_infos,
            hook_errors,
            prevented_continuation,
            stop_reason,
            has_output,
            total_duration_ms,
            ..
        } => element! {
            StopHookSummaryMessage(
                add_margin: add_margin,
                verbose: verbose,
                is_transcript_mode: is_transcript_mode,
                hook_label: hook_label.clone(),
                hook_count: *hook_count,
                hook_infos: hook_infos.clone(),
                hook_errors: hook_errors.clone(),
                prevented_continuation: *prevented_continuation,
                stop_reason: stop_reason.clone(),
                has_output: *has_output,
                total_duration_ms: *total_duration_ms,
            )
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:126-134`: "Allowed {commands.join(', ')}".
        SystemMessage::PermissionRetry { commands, .. } => element! {
            SystemTextMessage(content: format!("Allowed {}", commands.join(", ")), add_margin: add_margin, level: Some(SystemMessageLevel::Info))
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:87-101`.
        SystemMessage::AgentsKilled { .. } => element! {
            SystemTextMessage(content: "All background agents stopped".to_string(), add_margin: add_margin, level: Some(SystemMessageLevel::Error))
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:103-109`: ant-only content row.
        SystemMessage::Thinking { content, .. } => element! {
            SystemTextMessage(content: format!("Thinking: {}", content), add_margin: add_margin, level: Some(SystemMessageLevel::Info))
        }
        .into_any(),
        // CC `SystemTextMessage.tsx:116-124`.
        SystemMessage::ScheduledTaskFire { content, .. } => element! {
            SystemTextMessage(content: format!("✻ {}", content), add_margin: add_margin, level: Some(SystemMessageLevel::Info))
        }
        .into_any(),
        // CC Message.tsx:228-237: local_command system rows render through
        // UserTextMessage (same tag dispatch as user text) and never enter
        // model history.
        SystemMessage::LocalCommand { content, .. } => element! {
            UserTextMessage(
                content: content.clone(),
                add_margin: add_margin,
                verbose: verbose,
                is_transcript_mode: is_transcript_mode,
            )
        }
        .into_any(),
        // CC renders null for both: neither subtype carries `content`, so
        // `SystemTextMessage.tsx:158-163` bails before the fallback row.
        SystemMessage::ApiMetrics { .. } | SystemMessage::FileSnapshot { .. } => {
            element! { View(height: 0u32) {} }.into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn render_prompt(text: &str, is_meta: bool) -> String {
        let message = RenderableMessage::user_block(
            "prompt",
            if is_meta {
                crate::types::message::UserContent::MetaText(text.to_string())
            } else {
                crate::types::message::UserContent::Text(text.to_string())
            },
        );
        futures::executor::block_on(async move {
            element! {
                ContextProvider(
                    value: Context::owned(*crate::utils::theme::get_theme(
                        crate::utils::theme::ThemeName::Dark,
                    )),
                ) {
                    Message(message: message)
                }
            }
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .next()
            .await
            .expect("message frame")
            .to_string()
        })
    }

    #[test]
    fn meta_prompt_is_model_only_while_normal_prompt_remains_visible() {
        assert!(!render_prompt("hidden init prompt", true).contains("hidden init prompt"));
        assert!(render_prompt("visible user prompt", false).contains("visible user prompt"));
    }

    #[test]
    fn partial_summary_message_matches_official_metadata_and_transcript() {
        use crate::types::message::{CompactMetadata, Message as RawMessage};
        for (direction, detail) in [("from", "from this point"), ("up_to", "up to this point")] {
            let mut user =
                crate::utils::messages::create_user_message("SUMMARY_BODY_SENTINEL".into());
            user.is_compact_summary = true;
            user.summarize_metadata = Some(CompactMetadata {
                messages_summarized: Some(3),
                direction: Some(direction.into()),
                user_context: Some("保留结论".into()),
                ..Default::default()
            });
            let rows = crate::utils::messages::normalize_messages(&[RawMessage::User(user)]);
            for transcript in [false, true] {
                let text = element! {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        Message(message: rows[0].clone(), is_transcript_mode: transcript)
                    }
                }
                .render(Some(100))
                .to_string();
                // CC Message.tsx:141-148 → CompactSummary.tsx:20-67: metadata
                // survives normalization and dispatch; transcript shows the body instead.
                assert!(text.contains("Summarized conversation"), "{text}");
                if transcript {
                    assert!(text.contains("SUMMARY_BODY_SENTINEL"), "{text}");
                    assert!(!text.contains("Context:"), "{text}");
                } else {
                    assert!(
                        text.contains(&format!("Summarized 3 messages {detail}")),
                        "{text}"
                    );
                    assert!(text.contains("Context: “保留结论”"), "{text}");
                    assert!(text.contains("ctrl+o to expand history"), "{text}");
                    assert!(!text.contains("SUMMARY_BODY_SENTINEL"), "{text}");
                }
            }
        }
    }
}
