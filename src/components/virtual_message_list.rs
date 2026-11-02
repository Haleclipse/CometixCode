//! Maps to: CC `components/VirtualMessageList.tsx`.
//!
//! The real CC component is tightly coupled to fullscreen `ScrollBox`, DOM
//! measurement, and React imperative handles. This Rust slice keeps the same
//! component boundary and ports the deterministic parts: sticky-prompt text,
//! key/range data shaping, search index math, scroll target math, and a
//! snapshot-driven virtual-list renderer. Live ScrollBoxHandle wiring remains
//! deferred to fullscreen virtual-scroll integration.

use crate::components::message_actions::strip_system_reminders;
use iocraft::prelude::*;
use std::collections::BTreeSet;

/// Maps to: CC `VirtualMessageList.tsx#HEADROOM`.
pub const HEADROOM: i64 = 3;
/// Maps to: CC `VirtualMessageList.tsx#STICKY_TEXT_CAP`.
pub const STICKY_TEXT_CAP: usize = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VirtualMessageKind {
    #[default]
    User,
    AttachmentQueuedCommand,
    Assistant,
    System,
    ToolResult,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtualTranscriptMessage {
    pub key: String,
    pub kind: VirtualMessageKind,
    /// First text block for user messages, or prompt text for queued commands.
    pub text: String,
    pub is_meta: bool,
    pub is_visible_in_transcript_only: bool,
    pub command_mode: Option<String>,
    pub search_text: Option<String>,
}

impl VirtualTranscriptMessage {
    pub fn user(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            kind: VirtualMessageKind::User,
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn queued_command(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            kind: VirtualMessageKind::AttachmentQueuedCommand,
            text: text.into(),
            ..Self::default()
        }
    }
}

/// Maps to: CC `VirtualMessageList.tsx#defaultExtractSearchText` fallback
/// lowering behavior.
pub fn default_extract_search_text(message: &VirtualTranscriptMessage) -> String {
    message
        .search_text
        .as_deref()
        .unwrap_or(&message.text)
        .to_lowercase()
}

/// Maps to: CC `Messages.tsx#extractSearchText` (:879-912), the extractor
/// `Messages.tsx` hands down as `VirtualMessageList`'s `extractSearchText` prop
/// (:103, :972).
///
/// For a tool_result row the owning tool's `extractSearchText` wins over the
/// field-name heuristic; `None` is CC's "tool didn't implement it" and keeps
/// the heuristic (:899-901). Callers with no tool-lookup path fall back to
/// [`default_extract_search_text`] (`VirtualMessageList.tsx:40-46`). CC lowers
/// once here because the per-keystroke match loop only does `indexOf`.
pub fn extract_search_text(
    message: &VirtualTranscriptMessage,
    tool_result: Option<(&dyn crate::tool::ToolCall, &crate::tool::ToolOutput)>,
) -> String {
    match tool_result.and_then(|(tool, data)| tool.extract_search_text(data)) {
        Some(text) => text.to_lowercase(),
        None => default_extract_search_text(message),
    }
}

/// Maps to: CC `VirtualMessageList.tsx#computeStickyPromptText`.
pub fn compute_sticky_prompt_text(message: &VirtualTranscriptMessage) -> Option<String> {
    let raw = match message.kind {
        VirtualMessageKind::User => {
            if message.is_meta || message.is_visible_in_transcript_only {
                return None;
            }
            message.text.as_str()
        }
        VirtualMessageKind::AttachmentQueuedCommand => {
            if message.command_mode.as_deref() == Some("task-notification") || message.is_meta {
                return None;
            }
            message.text.as_str()
        }
        _ => return None,
    };
    let text = strip_system_reminders(raw);
    if text.starts_with('<') || text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Maps to: CC `VirtualMessageList.tsx#targetFor`.
pub fn target_for_item_top(item_top: i64) -> i64 {
    (item_top - HEADROOM).max(0)
}

/// Maps to: CC incremental `keysRef` update in `VirtualMessageList`.
pub fn update_virtual_keys(
    previous_keys: &[String],
    previous_first_key: Option<&str>,
    messages: &[VirtualTranscriptMessage],
    item_key_changed: bool,
) -> Vec<String> {
    if item_key_changed
        || messages.len() < previous_keys.len()
        || messages.first().map(|message| message.key.as_str()) != previous_first_key
    {
        return messages.iter().map(|message| message.key.clone()).collect();
    }
    let mut keys = previous_keys.to_vec();
    for message in messages.iter().skip(keys.len()) {
        keys.push(message.key.clone());
    }
    keys
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchIndexState {
    pub matches: Vec<usize>,
    pub prefix_sum: Vec<usize>,
    pub ptr: usize,
    pub total: usize,
    pub placeholder_current: usize,
}

fn count_occurrences(text: &str, query: &str) -> usize {
    if query.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    let mut search_from = 0usize;
    while let Some(pos) = text[search_from..].find(query) {
        count += 1;
        search_from += pos + query.len();
    }
    count
}

/// Maps to: CC `JumpHandle.setSearchQuery` match/prefix/nearest-anchor math.
pub fn build_search_index_state(
    messages: &[VirtualTranscriptMessage],
    query: &str,
    offsets: &[i64],
    start: usize,
    first_top: Option<i64>,
    scroll_top: i64,
    anchor_scroll_top: Option<i64>,
) -> SearchIndexState {
    let lowered_query = query.to_lowercase();
    let mut matches = Vec::<usize>::new();
    let mut prefix_sum = vec![0usize];
    if !lowered_query.is_empty() {
        for (index, message) in messages.iter().enumerate() {
            // CC resolves the row's tool through `lookups.toolUseByToolUseID` +
            // `findToolByName` (Messages.tsx:892-895). That lookup and the
            // typed tool output it feeds are not carried on the transcript DTO
            // yet, so every row still takes the heuristic branch.
            let text = extract_search_text(message, None);
            let count = count_occurrences(&text, &lowered_query);
            if count > 0 {
                matches.push(index);
                prefix_sum.push(prefix_sum.last().copied().unwrap_or(0) + count);
            }
        }
    }

    let total = prefix_sum.last().copied().unwrap_or(0);
    let mut ptr = 0usize;
    let origin = first_top
        .filter(|top| *top >= 0)
        .map(|top| top - offsets.get(start).copied().unwrap_or(0))
        .unwrap_or(0);
    if !matches.is_empty() {
        let current_top = anchor_scroll_top.unwrap_or(scroll_top);
        let mut best = i64::MAX;
        for (k, msg_index) in matches.iter().enumerate() {
            let candidate = origin + offsets.get(*msg_index).copied().unwrap_or(0);
            let distance = (candidate - current_top).abs();
            if distance <= best {
                best = distance;
                ptr = k;
            }
        }
    }
    let placeholder_current = if matches.is_empty() {
        0
    } else {
        prefix_sum.get(ptr + 1).copied().unwrap_or(total)
    };

    SearchIndexState {
        matches,
        prefix_sum,
        ptr,
        total,
        placeholder_current,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StickyPromptSnapshot {
    pub idx: Option<usize>,
    pub text: Option<String>,
    pub estimate: i64,
    pub first_visible: usize,
}

fn collapse_sticky_text(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let paragraph_end = trimmed
        .find("\n\n")
        .or_else(|| trimmed.find("\n \n"))
        .unwrap_or(trimmed.len());
    let collapsed = trimmed[..paragraph_end]
        .chars()
        .take(STICKY_TEXT_CAP)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

/// Maps to: CC `VirtualMessageList.tsx#StickyTracker` first-visible, sticky
/// prompt selection, duplicate-visible skip, and estimate math.
pub fn compute_sticky_prompt_snapshot(
    messages: &[VirtualTranscriptMessage],
    start: usize,
    end: usize,
    offsets: &[i64],
    item_tops: &[Option<i64>],
    scroll_top: i64,
    pending_delta: i64,
    is_sticky: bool,
) -> StickyPromptSnapshot {
    let target = (scroll_top + pending_delta).max(0);
    let mut first_visible = start;
    let mut first_visible_top = -1i64;
    let clamped_end = end.min(messages.len()).min(item_tops.len());
    for i in (start..clamped_end).rev() {
        if let Some(top) = item_tops[i]
            && top >= 0
        {
            if top < target {
                break;
            }
            first_visible_top = top;
        }
        first_visible = i;
    }

    let mut idx = None;
    let mut text = None;
    if first_visible > 0 && !is_sticky {
        for i in (0..first_visible).rev() {
            let Some(candidate) = compute_sticky_prompt_text(&messages[i]) else {
                continue;
            };
            if let Some(top) = item_tops.get(i).and_then(|top| *top)
                && top >= 0
                && top + 1 >= target
            {
                continue;
            }
            idx = Some(i);
            text = collapse_sticky_text(&candidate);
            break;
        }
    }

    let base_offset = if first_visible_top >= 0 {
        first_visible_top - offsets.get(first_visible).copied().unwrap_or(0)
    } else {
        0
    };
    let estimate = idx
        .and_then(|i| offsets.get(i).copied())
        .map(|offset| (base_offset + offset).max(0))
        .unwrap_or(-1);

    StickyPromptSnapshot {
        idx,
        text,
        estimate,
        first_visible,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtualVisibleItem {
    pub index: usize,
    pub key: String,
    pub text: String,
    pub clickable: bool,
    pub hovered: bool,
    pub expanded: bool,
}

/// Maps to: CC `VirtualMessageList.tsx` render `messages.slice(start,end)`
/// per-item clickable/hover/expanded shaping.
pub fn virtual_visible_items(
    messages: &[VirtualTranscriptMessage],
    start: usize,
    end: usize,
    clickable_keys: &BTreeSet<String>,
    expanded_keys: &BTreeSet<String>,
    hovered_key: Option<&str>,
) -> Vec<VirtualVisibleItem> {
    messages
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(index, message)| {
            let clickable = clickable_keys.contains(&message.key);
            VirtualVisibleItem {
                index,
                key: message.key.clone(),
                text: message.text.clone(),
                clickable,
                hovered: clickable && hovered_key == Some(message.key.as_str()),
                expanded: expanded_keys.contains(&message.key),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtualMessageListData {
    pub messages: Vec<VirtualTranscriptMessage>,
    pub start: usize,
    pub end: usize,
    pub top_spacer: u32,
    pub bottom_spacer: u32,
    pub clickable_keys: BTreeSet<String>,
    pub expanded_keys: BTreeSet<String>,
    pub hovered_key: Option<String>,
}

#[derive(Default, Props)]
pub struct VirtualMessageListProps {
    pub data: VirtualMessageListData,
}

/// Maps to: CC `components/VirtualMessageList.tsx#VirtualMessageList` render
/// shape (top spacer, mounted slice, bottom spacer). Runtime measurement,
/// cursor nav refs, search imperative refs, and sticky scroll subscriptions are
/// deferred to the iocraft fullscreen virtual-scroll integration.
#[component]
pub fn VirtualMessageList(props: &VirtualMessageListProps) -> impl Into<AnyElement<'static>> {
    let data = props.data.clone();
    let visible_items = virtual_visible_items(
        &data.messages,
        data.start,
        data.end,
        &data.clickable_keys,
        &data.expanded_keys,
        data.hovered_key.as_deref(),
    );

    element! {
        View(flex_direction: FlexDirection::Column) {
            #((data.top_spacer > 0).then(|| element! {
                View(height: data.top_spacer, flex_shrink: 0.0f32)
            }))
            #(visible_items.into_iter().map(|item| element! {
                View(flex_direction: FlexDirection::Column, padding_bottom: if item.expanded { 1u32 } else { 0u32 }) {
                    Text(content: format!("{}{}", if item.hovered { "› " } else if item.clickable { "  " } else { "" }, item.text), color: if item.hovered { Color::Cyan } else { Color::Reset }, wrap: TextWrap::Wrap)
                }
            }).collect::<Vec<_>>())
            #((data.bottom_spacer > 0).then(|| element! {
                View(height: data.bottom_spacer, flex_shrink: 0.0f32)
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(key: &str, text: &str) -> VirtualTranscriptMessage {
        VirtualTranscriptMessage::user(key, text)
    }

    /// Stands in for a tool whose `extractSearchText` is either absent or
    /// returns the official empty-string opt-out (`FileReadTool.ts:414-416`).
    struct SearchTextTool(Option<&'static str>);

    impl crate::tool::ToolCall for SearchTextTool {
        fn name(&self) -> &'static str {
            "SearchTextTool"
        }

        // Test-only fixture; the trait member is required (CC Tool.ts:518).
        fn prompt(
            &self,
            tool: &crate::types::tools::Tool,
            _options: &crate::tool::ToolPromptOptions<'_>,
        ) -> String {
            tool.description.clone()
        }

        fn extract_search_text(&self, _data: &crate::tool::ToolOutput) -> Option<String> {
            self.0.map(str::to_string)
        }

        fn call<'a>(
            &'a self,
            _args: &'a serde_json::Value,
            _request: &'a crate::types::permissions::PermissionRequest,
            _context: &'a crate::tool::ToolUseContext,
            _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
            _parent_message: Option<&'a crate::types::message::AssistantMessage>,
            _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
        ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
            unreachable!("search-text stub is never executed")
        }
    }

    /// Maps to: CC `Messages.tsx:896-901`.
    #[test]
    fn tool_owned_search_text_wins_over_the_heuristic_but_only_when_implemented() {
        let message = VirtualTranscriptMessage {
            search_text: Some("Heuristic Text".to_string()),
            ..user("t1", "row text")
        };
        let output = crate::tool::ToolOutput::Composed {
            content: String::new(),
            status: crate::types::message::ToolResultStatus::Success,
        };

        assert_eq!(
            extract_search_text(&message, None),
            "heuristic text",
            "no tool binding keeps the fallback extractor"
        );
        let unimplemented = SearchTextTool(None);
        assert_eq!(
            extract_search_text(&message, Some((&unimplemented, &output))),
            "heuristic text",
            "an unimplemented method keeps the heuristic"
        );
        let indexing = SearchTextTool(Some("Tool Owned"));
        assert_eq!(
            extract_search_text(&message, Some((&indexing, &output))),
            "tool owned"
        );
        let opted_out = SearchTextTool(Some(""));
        assert_eq!(
            extract_search_text(&message, Some((&opted_out, &output))),
            "",
            "an empty string is the tool opting out of the index"
        );
    }

    #[test]
    fn virtual_sticky_prompt_text_matches_official_filters() {
        assert_eq!(
            compute_sticky_prompt_text(&user("u1", "hello")),
            Some("hello".to_string())
        );
        assert_eq!(
            compute_sticky_prompt_text(&user(
                "u2",
                "<system-reminder>remember</system-reminder>real prompt"
            )),
            Some("real prompt".to_string())
        );
        assert_eq!(
            compute_sticky_prompt_text(&user("xml", "<bash-stdout>x</bash-stdout>")),
            None
        );
        assert_eq!(
            compute_sticky_prompt_text(&VirtualTranscriptMessage {
                is_meta: true,
                ..user("meta", "hidden")
            }),
            None
        );
        assert_eq!(
            compute_sticky_prompt_text(&VirtualTranscriptMessage::queued_command("q", "queued")),
            Some("queued".to_string())
        );
        assert_eq!(
            compute_sticky_prompt_text(&VirtualTranscriptMessage {
                command_mode: Some("task-notification".to_string()),
                ..VirtualTranscriptMessage::queued_command("q", "queued")
            }),
            None
        );
    }

    #[test]
    fn virtual_keys_update_append_fast_path_and_rebuild_conditions() {
        let messages = vec![user("a", "A"), user("b", "B"), user("c", "C")];
        assert_eq!(
            update_virtual_keys(
                &["a".to_string(), "b".to_string()],
                Some("a"),
                &messages,
                false
            ),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            update_virtual_keys(&["old".to_string()], Some("old"), &messages, false),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            update_virtual_keys(
                &[
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                    "d".to_string()
                ],
                Some("a"),
                &messages,
                false
            ),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn virtual_search_index_counts_occurrences_and_chooses_nearest_anchor_with_later_ties() {
        let messages = vec![user("a", "foo one foo"), user("b", "bar"), user("c", "foo")];
        let state =
            build_search_index_state(&messages, "FOO", &[0, 10, 20], 0, Some(0), 10, Some(10));
        assert_eq!(state.matches, vec![0, 2]);
        assert_eq!(state.prefix_sum, vec![0, 2, 3]);
        assert_eq!(state.total, 3);
        assert_eq!(state.ptr, 1);
        assert_eq!(state.placeholder_current, 3);
    }

    #[test]
    fn virtual_sticky_snapshot_matches_first_visible_and_estimate_math() {
        let messages = vec![
            user("p0", "first prompt\n\nsecond paragraph"),
            VirtualTranscriptMessage {
                kind: VirtualMessageKind::Assistant,
                key: "a1".to_string(),
                text: "answer".to_string(),
                ..VirtualTranscriptMessage::default()
            },
            user("p2", "visible prompt"),
            VirtualTranscriptMessage {
                kind: VirtualMessageKind::Assistant,
                key: "a2".to_string(),
                text: "answer 2".to_string(),
                ..VirtualTranscriptMessage::default()
            },
        ];
        let snap = compute_sticky_prompt_snapshot(
            &messages,
            1,
            4,
            &[0, 5, 10, 15],
            &[Some(0), Some(5), Some(10), Some(15)],
            8,
            0,
            false,
        );
        assert_eq!(snap.first_visible, 2);
        assert_eq!(snap.idx, Some(0));
        assert_eq!(snap.text.as_deref(), Some("first prompt"));
        assert_eq!(snap.estimate, 0);

        let sticky = compute_sticky_prompt_snapshot(
            &messages,
            1,
            4,
            &[0, 5, 10, 15],
            &[Some(0), Some(5), Some(10), Some(15)],
            8,
            0,
            true,
        );
        assert_eq!(sticky.text, None);
    }

    #[test]
    fn virtual_visible_items_shape_click_hover_and_expanded_state() {
        let messages = vec![user("a", "A"), user("b", "B"), user("c", "C")];
        let clickable = BTreeSet::from(["b".to_string()]);
        let expanded = BTreeSet::from(["b".to_string()]);
        let items = virtual_visible_items(&messages, 1, 3, &clickable, &expanded, Some("b"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].index, 1);
        assert!(items[0].clickable);
        assert!(items[0].hovered);
        assert!(items[0].expanded);
        assert!(!items[1].clickable);
    }

    #[test]
    fn virtual_target_for_uses_headroom() {
        assert_eq!(target_for_item_top(10), 7);
        assert_eq!(target_for_item_top(2), 0);
    }

    #[test]
    fn virtual_message_list_renders_spacers_and_slice() {
        let text = element! {
            VirtualMessageList(data: VirtualMessageListData {
                messages: vec![user("a", "A"), user("b", "B"), user("c", "C")],
                start: 1,
                end: 3,
                top_spacer: 1,
                bottom_spacer: 1,
                clickable_keys: BTreeSet::from(["b".to_string()]),
                expanded_keys: BTreeSet::from(["b".to_string()]),
                hovered_key: Some("b".to_string()),
            })
        }
        .render(Some(80))
        .to_string();
        assert!(!text.contains('A'), "canvas=\n{text}");
        assert!(text.contains("› B"), "canvas=\n{text}");
        assert!(text.contains('C'), "canvas=\n{text}");
    }
}
