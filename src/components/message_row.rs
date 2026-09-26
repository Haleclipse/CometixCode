//! Maps to: CC `components/MessageRow.tsx`.
//! The row boundary mirrors Claude Code's native-scrollback safeguards: static
//! rows reuse their retained child tree, while dynamic rows are wrapped in
//! `OffscreenFreeze` so timer/spinner updates stop once they move into terminal
//! scrollback.

use super::message::Message;
use super::messages_list::MessageLookups;
use crate::types::message::{RenderableMessage, RenderableMessageKind, SystemMessage};
use iocraft::prelude::*;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Default, Props)]
pub struct MessageRowProps {
    pub messages: Arc<Vec<RenderableMessage>>,
    pub index: usize,
    /// Maps to: CC `Messages.tsx:792-795` — `conversationId` is part of every
    /// row key (`${msg.uuid}-${conversationId}`), so a compact reset's bump
    /// (REPL.tsx:3461-3463) invalidates cached row subtrees here too.
    pub conversation_id: u64,
    pub add_margin: bool,
    /// Whether the previous renderable row is also a user row.
    pub is_user_continuation: bool,
    /// Whether non-skippable content follows this row. Currently only used by
    /// collapsed read/search rows in the official implementation.
    pub has_content_after: bool,
    /// Animation gate owned by the row layer in official `MessageRow.tsx`.
    pub can_animate: bool,
    /// Overall query loading state, needed by active collapsed groups.
    pub is_loading: bool,
    /// Shared official `buildMessageLookups(...)`-style output.
    pub lookups: Option<Arc<MessageLookups>>,
    /// Official tool/message UI expansion gate.
    pub verbose: bool,
    /// Ctrl+O transcript mode expansion gate.
    pub is_transcript_mode: bool,
    /// Cometix: expand thinking blocks by default.
    pub expand_thinking: bool,
    /// Cometix: expand collapsed read/search groups by default.
    pub expand_collapsed_read_search: bool,
    /// Terminal columns, included in the row memo key because wrapping changes
    /// visible output even when message content is unchanged.
    pub columns: u16,
    /// Maps to: CC `Messages.tsx:260` `inProgressToolUseIDs: Set<string>`,
    /// threaded down from REPL-owned state (`REPL.tsx:1897`).
    pub in_progress_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `MessageRow.tsx:40` `streamingToolUseIDs: Set<string>`,
    /// reduced from REPL-owned `streamingToolUses` (`REPL.tsx:1255`).
    pub streaming_tool_use_ids: Arc<HashSet<String>>,
    /// Maps to: CC `MessageRow.tsx:121` `tools` (destructured from the row
    /// props, declared on `Props` beside `commands`) — the live main-loop pool
    /// arriving from `Messages.tsx:822`, forwarded to `Message` at `:201`.
    ///
    /// Deliberately absent from [`MessageRowRenderKey`]: CC's
    /// `areMessageRowPropsEqual` (`MessageRow.tsx:306-356`) never compares
    /// `tools`, so a resolved row keeps its rendered tree when the pool
    /// changes. The pool-change re-render happens one level up, where CC's
    /// `Messages` comparator does compare it (`Messages.tsx:1079-1087`).
    pub tools: Arc<Vec<crate::types::tools::Tool>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MessageRowRenderKey {
    message: RenderableMessage,
    /// CC row-key generation term (`Messages.tsx:793`).
    conversation_id: u64,
    add_margin: bool,
    is_user_continuation: bool,
    has_content_after: bool,
    verbose: bool,
    is_transcript_mode: bool,
    expand_thinking: bool,
    expand_collapsed_read_search: bool,
    columns: u16,
}

#[derive(Default)]
pub struct MessageRow {
    last_static_key: Option<MessageRowRenderKey>,
}

impl Component for MessageRow {
    type Props<'a> = MessageRowProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let Some(message) = props.messages.get(props.index).cloned() else {
            if self.last_static_key.is_none() {
                return;
            }
            self.last_static_key = None;
            let mut style = iocraft::taffy::style::Style::default();
            style.flex_direction = iocraft::taffy::style::FlexDirection::Column;
            style.size.width = iocraft::taffy::style::Dimension::percent(1.0);
            updater.set_layout_style_if_changed(style);
            updater.update_children(Vec::<AnyElement<'static>>::new(), None);
            return;
        };

