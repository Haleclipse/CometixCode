//! Maps to: CC `components/MessageSelector.tsx`.
//!
//! This module keeps the official Rewind/MessageSelector UI in Rust with CC's
//! ownership shape: `selected_index` / `message_to_restore` are component
//! state (CC useState), keyboard input goes through the keybinding runtime
//! (`messageSelector:*` + `confirm:no`), and the caller only provides the
//! original messages plus restore/close callbacks. The selector subscribes to
//! file history and owns the metadata and selected-point diff like CC.

use crate::components::custom_select::{
    Select, SelectInputOptionData, SelectLayout, SelectOptionData,
};
use crate::components::design_system::divider::Divider;
use crate::components::spinner::Spinner;
use crate::constants::figures;
use crate::types::message::{AssistantContent, Message, UserContent, UserMessage};
// Maps to: CC MessageSelector.tsx:48-57 — tags come from constants/xml.
// (CC spells bash-input / command-args as literals there; both live in
// constants/xml.ts, so they are imported alongside.)
use crate::constants::xml::{
    BASH_INPUT_TAG, BASH_STDERR_TAG, BASH_STDOUT_TAG, COMMAND_ARGS_TAG, COMMAND_MESSAGE_TAG,
    LOCAL_COMMAND_STDERR_TAG, LOCAL_COMMAND_STDOUT_TAG, TASK_NOTIFICATION_TAG,
    TEAMMATE_MESSAGE_TAG, TICK_TAG,
};
use crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::format::format_relative_time_ago_millis;
use crate::utils::messages::{extract_tag, is_empty_message_text};
use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// Maps to: CC `MessageSelector.tsx:92` `MAX_VISIBLE_MESSAGES = 7` — the
/// pick list always windows to 7 options, centered on the selection with
/// top/bottom clamping, and truncates silently (no "more above/below"
/// indicators, no scroll arrows).
pub const MAX_VISIBLE_MESSAGES: usize = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RestoreOption {
    #[default]
    Both,
    Conversation,
    Code,
    Summarize,
    SummarizeUpTo,
    Nevermind,
}

impl RestoreOption {
    pub fn official_value(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Conversation => "conversation",
            Self::Code => "code",
            Self::Summarize => "summarize",
            Self::SummarizeUpTo => "summarize_up_to",
            Self::Nevermind => "nevermind",
        }
    }
}

/// Maps to: CC `MessageSelector.tsx#isSummarizeOption`.
pub fn is_summarize_option(option: Option<RestoreOption>) -> bool {
    matches!(
        option,
        Some(RestoreOption::Summarize | RestoreOption::SummarizeUpTo)
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreOptionConfig {
    pub value: RestoreOption,
    pub label: String,
    pub input_placeholder: Option<String>,
    pub allow_empty_submit_to_cancel: bool,
    pub show_label_with_value: bool,
    pub label_value_separator: Option<String>,
}

impl RestoreOptionConfig {
    fn plain(value: RestoreOption, label: &str) -> Self {
        Self {
            value,
            label: label.to_string(),
            input_placeholder: None,
            allow_empty_submit_to_cancel: false,
            show_label_with_value: false,
            label_value_separator: None,
        }
    }

    fn summarize(value: RestoreOption, label: &str) -> Self {
        Self {
            value,
            label: label.to_string(),
            input_placeholder: Some("add context (optional)".to_string()),
            allow_empty_submit_to_cancel: true,
            show_label_with_value: true,
            label_value_separator: Some(": ".to_string()),
        }
    }
}

/// Maps to: CC `MessageSelector.tsx#getRestoreOptions`.
///
/// The `include_ant_only_summarize_up_to` parameter is the build-audience gate
/// represented in CC by `"external" === 'ant'`.
pub fn get_restore_options(
    can_restore_code: bool,
    include_ant_only_summarize_up_to: bool,
) -> Vec<RestoreOptionConfig> {
    let mut options = if can_restore_code {
        vec![
            RestoreOptionConfig::plain(RestoreOption::Both, "Restore code and conversation"),
            RestoreOptionConfig::plain(RestoreOption::Conversation, "Restore conversation"),
            RestoreOptionConfig::plain(RestoreOption::Code, "Restore code"),
        ]
    } else {
        vec![RestoreOptionConfig::plain(
            RestoreOption::Conversation,
            "Restore conversation",
        )]
    };

    options.push(RestoreOptionConfig::summarize(
        RestoreOption::Summarize,
        "Summarize from here",
    ));
    if include_ant_only_summarize_up_to {
        options.push(RestoreOptionConfig::summarize(
            RestoreOption::SummarizeUpTo,
            "Summarize up to here",
        ));
    }
    options.push(RestoreOptionConfig::plain(
        RestoreOption::Nevermind,
        "Never mind",
    ));
    options
}

/// Maps to: CC `utils/fileHistory.ts` `DiffStats` as consumed by
/// `MessageSelector.tsx` — re-exported from the file-history subsystem.
pub use crate::utils::file_history::DiffStats;

fn last_text_block(message: &UserMessage) -> Option<&str> {
    match message.content.last() {
        Some(UserContent::Text(text)) => Some(text.trim()),
        _ => None,
    }
}

fn contains_tag(text: &str, tag: &str) -> bool {
    text.contains(&format!("<{tag}>")) || text.contains(&format!("<{tag} "))
}

pub(crate) fn is_non_user_authored_message_text(text: &str) -> bool {
    [
        LOCAL_COMMAND_STDOUT_TAG,
        LOCAL_COMMAND_STDERR_TAG,
        BASH_STDOUT_TAG,
        BASH_STDERR_TAG,
        TASK_NOTIFICATION_TAG,
        TICK_TAG,
    ]
    .iter()
    .any(|tag| text.contains(&format!("<{tag}>")))
        || text.contains(&format!("<{TEAMMATE_MESSAGE_TAG}"))
}

/// Maps to: CC `MessageSelector.tsx#selectableUserMessagesFilter`.
pub fn selectable_user_messages_filter(message: &Message) -> bool {
    let Message::User(user) = message else {
        return false;
    };
    if matches!(
        user.content.first(),
        Some(
            UserContent::ToolResult(_)
                | UserContent::MetaText(_)
                | UserContent::MetaImage { .. }
                | UserContent::RawImage { is_meta: true, .. }
                | UserContent::MetaDocument { .. }
        )
    ) || crate::utils::messages::is_synthetic_message(message)
        || user.is_compact_summary
        || user.is_visible_in_transcript_only
    {
        return false;
    }
    !is_non_user_authored_message_text(last_text_block(user).unwrap_or(""))
}

/// Maps to: CC MessageSelector.tsx:109-120, useMemo([messages]).
/// L1: retain raw message indices, rather than cloning image payloads for options.
pub fn build_message_selector_options(messages: &[Message]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| selectable_user_messages_filter(message).then_some(index))
        .chain(std::iter::once(messages.len()))
        .collect()
}

/// Maps to: CC `MessageSelector.tsx` `firstVisibleIndex` calculation, with
/// the visible-count made dynamic (terminal-height derived) following the
/// resume LogSelector pattern (`log_selector.rs` visible_count).
pub fn first_visible_message_index(selected_index: usize, option_count: usize) -> usize {
    first_visible_message_index_for(selected_index, option_count, MAX_VISIBLE_MESSAGES)
}

pub fn first_visible_message_index_for(
    selected_index: usize,
    option_count: usize,
    visible_count: usize,
) -> usize {
    let visible_count = visible_count.max(1);
    let half = visible_count / 2;
    selected_index
        .saturating_sub(half)
        .min(option_count.saturating_sub(visible_count))
}

/// Maps to: CC `MessageSelector.tsx#UserMessageOption` display derivation.
pub fn user_message_option_display(
    user_message: &UserMessage,
    is_current: bool,
    padding_right: Option<usize>,
    columns: usize,
) -> String {
    if is_current {
        return "(current)".to_string();
    }

    let raw_message_text = last_text_block(user_message)
        .unwrap_or("(no prompt)")
        .trim();
    let message_text = crate::utils::display_tags::strip_display_tags(raw_message_text);

    if is_empty_message_text(&message_text) {
        return "((empty message))".to_string();
    }

    if contains_tag(&message_text, BASH_INPUT_TAG)
        && let Some(input) =
            extract_tag(&message_text, BASH_INPUT_TAG).filter(|text| !text.is_empty())
    {
        return format!("! {input}");
    }

    if contains_tag(&message_text, COMMAND_MESSAGE_TAG) {
        let command_message = extract_tag(&message_text, COMMAND_MESSAGE_TAG);
        let args = extract_tag(&message_text, COMMAND_ARGS_TAG).unwrap_or_default();
        let is_skill_format = extract_tag(&message_text, "skill-format").as_deref() == Some("true");
        if let Some(command_message) = command_message.filter(|text| !text.is_empty()) {
            if is_skill_format {
                return format!("Skill({command_message})");
            }
            return format!("/{command_message} {args}");
        }
    }

    if let Some(padding_right) = padding_right.filter(|padding| *padding > 0) {
        // Maps to: CC `truncate(messageText, columns - paddingRight, true)` —
        // the singleLine flag keeps multi-line prompts to one row in the
        // pick list.
        crate::utils::truncate::truncate(&message_text, columns.saturating_sub(padding_right), true)
    } else {
        String::from_utf16_lossy(&message_text.encode_utf16().take(500).collect::<Vec<_>>())
            .split('\n')
            .take(4)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Maps to: CC `MessageSelector.tsx#DiffStatsText`.
pub fn diff_stats_text(diff_stats: Option<&DiffStats>) -> Option<String> {
    let diff_stats = diff_stats?;
    if diff_stats.files_changed.is_empty() {
        return None;
    }
    Some(format!(
        "+{} -{}",
        diff_stats.insertions, diff_stats.deletions
    ))
}

/// Maps to: CC `MessageSelector.tsx#getRestoreOptionConversationText`.
pub fn get_restore_option_conversation_text(option: RestoreOption) -> &'static str {
    match option {
        RestoreOption::Summarize => "Messages after this point will be summarized.",
        RestoreOption::SummarizeUpTo => {
            "Preceding messages will be summarized. This and subsequent messages will remain unchanged — you will stay at the end of the conversation."
        }
        RestoreOption::Both | RestoreOption::Conversation => "The conversation will be forked.",
        RestoreOption::Code | RestoreOption::Nevermind => "The conversation will be unchanged.",
    }
}

/// Maps to: CC `MessageSelector.tsx#RestoreCodeConfirmation` file-label
/// derivation (basename / "a and b" / "a and N other files").
pub fn restore_code_file_label(diff_stats: &DiffStats) -> String {
    let num_files_changed = diff_stats.files_changed.len();
    match num_files_changed {
        0 => String::new(),
        1 => basename(&diff_stats.files_changed[0]),
        2 => format!(
            "{} and {}",
            basename(&diff_stats.files_changed[0]),
            basename(&diff_stats.files_changed[1])
        ),
        _ => format!(
            "{} and {} other files",
            basename(&diff_stats.files_changed[0]),
            num_files_changed - 1
        ),
    }
}

/// Maps to: CC `MessageSelector.tsx#RestoreCodeConfirmation`.
pub fn restore_code_confirmation_text(
    diff_stats_for_restore: Option<&DiffStats>,
) -> Option<String> {
    let diff_stats = diff_stats_for_restore?;
    if diff_stats.files_changed.is_empty() {
        return Some("The code has not changed (nothing will be restored).".to_string());
    }

    Some(format!(
        "The code will be restored {} in {}.",
        diff_stats_text(Some(diff_stats)).unwrap_or_default(),
        restore_code_file_label(diff_stats)
    ))
}

/// Maps to: CC `MessageSelector.tsx#RestoreOptionDescription`.
pub fn restore_option_description_lines(
    selected_restore_option: RestoreOption,
    can_restore_code: bool,
    diff_stats_for_restore: Option<&DiffStats>,
) -> Vec<String> {
    let mut lines = vec![get_restore_option_conversation_text(selected_restore_option).to_string()];
    if !is_summarize_option(Some(selected_restore_option)) {
        let show_code_restore = can_restore_code
            && matches!(
                selected_restore_option,
                RestoreOption::Both | RestoreOption::Code
            );
        if show_code_restore {
            if let Some(text) = restore_code_confirmation_text(diff_stats_for_restore) {
                lines.push(text);
            }
        } else {
            lines.push("The code will be unchanged.".to_string());
        }
    }
    lines
}

/// Maps to: CC `MessageSelector.tsx#computeDiffStatsBetweenMessages`.
pub fn compute_diff_stats_between_messages(
    messages: &[Message],
    from_message_id: &str,
    to_message_id: Option<&str>,
) -> Option<DiffStats> {
    let start = messages
        .iter()
        .position(|message| message.uuid() == from_message_id)?;
    let end = to_message_id
        .and_then(|id| messages.iter().position(|m| m.uuid() == id))
        .unwrap_or(messages.len());
    let mut stats = DiffStats::default();
    for message in messages.iter().take(end).skip(start + 1) {
        let Message::User(user) = message else {
            continue;
        };
        let Some(UserContent::ToolResult(result)) = user.content.first() else {
            continue;
        };
        let Some(result) = result.tool_use_result.as_ref() else {
            continue;
        };
        let Some(path) = result
            .get("filePath")
            .and_then(serde_json::Value::as_str)
            .filter(|p| !p.is_empty())
        else {
            continue;
        };
        let Some(patch) = result
            .get("structuredPatch")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        if !stats.files_changed.iter().any(|file| file == path) {
            stats.files_changed.push(path.to_string());
        }
        if result.get("type").and_then(serde_json::Value::as_str) == Some("create") {
            if let Some(content) = result.get("content").and_then(serde_json::Value::as_str) {
                stats.insertions += content.split('\n').count();
            }
        } else {
            for hunk in patch {
                let Some(lines) = hunk.get("lines").and_then(serde_json::Value::as_array) else {
                    break;
                };
                for line in lines.iter().filter_map(serde_json::Value::as_str) {
                    stats.insertions += usize::from(line.starts_with('+'));
                    stats.deletions += usize::from(line.starts_with('-'));
                }
            }
        }
    }
    Some(stats)
}

/// Maps to: CC MessageSelector.tsx#messagesAfterAreOnlySynthetic.
pub fn messages_after_are_only_synthetic(messages: &[Message], from_index: usize) -> bool {
    messages.iter().skip(from_index + 1).all(|message| {
        if crate::utils::messages::is_synthetic_message(message) {
            return true;
        }
        match message {
            Message::User(user) => matches!(
                user.content.first(),
                Some(
                    UserContent::ToolResult(_)
                        | UserContent::MetaText(_)
                        | UserContent::MetaImage { .. }
                        | UserContent::RawImage { is_meta: true, .. }
                        | UserContent::MetaDocument { .. }
                )
            ),
            Message::Assistant(assistant) => !assistant.content.iter().any(|block| {
                matches!(block, AssistantContent::Text(text) if !text.trim().is_empty())
                    || matches!(block, AssistantContent::ToolUse(_))
            }),
            _ => true,
        }
    })
}

/// Keybinding handlers must be `Send + 'static`, so they cannot capture the
/// `HandlerMut` props. They set this pending action and the component body
/// dispatches the CC handleSelect / handleEscape logic — the same seam as
/// PromptInput's take_pending_* pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingSelectorAction {
    Select,
    Escape,
}

/// L1 async callback carrier for CC MessageSelectorProps.onRestoreCode.
pub type RestoreCodeCallback =
    Arc<dyn Fn(String) -> futures::future::BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub type SummarizeCallback = Arc<
    dyn Fn(
            String,
            Option<String>,
            crate::types::message::PartialCompactDirection,
        ) -> futures::future::BoxFuture<'static, Result<(), String>>
        + Send
        + Sync,
>;

#[derive(Default, Props)]
pub struct MessageSelectorProps<'a> {
    /// Maps to: CC `messages` prop — the live transcript Arc, passed by
    /// reference (zero-copy). The component derives the selectable options
    /// via `use_memo` keyed on this Arc's pointer, mirroring CC's
    /// useMemo([messages]) reference semantics.
    pub messages: Arc<Vec<Message>>,
    /// Maps to: CC `onRestoreMessage` — rewind to just before this uuid
    /// (restoreConversationDirectly path).
    pub on_restore_message: Option<RestoreCodeCallback>,
    pub on_summarize: Option<SummarizeCallback>,
    /// Maps to CC onPreRestore; the REPL owns request cancellation.
    pub on_pre_restore: HandlerMut<'a, ()>,
    /// Maps to CC onRestoreCode. Optional only for provider-less callers.
    pub on_restore_code: Option<RestoreCodeCallback>,
    /// Maps to: CC `onClose`.
    pub on_close: HandlerMut<'a, ()>,
    /// Maps to: CC `preselectedMessage` — skip pick-list, land on confirm.
    /// Carried in the selector's own message shape (the dialog auto-rewind
    /// path that feeds it is not wired yet).
    pub preselected_message: Option<UserMessage>,
    /// 0 = derive from the live terminal width (CC useTerminalSize).
    pub columns: usize,
}