        let next_static_key = message_row_static_key(&message, props);
        if next_static_key.is_some() && self.last_static_key.as_ref() == next_static_key.as_ref() {
            return;
        }
        let static_cache_key = next_static_key.as_ref().map(message_row_cache_key);
        self.last_static_key = next_static_key;
        let message = apply_row_runtime_state(message, props);
        // CC `MessageRow.tsx:172-186`: computed AFTER the runtime-state pass,
        // so a collapsed group reads the same liveness both places.
        let should_animate =
            should_animate_row(&message, props.can_animate, &props.in_progress_tool_use_ids);

        let mut style = iocraft::taffy::style::Style::default();
        style.flex_direction = iocraft::taffy::style::FlexDirection::Column;
        style.size.width = iocraft::taffy::style::Dimension::percent(1.0);
        updater.set_layout_style_if_changed(style);

        if let Some(cache_key) = static_cache_key {
            updater.update_children(
                std::iter::once(element! {
                    OffscreenFreeze(skip_poll: true) {
                        CachedSubtree(cache_key: cache_key, damage_on_restore: false) {
                            View(width: 100pct, flex_direction: FlexDirection::Column) {
                                Message(
                                    message: message,
                                    add_margin: props.add_margin,
                                    can_animate: should_animate,
                                    verbose: props.verbose,
                                    is_transcript_mode: props.is_transcript_mode,
                                    expand_thinking: props.expand_thinking,
                                    expand_collapsed_read_search: props.expand_collapsed_read_search,
                                    in_progress_tool_use_ids: Arc::clone(&props.in_progress_tool_use_ids),
                                    lookups: props.lookups.clone(),
                                    // CC `MessageRow.tsx:201` `tools={tools}`.
                                    tools: Arc::clone(&props.tools),
                                )
                            }
                        }
                    }
                }),
                None,
            );
        } else {
            updater.update_children(
                std::iter::once(element! {
                    OffscreenFreeze(skip_poll: true) {
                        View(width: 100pct, flex_direction: FlexDirection::Column) {
                            Message(
                                message: message,
                                add_margin: props.add_margin,
                                can_animate: should_animate,
                                verbose: props.verbose,
                                is_transcript_mode: props.is_transcript_mode,
                                expand_thinking: props.expand_thinking,
                                expand_collapsed_read_search: props.expand_collapsed_read_search,
                                in_progress_tool_use_ids: Arc::clone(&props.in_progress_tool_use_ids),
                                lookups: props.lookups.clone(),
                                // CC `MessageRow.tsx:201` `tools={tools}`.
                                tools: Arc::clone(&props.tools),
                            )
                        }
                    }
                }),
                None,
            );
        }
    }
}

fn message_row_cache_key(key: &MessageRowRenderKey) -> String {
    let mut hasher = DefaultHasher::new();
    format!("{key:?}").hash(&mut hasher);
    // `${msg.uuid}-${conversationId}` (CC Messages.tsx:793) plus the content
    // hash: a compact reset's generation bump invalidates cached subtrees even
    // for a row whose uuid and content survive the reset.
    format!(
        "message-row-{}-{}-{:x}",
        key.message.uuid,
        key.conversation_id,
        hasher.finish()
    )
}

fn message_row_static_key(
    message: &RenderableMessage,
    props: &MessageRowProps,
) -> Option<MessageRowRenderKey> {
    message_row_static_key_from_parts(
        message,
        props.conversation_id,
        props.add_margin,
        props.is_user_continuation,
        props.has_content_after,
        props.lookups.as_deref(),
        props.verbose,
        props.is_transcript_mode,
        props.expand_thinking,
        props.expand_collapsed_read_search,
        props.columns,
        &props.in_progress_tool_use_ids,
        &props.streaming_tool_use_ids,
    )
}

#[allow(clippy::too_many_arguments)]
fn message_row_static_key_from_parts(
    message: &RenderableMessage,
    conversation_id: u64,
    add_margin: bool,
    is_user_continuation: bool,
    has_content_after: bool,
    lookups: Option<&MessageLookups>,
    verbose: bool,
    is_transcript_mode: bool,
    expand_thinking: bool,
    expand_collapsed_read_search: bool,
    columns: u16,
    in_progress_tool_use_ids: &HashSet<String>,
    streaming_tool_use_ids: &HashSet<String>,
) -> Option<MessageRowRenderKey> {
    should_render_statically(
        message,
        lookups,
        is_transcript_mode,
        in_progress_tool_use_ids,
        streaming_tool_use_ids,
    )
    .then(|| MessageRowRenderKey {
        message: message.clone(),
        conversation_id,
        add_margin,
        is_user_continuation,
        has_content_after,
        verbose,
        is_transcript_mode,
        expand_thinking,
        expand_collapsed_read_search,
        columns,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn message_row_static_memo_key(
    message: &RenderableMessage,
    conversation_id: u64,
    add_margin: bool,
    is_user_continuation: bool,
    has_content_after: bool,
    lookups: Option<&MessageLookups>,
    verbose: bool,
    is_transcript_mode: bool,
    expand_thinking: bool,
    expand_collapsed_read_search: bool,
    columns: u16,
    in_progress_tool_use_ids: &HashSet<String>,
    streaming_tool_use_ids: &HashSet<String>,
) -> Option<String> {
    message_row_static_key_from_parts(
        message,
        conversation_id,
        add_margin,
        is_user_continuation,
        has_content_after,
        lookups,
        verbose,
        is_transcript_mode,
        expand_thinking,
        expand_collapsed_read_search,
        columns,
        in_progress_tool_use_ids,
        streaming_tool_use_ids,
    )
    .as_ref()
    .map(message_row_cache_key)
}

/// The row's block is the first non-identity block — normalize
/// guarantees one real block per row.
fn assistant_tool_use_id(message: &RenderableMessage) -> Option<&str> {
    match &message.kind {
        RenderableMessageKind::Assistant { message } => match message.first_content_block() {
            Some(crate::types::message::AssistantContent::ToolUse(tool_use))
                if !tool_use.id.0.is_empty() =>
            {
                Some(tool_use.id.0.as_str())
            }
            _ => None,
        },
        _ => None,
    }
}

/// CC `useGetToolFromMessages` (UserToolResultMessage.tsx:43) resolves
/// the source tool through lookups — the result row does not store the tool
/// name. This reads it from the tool_use block on the looked-up row.
pub(crate) fn assistant_tool_use_name(message: &RenderableMessage) -> Option<String> {
    match &message.kind {
        RenderableMessageKind::Assistant { message } => match message.first_content_block() {
            Some(crate::types::message::AssistantContent::ToolUse(tool_use)) => {
                Some(tool_use.name.clone())
            }
            _ => None,
        },
        _ => None,
    }
}

/// The lookup companion to [`assistant_tool_use_name`]: CC's reject leaf
/// renders from the paired tool_use INPUT (`UserToolRejectMessage.tsx:14`).
pub(crate) fn assistant_tool_use_input(message: &RenderableMessage) -> Option<serde_json::Value> {
    match &message.kind {
        RenderableMessageKind::Assistant { message } => match message.first_content_block() {
            Some(crate::types::message::AssistantContent::ToolUse(tool_use)) => {
                Some(tool_use.input.clone())
            }
            _ => None,
        },
        _ => None,
    }
}

/// The ids covered by a group's `results` rows. CC's grouped renderer
/// pairs members with results the same way (`GroupedToolUseContent.tsx:39-48`);
/// here the presence of a member's result row stands in for
/// `resolvedToolUseIDs` on `lookups: None` mounts.
fn grouped_result_ids(group: &crate::types::message::GroupedToolUseMessage) -> HashSet<&str> {
    group
        .results
        .iter()
        .filter_map(|row| match &row.kind {
            RenderableMessageKind::User { message } => match message.first_content_block() {
                Some(crate::types::message::UserContent::ToolResult(tool_result))
                    if !tool_result.tool_use_id.0.is_empty() =>
                {
                    Some(tool_result.tool_use_id.0.as_str())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn is_assistant_tool_use(message: &RenderableMessage) -> bool {
    matches!(
        &message.kind,
        RenderableMessageKind::Assistant { message } if matches!(
            message.first_content_block(),
            Some(crate::types::message::AssistantContent::ToolUse(_))
        )
    )
}

/// Maps to: CC `Messages.tsx:1101-1140` `shouldRenderStatically`.
pub(crate) fn should_render_statically(
    message: &RenderableMessage,
    lookups: Option<&MessageLookups>,
    is_transcript_mode: bool,
    in_progress_tool_use_ids: &HashSet<String>,
    streaming_tool_use_ids: &HashSet<String>,
) -> bool {
    if is_transcript_mode {
        return true;
    }

    match &message.kind {
        RenderableMessageKind::Attachment(_) => true,
        // Progress never reaches the render list (`Messages.tsx:590` filters
        // it before this decision in CC); totality arm only.
        RenderableMessageKind::Progress { .. } => true,
        RenderableMessageKind::User {
            message: user_message,
        } => match user_message.first_content_block() {
            Some(crate::types::message::UserContent::ToolResult(tool_result))
                if !tool_result.tool_use_id.0.is_empty() =>
            {
                sibling_tools_resolved(&tool_result.tool_use_id.0, lookups)
            }
            _ => true,
        },
        RenderableMessageKind::Assistant { .. } if is_assistant_tool_use(message) => {
            match assistant_tool_use_id(message) {
                Some(tool_use_id) => {
                    // CC `Messages.tsx:1122-1127`, in CC's order: streaming
                    // first, then in-progress. Both sets are REPL-owned state
                    // (`REPL.tsx:1255,1897`), not re-derived from this row — a
                    // row that carried its own status made the transcript the
                    // authority and inverted CC's direction.
                    if streaming_tool_use_ids.contains(tool_use_id)
                        || in_progress_tool_use_ids.contains(tool_use_id)
                    {
                        return false;
                    }
                    sibling_tools_resolved(tool_use_id, lookups)
                }
                None => true,
            }
        }
        RenderableMessageKind::Assistant { .. } => true,
        RenderableMessageKind::System(SystemMessage::ApiError { .. }) => false,
        RenderableMessageKind::System(_) => true,
        // CC `Messages.tsx:1142-1151`: a group is static when every member
        // row's tool_use id is in `lookups.resolvedToolUseIDs` (a member
        // without a tool_use block fails the `every`). The Rust arm keeps its
        // two existing extensions — the live-set guard and, for
        // `lookups: None` mounts, treating a present result row in
        // `group.results` as resolved — but sources both from the rows now
        // that the group carries no derived state (types/message.ts:140-144).
        RenderableMessageKind::GroupedToolUse(group) => {
            let result_ids = grouped_result_ids(group);
            group.messages.iter().all(|member| {
                assistant_tool_use_id(member).is_some_and(|tool_use_id| {
                    !in_progress_tool_use_ids.contains(tool_use_id)
                        && (result_ids.contains(tool_use_id)
                            || lookups
                                .map(|lookups| lookups.resolved_tool_use_ids.contains(tool_use_id))
                                .unwrap_or(false))
                })
            })
        }
        RenderableMessageKind::CollapsedReadSearch(_) => false,
    }
}

/// Maps to: CC `MessageRow.tsx:172-186`.
///
/// ```ts
/// let shouldAnimate = false
/// if (canAnimate) {
///   if (isGrouped) {
///     shouldAnimate = msg.messages.some(m => {
///       const content = m.message.content[0]
///       return content?.type === 'tool_use' && inProgressToolUseIDs.has(content.id)
///     })
///   } else if (isCollapsed) {
///     shouldAnimate = hasAnyToolInProgress(msg, inProgressToolUseIDs)
///   } else {
///     const toolUseID = getToolUseID(msg)
///     shouldAnimate = !toolUseID || inProgressToolUseIDs.has(toolUseID)
///   }
/// }
/// ```
///
/// `canAnimate` is the outer gate (the query is running); which rows actually
/// animate is a per-row question answered by the live set. Cometix passed the
/// gate straight through, so every row spun for the duration of a turn instead
/// of only the row whose tool was executing.
///
/// The non-tool branch keeps CC's `!toolUseID ||`: a row with no tool use at
/// all — the streaming assistant text — animates whenever the gate is open.
fn should_animate_row(
    message: &RenderableMessage,
    can_animate: bool,
    in_progress_tool_use_ids: &HashSet<String>,
) -> bool {
    if !can_animate {
        return false;
    }
    match &message.kind {
        RenderableMessageKind::GroupedToolUse(group) => group.messages.iter().any(|member| {
            assistant_tool_use_id(member)
                .is_some_and(|tool_use_id| in_progress_tool_use_ids.contains(tool_use_id))
        }),
        RenderableMessageKind::CollapsedReadSearch(group) => {
            crate::utils::collapse_read_search::has_any_tool_in_progress(
                group,
                in_progress_tool_use_ids,
            )
        }
        RenderableMessageKind::Assistant { .. } if is_assistant_tool_use(message) => {
            match assistant_tool_use_id(message) {
                Some(tool_use_id) => in_progress_tool_use_ids.contains(tool_use_id),
                None => true,
            }
        }
        _ => true,
    }
}

fn apply_row_runtime_state(
    mut message: RenderableMessage,
    props: &MessageRowProps,
) -> RenderableMessage {
    // Maps to: CC `MessageRow.tsx:144-147`
    //
    //   const isActiveCollapsedGroup =
    //     isCollapsed &&
    //     (hasAnyToolInProgress(msg, inProgressToolUseIDs) ||
    //       (isLoading && !hasContentAfter))
    //
    // with CC's own note that `hasAnyToolInProgress` takes priority: "if tools
    // are running, always show active regardless of what else is in the message
    // list (avoids false finalization during parallel execution)".
    //
    // Cometix had only the second disjunct, so a collapsed group whose tools
    // were still executing finalized early whenever anything followed it —
    // exactly the parallel-execution case the CC comment calls out — and it
    // showed as active while merely loading with no live tool at all.
    if let RenderableMessageKind::CollapsedReadSearch(group) = &mut message.kind {
        if crate::utils::collapse_read_search::has_any_tool_in_progress(
            group,
            &props.in_progress_tool_use_ids,
        ) || (props.is_loading && !props.has_content_after)
        {
            group.active = true;
        }
    }

    // Progress hydration for tool-use rows is gone with the row field itself:
    // `render_assistant` reads `lookups.progress_messages_by_tool_use_id`
    // directly, matching CC `MessageRow.tsx:154-155` /
    // `getProgressMessagesFromLookup` (utils/messages.ts:1435-1443).

    // The Agent display variant (and its embedded progress carrier) is
    // gone — tool_result rows render progress through the component props
    // channel (CC `progressMessagesForMessage`), resolved from
    // `lookups.progress_messages_by_tool_use_id` at the mount site.

    message
}

fn sibling_tools_resolved(tool_use_id: &str, lookups: Option<&MessageLookups>) -> bool {
    let Some(lookups) = lookups else {
        return true;
    };

    lookups
        .sibling_tool_use_ids
        .get(tool_use_id)
        .map(|sibling_ids| {
            sibling_ids
                .iter()
                .all(|id| lookups.resolved_tool_use_ids.contains(id))
        })
        .unwrap_or_else(|| lookups.resolved_tool_use_ids.contains(tool_use_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{
        GroupedToolUseMessage, SystemMessageLevel, ToolResultStatus, ToolUseProgressMessage,
    };
    use std::collections::BTreeSet;

    fn user_message(id: &str) -> RenderableMessage {
        RenderableMessage::user(id, "hello")
    }

    fn api_error(id: &str) -> RenderableMessage {
        RenderableMessage {
            uuid: id.to_string(),
            kind: RenderableMessageKind::System(SystemMessage::ApiError {
                base: crate::types::message::SystemBase::with_uuid(id),
                error: "rate limited".to_string(),
                retry_in_ms: 2000,
                retry_attempt: 1,
                max_retries: 3,
            }),
        }
    }

    fn system_text(id: &str) -> RenderableMessage {
        RenderableMessage::system_notice(id, "notice", SystemMessageLevel::Info)
    }

    fn tool_use(id: &str, tool_use_id: &str) -> RenderableMessage {
        RenderableMessage::assistant_block(
            id,
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(tool_use_id.to_string()),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "echo hi"}),
            }),
        )
    }

    fn tool_result(id: &str, tool_use_id: &str) -> RenderableMessage {
        RenderableMessage::user_tool_result(id, tool_use_id, "ok", false)
    }

    fn resolved_lookup(tool_use_id: &str) -> MessageLookups {
        let mut lookups = MessageLookups::default();
        lookups
            .resolved_tool_use_ids
            .insert(tool_use_id.to_string());
        lookups.sibling_tool_use_ids.insert(
            tool_use_id.to_string(),
            BTreeSet::from([tool_use_id.to_string()]),
        );
        lookups
    }

    #[test]
    fn should_render_statically_matches_official_prompt_mode_core_cases() {
        let lookups = resolved_lookup("toolu_1");
        let idle = HashSet::new();
        let live: HashSet<String> = ["toolu_1".to_string()].into_iter().collect();

        assert!(should_render_statically(
            &user_message("u1"),
            Some(&lookups),
            false,
            &idle,
            &idle
        ));
        assert!(should_render_statically(
            &system_text("s1"),
            Some(&lookups),
            false,
            &idle,
            &idle
        ));
        assert!(!should_render_statically(
            &api_error("api1"),
            Some(&lookups),
            false,
            &idle,
            &idle
        ));
        // CC `Messages.tsx:1122-1127`: liveness comes from the REPL-owned set,
        // not from anything stored on the row — the same row is dynamic while
        // its id is in the set and static once the actor removes it.
        let row = tool_use("tool1", "toolu_1");
        assert!(!should_render_statically(
            &row,
            Some(&lookups),
            false,
            &live,
            &idle
        ));
        assert!(should_render_statically(
            &row,
            Some(&lookups),
            false,
            &idle,
            &idle
        ));
        // CC `Messages.tsx:1122` checks the streaming set FIRST: a tool_use
        // block that has streamed in but not started executing is in neither
        // the live set nor the resolved set, and would otherwise go static.
        assert!(!should_render_statically(
            &row,
            Some(&lookups),
            false,
            &idle,
            &live
        ));
        assert!(should_render_statically(
            &tool_result("result1", "toolu_1"),
            Some(&lookups),
            false,
            &idle,
            &idle
        ));
    }

    #[test]
    fn should_render_statically_keeps_collapsed_groups_dynamic_in_prompt_mode() {
        let collapsed = RenderableMessage {
            uuid: "collapsed".to_string(),
            kind: RenderableMessageKind::CollapsedReadSearch(Default::default()),
        };
        let idle = HashSet::new();

        assert!(!should_render_statically(
            &collapsed, None, false, &idle, &idle
        ));
        assert!(should_render_statically(
            &collapsed, None, true, &idle, &idle
        ));
    }

    #[test]
    fn static_row_key_ignores_loading_and_animation_props_like_official_comparator() {
        let message = user_message("u1");
        let base = MessageRowProps {
            messages: Arc::new(vec![message.clone()]),
            index: 0,
            can_animate: false,
            is_loading: false,
            ..Default::default()
        };
        let loading = MessageRowProps {
            messages: Arc::clone(&base.messages),
            index: base.index,
            can_animate: true,
            is_loading: true,
            ..Default::default()
        };

        assert_eq!(
            message_row_static_key(&message, &base),
            message_row_static_key(&message, &loading)
        );
    }

    #[test]
    fn collapsed_read_search_uses_loading_prop_for_active_row_state_like_official() {
        let collapsed = RenderableMessage {
            uuid: "collapsed".to_string(),
            kind: RenderableMessageKind::CollapsedReadSearch(Default::default()),
        };
        let props = MessageRowProps {
            messages: Arc::new(vec![collapsed.clone()]),
            index: 0,
            is_loading: true,
            has_content_after: false,
            ..Default::default()
        };

        let message = apply_row_runtime_state(collapsed, &props);
        match message.kind {
            RenderableMessageKind::CollapsedReadSearch(group) => assert!(group.active),
            _ => panic!("expected collapsed read/search message"),
        }
    }

    /// Maps to: CC `MessageRow.tsx:172-186`.
    #[test]
    fn only_live_rows_animate_while_the_gate_is_open() {
        let idle = HashSet::new();
        let live: HashSet<String> = ["toolu_1".to_string()].into_iter().collect();
        let row = tool_use("tool1", "toolu_1");

        // The gate is the outer condition; which rows animate is per-row.
        assert!(!should_animate_row(&row, false, &live));
        assert!(should_animate_row(&row, true, &live));
        // Cometix passed the gate straight through, so this used to be true:
        // every row spun for the whole turn, not just the executing one.
        assert!(!should_animate_row(&row, true, &idle));

        // CC's `!toolUseID ||`: a row with no tool use animates on the gate
        // alone — that is the streaming assistant text.
        assert!(should_animate_row(&user_message("u1"), true, &idle));
    }

    /// Maps to: CC `MessageRow.tsx:144-147` — `hasAnyToolInProgress` is the
    /// first disjunct and, per CC's own comment, takes priority "regardless of
    /// what else is in the message list (avoids false finalization during
    /// parallel execution)".
    #[test]
    fn collapsed_read_search_stays_active_while_a_member_tool_is_live() {
        let group = crate::types::message::CollapsedReadSearchGroup {
            verbose_entries: vec![crate::types::message::CollapsedReadSearchEntry::ToolUse {
                tool_name: "Read".to_string(),
                input: None,
                tool_use_id: Some("toolu_live".to_string()),
                description: "Read src/main.rs".to_string(),
                status: crate::types::message::ToolUseStatus::Queued,
            }],
            ..Default::default()
        };
        let collapsed = RenderableMessage {
            uuid: "collapsed".to_string(),
            kind: RenderableMessageKind::CollapsedReadSearch(group),
        };
        // The second disjunct is off: the query settled and content follows.
        let settled = MessageRowProps {
            messages: Arc::new(vec![collapsed.clone()]),
            index: 0,
            is_loading: false,
            has_content_after: true,
            ..Default::default()
        };
        let live = MessageRowProps {
            in_progress_tool_use_ids: Arc::new(
                ["toolu_live".to_string()]
                    .into_iter()
                    .collect::<HashSet<_>>(),
            ),
            ..MessageRowProps {
                messages: Arc::new(vec![collapsed.clone()]),
                index: 0,
                is_loading: false,
                has_content_after: true,
                ..Default::default()
            }
        };

        let idle_row = apply_row_runtime_state(collapsed.clone(), &settled);
        match idle_row.kind {
            RenderableMessageKind::CollapsedReadSearch(group) => assert!(!group.active),
            _ => panic!("expected collapsed read/search message"),
        }

        let live_row = apply_row_runtime_state(collapsed, &live);
        match live_row.kind {
            RenderableMessageKind::CollapsedReadSearch(group) => assert!(group.active),
            _ => panic!("expected collapsed read/search message"),
        }
    }

    // The row-hydration test that lived here is gone with the hydration
    // itself: the tool-use row no longer carries progress, and
    // `render_assistant` reads `lookups.progress_messages_by_tool_use_id`
    // directly (CC `MessageRow.tsx:154-155`). Collection is pinned by
    // `build_message_lookups_tracks_progress_messages_by_tool_use_id`.

    #[test]
    fn should_render_statically_requires_grouped_tool_results_to_settle() {
        // Settled-ness comes from the rows — a member with no matching
        // result row (and no lookups) keeps the group transient; adding the
        // result row settles it.
        let unresolved = RenderableMessage {
            uuid: "group".to_string(),
            kind: RenderableMessageKind::GroupedToolUse(GroupedToolUseMessage {
                tool_name: "Bash".to_string(),
                messages: vec![tool_use("tool1", "toolu_1")],
                results: Vec::new(),
            }),
        };
        assert!(!should_render_statically(
            &unresolved,
            None,
            false,
            &HashSet::new(),
            &HashSet::new()
        ));

        let resolved = RenderableMessage {
            uuid: "group".to_string(),
            kind: RenderableMessageKind::GroupedToolUse(GroupedToolUseMessage {
                tool_name: "Bash".to_string(),
                messages: vec![tool_use("tool1", "toolu_1")],
                results: vec![tool_result("result1", "toolu_1")],
            }),
        };
        assert!(should_render_statically(
            &resolved,
            None,
            false,
            &HashSet::new(),
            &HashSet::new()
        ));
    }
}