/// Maps to: CC `MessageSelector.tsx#UserMessageOption` — "(current)" and
/// "((empty message))" render italic; bash inputs split the "!" prefix into
/// the bashBorder color. Text derivation lives in
/// `user_message_option_display`.
fn user_message_option_element(
    message: &UserMessage,
    is_current: bool,
    padding_right: Option<usize>,
    columns: usize,
    color: Option<Color>,
    theme: &crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let display = user_message_option_display(message, is_current, padding_right, columns);
    let italic = display == "(current)" || display == "((empty message))";
    let raw = last_text_block(message).unwrap_or("");
    let is_bash = raw.contains("<bash-input>")
        && extract_tag(raw, BASH_INPUT_TAG).is_some_and(|text| !text.is_empty());
    if let Some(input) = display.strip_prefix("! ").filter(|_| is_bash) {
        let input = input.to_string();
        let bash_border = theme.bash_border;
        let text_color = color.unwrap_or(Color::Reset);
        return element! {
            View(flex_direction: FlexDirection::Row, width: 100pct) {
                Text(content: "!", color: bash_border, wrap: TextWrap::Wrap)
                Text(content: format!(" {input}"), color: text_color, wrap: TextWrap::Wrap)
            }
        }
        .into_any();
    }
    element! {
        Text(
            content: display,
            color: color.unwrap_or(Color::Reset),
            italic: italic,
            wrap: TextWrap::Wrap,
        )
    }
    .into_any()
}

/// Maps to: CC `MessageSelector.tsx#RestoreCodeConfirmation` — prose is dim,
/// the inline +N/-N counts use diffAddedWord/diffRemovedWord (DiffStatsText).
fn restore_code_confirmation_element(
    diff_stats_for_restore: Option<&DiffStats>,
    theme: &crate::utils::theme::Theme,
) -> Option<AnyElement<'static>> {
    let diff_stats = diff_stats_for_restore?;
    if diff_stats.files_changed.is_empty() {
        return Some(
            element! {
                Text(content: "The code has not changed (nothing will be restored).", dim: true)
            }
            .into_any(),
        );
    }
    let file_label = restore_code_file_label(diff_stats);
    let insertions = diff_stats.insertions;
    let deletions = diff_stats.deletions;
    Some(
        element! {
            View(flex_direction: FlexDirection::Row) {
                Text(content: "The code will be restored ", dim: true, wrap: TextWrap::NoWrap)
                Text(content: format!("+{insertions} "), color: theme.diff_added_word, dim: true, wrap: TextWrap::NoWrap)
                Text(content: format!("-{deletions}"), color: theme.diff_removed_word, dim: true, wrap: TextWrap::NoWrap)
                Text(content: format!(" in {file_label}."), dim: true, wrap: TextWrap::NoWrap)
            }
        }
        .into_any(),
    )
}

/// Maps to: CC `components/MessageSelector.tsx#MessageSelector`.
///
/// Internal state mirrors CC: `selected_index` starts on the trailing
/// "(current)" option (CC :121 `useState(messageOptions.length - 1)`),
/// `message_to_restore` starts from `preselected_message` (CC :134).
/// Keyboard input goes through the keybinding runtime — `messageSelector:*`
/// in the MessageSelector context and `confirm:no` in Confirmation,
/// matching CC :369-389 — plus useExitOnCtrlCDWithKeybindings (CC :336).
#[component]
pub fn MessageSelector<'a>(
    props: &mut MessageSelectorProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // CC styles by theme keys: divider/title/selected = suggestion, pointer =
    // permission, metadata = inactive/warning, diff counts = diffAddedWord/
    // diffRemovedWord, errors = error.
    let theme = hooks
        .try_use_context::<crate::utils::theme::Theme>()
        .map(|theme| *theme)
        .unwrap_or_else(|| *crate::utils::theme::current());
    // CC :109-120 — useMemo(() => [...messages.filter(...), virtualCurrent],
    // [messages]). The Arc pointer is the Rust `!==` (reference identity), so
    // the options rebuild exactly when REPL swaps the messages Arc (e.g.
    // stream appends while the selector is open) and never per frame.
    let (_option_source, option_indices): (Arc<Vec<Message>>, Arc<Vec<usize>>) = hooks.use_memo(
        {
            let messages = Arc::clone(&props.messages);
            move || {
                let indices = Arc::new(build_message_selector_options(&messages));
                (messages, indices)
            }
        },
        Arc::as_ptr(&props.messages) as usize,
    );
    let current_prompt =
        hooks.use_const(|| crate::utils::messages::create_user_message(String::new()));
    let options: Vec<&UserMessage> = option_indices
        .iter()
        .map(|index| match props.messages.get(*index) {
            Some(Message::User(user)) => user,
            _ => &current_prompt,
        })
        .collect();
    let option_count = options.len();
    let has_messages_to_select = option_count > 1;

    // CC :121 — the initial selection is the trailing "(current)" option.
    let selected_index = hooks.use_state(|| option_count.saturating_sub(1));
    // CC :134-136 useState(preselectedMessage).
    let mut message_to_restore = hooks.use_state({
        let preselected = props.preselected_message.clone();
        move || preselected.map(Arc::new)
    });
    // Maps to CC useAppState(s => s.fileHistory) and metadata effect.
    let is_file_history_enabled = crate::utils::file_history::file_history_enabled();
    let file_history =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(&mut hooks, |state| {
            Arc::clone(&state.file_history)
        });
    let (_metadata_source, _metadata_history, file_history_metadata) = hooks.use_memo(
        {
            let history = file_history.clone();
            let messages = Arc::clone(&props.messages);
            let indices = Arc::clone(&option_indices);
            move || {
                let Some(history) = history else {
                    return (messages, None, BTreeMap::new());
                };
                let metadata = indices
                    .iter()
                    .enumerate()
                    .filter_map(|(index, message_index)| {
                        let message = messages.get(*message_index)?;
                        let next = indices.get(index + 1).and_then(|i| messages.get(*i));
                        let stats = crate::utils::file_history::file_history_can_restore(
                            &history,
                            message.uuid(),
                        )
                        .then(|| {
                            compute_diff_stats_between_messages(
                                &messages,
                                message.uuid(),
                                next.map(Message::uuid),
                            )
                        })
                        .flatten();
                        Some((index, stats))
                    })
                    .collect::<BTreeMap<_, _>>();
                (messages, Some(history), metadata)
            }
        },
        (
            Arc::as_ptr(&props.messages) as usize,
            file_history.as_ref().map(|h| Arc::as_ptr(h) as usize),
        ),
    );
    let mut diff_stats_for_restore = hooks.use_state(|| Option::<DiffStats>::None);
    let mut pending_selection = hooks.use_state(|| Option::<Arc<UserMessage>>::None);
    let mut diff_request = hooks
        .use_state(|| Option::<(String, Arc<crate::utils::file_history::FileHistoryState>)>::None);
    let load_diff = hooks.use_async_handler({
        move |(uuid, history): (String, Arc<crate::utils::file_history::FileHistoryState>)| {
            async move {
                let stats =
                    crate::utils::file_history::file_history_get_diff_stats(&history, &uuid).await;
                // CC effect cleanup: ignore a completion for an obsolete selection/store.
                if diff_request
                    .read()
                    .as_ref()
                    .is_some_and(|(id, state)| id == &uuid && Arc::ptr_eq(state, &history))
                {
                    diff_stats_for_restore.set(stats);
                    let selected = pending_selection.read().clone();
                    if let Some(selected) = selected.filter(|message| message.uuid == uuid) {
                        message_to_restore.set(Some(selected));
                        pending_selection.set(None);
                    }
                }
            }
        }
    });
    let requested = pending_selection
        .read()
        .as_ref()
        .map(|message| message.uuid.clone())
        .or_else(|| {
            props
                .preselected_message
                .as_ref()
                .map(|message| message.uuid.clone())
        })
        .zip(file_history.clone());
    if *diff_request.read() != requested {
        diff_request.set(requested.clone());
        if let Some(request) = requested {
            load_diff(request);
        }
    }
    // CC :158-159 selectedRestoreOption, seeded like the Select's
    // defaultFocusValue (canRestoreCode ? 'both' : 'conversation').
    let initial_restore_option = if diff_stats_for_restore
        .read()
        .as_ref()
        .is_some_and(|stats| !stats.files_changed.is_empty())
    {
        RestoreOption::Both
    } else {
        RestoreOption::Conversation
    };
    let mut selected_restore_option = hooks.use_state(move || initial_restore_option);
    // CC :104/:154-156 — async restore bookkeeping; restoring_option selects
    // the summary spinner while awaiting the REPL-owned onSummarize callback.
    let mut error = hooks.use_state(|| Option::<String>::None);
    let mut is_restoring = hooks.use_state(|| false);
    let mut restoring_option = hooks.use_state(|| Option::<RestoreOption>::None);
    let mut feedback = hooks.use_state(BTreeMap::<String, String>::new);
    let mut input_accepted = hooks.use_state(|| Option::<String>::None);
    let mut input_cancelled = hooks.use_state(|| false);
    let mut pending_action = hooks.use_state(|| Option::<PendingSelectorAction>::None);
    // CC Select owns navigation/input; retained hook events return onChange
    // and onCancel to this component's lifetime-bound callbacks.
    let restore_options = get_restore_options(initial_restore_option == RestoreOption::Both, false);
    let restore_select = crate::components::custom_select::use_select_state(
        &mut hooks,
        crate::components::custom_select::UseSelectStateProps {
            visible_option_count: Some(5),
            values: restore_options
                .iter()
                .map(|o| o.value.official_value().to_string())
                .collect(),
            default_value: None,
            focus_value: Some(initial_restore_option.official_value().to_string()),
        },
    );
    let restore_events = crate::components::custom_select::use_select_input(
        &mut hooks,
        restore_select,
        crate::components::custom_select::UseSelectInputOptions {
            is_disabled: message_to_restore.read().is_none()
                || is_restoring.get()
                || error.read().is_some(),
            has_on_cancel: message_to_restore.read().is_some(),
            option_metas: restore_options
                .iter()
                .map(
                    |o| crate::components::custom_select::SelectInputOptionMeta {
                        value: o.value.official_value().to_string(),
                        is_input: o.input_placeholder.is_some(),
                        allow_empty_submit_to_cancel: o.allow_empty_submit_to_cancel,
                        input_has_value: feedback
                            .read()
                            .get(o.value.official_value())
                            .is_some_and(|s| !s.is_empty()),
                        ..Default::default()
                    },
                )
                .collect(),
            ..Default::default()
        },
    );
    if let Some(option) = restore_options
        .iter()
        .find(|o| Some(o.value.official_value()) == restore_select.focused_value().as_deref())
        && selected_restore_option.get() != option.value
    {
        selected_restore_option.set(option.value);
    }
    let mut restore_result =
        hooks.use_state(|| Option::<(RestoreOption, Result<(), String>)>::None);
    let restore = hooks.use_async_handler({
        let code = props.on_restore_code.clone();
        let conversation = props.on_restore_message.clone();
        let summarize = props.on_summarize.clone();
        move |(uuid, option, feedback): (String, RestoreOption, Option<String>)| {
            let code = code.clone();
            let conversation = conversation.clone();
            let summarize = summarize.clone();
            async move {
                let result = if is_summarize_option(Some(option)) {
                    let direction = if option == RestoreOption::SummarizeUpTo {
                        crate::types::message::PartialCompactDirection::UpTo
                    } else {
                        crate::types::message::PartialCompactDirection::From
                    };
                    match summarize {
                        Some(callback) => callback(uuid, feedback, direction).await,
                        None => Err("Summarization is unavailable.".into()),
                    }
                    .map_err(|error| format!("Failed to summarize:\n{error}"))
                } else {
                    let code_error = if matches!(option, RestoreOption::Code | RestoreOption::Both)
                    {
                        match code {
                            Some(callback) => callback(uuid.clone()).await.err(),
                            None => Some("File history restoration is unavailable.".into()),
                        }
                    } else {
                        None
                    };
                    // CC invokes both callbacks even if code restoration fails.
                    let conversation_error =
                        if matches!(option, RestoreOption::Conversation | RestoreOption::Both) {
                            match conversation {
                                Some(callback) => callback(uuid).await.err(),
                                None => Some("Conversation restoration is unavailable.".into()),
                            }
                        } else {
                            None
                        };
                    match (conversation_error, code_error) {
                        (Some(conversation), Some(code)) => Err(format!(
                            "Failed to restore the conversation and code:\n{conversation}\n{code}"
                        )),
                        (Some(error), None) => {
                            Err(format!("Failed to restore the conversation:\n{error}"))
                        }
                        (None, Some(error)) => Err(format!("Failed to restore the code:\n{error}")),
                        _ => Ok(()),
                    }
                };
                restore_result.set(Some((option, result)));
            }
        }
    });
    let completed = restore_result.read().clone();
    if let Some((_, result)) = completed {
        restore_result.set(None);
        is_restoring.set(false);
        restoring_option.set(None);
        message_to_restore.set(None);
        match result {
            Ok(()) => (props.on_close)(()),
            Err(reason) => {
                crate::utils::debug::log_for_debugging(&reason);
                error.set(Some(reason));
            }
        }
    }
    let accepted_input = input_accepted.read().clone();
    if accepted_input.is_some() {
        input_accepted.set(None);
    }
    let accepted = restore_events.take_accepted().or(accepted_input);
    if restore_events.take_cancelled() || input_cancelled.get() {
        input_cancelled.set(false);
        if props.preselected_message.is_some() {
            (props.on_close)(());
        } else {
            message_to_restore.set(None);
        }
    }
    if let Some(option) = accepted.and_then(|value| {
        restore_options
            .iter()
            .find(|o| o.value.official_value() == value)
            .map(|o| o.value)
    }) {
        let selected_message = message_to_restore.read().clone();
        if let Some(message) = selected_message {
            match option {
                RestoreOption::Nevermind => {
                    if props.preselected_message.is_some() {
                        (props.on_close)(());
                    } else {
                        message_to_restore.set(None);
                    }
                }
                option => {
                    (props.on_pre_restore)(());
                    is_restoring.set(true);
                    error.set(None);
                    if is_summarize_option(Some(option)) {
                        restoring_option.set(Some(option));
                    }
                    let text = feedback
                        .read()
                        .get(option.official_value())
                        .map(|text| text.trim().to_string())
                        .filter(|text| !text.is_empty());
                    restore((message.uuid.clone(), option, text));
                }
            }
        }
    }

    // CC :336 useExitOnCtrlCDWithKeybindings — double Ctrl-C/D exits.
    let exit_state = use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    // CC UserMessageOption reads `columns` from useTerminalSize().
    let (terminal_width, _terminal_rows) = hooks.use_terminal_size();

    let keybinding_runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());

    // CC :369-373 — escape closes; bound in the Confirmation context, active
    // only while the pick list is showing.
    {
        let message_to_restore = message_to_restore;
        let mut pending_action = pending_action;
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            "confirm:no",
            ContextName::Confirmation,
            move || message_to_restore.read().is_none(),
            move || {
                pending_action.set(Some(PendingSelectorAction::Escape));
                true
            },
        );
    }
    // CC :376-389 useKeybindings — navigation, active while picking.
    let nav_active = {
        let is_restoring = is_restoring;
        let error = error;
        let message_to_restore = message_to_restore;
        move || {
            !is_restoring.get()
                && error.read().is_none()
                && message_to_restore.read().is_none()
                && has_messages_to_select
        }
    };
    {
        let mut selected_index = selected_index;
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            "messageSelector:up",
            ContextName::MessageSelector,
            nav_active,
            move || {
                selected_index.set(selected_index.get().saturating_sub(1));
                true
            },
        );
    }
    {
        let mut selected_index = selected_index;
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            "messageSelector:down",
            ContextName::MessageSelector,
            nav_active,
            move || {
                selected_index.set((selected_index.get() + 1).min(option_count.saturating_sub(1)));
                true
            },
        );
    }
    {
        let mut selected_index = selected_index;
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            "messageSelector:top",
            ContextName::MessageSelector,
            nav_active,
            move || {
                selected_index.set(0);
                true
            },
        );
    }
    {
        let mut selected_index = selected_index;
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            "messageSelector:bottom",
            ContextName::MessageSelector,
            nav_active,
            move || {
                selected_index.set(option_count.saturating_sub(1));
                true
            },
        );
    }
    {
        let mut pending_action = pending_action;
        use_keybinding(
            &mut hooks,
            keybinding_runtime,
            "messageSelector:select",
            ContextName::MessageSelector,
            nav_active,
            move || {
                pending_action.set(Some(PendingSelectorAction::Select));
                true
            },
        );
    }

    // Keybinding handlers are Send + 'static and cannot call the HandlerMut
    // props; dispatch the CC handleSelect / handleEscape logic here instead.
    if let Some(action) = pending_action.get() {
        pending_action.set(None);
        match action {
            PendingSelectorAction::Select => {
                // Maps to: CC handleSelect (:224-249).
                let selected = options
                    .get(selected_index.get().min(option_count.saturating_sub(1)))
                    .map(|message| Arc::new((*message).clone()));
                if let Some(selected) = selected {
                    if selected.uuid == current_prompt.uuid {
                        // The virtual current prompt is not in messages —
                        // no-op close (CC :236-239).
                        (props.on_close)(());
                    } else if !is_file_history_enabled {
                        // CC :241-244 restoreConversationDirectly →
                        // onRestoreMessage + onClose.
                        (props.on_pre_restore)(());
                        is_restoring.set(true);
                        restore((selected.uuid.clone(), RestoreOption::Conversation, None));
                    } else {
                        // CC :246-248 — land on the confirm view.
                        restore_select
                            .navigation
                            .focus_option(initial_restore_option.official_value());
                        if file_history.is_some() {
                            pending_selection.set(Some(selected));
                        } else {
                            message_to_restore.set(Some(selected));
                        }
                    }
                }
            }
            PendingSelectorAction::Escape => {
                // Maps to: CC handleEscape (:338-346) — from a non-
                // preselected confirm view go back to the list; otherwise
                // close. confirm:no is only active while picking, so today
                // this always closes; the branch keeps CC's shape for when
                // the confirm view takes keyboard input.
                if message_to_restore.read().is_some() && props.preselected_message.is_none() {
                    message_to_restore.set(None);
                } else {
                    (props.on_close)(());
                }
            }
        }
    }

    let selected_index_value = selected_index.get().min(option_count.saturating_sub(1));
    let message_to_restore_view = message_to_restore.read().clone();
    let error_view = error.read().clone();
    let selected_restore_option_value = selected_restore_option.get();
    let is_restoring_value = is_restoring.get();
    let restoring_option_value = restoring_option.get();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let diff_stats_for_restore = diff_stats_for_restore.read().clone();

    // CC :124-130 — fixed window of MAX_VISIBLE_MESSAGES(7), selection
    // centered with top/bottom clamping; the slice truncates silently.
    let visible_count = MAX_VISIBLE_MESSAGES;
    let columns = if props.columns > 0 {
        props.columns
    } else {
        terminal_width as usize
    };
    let first_visible_index =
        first_visible_message_index_for(selected_index_value, option_count, visible_count);
    let can_restore_code = diff_stats_for_restore
        .as_ref()
        .is_some_and(|stats| !stats.files_changed.is_empty());
    let show_pick_list = error_view.is_none()
        && message_to_restore_view.is_none()
        && props.preselected_message.is_none()
        && has_messages_to_select;
    let fig = figures::get();

    // CC :463-526 confirm view. The blocks are siblings inside the gap
    // container (CC renders them in a fragment), so each is separated by one
    // blank row.
    let confirm_children: Vec<AnyElement<'static>> = message_to_restore_view
        .as_ref()
        .filter(|_| error_view.is_none() && has_messages_to_select)
        .map(|message| {
            let confirm_prefix = if diff_stats_for_restore.is_none() {
                "the conversation "
            } else {
                ""
            };
            let restore_options = get_restore_options(can_restore_code, false);
            let mut children: Vec<AnyElement<'static>> = Vec::new();
            children.push(element! {
                Text(content: format!("Confirm you want to restore {confirm_prefix}to the point before you sent this message:"))
            }.into_any());
            // CC :470-488 — dim-left-bordered message preview + relative time.
            children.push({
                let message_element = user_message_option_element(
                    message,
                    false,
                    None,
                    columns.max(20),
                    Some(theme.text),
                    &theme,
                );
                let time_line = format!(
                    "({})",
                    format_relative_time_ago_millis(message.timestamp.timestamp_millis(), now_ms)
                );
                element! {
                    View(
                        flex_direction: FlexDirection::Column,
                        padding_left: 1u32,
                        border_style: BorderStyle::Single,
                        border_top: Some(false),
                        border_right: Some(false),
                        border_bottom: Some(false),
                        border_left: Some(true),
                        border_left_dim_color: Some(true),
                    ) {
                        #(message_element)
                        Text(content: time_line, dim: true)
                    }
                }
                .into_any()
            });
            // CC RestoreOptionDescription (:489-493 / :644-670).
            children.push({
                let conversation_line =
                    get_restore_option_conversation_text(selected_restore_option_value).to_string();
                let show_code_restore = can_restore_code
                    && matches!(
                        selected_restore_option_value,
                        RestoreOption::Both | RestoreOption::Code
                    );
                let code_line: Option<AnyElement<'static>> =
                    if is_summarize_option(Some(selected_restore_option_value)) {
                        None
                    } else if show_code_restore {
                        restore_code_confirmation_element(diff_stats_for_restore.as_ref(), &theme)
                    } else {
                        Some(
                            element! { Text(content: "The code will be unchanged.", dim: true) }
                                .into_any(),
                        )
                    };
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: conversation_line, dim: true)
                        #(code_line)
                    }
                }
                .into_any()
            });
            // CC :494-516 — Summarizing spinner or the restore-option Select.
            children.push(
                if is_restoring_value && is_summarize_option(restoring_option_value) {
                    // CC: <Box flexDirection="row" gap={1}><Spinner/><Text>…
                    element! {
                        View(flex_direction: FlexDirection::Row, gap: 1u32) {
                            Spinner()
                            Text(content: "Summarizing…")
                        }
                    }
                    .into_any()
                } else {
                    // CC Select with all defaults: layout='compact',
                    // visibleOptionCount=5, numeric indexes visible. The
                    // summarize option is input-type (SelectInputOption);
                    // focus is driven by selected_restore_option (CC
                    // defaultFocusValue/onFocus). The committed-value ✔
                    // never shows — selection closes the view immediately.
                    let select_options = restore_options
                        .iter()
                        .map(|option| SelectOptionData {
                            label: option.label.clone(),
                            value: option.value.official_value().to_string(),
                            input: option.input_placeholder.as_ref().map(|placeholder| {
                                SelectInputOptionData {
                                    placeholder: Some(placeholder.clone()),
                                    // CC inputValues entry — the summarize
                                    // feedback joins with onSummarize wiring.
                                    value: String::new(),
                                    show_label_with_value: option.show_label_with_value,
                                    label_value_separator: option.label_value_separator.clone(),
                                }
                            }),
                            ..Default::default()
                        })
                        .collect::<Vec<_>>();
                    let focused_index = restore_options
                        .iter()
                        .position(|option| option.value == selected_restore_option_value)
                        .unwrap_or(0);
                    element! {
                        // Theme re-provided so the shared Select (which
                        // requires the context) renders in headless tests.
                        ContextProvider(value: Context::owned(theme)) {
                            Select(
                                is_disabled: is_restoring_value,
                                options: select_options,
                                focused_index: focused_index,
                                visible_option_count: 5usize,
                                layout: SelectLayout::Compact,
                                on_input_change: Handler::from(move |(key, text): (String, String)| { let mut feedback = feedback; feedback.write().insert(key, text); }),
                                on_input_submit: Handler::from(move |(key, _text): (String, String)| { let mut accepted = input_accepted; accepted.set(Some(key)); }),
                                on_cancel: Handler::from(move |_| { let mut cancelled = input_cancelled; cancelled.set(true); }),
                            )
                        }
                    }
                    .into_any()
                },
            );
            if can_restore_code {
                children.push(element! {
                    View(margin_bottom: 1u32) {
                        Text(content: format!("{} Rewinding does not affect files edited manually or via bash.", fig.warning), dim: true)
                    }
                }.into_any());
            }
            children
        })
        .unwrap_or_default();

    // CC :527-611 pick list. Subtitle and list are siblings inside the gap
    // container (CC fragment), separated by one blank row.
    let pick_list_children: Vec<AnyElement<'static>> = if show_pick_list {
        let subtitle = if is_file_history_enabled {
            "Restore the code and/or conversation to the point before…"
        } else {
            "Restore and fork the conversation to the point before…"
        };
        let list = element! {
            // CC :538 <Box width="100%" flexDirection="column"> — the 7-item
            // window renders bare: no pagination indicators (2.1.88).
            View(width: 100pct, flex_direction: FlexDirection::Column) {
                #(options.iter().enumerate().skip(first_visible_index).take(visible_count).map(|(option_index, message)| {
                    let selected = option_index == selected_index_value;
                    let is_current = message.uuid == current_prompt.uuid;
                    let metadata_loaded = file_history_metadata.contains_key(&option_index);
                    let metadata = file_history_metadata.get(&option_index).cloned().flatten();
                    let message_element = user_message_option_element(
                        message,
                        is_current,
                        Some(10),
                        columns.max(20),
                        selected.then_some(theme.suggestion),
                        &theme,
                    );
                    let metadata_element: Option<AnyElement<'static>> = (is_file_history_enabled && metadata_loaded).then(|| {
                        match metadata {
                            Some(stats) if !stats.files_changed.is_empty() => {
                                let file_label = if stats.files_changed.len() == 1 {
                                    format!("{} ", basename(&stats.files_changed[0]))
                                } else {
                                    format!("{} files changed ", stats.files_changed.len())
                                };
                                let insertions = stats.insertions;
                                let deletions = stats.deletions;
                                // CC :584-596 — dimColor={!isSelected}
                                // color="inactive" prose with DiffStatsText
                                // (+N diffAddedWord / -N diffRemovedWord).
                                element! {
                                    View(height: 1u32, flex_direction: FlexDirection::Row) {
                                        Text(content: file_label, dim: !selected, color: theme.inactive, wrap: TextWrap::NoWrap)
                                        Text(content: format!("+{insertions} "), dim: !selected, color: theme.diff_added_word, wrap: TextWrap::NoWrap)
                                        Text(content: format!("-{deletions}"), dim: !selected, color: theme.diff_removed_word, wrap: TextWrap::NoWrap)
                                    }
                                }
                                .into_any()
                            }
                            Some(_) => element! {
                                View(height: 1u32) {
                                    Text(content: "No code changes", dim: !selected, color: theme.inactive, wrap: TextWrap::NoWrap)
                                }
                            }
                            .into_any(),
                            // CC :599-601 — <Text dimColor color="warning">.
                            None => element! {
                                View(height: 1u32) {
                                    Text(content: format!("{} No code restore", fig.warning), dim: true, color: theme.warning, wrap: TextWrap::NoWrap)
                                }
                            }
                            .into_any(),
                        }
                    });
                    element! {
                        // CC :555-606 — height 3 (file history) / 2 with
                        // overflow hidden; a two-column row of pointer
                        // gutter (width 2) + content column, so the
                        // metadata line indents under the message text.
                        View(
                            height: if is_file_history_enabled { 3u32 } else { 2u32 },
                            overflow: Overflow::Hidden,
                            width: 100pct,
                            flex_direction: FlexDirection::Row,
                        ) {
                            View(width: 2u32, min_width: 2u32) {
                                #(if selected {
                                    // CC: pointer is permission-colored bold.
                                    element! { Text(content: format!("{} ", fig.pointer), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap) }
                                } else {
                                    element! { Text(content: "  ", wrap: TextWrap::NoWrap) }
                                })
                            }
                            View(flex_direction: FlexDirection::Column) {
                                View(flex_shrink: 1.0, height: 1u32, overflow: Overflow::Hidden) {
                                    #(message_element)
                                }
                                #(metadata_element)
                            }
                        }
                    }
                }).collect::<Vec<_>>())
            }
        }
        .into_any();
        vec![element! { Text(content: subtitle) }.into_any(), list]
    } else {
        Vec::new()
    };

    // CC :612-623 footer hint, fed by the live exit state.
    let hint_line = if exit_state.pending {
        format!(
            "Press {} again to exit",
            exit_state.key_name.unwrap_or("Ctrl-C")
        )
    } else if error_view.is_none() && has_messages_to_select {
        "Enter to continue · Esc to exit".to_string()
    } else {
        "Esc to exit".to_string()
    };
    let show_hint = message_to_restore_view.is_none();

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            Divider(color: Some(theme.suggestion))
            // CC :448 <Box flexDirection="column" marginX={1} gap={1}> — the
            // gap separates title / subtitle / list / hint by one blank row.
            View(flex_direction: FlexDirection::Column, margin_left: 1u32, margin_right: 1u32, gap: 1u32) {
                Text(content: "Rewind", weight: Weight::Bold, color: theme.suggestion, wrap: TextWrap::NoWrap)
                #(error_view.as_ref().map(|error| element! {
                    Text(content: format!("Error: {error}"), color: theme.error)
                }))
                #((!has_messages_to_select).then(|| element! {
                    Text(content: "Nothing to rewind to yet.")
                }))
                #(confirm_children)
                #(pick_list_children)
                #(show_hint.then(move || element! {
                    Text(content: hint_line, dim: true, italic: true)
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, text: &str) -> UserMessage {
        let mut message = crate::utils::messages::create_user_message(text.to_string());
        message.uuid = id.to_string();
        message.timestamp = chrono::DateTime::from_timestamp_millis(1_000).unwrap();
        message
    }
    fn raw_user(id: impl AsRef<str>, text: impl AsRef<str>) -> Message {
        Message::User(user(id.as_ref(), text.as_ref()))
    }
    fn tool_result(id: &str, output: serde_json::Value) -> Message {
        let mut message = user(id, "");
        let result = crate::types::message::ToolResult {
            tool_use_result: Some(output),
            tool_use_id: crate::types::ids::ToolUseId("tool-id".into()),
            content: String::new(),
            is_error: false,
            content_blocks: Vec::new(),
        };
        message.content = vec![UserContent::ToolResult(result)];
        Message::User(message)
    }

    #[test]
    fn restore_options_match_official_order_and_input_props() {
        let labels = get_restore_options(true, false)
            .into_iter()
            .map(|option| {
                (
                    option.value.official_value().to_string(),
                    option.label,
                    option.input_placeholder,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                (
                    "both".to_string(),
                    "Restore code and conversation".to_string(),
                    None
                ),
                (
                    "conversation".to_string(),
                    "Restore conversation".to_string(),
                    None
                ),
                ("code".to_string(), "Restore code".to_string(), None),
                (
                    "summarize".to_string(),
                    "Summarize from here".to_string(),
                    Some("add context (optional)".to_string())
                ),
                ("nevermind".to_string(), "Never mind".to_string(), None),
            ]
        );
        assert_eq!(
            get_restore_options(false, true)
                .into_iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![
                RestoreOption::Conversation,
                RestoreOption::Summarize,
                RestoreOption::SummarizeUpTo,
                RestoreOption::Nevermind,
            ]
        );
    }

    #[test]
    fn selectable_user_messages_filter_matches_official_exclusions() {
        assert!(selectable_user_messages_filter(&raw_user("u1", "hello")));
        assert!(!selectable_user_messages_filter(&Message::Assistant(
            crate::utils::messages::create_assistant_message("hello".into())
        )));
        assert!(!selectable_user_messages_filter(&tool_result(
            "tool",
            serde_json::Value::Null
        )));
        let mut meta = user("meta", "");
        meta.content = vec![UserContent::MetaText("hidden".into())];
        assert!(!selectable_user_messages_filter(&Message::User(meta)));
        for text in [
            "<bash-stdout>output</bash-stdout>",
            "<teammate-message agent=\"a\">hello</teammate-message>",
            crate::utils::messages::CANCEL_MESSAGE,
        ] {
            assert!(!selectable_user_messages_filter(&raw_user(
                "excluded", text
            )));
        }
        let mut summary = user("summary", "summary");
        summary.is_compact_summary = true;
        assert!(!selectable_user_messages_filter(&Message::User(summary)));
        let mut hidden = user("hidden", "hidden");
        hidden.is_visible_in_transcript_only = true;
        assert!(!selectable_user_messages_filter(&Message::User(hidden)));
    }

    #[test]
    fn build_message_selector_options_filters_transcript_and_appends_current() {
        let mut multi = user("multi", "first");
        multi.content.push(UserContent::Text("last".into()));
        let mut image = user("image", "not the title");
        image.content.push(UserContent::Image {
            media_type: "image/png".into(),
            data: "image-data".into(),
        });
        let mut image_only = image.clone();
        image_only.content.remove(0);
        let messages = vec![
            raw_user("u1", "hello"),
            raw_user("stdout", "<bash-stdout>output</bash-stdout>"),
            Message::User(multi.clone()),
            Message::User(image.clone()),
            Message::User(image_only),
        ];
        assert_eq!(
            build_message_selector_options(&messages),
            vec![0, 2, 3, 4, 5]
        );
        assert_eq!(user_message_option_display(&multi, false, None, 80), "last");
        assert_eq!(
            user_message_option_display(&image, false, None, 80),
            "(no prompt)"
        );
        assert_eq!(multi.timestamp.timestamp_millis(), 1_000);
    }

    #[test]
    fn user_message_option_display_handles_current_empty_bash_commands_and_skills() {
        assert_eq!(
            user_message_option_display(&user("current", ""), true, None, 80),
            "(current)"
        );
        assert_eq!(
            user_message_option_display(&user("empty", ""), false, None, 80),
            "((empty message))"
        );
        assert_eq!(
            user_message_option_display(
                &user("bash", "<bash-input>cargo test</bash-input>"),
                false,
                None,
                80
            ),
            "! cargo test"
        );
        assert_eq!(
            user_message_option_display(
                &user(
                    "cmd",
                    "<command-message>model</command-message><command-args>opus</command-args>"
                ),
                false,
                None,
                80
            ),
            "/model opus"
        );
        assert_eq!(
            user_message_option_display(
                &user(
                    "skill",
                    "<command-message>review</command-message><skill-format>true</skill-format>"
                ),
                false,
                None,
                80
            ),
            "Skill(review)"
        );
        assert_eq!(
            user_message_option_display(
                &user(
                    "display",
                    "<ide_opened_file>foo.rs</ide_opened_file>real prompt"
                ),
                false,
                None,
                80
            ),
            "real prompt"
        );
    }

    #[test]
    fn compute_diff_stats_between_messages_counts_file_edit_and_create_results() {
        let messages = vec![
            raw_user("start", "before"),
            tool_result(
                "edit1",
                serde_json::json!({"filePath":"src/a.rs","structuredPatch":[{"lines":[" context","+new","-old"]}]}),
            ),
            tool_result(
                "create",
                serde_json::json!({"filePath":"src/b.rs","type":"create","content":"one\r\ntwo","structuredPatch":[]}),
            ),
            raw_user("next", "after"),
            tool_result(
                "edit2",
                serde_json::json!({"filePath":"src/c.rs","structuredPatch":[{"lines":["+late"]}]}),
            ),
        ];
        let stats = compute_diff_stats_between_messages(&messages, "start", Some("next")).unwrap();
        assert_eq!(stats.files_changed, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!((stats.insertions, stats.deletions), (3, 1));
        assert!(compute_diff_stats_between_messages(&messages, "missing", None).is_none());
        assert!(
            compute_diff_stats_between_messages(&messages, "next", Some("start"))
                .unwrap()
                .files_changed
                .is_empty()
        );
    }

    #[test]
    fn restore_descriptions_match_official_copy_and_file_labels() {
        let stats = DiffStats {
            files_changed: vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
            ],
            insertions: 4,
            deletions: 2,
        };
        assert_eq!(
            get_restore_option_conversation_text(RestoreOption::SummarizeUpTo),
            "Preceding messages will be summarized. This and subsequent messages will remain unchanged — you will stay at the end of the conversation."
        );
        assert_eq!(
            restore_option_description_lines(RestoreOption::Both, true, Some(&stats)),
            vec![
                "The conversation will be forked.".to_string(),
                "The code will be restored +4 -2 in a.rs and 2 other files.".to_string(),
            ]
        );
        assert_eq!(
            restore_option_description_lines(RestoreOption::Conversation, true, Some(&stats))[1],
            "The code will be unchanged."
        );
    }

    #[test]
    fn messages_after_are_only_synthetic_matches_official_meaningful_content_rules() {
        let mut meta = user("meta", "");
        meta.content = vec![UserContent::MetaText("hidden".into())];
        let mut messages = vec![
            raw_user("start", "before"),
            raw_user("synthetic", crate::utils::messages::CANCEL_MESSAGE),
            tool_result("tool", serde_json::Value::Null),
            Message::User(meta),
        ];
        assert!(messages_after_are_only_synthetic(&messages, 0));
        messages.push(Message::Assistant(
            crate::utils::messages::create_assistant_message("answer".into()),
        ));
        assert!(!messages_after_are_only_synthetic(&messages, 0));
    }

    #[test]
    fn message_selector_renders_pick_list_and_metadata() {
        use crate::utils::file_history::{FileHistorySnapshot, FileHistoryState};
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                file_history: Arc::new(FileHistoryState {
                    snapshots: vec![FileHistorySnapshot {
                        message_id: "u1".into(),
                        tracked_file_backups: Default::default(),
                        timestamp: chrono::Utc::now(),
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
            None,
        );
        let text = element! {
            ContextProvider(value: Context::owned(store)) {
                MessageSelector(messages: Arc::new(vec![raw_user("u1", "hello world"), tool_result("edit", serde_json::json!({"filePath":"src/main.rs","structuredPatch":[{"lines":["+a","+b","-c"]}]}))]), columns: 80usize)
            }
        }.render(Some(100)).to_string();
        // CC MessageSelector.tsx:391-441: metadata comes from the subscribed file history plus raw tool results.
        for expected in [
            "Rewind",
            "Restore the code and/or conversation",
            "hello world",
            "main.rs +2 -1",
            "Enter to continue",
        ] {
            assert!(text.contains(expected), "missing {expected}:\n{text}");
        }
    }

    #[test]
    fn message_selector_defaults_to_current_option() {
        // CC MessageSelector.tsx:121 — useState(messageOptions.length - 1):
        // the trailing "(current)" virtual option is selected on open.
        let messages = Arc::new(vec![raw_user("u1", "first"), raw_user("u2", "second")]);
        let text = element! {
            MessageSelector(
                messages,
                columns: 80usize,
            )
        }
        .render(Some(100))
        .to_string();
        let fig = figures::get();
        assert!(
            text.contains(&format!("{} (current)", fig.pointer)),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains(&format!("{} first", fig.pointer)),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn restore_confirmation_matches_official_navigation_reentry_and_callbacks() {
        use futures::StreamExt;
        let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let pre = calls.clone();
        let restore = calls.clone();
        let close = calls.clone();
        let messages = Arc::new(vec![raw_user("u1", "restore this prompt")]);
        // CC MessageSelector.tsx:499-516 mounts the Select input option;
        // useTextInput.ts:105-106 requires the shared notification/store context.
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(store)) {
                    ContextProvider(value: Context::owned(KeybindingRuntime::with_default_bindings())) {
                        MessageSelector(
                            messages,
                            on_pre_restore: move |_| pre.lock().unwrap().push("pre".into()),
                            on_restore_message: Some(Arc::new(move |uuid: String| -> futures::future::BoxFuture<'static, Result<(), String>> { restore.lock().unwrap().push(uuid); Box::pin(async { Ok(()) }) }) as RestoreCodeCallback),
                            on_close: move |_| close.lock().unwrap().push("close".into()),
                        )
                    }
                }
            };
            // CC MessageSelector.tsx:463-516: returning to the list unmounts
            // Select; entering again must focus Conversation. Crossing the
            // Summarize input option must not trap arrow navigation.
            let events = futures::stream::unfold(
                vec![
                    KeyCode::Up,
                    KeyCode::Enter,
                    KeyCode::Down,
                    KeyCode::Down,
                    KeyCode::Enter,
                    KeyCode::Enter,
                    KeyCode::Enter,
                ]
                .into_iter(),
                |mut events| async move {
                    let code = events.next()?;
                    futures_timer::Delay::new(std::time::Duration::from_millis(60)).await;
                    Some((
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)),
                        events,
                    ))
                },
            );
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
            while crate::utils::race(frames.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_millis(250)).await;
                None
            })
            .await
            .is_some()
            {}
        });
        assert_eq!(*calls.lock().unwrap(), vec!["pre", "u1", "close"]);
    }

    #[test]
    fn message_selector_windows_to_fixed_seven_silently() {
        // CC :92/:124-130 — fixed MAX_VISIBLE_MESSAGES(7) window, silent
        // truncation: no "more above/below" indicators (2.1.88).
        let messages = Arc::new(
            (1..=10)
                .map(|i| raw_user(format!("u{i}"), format!("prompt-{i}")))
                .collect::<Vec<_>>(),
        );
        let text = element! {
            MessageSelector(
                messages,
                columns: 80usize,
            )
        }
        .render(Some(100))
        .to_string();
        // 11 options (10 prompts + current); initial selection is the end →
        // firstVisibleIndex = min(10 - 3, 11 - 7) = 4 → prompts 5..10 + (current).
        assert!(text.contains("prompt-5"), "canvas=\n{text}");
        assert!(text.contains("prompt-10"), "canvas=\n{text}");
        assert!(text.contains("(current)"), "canvas=\n{text}");
        assert!(!text.contains("prompt-4"), "canvas=\n{text}");
        assert!(!text.contains("more above"), "canvas=\n{text}");
        assert!(!text.contains("more below"), "canvas=\n{text}");
    }

    #[tokio::test]
    async fn message_selector_renders_confirm_state() {
        use crate::utils::file_history::{
            FileHistoryBackup, FileHistorySnapshot, FileHistoryState,
        };
        use futures::StreamExt;
        let path =
            std::env::temp_dir().join(format!("rewind-confirm-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, "created\n").unwrap();
        let file = path.to_string_lossy().into_owned();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                file_history: Arc::new(FileHistoryState {
                    snapshots: vec![FileHistorySnapshot {
                        message_id: "u1".into(),
                        tracked_file_backups: BTreeMap::from([(
                            file.clone(),
                            FileHistoryBackup {
                                backup_file_name: None,
                                version: 1,
                                backup_time: chrono::Utc::now(),
                            },
                        )]),
                        timestamp: chrono::Utc::now(),
                    }],
                    tracked_files: std::collections::BTreeSet::from([file]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            None,
        );
        let mut app = element! { ContextProvider(value: Context::owned(store)) {
            MessageSelector(messages: Arc::new(vec![raw_user("u1", "restore here")]), preselected_message: Some(user("u1", "restore here")), columns: 80usize)
        }};
        let mut frames = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(futures::stream::pending()).with_size(100, 30),
        ));
        let mut last = String::new();
        while let Some(frame) = crate::utils::race(frames.next(), async {
            futures_timer::Delay::new(std::time::Duration::from_millis(200)).await;
            None
        })
        .await
        {
            last = frame.to_string();
            if last.contains("1. Restore code and conversation") {
                break;
            }
        }
        std::fs::remove_file(path).unwrap();
        // CC :141-152,463-516: await disk diff for a preselected message, then expose code restore.
        for expected in [
            "Confirm you want to restore",
            "restore here",
            "The conversation will be forked.",
            "The code will be restored +0 -1",
            "1. Restore code and conversation",
            "4. Summarize from here",
        ] {
            assert!(last.contains(expected), "missing {expected}:\n{last}");
        }
    }

    #[test]
    fn summarize_input_matches_official_feedback_navigation_and_enter() {
        use futures::StreamExt;
        let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let pre = calls.clone();
        let summary = calls.clone();
        let close = calls.clone();
        futures::executor::block_on(async move {
            let store = crate::state::store::AppStore::new(Default::default(), None);
            let mut app = element! {
                ContextProvider(value: Context::owned(store)) {
                ContextProvider(value: Context::owned(KeybindingRuntime::with_default_bindings())) {
                    MessageSelector(
                        messages: Arc::new(vec![raw_user("u1", "summarize this")]),
                        on_pre_restore: move |_| pre.lock().unwrap().push("pre".into()),
                        on_summarize: Some(Arc::new(move |uuid: String, feedback: Option<String>, direction: crate::types::message::PartialCompactDirection| -> futures::future::BoxFuture<'static, Result<(), String>> {
                            summary.lock().unwrap().push(format!("{uuid}:{}:{}", direction.as_str(), feedback.unwrap_or_default()));
                            Box::pin(async { futures_timer::Delay::new(std::time::Duration::from_millis(80)).await; Ok(()) })
                        }) as SummarizeCallback),
                        on_close: move |_| close.lock().unwrap().push("close".into()),
                    )
                }
                }
            };
            // CC MessageSelector.tsx:160-197,266-289: switching focus preserves per-option feedback.
            let events = futures::stream::unfold(
                vec![
                    KeyCode::Up,
                    KeyCode::Enter,
                    KeyCode::Down,
                    KeyCode::Char('k'),
                    KeyCode::Char('e'),
                    KeyCode::Char('e'),
                    KeyCode::Char('p'),
                    KeyCode::Up,
                    KeyCode::Down,
                    KeyCode::Enter,
                ]
                .into_iter(),
                |mut events| async move {
                    let code = events.next()?;
                    futures_timer::Delay::new(std::time::Duration::from_millis(60)).await;
                    Some((
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)),
                        events,
                    ))
                },
            );
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
            let mut saw_input = false;
            let mut saw_spinner = false;
            while let Some(frame) = crate::utils::race(frames.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_millis(300)).await;
                None
            })
            .await
            {
                let text = frame.to_string();
                saw_input |= text.contains("keep");
                saw_spinner |= text.contains("Summarizing");
            }
            assert!(saw_input && saw_spinner);
        });
        assert_eq!(*calls.lock().unwrap(), vec!["pre", "u1:from:keep", "close"]);
    }
}
