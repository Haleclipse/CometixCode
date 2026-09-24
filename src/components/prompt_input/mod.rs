//! Maps to: CC components/PromptInput/ (21 files)
//! CC data flow:
//!   PromptInput owns: input, cursorOffset, exitMessage, suggestions
//!   PromptInput passes to children:
//!     → Footer: exitMessage, suppressHint
//!     → SuggestionList: suggestions, selectedSuggestion
//!     → BaseTextInput: inputState, invert, focus
//! Hooks extracted to crate::hooks/:
//!   use_double_press — time-based double-tap (CC useDoublePress)
//!   use_exit         — Ctrl-C/D exit flow (CC useExitOnCtrlCD)
//!   use_text_input        — readline editing + cursor model (CC useTextInput)
//!   use_arrow_key_history — Up/Down prompt history (CC useArrowKeyHistory)
//!   use_history_search    — Ctrl-R reverse search (CC useHistorySearch)
//!   use_typeahead         — suggestion engine (CC useTypeahead)
//! Prompt chrome:
//!   main-screen input row maps to CC's <Box borderStyle="round"
//!   borderLeft={false} borderRight={false} width="100%">, not Divider.

pub mod footer_layout_wake;
pub mod history_search_input;
pub mod input_modes;
pub mod input_paste;
pub mod issue_flag_banner;
pub mod notifications;
pub mod prompt_input_footer;
pub mod prompt_input_footer_left_side;
pub mod prompt_input_footer_suggestions;
pub mod prompt_input_help_menu;
pub mod prompt_input_mode_indicator;
pub mod prompt_input_queued_commands;
pub mod prompt_input_stash_notice;
pub mod sandbox_prompt_footer_hint;
pub mod shimmered_input;
pub mod use_maybe_truncate_input;
pub mod use_prompt_input_placeholder;
pub mod use_show_fast_icon_hint;
pub mod use_swarm_banner;
pub mod utils;
pub mod voice_indicator;

use crate::commands::Command;
use crate::commands::fast::FastModePicker;
use crate::components::base_text_input::{BaseInputState, BaseTextInput};
use crate::components::bridge_dialog::{BridgeDialog, BridgeDialogSnapshot};
use crate::components::global_search_dialog::GlobalSearchDialog;
use crate::components::history_search_dialog::HistorySearchDialog;
use crate::components::model_picker::{
    ModelEffortLevel, ModelPicker, ModelPickerSelection, apply_model_picker_selection,
};
use crate::components::prompt_input::input_modes::PromptInputMode;
use crate::components::prompt_input::prompt_input_help_menu::{
    PROMPT_INPUT_HELP_MENU_HEIGHT, PromptInputHelpMenu,
};
use crate::components::prompt_input::prompt_input_mode_indicator::PromptInputModeIndicator;
use crate::components::prompt_input::prompt_input_queued_commands::PromptInputQueuedCommands;
use crate::components::prompt_input::prompt_input_stash_notice::PromptInputStashNotice;
use crate::components::quick_open_dialog::QuickOpenDialog;
#[cfg(test)]
use crate::components::tasks::AsyncAgentDetailData;
use crate::components::tasks::background_tasks_dialog::{
    BackgroundTaskCategory, BackgroundTaskDetailData, BackgroundTasksDialog,
    BackgroundTasksDialogItem,
};
#[cfg(test)]
use crate::components::tasks::task_status_utils::TaskStatus;
use crate::components::teams::teams_dialog::{
    TeammateStatusData, TeamsDialog, TeamsDialogAction, TeamsDialogData,
};
use crate::components::thinking_toggle::{ThinkingToggle, thinking_toggle_requires_confirmation};
use crate::components::vim_text_input::{VimMode, VimTextInput};
use crate::context::notifications::use_notifications;
use crate::hooks::notifs::external_editor_hint::{
    ApiKeyVerificationStatus, external_editor_hint_notification_from_state,
};
use crate::hooks::use_api_key_verification::VerificationStatus;
use crate::hooks::use_arrow_key_history::use_arrow_key_history;
use crate::hooks::use_double_press::use_double_press;
use crate::hooks::use_history_search::{UseHistorySearchOptions, use_history_search};
use crate::hooks::use_text_input::{HistoryDirection, UseTextInputOptions, use_text_input};
use crate::hooks::use_typeahead::{SuggestionKind, use_typeahead};
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{use_keybinding, use_register_keybinding_context};
use crate::state::app_state::use_app_state;
use crate::state::app_state_store::FooterItem;
use crate::tools::agent_tool::agent_color_manager::parse_agent_color_name;
use crate::types::permissions::PermissionMode;
use crate::utils::prompt_editor::{EditorResult, ExternalEditorRuntime};
use crate::utils::prompt_history;
use crate::utils::theme::{Theme, ThemeColorKey};
use iocraft::prelude::*;
use prompt_input_footer_suggestions::SuggestionList;
use std::collections::BTreeMap;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

const PROMPT_CHAR: &str = "❯";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptDeferredAction {
    Stash,
    ExternalEditor,
    Newline,
    Undo,
    Submit,
    SubmitSuggestion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VimPendingCommand {
    Delete,
    Replace,
}

fn delete_vim_logical_line(value: &str, cursor_offset: usize) -> (String, usize) {
    let offset = crate::utils::cursor::clamp_cursor(value, cursor_offset);
    let start = value[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = value[offset..]
        .find('\n')
        .map_or(value.len(), |index| offset + index + 1);
    let mut next = value.to_string();
    next.replace_range(start..end, "");
    let next_offset = start.min(next.len());
    (next, next_offset)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PromptEditSnapshot {
    text: String,
    cursor_offset: usize,
    pasted_contents: std::collections::BTreeMap<usize, input_paste::PastedContent>,
}

#[derive(Clone, Debug)]
struct PromptStash {
    text: String,
    cursor_offset: usize,
    mode: PromptInputMode,
    pasted_contents: std::collections::BTreeMap<usize, input_paste::PastedContent>,
}

/// Maps to: CC `components/tasks/BackgroundTaskStatus.tsx:49-57` — the footer
/// pill's task set. Same source as the dialog items, minus (ant builds only)
/// panel agents: `isPanelAgentTask` = local_agent with
/// `agentType !== 'main-session'` (CC `LocalAgentTask.tsx:216-218`), excluded
/// behind the build-time `USER_TYPE === 'ant'` define → the
/// `anthropic_internal` cargo feature. The dialog does NOT apply this
/// exclusion.
fn prompt_pill_tasks(
    items: &[BackgroundTasksDialogItem],
) -> Vec<crate::tasks::pill_label::PillTask> {
    use crate::tasks::pill_label::PillTask;
    items
        .iter()
        .filter_map(|item| match item.category {
            // Monitor-kind shells are not distinguished by the shell snapshot
            // yet (Bash parity batch); every shell counts as a plain shell.
            BackgroundTaskCategory::Shell => Some(PillTask::LocalBash { is_monitor: false }),
            BackgroundTaskCategory::LocalAgent => {
                if cfg!(feature = "anthropic_internal") {
                    if let BackgroundTaskDetailData::Agent(agent) = &item.detail {
                        if agent.agent_type.as_deref() != Some("main-session") {
                            return None;
                        }
                    }
                }
                Some(PillTask::LocalAgent)
            }
            // The items source only produces Shell + LocalAgent today.
            _ => None,
        })
        .collect()
}

fn prompt_teams_data() -> Option<TeamsDialogData> {
    let teammates = crate::tasks::in_process_teammate_task::get_all_in_process_teammate_tasks();
    if teammates.is_empty() {
        return None;
    }
    let team_name = teammates
        .first()
        .map(|task| task.identity.team_name.clone())
        .unwrap_or_default();
    Some(TeamsDialogData {
        team_name,
        supports_hide_show: false,
        teammates: teammates
            .into_iter()
            .map(|task| TeammateStatusData {
                agent_id: task.identity.agent_id,
                name: task.identity.agent_name,
                status: task.status,
                is_hidden: false,
                mode: serde_json::to_value(task.permission_mode)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string)),
                model: task.model,
                color: task
                    .identity
                    .color
                    .as_deref()
                    .and_then(parse_agent_color_name)
                    .map(|color| color.theme_key()),
                worktree_path: None,
                cwd: None,
                prompt: Some(task.prompt),
                tasks: Vec::new(),
            })
            .collect(),
    })
}

/// Maps to CC PromptInput.tsx `getBorderColor()`.
fn prompt_border_color(mode: PromptInputMode, theme: &Theme) -> Color {
    if mode == PromptInputMode::Bash {
        return theme.bash_border;
    }
    crate::utils::teammate::get_teammate_color()
        .and_then(|name| parse_agent_color_name(&name))
        .map(|color| theme.color(color.theme_key()))
        .unwrap_or(theme.prompt_border)
}

/// Maps to CC PromptInput.tsx `buildBorderText(showFastIcon,
/// showFastIconHint, fastModeCooldown)`.
fn build_fast_border_text(
    show_fast_icon: bool,
    show_fast_icon_hint: bool,
    fast_mode_cooldown: bool,
    theme: &Theme,
) -> Option<BorderText> {
    show_fast_icon.then(|| {
        let fast_segment = if show_fast_icon_hint {
            format!(
                "{} \x1b[2m/fast\x1b[0m",
                crate::components::fast_icon::get_fast_icon_string(true, fast_mode_cooldown, theme,)
            )
        } else {
            crate::components::fast_icon::get_fast_icon_string(true, fast_mode_cooldown, theme)
        };
        BorderText {
            content: format!(" {fast_segment} "),
            position: BorderTextPosition::Top,
            align: BorderTextAlign::End,
            offset: 0,
        }
    })
}

/// Maps to CC PromptInput.tsx `showFastIcon`.
fn should_show_fast_icon(
    feature_enabled: bool,
    user_enabled: bool,
    available: bool,
    cooldown: bool,
) -> bool {
    feature_enabled && user_enabled && (available || cooldown)
}

/// Maps to: PromptInput.tsx `imageRefPositions` — `[Image #N]` chip ranges as
/// UTF-8 byte offsets (Cometix cursor unit).
fn image_ref_positions(value: &str) -> Vec<(usize, usize)> {
    prompt_history::parse_references(value)
        .into_iter()
        .filter(|r| r.match_text.starts_with("[Image"))
        .map(|r| (r.index, r.index + r.match_text.len()))
        .collect()
}

/// Maps to: PromptInput.tsx useEffect that snaps a caret strictly inside an
/// image chip to the nearer boundary (up/down / click landings). Kept local to
/// PromptInput — not Cursor.snapOutOfImageRef (word-motion `toward` API).
fn snap_cursor_inside_image_ref(
    value: &str,
    cursor_offset: usize,
    positions: &[(usize, usize)],
) -> usize {
    let offset = crate::utils::cursor::clamp_cursor(value, cursor_offset);
    if let Some(&(start, end)) = positions
        .iter()
        .find(|&&(start, end)| offset > start && offset < end)
    {
        let mid = (start + end) / 2;
        return if offset < mid { start } else { end };
    }
    offset
}

/// Maps to: PromptInput.tsx `combinedHighlights`.
fn combined_highlights(
    value: &str,
    cursor_offset: usize,
    slash_command_triggers: &[std::ops::Range<usize>],
) -> Vec<shimmered_input::TextHighlight> {
    let mut highlights = Vec::new();
    let rainbow = [
        (ThemeColorKey::RainbowRed, ThemeColorKey::RainbowRedShimmer),
        (
            ThemeColorKey::RainbowOrange,
            ThemeColorKey::RainbowOrangeShimmer,
        ),
        (
            ThemeColorKey::RainbowYellow,
            ThemeColorKey::RainbowYellowShimmer,
        ),
        (
            ThemeColorKey::RainbowGreen,
            ThemeColorKey::RainbowGreenShimmer,
        ),
        (
            ThemeColorKey::RainbowBlue,
            ThemeColorKey::RainbowBlueShimmer,
        ),
        (
            ThemeColorKey::RainbowIndigo,
            ThemeColorKey::RainbowIndigoShimmer,
        ),
        (
            ThemeColorKey::RainbowViolet,
            ThemeColorKey::RainbowVioletShimmer,
        ),
    ];
    let lower = value.to_lowercase();
    for keyword in ["ultrathink", "ultrareview"] {
        let mut search_byte = 0usize;
        while let Some(relative) = lower[search_byte..].find(keyword) {
            let start_byte = search_byte + relative;
            let start = value[..start_byte].encode_utf16().count();
            for (index, _) in keyword.chars().enumerate() {
                let (color, shimmer) = rainbow[index % rainbow.len()];
                highlights.push(shimmered_input::TextHighlight {
                    start: start + index,
                    end: start + index + 1,
                    color: Some(color),
                    dim_color: false,
                    inverse: false,
                    shimmer_color: Some(shimmer),
                    priority: 10,
                });
            }
            search_byte = start_byte + keyword.len();
        }
    }
    // Maps to: CC PromptInput.tsx:904-912 — only the validated triggers.
    for trigger in slash_command_triggers {
        highlights.push(shimmered_input::TextHighlight {
            start: value[..trigger.start].encode_utf16().count(),
            end: value[..trigger.end].encode_utf16().count(),
            color: Some(ThemeColorKey::Suggestion),
            dim_color: false,
            inverse: false,
            shimmer_color: None,
            priority: 5,
        });
    }
    // Invert the [Image #N] chip when the cursor is at chip.start (the
    // "selected" state) so backspace-to-delete is visually obvious.
    // Highlight ranges are UTF-16 for ShimmeredInput; cursor compare uses
    // UTF-8 byte offsets like Cometix editing.
    let cursor = crate::utils::cursor::clamp_cursor(value, cursor_offset);
    for &(start, end) in &image_ref_positions(value) {
        if cursor == start {
            highlights.push(shimmered_input::TextHighlight {
                start: value[..start].encode_utf16().count(),
                end: value[..end].encode_utf16().count(),
                color: None,
                dim_color: false,
                inverse: true,
                shimmer_color: None,
                priority: 8,
            });
        }
    }
    // Maps to: PromptInput.tsx combinedHighlights "btw" branch (solid yellow).
    for trigger in crate::utils::side_question::find_btw_trigger_positions(value) {
        highlights.push(shimmered_input::TextHighlight {
            start: value[..trigger.start].encode_utf16().count(),
            end: value[..trigger.end].encode_utf16().count(),
            color: Some(ThemeColorKey::Warning),
            dim_color: false,
            inverse: false,
            shimmer_color: None,
            priority: 15,
        });
    }
    highlights
}

fn prompt_text_input_columns(total_columns: u16) -> usize {
    // Maps exactly to PromptInput.tsx `textInputColumns = columns - 3` for the
    // external build (Buddy companion reservation is intentionally absent).
    // Cursor::from_text then reserves one further cell, as CC Cursor.fromText
    // constructs MeasuredText with `columns - 1`.
    (total_columns as usize).saturating_sub(3).max(1)
}

/// Maps to CC `PromptInput.tsx` `insertWithSpacing` used by quick/global search.
fn insert_search_dialog_text(
    mut input: State<String>,
    mut cursor_offset: State<usize>,
    text: String,
) {
    let mut current = input.read().clone();
    let offset = crate::utils::cursor::clamp_cursor(&current, cursor_offset.get());
    let preceding_is_space = current
        .get(..offset)
        .and_then(|prefix| prefix.chars().next_back())
        .is_none_or(char::is_whitespace);
    let insertion = if preceding_is_space {
        text
    } else {
        format!(" {text}")
    };
    current.insert_str(offset, &insertion);
    cursor_offset.set(offset + insertion.len());
    input.set(current);
}

fn restore_stash_or_clear(
    mut input: State<String>,
    mut cursor_offset: State<usize>,
    mut pasted_contents: State<std::collections::BTreeMap<usize, input_paste::PastedContent>>,
    mut stashed_prompt: State<Option<PromptStash>>,
    mut input_mode: State<PromptInputMode>,
) {
    let stash = stashed_prompt.read().clone();
    if let Some(stash) = stash {
        input.set(stash.text);
        cursor_offset.set(stash.cursor_offset);
        pasted_contents.set(stash.pasted_contents);
        input_mode.set(stash.mode);
        stashed_prompt.set(None);
    } else {
        input.set(String::new());
        cursor_offset.set(0);
        pasted_contents.set(std::collections::BTreeMap::new());
        input_mode.set(PromptInputMode::Prompt);
    }
}

/// Maps to PromptInput.tsx `handleModelSelect`: model changes clear the
/// session override and disable Fast mode when the selected model cannot use it.
/// Effort apply is official ModelPicker `handleSelect` / `bt`.
fn apply_prompt_model_selection(
    state: &mut crate::state::app_state_store::AppState,
    model: Option<String>,
    selected_value: &str,
    picked_effort: ModelEffortLevel,
    has_toggled_effort: bool,
) -> bool {
    apply_model_picker_selection(
        state,
        model,
        selected_value,
        picked_effort,
        has_toggled_effort,
    )
}

fn external_editor_api_key_status(status: VerificationStatus) -> ApiKeyVerificationStatus {
    match status {
        VerificationStatus::Valid => ApiKeyVerificationStatus::Valid,
        VerificationStatus::Invalid => ApiKeyVerificationStatus::Invalid,
        VerificationStatus::Missing => ApiKeyVerificationStatus::Missing,
        VerificationStatus::Loading | VerificationStatus::Error => {
            ApiKeyVerificationStatus::Unknown
        }
    }
}

fn visible_prompt_suggestion(
    state: &crate::state::app_state_store::PromptSuggestionState,
    input: &str,
    is_loading: bool,
    prompt_mode: bool,
    has_typeahead: bool,
    viewing_agent: bool,
) -> Option<String> {
    (!is_loading && input.is_empty() && prompt_mode && !has_typeahead && !viewing_agent)
        .then(|| state.text.clone())
        .flatten()
}

fn current_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptSubmission {
    pub text: String,
    pub pasted_contents: std::collections::BTreeMap<usize, input_paste::PastedContent>,
    /// Maps to: CC `onSubmit(..., { fromKeybinding: true })`.
    pub from_keybinding: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptInputTextUpdate {
    pub revision: u64,
    pub text: String,
    /// CC REPL restoreMessageSync's independently optional state updates.
    pub mode: Option<PromptInputMode>,
    pub pasted_contents: Option<BTreeMap<usize, input_paste::PastedContent>>,
    pub cursor_offset: Option<usize>,
}

#[derive(Default, Props)]
pub struct PromptInputProps<'a> {
    /// Maps to: CC `PromptInputProps.debug`, forwarded from REPL launch props.
    pub debug: bool,
    /// Maps to: CC `PromptInputProps.apiKeyStatus`, owned by REPL's
    /// `useApiKeyVerification` hook.
    pub api_key_status: VerificationStatus,
    /// Maps to: CC `PromptInput.tsx:239` `autoUpdaterResult` — REPL-owned
    /// state (REPL.tsx:1408), prop-drilled to Footer → Notifications.
    pub auto_updater_result: Option<crate::utils::auto_updater::AutoUpdaterResult>,
    /// Maps to: CC `PromptInput.tsx:238` `onAutoUpdaterResult` — REPL
    /// `setAutoUpdaterResult` (REPL.tsx:6829).
    pub on_auto_updater_result: Handler<crate::utils::auto_updater::AutoUpdaterResult>,
    /// Maps to: CC `PromptInput.tsx:229,317` `ideSelection` — REPL-owned state
    /// (REPL.tsx:1111), prop-drilled to Footer (`:3069`) → Notifications.
    pub ide_selection: Option<crate::hooks::use_ide_selection::IdeSelection>,
    /// Maps to: CC `PromptInput.tsx:237` `messages` prop (REPL-owned), passed
    /// through `PromptInputFooter` (`:3073`) to `Notifications` for its
    /// component-local tokenUsage derivation.
    pub messages: std::sync::Arc<Vec<crate::types::message::Message>>,
    /// CC `useTypeahead({ commands })` launch snapshot. Provider-less tests may
    /// omit it and use the same cwd-based resolver as `main.rs`.
    pub commands: Option<Arc<Vec<Command>>>,
    /// Initial controlled value seam. REPL owns the durable prompt snapshot so
    /// local-command unmount/remount cycles preserve unsent text.
    pub initial_input: Option<String>,
    /// Maps to: CC `PromptInputProps.onInputChange`. A shared `Handler` (not
    /// `HandlerMut`) so the text input's change event can call it directly,
    /// the way CC's `onChange` calls `onInputChange`.
    pub on_input_change: Handler<String>,
    /// Retained equivalent of REPL's controlled input/mode/paste/cursor state.
    /// Echoes edits with the same revision; only owner-initiated updates advance it.
    pub on_input_state_change: Handler<PromptInputTextUpdate>,
    /// Projects CC `useIsModalOverlayActive()` to the REPL-level dynamic
    /// command-keybinding owner.
    pub on_modal_overlay_change: HandlerMut<'a, bool>,
    /// Retained equivalents of CC's controlled-value and `insertTextRef` seams.
    /// A new revision applies each update exactly once.
    pub controlled_input: Option<PromptInputTextUpdate>,
    pub insert_text: Option<PromptInputTextUpdate>,
    /// Deterministic adapter seam for embedded owners and offline tests. Normal
    /// production reads the platform clipboard only after `chat:imagePaste`.
    pub clipboard_image_override: Option<crate::utils::image_paste::ClipboardImage>,
    /// Test/runtime override; normal production reads the configured editor mode.
    pub vim_enabled_override: Option<bool>,
    pub on_submit: HandlerMut<'a, PromptSubmission>,
    pub on_exit: HandlerMut<'a, ()>,
    /// Maps to: CC `onShowMessageSelector` — empty-input double-Escape opens
    /// the rewind MessageSelector (PromptInput.tsx:1663).
    pub on_show_message_selector: HandlerMut<'a, ()>,
    pub is_local_command_ui_active: bool,
    pub is_loading: bool,
    /// Whether `messages` state already contains an assistant message. Used by
    /// the thinking-toggle mid-conversation confirmation gate.
    pub has_assistant_messages: bool,
    pub permission_mode: PermissionMode,
    /// Live team/agent projection consumed by the official swarm banner policy.
    /// Viewing-agent name/color are filled from `AppState.viewing_agent_task_id`
    /// inside PromptInput (CC `viewingAgentTaskId`), not passed as a prop.
    pub swarm_banner_input: use_swarm_banner::SwarmBannerInput,
    /// Shift+Tab intent callback. REPL owns the actual permission-mode cycle
    /// via `utils::permissions::get_next_permission_mode`.
    pub on_permission_mode_cycle: HandlerMut<'a, ()>,
}

/// Maps to: CC PromptInput.tsx:1148-1160 `onChange` — cancel pending prompt
/// suggestion / speculation work and reset autocomplete for the new text.
/// Called from the text input's change event for typed edits, and from the
/// render-body `prev_input` diff for programmatic ones.
fn on_input_text_changed(
    app_store: Option<&crate::state::store::AppStore>,
    text: &str,
    mut prev_input: iocraft::hooks::State<String>,
    mut typeahead_selection_reset: iocraft::hooks::Ref<bool>,
    mut autocomplete_dismissed_input: iocraft::hooks::State<Option<String>>,
    mut help_open: iocraft::hooks::State<bool>,
) {
    crate::services::prompt_suggestion::prompt_suggestion::abort_prompt_suggestion();
    if let Some(active) = app_store.and_then(|store| {
        let state = store.get();
        match &state.speculation {
            crate::state::app_state_store::SpeculationState::Active(active) => {
                Some(active.clone())
            }
            crate::state::app_state_store::SpeculationState::Idle => None,
        }
    }) {
        active.abort_controller.abort();
        let context = active.cache_safe_params.tool_use_context.clone();
        tokio::spawn(async move {
            crate::services::prompt_suggestion::speculation::abort_speculation(&context).await;
        });
    }
    prev_input.set(text.to_string());
    typeahead_selection_reset.set(true);
    autocomplete_dismissed_input.set(None);
    if help_open.get() {
        help_open.set(false);
    }
}

#[component]
pub fn PromptInput<'a>(
    props: &mut PromptInputProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Maps to CC PromptInput.tsx: `input` + `cursorOffset` are controlled by
    // PromptInput and passed down into TextInput/useTextInput.
    let initial_commands_fallback = hooks.use_const({
        let provided = props.commands.clone();
        move || {
            provided.unwrap_or_else(|| {
                #[cfg(test)]
                {
                    Arc::new(crate::commands::declared_commands_for_tests())
                }
                #[cfg(not(test))]
                {
                    Arc::new(crate::commands::get_commands(
                        &crate::bootstrap::state::get_original_cwd(),
                    ))
                }
            })
        }
    });
    // Local launch commands are immutable, but `useMergedCommands` returns a
    // new Arc when live MCP prompts change. Current props must therefore win;
    // the const value is only the provider-less fallback, not a frozen copy of
    // the first merged command array.
    let commands = props.commands.clone().unwrap_or(initial_commands_fallback);
    // Maps to: CC PromptInput.tsx:367 `const [isAutoUpdating, setIsAutoUpdating]
    // = useState(false)` — PromptInput-local; the setter travels down as
    // `onChangeIsUpdating` (:3056) and the updater children's check loops
    // drive it (AutoUpdater.tsx:112/:170, NativeAutoUpdater.tsx:99/:166).
    let is_auto_updating = hooks.use_state(|| false);
    let on_change_is_updating: Handler<bool> = Handler::from(move |value: bool| {
        let mut is_auto_updating = is_auto_updating;
        is_auto_updating.set(value);
    });
    let app_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    // Maps to: CC `PromptInput.tsx:1031` — `const { addNotification,
    // removeNotification } = useNotifications()`, called ONCE at the top of the
    // component. React forbids a conditional hook call and so does iocraft;
    // the five scattered call sites this replaces included three inside
    // branches, which is a render-time "Unexpected hook type!" panic the moment
    // the hook owns a slot.
    let notifications = crate::context::notifications::use_notifications(&mut hooks);
    let initial_app_state = app_store.as_ref().map(|store| store.get());
    let prompt_suggestion_state =
        use_app_state(&mut hooks, |state| state.prompt_suggestion.clone());
    let prompt_suggestion_viewing_agent =
        use_app_state(&mut hooks, |state| state.viewing_agent_task_id.is_some());
    let initial_model = initial_app_state
        .as_ref()
        .and_then(|state| state.main_loop_model.clone());
    let initial_thinking = initial_app_state
        .as_ref()
        .and_then(|state| state.thinking_enabled)
        .unwrap_or(true);
    let fast_mode_on = initial_app_state
        .as_ref()
        .is_some_and(|state| state.fast_mode);
    let fast_mode_feature_enabled = crate::utils::fast_mode::is_fast_mode_enabled();
    let fast_mode_cooldown =
        fast_mode_feature_enabled && crate::utils::fast_mode::is_fast_mode_cooldown();
    // Maps to CC PromptInput.tsx `showFastIcon`: user state alone is not
    // sufficient; the feature must be available (or currently cooling down).
    let show_fast_icon = should_show_fast_icon(
        fast_mode_feature_enabled,
        fast_mode_on,
        crate::utils::fast_mode::is_fast_mode_available(),
        fast_mode_cooldown,
    );
    let fast_icon_hint =
        use_show_fast_icon_hint::use_show_fast_icon_hint(&mut hooks, show_fast_icon);
    let initial_input = props.initial_input.clone().unwrap_or_default();
    let initial_cursor_offset = initial_input.len();
    let initial_edit_snapshot = PromptEditSnapshot {
        text: initial_input.clone(),
        cursor_offset: initial_cursor_offset,
        pasted_contents: std::collections::BTreeMap::new(),
    };
    let mut input = hooks.use_state(move || initial_input);
    let mut cursor_offset = hooks.use_state(move || initial_cursor_offset);
    let mut input_mode = hooks.use_state(|| PromptInputMode::Prompt);
    let vim_enabled = props
        .vim_enabled_override
        .unwrap_or_else(utils::is_vim_mode_enabled);
    let mut vim_mode = hooks.use_state(|| VimMode::Insert);
    let mut vim_pending_command = hooks.use_state(|| Option::<VimPendingCommand>::None);
    let mut submit_count = hooks.use_state(|| 0usize);
    let mut pasted_contents = hooks.use_state(std::collections::BTreeMap::new);
    let mut last_controlled_revision = hooks.use_state(|| Option::<u64>::None);
    let mut last_insert_revision = hooks.use_state(|| Option::<u64>::None);
    if let Some(update) = props.controlled_input.as_ref() {
        if last_controlled_revision.get() != Some(update.revision) {
            input.set(update.text.clone());
            cursor_offset.set(crate::utils::cursor::clamp_cursor(
                &update.text,
                update.cursor_offset.unwrap_or(update.text.len()),
            ));
            if let Some(mode) = update.mode {
                input_mode.set(mode);
            }
            if let Some(contents) = &update.pasted_contents {
                pasted_contents.set(contents.clone());
            }
            last_controlled_revision.set(Some(update.revision));
        }
    }
    if let Some(update) = props.insert_text.as_ref() {
        if last_insert_revision.get() != Some(update.revision) {
            insert_search_dialog_text(input, cursor_offset, update.text.clone());
            last_insert_revision.set(Some(update.revision));
        }
    }
    // Refs, not state: the undo buffer is never rendered, and it is recorded
    // by diffing snapshots during render — a `State` write there is a
    // render-phase update that costs the frame a second update pass.
    let mut undo_stack = hooks.use_ref(Vec::<PromptEditSnapshot>::new);
    let mut last_edit_snapshot = hooks.use_ref(move || initial_edit_snapshot);
    let mut stashed_prompt = hooks.use_state(|| Option::<PromptStash>::None);
    let mut show_model_picker = hooks.use_state(|| false);
    let mut show_fast_mode_picker = hooks.use_state(|| false);
    let mut show_thinking_toggle = hooks.use_state(|| false);
    let mut thinking_toggle_focus = hooks.use_state(|| usize::from(!initial_thinking));
    let mut thinking_confirmation = hooks.use_state(|| Option::<bool>::None);
    let mut is_external_editor_active = hooks.use_state(|| false);
    let mut editor_result = hooks.use_state(|| Option::<EditorResult>::None);
    let mut clipboard_image_result =
        hooks.use_state(|| Option::<Option<crate::utils::image_paste::ClipboardImage>>::None);
    use_maybe_truncate_input::use_maybe_truncate_input(
        &mut hooks,
        use_maybe_truncate_input::MaybeTruncateInputState {
            input,
            cursor_offset,
            pasted_contents,
        },
    );
    if input_mode.get() == PromptInputMode::Prompt && input.read().starts_with('!') {
        let value = input_modes::get_value_from_input(&input.read());
        input_mode.set(PromptInputMode::Bash);
        cursor_offset.set(cursor_offset.get().saturating_sub(1).min(value.len()));
        input.set(value);
    }
    let mut prev_input = hooks.use_state(|| String::new());
    let mut autocomplete_dismissed_input = hooks.use_state(|| Option::<String>::None);
    let mut should_submit_suggestion = hooks.use_state(|| false);
    let mut should_cycle_permission_mode = hooks.use_state(|| false);
    let mut should_stash = hooks.use_state(|| false);
    let mut should_open_external_editor = hooks.use_state(|| false);
    let mut should_insert_newline = hooks.use_state(|| false);
    let mut should_undo = hooks.use_state(|| false);
    let mut should_submit_action = hooks.use_state(|| false);
    let mut help_open = hooks.use_state(|| false);
    let mut show_quick_open = hooks.use_state(|| false);
    let mut show_global_search = hooks.use_state(|| false);
    let mut show_history_picker = hooks.use_state(|| false);
    let mut show_background_tasks = hooks.use_state(|| false);
    let mut show_teams_dialog = hooks.use_state(|| false);
    let mut show_bridge_dialog = hooks.use_state(|| false);
    // PromptInput-scoped footer layout wake (L1, PORTING.md): created before
    // any dialog early-return so the hook slot order is stable; injected via
    // a narrow ContextProvider around the Footer subtree below. Producers
    // (memory 10s poll / sandbox subscription + decay / apiKeyHelper 1s poll)
    // bump it on visibility flips only; the bump wakes PromptInput without
    // touching AppStore.
    let footer_layout_wake = footer_layout_wake::use_footer_layout_wake(&mut hooks);
    // Maps to: CC `useAppState(s => s.footerSelection)` — AppState so pills
    // rendered outside PromptInput can read focus without prop-drilling.
    let raw_footer_selection = use_app_state(&mut hooks, |state| state.footer_selection);
    // Maps to: CC PromptInput `bridgeFooterVisible` — must match pill visibility
    // so bridge is not an invisible selection stop.
    let bridge_footer_visible = use_app_state(&mut hooks, |state| {
        state.repl_bridge_connected
            && (state.repl_bridge_explicit || state.repl_bridge_reconnecting)
    });
    // CC `AppState.tsx:138-148`: "Do NOT return new objects from the selector
    // -- Object.is will always see them as changed. … For multiple independent
    // fields, call the hook multiple times." One selector per field, and the
    // snapshot is assembled OUTSIDE the selectors so `BridgeDialog` keeps
    // taking it as a pure input.
    let bridge_connected = use_app_state(&mut hooks, |state| state.repl_bridge_connected);
    let bridge_session_active = use_app_state(&mut hooks, |state| state.repl_bridge_session_active);
    let bridge_reconnecting = use_app_state(&mut hooks, |state| state.repl_bridge_reconnecting);
    let bridge_connect_url =
        use_app_state(&mut hooks, |state| state.repl_bridge_connect_url.clone());
    let bridge_session_url =
        use_app_state(&mut hooks, |state| state.repl_bridge_session_url.clone());
    let bridge_error = use_app_state(&mut hooks, |state| state.repl_bridge_error.clone());
    let bridge_explicit = use_app_state(&mut hooks, |state| state.repl_bridge_explicit);
    let bridge_environment_id =
        use_app_state(&mut hooks, |state| state.repl_bridge_environment_id.clone());
    let bridge_session_id = use_app_state(&mut hooks, |state| state.repl_bridge_session_id.clone());
    let bridge_verbose = use_app_state(&mut hooks, |state| state.verbose);
    let bridge_dialog_snapshot = BridgeDialogSnapshot {
        connected: bridge_connected,
        session_active: bridge_session_active,
        reconnecting: bridge_reconnecting,
        connect_url: bridge_connect_url,
        session_url: bridge_session_url,
        error: bridge_error,
        explicit: bridge_explicit,
        environment_id: bridge_environment_id,
        session_id: bridge_session_id,
        verbose: bridge_verbose,
        repo_name: None,
        branch_name: None,
    };
    let external_editor_hint_signature = hooks.use_state(|| Option::<String>::None);
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let editor_channel = hooks.use_const(|| Arc::new(async_channel::unbounded::<String>()));
    let image_paste_channel = hooks.use_const(|| {
        Arc::new(async_channel::unbounded::<
            Option<crate::utils::image_paste::ClipboardImage>,
        >())
    });
    let deferred_action_channel =
        hooks.use_const(|| Arc::new(async_channel::unbounded::<PromptDeferredAction>()));
    let deferred_action_receiver = deferred_action_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(action) = deferred_action_receiver.recv().await {
            // Input editing and action hooks subscribe independently. Let the
            // editing owner commit this key batch before reading its state.
            futures_timer::Delay::new(std::time::Duration::from_millis(5)).await;
            match action {
                PromptDeferredAction::Stash => should_stash.set(true),
                PromptDeferredAction::ExternalEditor => should_open_external_editor.set(true),
                PromptDeferredAction::Newline => should_insert_newline.set(true),
                PromptDeferredAction::Undo => should_undo.set(true),
                PromptDeferredAction::Submit => should_submit_action.set(true),
                PromptDeferredAction::SubmitSuggestion => should_submit_suggestion.set(true),
            }
        }
    });
    let image_paste_receiver = image_paste_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(image) = image_paste_receiver.recv().await {
            clipboard_image_result.set(Some(image));
        }
    });
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(current) = editor_receiver.recv().await {
            let result = match editor_runtime {
                Some(runtime) => runtime.edit_prompt(&current).await,
                None => EditorResult {
                    content: None,
                    error: Some("External editor is unavailable".to_string()),
                },
            };
            editor_result.set(Some(result));
        }
    });
    let completed_image_paste = { clipboard_image_result.read().clone() };
    if let Some(image) = completed_image_paste {
        clipboard_image_result.set(None);
        if let Some(image) = image {
            let id = pasted_contents
                .read()
                .keys()
                .next_back()
                .copied()
                .unwrap_or(0)
                + 1;
            let mut next = pasted_contents.read().clone();
            next.insert(
                id,
                input_paste::PastedContent::Image {
                    id,
                    media_type: Some(image.media_type),
                    data: Some(image.base64),
                    filename: Some("Pasted image".to_string()),
                    dimensions: image.dimensions,
                    source_path: None,
                },
            );
            pasted_contents.set(next);
            insert_search_dialog_text(input, cursor_offset, prompt_history::format_image_ref(id));
        } else {
            let mut notifications = notifications.clone();
            notifications.add_notification(
                crate::context::notifications::Notification::text(
                    "no-image-in-clipboard",
                    "No image found in clipboard. Use the configured image-paste shortcut after copying an image.",
                    crate::context::notifications::NotificationPriority::High,
                )
                .with_color(crate::context::notifications::NotificationColor::Warning),
            );
        }
    }
    let completed_editor = { editor_result.read().clone() };
    if let Some(result) = completed_editor {
        editor_result.set(None);
        is_external_editor_active.set(false);
        if let Some(error) = result.error {
            let mut notifications = notifications.clone();
            notifications.add_notification(
                crate::context::notifications::Notification::text(
                    "external-editor-error",
                    error,
                    crate::context::notifications::NotificationPriority::High,
                )
                .with_color(crate::context::notifications::NotificationColor::Warning),
            );
        } else if let Some(content) = result.content {
            if content != *input.read() {
                cursor_offset.set(content.len());
                input.set(content);
            }
        }
    }
    let local_command_ui_active = props.is_local_command_ui_active;
    let search_dialog_active =
        show_quick_open.get() || show_global_search.get() || show_history_picker.get();
    // CC BackgroundTaskStatus.tsx:46 and BackgroundTasksDialog.tsx:176 share
    // AppState.tasks. Shell items now read this same subscribed snapshot;
    // local-agent data still uses its established registry projection.
    let tasks = use_app_state(&mut hooks, |state| state.tasks.clone());
    // Maps to: CC `useAppState(s => s.mcp.resources)` and
    // `useAppState(s => s.agentDefinitions.activeAgents)` consumed by
    // `useTypeahead`/`generateUnifiedSuggestions`. Keep these selectors in
    // PromptInput so source updates wake the input without a second registry.
    let typeahead_mcp_resources = use_app_state(&mut hooks, |state| state.mcp.resources.clone());
    let typeahead_agents = use_app_state(&mut hooks, |state| {
        state.agent_definitions.active_agents.clone()
    });
    let background_task_items =
        crate::components::tasks::background_tasks_dialog::background_task_items(&tasks);
    // Maps to: CC BackgroundTaskStatus.tsx:49-57 `runningTasks` — the pill's
    // set (ant builds exclude panel agents); the pill label + count read it.
    let pill_tasks = prompt_pill_tasks(&background_task_items);
    let teams_dialog_data = prompt_teams_data();
    let prompt_dialog_active = search_dialog_active
        || show_model_picker.get()
        || show_fast_mode_picker.get()
        || show_thinking_toggle.get()
        || show_background_tasks.get()
        || show_teams_dialog.get()
        || show_bridge_dialog.get()
        || is_external_editor_active.get();
    (props.on_modal_overlay_change)(prompt_dialog_active);

    let vim_columns = prompt_text_input_columns(hooks.use_terminal_size().0);
    hooks.use_propagated_terminal_events(move |event| {
        if !vim_enabled || local_command_ui_active || prompt_dialog_active {
            return;
        }
        let TerminalEvent::Key(KeyEvent {
            code,
            kind,
            modifiers,
            ..
        }) = event.event()
        else {
            return;
        };
        if *kind == KeyEventKind::Release || modifiers.contains(KeyModifiers::CONTROL) {
            return;
        }
        if vim_mode.get() == VimMode::Insert {
            if matches!(code, KeyCode::Esc) {
                vim_mode.set(VimMode::Normal);
                event.stop_propagation();
            }
            return;
        }
        let current = input.read().clone();
        let cursor =
            crate::utils::cursor::Cursor::from_text(current, vim_columns, cursor_offset.get());
        let pending_vim_command = *vim_pending_command.read();
        if let Some(pending) = pending_vim_command {
            vim_pending_command.set(None);
            match (pending, code) {
                (VimPendingCommand::Delete, KeyCode::Char('d')) => {
                    let (next, offset) = delete_vim_logical_line(cursor.text(), cursor.offset());
                    input.set(next);
                    cursor_offset.set(offset);
                }
                (VimPendingCommand::Delete, KeyCode::Char('w')) => {
                    let next = cursor.delete_word_after();
                    input.set(next.text().to_string());
                    cursor_offset.set(next.offset());
                }
                (VimPendingCommand::Delete, KeyCode::Char('$')) => {
                    let next = cursor.delete_to_line_end().cursor;
                    input.set(next.text().to_string());
                    cursor_offset.set(next.offset());
                }
                (VimPendingCommand::Replace, KeyCode::Char(ch)) => {
                    let next = cursor.del().insert(&ch.to_string()).left();
                    input.set(next.text().to_string());
                    cursor_offset.set(next.offset());
                }
                _ => {}
            }
            event.stop_propagation();
            return;
        }
        let mut next_cursor = None;
        match code {
            KeyCode::Char('i') => vim_mode.set(VimMode::Insert),
            KeyCode::Char('a') => {
                cursor_offset.set(cursor.right().offset());
                vim_mode.set(VimMode::Insert);
            }
            KeyCode::Char('I') => {
                cursor_offset.set(cursor.start_of_line().offset());
                vim_mode.set(VimMode::Insert);
            }
            KeyCode::Char('A') => {
                cursor_offset.set(cursor.end_of_line().offset());
                vim_mode.set(VimMode::Insert);
            }
            KeyCode::Char('h') | KeyCode::Left => next_cursor = Some(cursor.left()),
            KeyCode::Char('l') | KeyCode::Right => next_cursor = Some(cursor.right()),
            KeyCode::Char('j') | KeyCode::Down => next_cursor = Some(cursor.down()),
            KeyCode::Char('k') | KeyCode::Up => next_cursor = Some(cursor.up()),
            KeyCode::Char('0') | KeyCode::Home => next_cursor = Some(cursor.start_of_line()),
            KeyCode::Char('$') | KeyCode::End => next_cursor = Some(cursor.end_of_line()),
            KeyCode::Char('w') => next_cursor = Some(cursor.next_word()),
            KeyCode::Char('b') => next_cursor = Some(cursor.prev_word()),
            KeyCode::Char('d') => vim_pending_command.set(Some(VimPendingCommand::Delete)),
            KeyCode::Char('r') => vim_pending_command.set(Some(VimPendingCommand::Replace)),
            KeyCode::Char('D') => {
                let next = cursor.delete_to_line_end().cursor;
                input.set(next.text().to_string());
                cursor_offset.set(next.offset());
            }
            KeyCode::Char('C') => {
                let next = cursor.delete_to_line_end().cursor;
                input.set(next.text().to_string());
                cursor_offset.set(next.offset());
                vim_mode.set(VimMode::Insert);
            }
            KeyCode::Char('o') => {
                let end = cursor.end_of_line().offset();
                let mut value = cursor.text().to_string();
                value.insert(end, '\n');
                input.set(value);
                cursor_offset.set(end + 1);
                vim_mode.set(VimMode::Insert);
            }
            KeyCode::Char('O') => {
                let start = cursor.start_of_line().offset();
                let mut value = cursor.text().to_string();
                value.insert(start, '\n');
                input.set(value);
                cursor_offset.set(start);
                vim_mode.set(VimMode::Insert);
            }
            KeyCode::Char('J') => {
                let end = cursor.end_of_line().offset();
                let mut value = cursor.text().to_string();
                if value[end..].starts_with('\n') {
                    value.replace_range(end..end + 1, " ");
                    input.set(value);
                    cursor_offset.set(end);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                let next = cursor.del();
                input.set(next.text().to_string());
                cursor_offset.set(next.offset());
            }
            KeyCode::Esc => {}
            KeyCode::Enter => return,
            KeyCode::Char(_) => {}
            _ => return,
        }
        if let Some(next) = next_cursor {
            cursor_offset.set(next.offset());
        }
        event.stop_propagation();
    });

    // Keep typed text separated from an image chip. Cursor movement already
    // treats chips atomically; this mirrors CC's post-image spacing filter.
    hooks.use_propagated_terminal_events(move |event| {
        if local_command_ui_active
            || prompt_dialog_active
            || (vim_enabled && vim_mode.get() == VimMode::Normal)
        {
            return;
        }
        let TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char(ch),
            kind,
            modifiers,
            ..
        }) = event.event()
        else {
            return;
        };
        if *kind == KeyEventKind::Release
            || ch.is_whitespace()
            || modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::META)
        {
            return;
        }
        let value = input.read().clone();
        let offset = crate::utils::cursor::clamp_cursor(&value, cursor_offset.get());
        let before = &value[..offset];
        let Some(open) = before.rfind("[Image #") else {
            return;
        };
        if before[open..].ends_with(']')
            && before[open + "[Image #".len()..before.len() - 1]
                .chars()
                .all(|ch| ch.is_ascii_digit())
        {
            insert_search_dialog_text(input, cursor_offset, ch.to_string());
            event.stop_propagation();
        }
    });

    let has_editable_queued_command =
        crate::utils::message_queue_manager::get_command_queue_snapshot()
            .iter()
            .any(crate::utils::message_queue_manager::is_queued_command_editable);
    hooks.use_propagated_terminal_events(move |event| {
        if !has_editable_queued_command || local_command_ui_active || prompt_dialog_active {
            return;
        }
        let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
            return;
        };
        if *kind == KeyEventKind::Release || !matches!(code, KeyCode::Esc) {
            return;
        }
        let current_input = input.read().clone();
        if let Some(result) = crate::utils::message_queue_manager::pop_all_editable(
            &current_input,
            cursor_offset.get(),
        ) {
            input.set(result.text);
            cursor_offset.set(result.cursor_offset);
            input_mode.set(PromptInputMode::Prompt);
            if !result.images.is_empty() {
                let mut restored = pasted_contents.read().clone();
                for image in result.images {
                    restored.insert(image.id(), image);
                }
                pasted_contents.set(restored);
            }
            event.stop_propagation();
        }
    });

    // Exit shell mode from an empty input before TextInput handles the key.
    // This is official view-local mode behavior rather than a configurable
    // application action.
    hooks.use_propagated_terminal_events(move |event| {
        let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
            return;
        };
        if *kind == KeyEventKind::Release
            || local_command_ui_active
            || prompt_dialog_active
            || input_mode.get() != PromptInputMode::Bash
            || !input.read().is_empty()
        {
            return;
        }
        if matches!(code, KeyCode::Backspace | KeyCode::Delete | KeyCode::Esc) {
            input_mode.set(PromptInputMode::Prompt);
            // CC PromptInput.tsx:2459-2465 changes mode without returning:
            // the same Escape still reaches doublePressEscFromEmpty below.
            if !matches!(code, KeyCode::Esc) {
                event.stop_propagation();
            }
        }
    });

    // Maps to CC PromptInput's Chat/Global action registry. Unlike the prior
    // raw-key branch this honors user keybinding overrides and chord ownership.
    let keybinding_runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let prompt_actions_active = !local_command_ui_active && !prompt_dialog_active;
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "app:quickOpen",
        ContextName::Global,
        move || prompt_actions_active,
        move || {
            show_quick_open.set(true);
            help_open.set(false);
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "app:globalSearch",
        ContextName::Global,
        move || prompt_actions_active,
        move || {
            show_global_search.set(true);
            help_open.set(false);
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "help:dismiss",
        ContextName::Help,
        move || help_open.get(),
        move || {
            help_open.set(false);
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "history:search",
        ContextName::Global,
        move || prompt_actions_active,
        move || {
            show_history_picker.set(true);
            help_open.set(false);
            true
        },
    );
    // Maps to: CC hooks/useBackgroundTaskNavigation.ts:81-83 — the
    // dialog-open gate is `isBackgroundTask(t) && t.type !==
    // 'in_process_teammate'` with NO ant panel-agent exclusion, and the
    // dialog items (BackgroundTasksDialog.tsx:225-228) carry none either;
    // only the pill's visibility/count set (BackgroundTaskStatus.tsx:49-57,
    // `pill_tasks` above) excludes panel agents. So the Tasks footer entry
    // (the route to the dialog) gates on the UNexcluded item set while the
    // pill label + count keep the excluded set; the pill itself hides via
    // `background_task_count > 0` in the footer. In external (non-ant)
    // builds the two sets are identical.
    let has_background_tasks = !background_task_items.is_empty();
    let footer_items = [
        has_background_tasks.then_some(FooterItem::Tasks),
        teams_dialog_data.is_some().then_some(FooterItem::Teams),
        bridge_footer_visible.then_some(FooterItem::Bridge),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    // Maps to: CC PromptInput effective `footerItemSelected` + clear effect —
    // null out AppState when the selected pill stopped rendering.
    let footer_item_selected = raw_footer_selection.filter(|item| footer_items.contains(item));
    if raw_footer_selection.is_some() && footer_item_selected.is_none() {
        if let Some(store) = app_store.as_ref() {
            // P3 §3a (F-A1): CC clears via `prev.footerSelection === null
            // ? prev : { ...prev, footerSelection: null }`
            // (PromptInput.tsx:671-673). Render-body write — an already-null
            // selection returns `prev` untouched (no frame scheduled).
            store.set_state(|prev| {
                if prev.footer_selection.is_none() {
                    return crate::state::store::UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.footer_selection = None;
                crate::state::store::UpdateDecision::Replace {
                    next: Arc::new(next),
                    result: (),
                }
            });
        }
    }
    let select_footer_item = {
        let store = app_store.clone();
        move |item: Option<FooterItem>| {
            if let Some(store) = store.as_ref() {
                // P3 §3a (F-A1): CC selects via `prev.footerSelection === item
                // ? prev : { ...prev, footerSelection: item }`
                // (PromptInput.tsx:686); an unchanged selection returns
                // `prev` untouched.
                store.set_state(|prev| {
                    if prev.footer_selection == item {
                        return crate::state::store::UpdateDecision::Same(());
                    }
                    let mut next = (**prev).clone();
                    next.footer_selection = item;
                    crate::state::store::UpdateDecision::Replace {
                        next: Arc::new(next),
                        result: (),
                    }
                });
            }
        }
    };
    for (action_name, delta) in [
        ("footer:up", -1isize),
        ("footer:previous", -1isize),
        ("footer:down", 1isize),
        ("footer:next", 1isize),
    ] {
        let items = footer_items.clone();
        let store = app_store.clone();
        let select = select_footer_item.clone();
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            action_name,
            ContextName::Footer,
            {
                let store = store.clone();
                move || {
                    store
                        .as_ref()
                        .and_then(|s| s.get().footer_selection)
                        .is_some()
                        && !prompt_dialog_active
                }
            },
            move || {
                let Some(current) = store.as_ref().and_then(|s| s.get().footer_selection) else {
                    return false;
                };
                let index = items.iter().position(|item| *item == current).unwrap_or(0);
                let next = if delta < 0 {
                    index.saturating_sub(delta.unsigned_abs())
                } else {
                    index
                        .saturating_add(delta as usize)
                        .min(items.len().saturating_sub(1))
                };
                if let Some(item) = items.get(next).copied() {
                    select(Some(item));
                }
                true
            },
        );
    }
    {
        let store = app_store.clone();
        let select = select_footer_item.clone();
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            "footer:openSelected",
            ContextName::Footer,
            {
                let store = store.clone();
                move || {
                    store
                        .as_ref()
                        .and_then(|s| s.get().footer_selection)
                        .is_some()
                        && !prompt_dialog_active
                }
            },
            move || {
                match store.as_ref().and_then(|s| s.get().footer_selection) {
                    Some(FooterItem::Tasks) => show_background_tasks.set(true),
                    Some(FooterItem::Teams) => show_teams_dialog.set(true),
                    Some(FooterItem::Bridge) => show_bridge_dialog.set(true),
                    Some(_) | None => return false,
                }
                select(None);
                true
            },
        );
    }
    {
        let store = app_store.clone();
        let select = select_footer_item.clone();
        use_keybinding(
            &mut hooks,
            keybinding_runtime.clone(),
            "footer:clearSelection",
            ContextName::Footer,
            {
                let store = store.clone();
                move || {
                    store
                        .as_ref()
                        .and_then(|s| s.get().footer_selection)
                        .is_some()
                        && !prompt_dialog_active
                }
            },
            move || {
                select(None);
                true
            },
        );
    }
    {
        let store = app_store.clone();
        let select = select_footer_item.clone();
        let mut input = input;
        let mut cursor_offset = cursor_offset;
        hooks.use_propagated_terminal_events(move |event| {
            if store
                .as_ref()
                .and_then(|s| s.get().footer_selection)
                .is_none()
                || prompt_dialog_active
            {
                return;
            }
            let TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char(ch),
                kind,
                modifiers,
                ..
            }) = event.event()
            else {
                return;
            };
            if *kind == KeyEventKind::Release
                || modifiers.intersects(
                    KeyModifiers::CONTROL
                        | KeyModifiers::ALT
                        | KeyModifiers::META
                        | KeyModifiers::SUPER,
                )
            {
                return;
            }
            select(None);
            let mut current = input.read().clone();
            let offset = crate::utils::cursor::clamp_cursor(&current, cursor_offset.get());
            current.insert(offset, *ch);
            input.set(current);
            cursor_offset.set(offset + ch.len_utf8());
            event.stop_propagation();
        });
    }
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:modelPicker",
        ContextName::Chat,
        move || prompt_actions_active,
        {
            let mut show_model_picker = show_model_picker;
            move || {
                show_model_picker.set(!show_model_picker.get());
                show_fast_mode_picker.set(false);
                show_thinking_toggle.set(false);
                help_open.set(false);
                true
            }
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:fastMode",
        ContextName::Chat,
        move || prompt_actions_active && crate::utils::fast_mode::is_fast_mode_enabled(),
        move || {
            show_fast_mode_picker.set(!show_fast_mode_picker.get());
            show_model_picker.set(false);
            show_thinking_toggle.set(false);
            help_open.set(false);
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:thinkingToggle",
        ContextName::Chat,
        move || prompt_actions_active,
        move || {
            show_thinking_toggle.set(!show_thinking_toggle.get());
            show_model_picker.set(false);
            show_fast_mode_picker.set(false);
            thinking_confirmation.set(None);
            help_open.set(false);
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:cycleMode",
        ContextName::Chat,
        move || prompt_actions_active,
        move || {
            should_cycle_permission_mode.set(true);
            help_open.set(false);
            true
        },
    );
    let deferred_stash_sender = deferred_action_channel.0.clone();
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:stash",
        ContextName::Chat,
        move || prompt_actions_active,
        move || {
            let _ = deferred_stash_sender.try_send(PromptDeferredAction::Stash);
            true
        },
    );
    let image_paste_sender = image_paste_channel.0.clone();
    let clipboard_image_override = props.clipboard_image_override.clone();
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:imagePaste",
        ContextName::Chat,
        move || prompt_actions_active,
        move || {
            let sender = image_paste_sender.clone();
            let override_image = clipboard_image_override.clone();
            std::thread::spawn(move || {
                let image =
                    override_image.or_else(crate::utils::image_paste::get_image_from_clipboard);
                let _ = sender.send_blocking(image);
            });
            true
        },
    );
    let editor_sender = editor_channel.0.clone();
    let external_editor_available = editor_runtime.is_some()
        && crate::utils::prompt_editor::external_editor_command().is_some();
    let deferred_editor_sender = deferred_action_channel.0.clone();
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:externalEditor",
        ContextName::Chat,
        move || prompt_actions_active && external_editor_available,
        move || {
            let _ = deferred_editor_sender.try_send(PromptDeferredAction::ExternalEditor);
            true
        },
    );
    let deferred_newline_sender = deferred_action_channel.0.clone();
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:newline",
        ContextName::Chat,
        move || prompt_actions_active,
        move || {
            let _ = deferred_newline_sender.try_send(PromptDeferredAction::Newline);
            true
        },
    );
    let deferred_undo_sender = deferred_action_channel.0.clone();
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "chat:undo",
        ContextName::Chat,
        move || prompt_actions_active,
        move || {
            let _ = deferred_undo_sender.try_send(PromptDeferredAction::Undo);
            true
        },
    );
    // CC PromptInput.tsx:2142-2146: history is handled by TextInput after cursor movement.
    let mut arrow_history = use_arrow_key_history(&mut hooks);
    let mut history_search = use_history_search(
        &mut hooks,
        UseHistorySearchOptions {
            input,
            cursor_offset,
            focus: !local_command_ui_active && !prompt_dialog_active,
        },
    );
    let history_search_active = history_search.is_active();
    let queued_commands = crate::hooks::use_command_queue::use_command_queue(&mut hooks);
    // Maps to: CC reading `getGlobalConfig().projects[...]` — cached direct
    // read; globalConfig is not part of CC AppState.
    let example_files = std::env::current_dir().ok().and_then(|cwd| {
        let key = crate::utils::config::normalize_project_path(&cwd.to_string_lossy());
        crate::utils::config::load_global_config()
            .projects
            .get(&key)
            .and_then(|project| project.example_files.clone())
    });
    let example_command = hooks.use_const(move || {
        crate::utils::example_commands::example_command_from_cache(
            example_files.as_deref(),
            crate::utils::example_commands::runtime_example_entropy(),
        )
    });

    let typeahead_input = input.read().clone();
    // Maps to: CC PromptInput.tsx:1508 — suppress the canonical suggestions,
    // so history navigation never competes with an invisible autocomplete.
    let typeahead = use_typeahead(
        &mut hooks,
        &typeahead_input,
        cursor_offset.get(),
        Arc::clone(&commands),
        Arc::new(typeahead_mcp_resources),
        Arc::new(typeahead_agents),
        input_mode.get() == PromptInputMode::Bash,
        history_search_active || arrow_history.history_index.get() > 0,
    );
    let thinking_toggle_shortcut =
        crate::keybindings::shortcut_format::get_shortcut_display_for_context_name(
            "chat:thinkingToggle",
            "Chat",
            "alt+t",
        );
    let has_suggestions = !history_search_active
        && typeahead.has_suggestions
        && (input_mode.get() == PromptInputMode::Prompt
            || matches!(
                typeahead.kind,
                SuggestionKind::Shell | SuggestionKind::Agent | SuggestionKind::SlackChannel
            ))
        && autocomplete_dismissed_input.read().as_deref() != Some(input.read().as_str());
    let typeahead_count = typeahead.suggestions.len() as i32;
    let mut typeahead_selected = typeahead.selected;
    let typeahead_selection_reset = typeahead.selection_reset;
    let typeahead_suggestions = Arc::clone(&typeahead.suggestions);
    let typeahead_kind = typeahead.kind;
    let typeahead_file_token = typeahead.file_token.clone();
    let max_column_width = typeahead.max_column_width;
    let typeahead_argument_hint = typeahead.command_argument_hint.clone();
    let typeahead_inline_ghost_text = typeahead.inline_ghost_text.clone();
    let typeahead_dismiss = typeahead.dismiss.clone();
    let empty_suggestions = hooks.use_const(Arc::<Vec<_>>::default);

    // Reset autocomplete selection when the actual input text changes. This is
    // the `onChange → selectedSuggestion=0` behavior from CC PromptInput.
    //
    // Typed changes already ran this in the text input's change event (the
    // `on_change` handler below, CC's `onChange`), so on a typing frame
    // `prev_input == input` and nothing here writes. This render-body diff
    // is the fallback for the programmatic edits (undo, stash, suggestion
    // accept, controlled updates, …) that set `input` without going through
    // the text input.
    if prev_input.read().as_str() != input.read().as_str() {
        on_input_text_changed(
            app_store.as_ref(),
            &input.read(),
            prev_input,
            typeahead_selection_reset,
            autocomplete_dismissed_input,
            help_open,
        );
    }

    // `useTypeahead` is the sole suggestion-state owner. Reuse its memoized
    // result for keybindings, terminal events, deferred submit, and rendering
    // instead of independently rebuilding the fuzzy result in each branch.
    let suggestions_for_accept = Arc::clone(&typeahead_suggestions);
    let file_token_for_accept = typeahead_file_token.clone();
    let inline_ghost_text_for_accept = typeahead_inline_ghost_text.clone();
    let commands_for_accept = Arc::clone(&commands);
    let mut autocomplete_dismissed_input_for_accept = autocomplete_dismissed_input;
    // Maps to: CC useTypeahead `useRegisterKeybindingContext('Autocomplete', …)` —
    // so Chat's history:previous/next resolvers last-win to autocomplete:*.
    use_register_keybinding_context(
        &mut hooks,
        keybinding_runtime.clone(),
        ContextName::Autocomplete,
        prompt_actions_active && has_suggestions,
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "autocomplete:accept",
        ContextName::Autocomplete,
        move || prompt_actions_active && has_suggestions,
        move || {
            let current_input = input.read().clone();
            let scope = crate::hooks::use_typeahead::TypeaheadHandlerScope {
                input: &current_input,
                cursor_offset: cursor_offset.get(),
                is_bash_mode: input_mode.get() == PromptInputMode::Bash,
                suggestions: suggestions_for_accept.as_slice(),
                selected_suggestion: typeahead_selected.get(),
                suggestion_type: typeahead_kind,
                file_token: file_token_for_accept.as_ref(),
                commands: commands_for_accept.as_slice(),
                effective_ghost_text: inline_ghost_text_for_accept
                    .as_ref()
                    .filter(|ghost| ghost.insert_position == cursor_offset.get()),
            };
            if let Some(action) = crate::hooks::use_typeahead::handle_autocomplete_accept(&scope) {
                match action {
                    crate::hooks::use_typeahead::SuggestionAcceptance::Replace {
                        input: value,
                        cursor_offset: offset,
                        dismiss,
                        submit_action,
                    } => {
                        cursor_offset.set(offset);
                        input.set(value.clone());
                        if dismiss {
                            autocomplete_dismissed_input_for_accept.set(Some(value));
                        }
                        debug_assert!(!submit_action);
                    }
                    crate::hooks::use_typeahead::SuggestionAcceptance::ClearSuggestions => {
                        autocomplete_dismissed_input_for_accept.set(Some(current_input.clone()));
                    }
                    crate::hooks::use_typeahead::SuggestionAcceptance::SubmitSuggestion
                    | crate::hooks::use_typeahead::SuggestionAcceptance::SubmitAction => {
                        debug_assert!(false, "handleTab never submits");
                    }
                }
                typeahead_selected.set(0);
            }
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "autocomplete:previous",
        ContextName::Autocomplete,
        move || prompt_actions_active && has_suggestions,
        move || {
            let selected = typeahead_selected.get();
            typeahead_selected.set(crate::hooks::use_typeahead::handle_autocomplete_previous(
                selected,
                typeahead_count,
            ));
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "autocomplete:next",
        ContextName::Autocomplete,
        move || prompt_actions_active && has_suggestions,
        move || {
            let selected = typeahead_selected.get();
            typeahead_selected.set(crate::hooks::use_typeahead::handle_autocomplete_next(
                selected,
                typeahead_count,
            ));
            true
        },
    );
    use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "autocomplete:dismiss",
        ContextName::Autocomplete,
        move || prompt_actions_active && has_suggestions,
        move || {
            // Maps to CC `handleAutocompleteDismiss`: cancel the debounced
            // producers, `clearSuggestions()`, then remember the input so the
            // synchronous producers stay quiet until it changes.
            typeahead_dismiss.handle_autocomplete_dismiss();
            autocomplete_dismissed_input.set(Some(input.read().clone()));
            true
        },
    );
    let deferred_submit_sender = deferred_action_channel.0.clone();
    let keybinding_runtime_for_submit = keybinding_runtime.clone();
    use_keybinding(
        &mut hooks,
        keybinding_runtime_for_submit,
        "chat:submit",
        ContextName::Chat,
        move || {
            prompt_actions_active && (!has_suggestions || typeahead_kind == SuggestionKind::Command)
        },
        move || {
            let action = if has_suggestions {
                PromptDeferredAction::SubmitSuggestion
            } else {
                PromptDeferredAction::Submit
            };
            let _ = deferred_submit_sender.try_send(action);
            true
        },
    );

    let pruned_pasted_contents =
        input_paste::prune_unreferenced_images(&input.read(), &pasted_contents.read());
    if pruned_pasted_contents != *pasted_contents.read() {
        pasted_contents.set(pruned_pasted_contents);
    }

    let current_edit_snapshot = PromptEditSnapshot {
        text: input.read().clone(),
        cursor_offset: cursor_offset.get(),
        pasted_contents: pasted_contents.read().clone(),
    };
    let observed_edit_snapshot = last_edit_snapshot.read().clone();
    if current_edit_snapshot != observed_edit_snapshot {
        if current_edit_snapshot.text != observed_edit_snapshot.text {
            let mut stack = undo_stack.read().clone();
            stack.push(observed_edit_snapshot);
            if stack.len() > 100 {
                stack.remove(0);
            }
            undo_stack.set(stack);
        }
        last_edit_snapshot.set(current_edit_snapshot);
    }

    // Autocomplete/help owns these keys before useTextInput sees them. Hooks are
    // polled in registration order, so stopping propagation here mirrors CC's
    // child autocomplete handlers blocking BaseTextInput.
    let suggestions_for_events = Arc::clone(&typeahead_suggestions);
    let file_token_for_events = typeahead_file_token.clone();
    let typeahead_kind_for_events = typeahead_kind;
    let inline_ghost_text_for_events = typeahead_inline_ghost_text.clone();
    let commands_for_events = Arc::clone(&commands);
    let mut autocomplete_dismissed_input_for_events = autocomplete_dismissed_input;
    let prompt_suggestion_for_keys = prompt_suggestion_state.clone();
    let app_store_for_suggestion_keys = app_store.clone();
    let mut input_mode_for_suggestion_keys = input_mode;
    let mut notifications_for_typeahead = notifications.clone();
    let keybinding_runtime_for_events = keybinding_runtime.clone();
    hooks.use_propagated_terminal_events({
        move |event| match event.event() {
            TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) if *kind != KeyEventKind::Release => {
                if local_command_ui_active || history_search_active || prompt_dialog_active {
                    return;
                }

                if help_open.get()
                    && matches!(code, KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete)
                {
                    help_open.set(false);
                    event.stop_propagation();
                    return;
                }

                if modifiers.is_empty()
                    && matches!(code, KeyCode::Char('?'))
                    && input.read().is_empty()
                {
                    help_open.set(!help_open.get());
                    event.stop_propagation();
                    return;
                }

                let prompt_suggestion_can_accept = input.read().is_empty()
                    && prompt_suggestion_for_keys.shown_at > 0
                    && prompt_suggestion_for_keys.text.is_some()
                    && !prompt_suggestion_viewing_agent;
                // Maps to CC useTypeahead `handleKeyDown` (bridged through
                // `useInput` in the source) followed by the `autocomplete:*`
                // keybinding fallbacks for hosts without a keybinding runtime.
                let current_input = input.read().clone();
                let visible_suggestions: &[prompt_input_footer_suggestions::SuggestionItem] =
                    if has_suggestions {
                        suggestions_for_events.as_slice()
                    } else {
                        &[]
                    };
                let scope = crate::hooks::use_typeahead::TypeaheadHandlerScope {
                    input: &current_input,
                    cursor_offset: cursor_offset.get(),
                    is_bash_mode: input_mode.get() == PromptInputMode::Bash,
                    suggestions: visible_suggestions,
                    selected_suggestion: typeahead_selected.get(),
                    suggestion_type: typeahead_kind_for_events,
                    file_token: file_token_for_events.as_ref(),
                    commands: commands_for_events.as_slice(),
                    effective_ghost_text: inline_ghost_text_for_events
                        .as_ref()
                        .filter(|ghost| ghost.insert_position == cursor_offset.get()),
                };
                let has_pending_chord = keybinding_runtime_for_events
                    .as_ref()
                    .is_some_and(|runtime| runtime.chord_pending());
                match crate::hooks::use_typeahead::handle_key_down(
                    *code,
                    *modifiers,
                    &scope,
                    prompt_suggestion_can_accept,
                    &thinking_toggle_shortcut,
                    has_pending_chord,
                ) {
                    Some(crate::hooks::use_typeahead::KeyDownEffect::AcceptPromptSuggestion) => {
                        let suggestion =
                            prompt_suggestion_for_keys.text.clone().unwrap_or_default();
                        // markAccepted()
                        if let Some(store) = app_store_for_suggestion_keys.as_ref() {
                            store.replace_with(|state| {
                                state.prompt_suggestion.accepted_at = current_time_millis();
                            });
                        }
                        // acceptSuggestionText(suggestionText)
                        let (mode, value) =
                            crate::hooks::use_typeahead::accept_suggestion_text(&suggestion);
                        input_mode_for_suggestion_keys.set(mode);
                        cursor_offset.set(value.len());
                        input.set(value);
                        event.stop_propagation();
                        return;
                    }
                    Some(crate::hooks::use_typeahead::KeyDownEffect::AddNotification(
                        notification,
                    )) => {
                        notifications_for_typeahead.add_notification(notification);
                        event.stop_propagation();
                        return;
                    }
                    Some(crate::hooks::use_typeahead::KeyDownEffect::SetSelectedSuggestion(
                        selected,
                    )) => {
                        typeahead_selected.set(selected);
                        event.stop_propagation();
                        return;
                    }
                    Some(crate::hooks::use_typeahead::KeyDownEffect::HandleEnter(action)) => {
                        match action {
                            Some(crate::hooks::use_typeahead::SuggestionAcceptance::Replace {
                                input: value,
                                cursor_offset: offset,
                                dismiss,
                                submit_action,
                            }) => {
                                cursor_offset.set(offset);
                                input.set(value.clone());
                                if dismiss {
                                    autocomplete_dismissed_input_for_events.set(Some(value));
                                }
                                if submit_action {
                                    should_submit_action.set(true);
                                }
                                typeahead_selected.set(0);
                            }
                            Some(
                                crate::hooks::use_typeahead::SuggestionAcceptance::SubmitSuggestion,
                            ) => {
                                should_submit_suggestion.set(true);
                            }
                            Some(
                                crate::hooks::use_typeahead::SuggestionAcceptance::SubmitAction,
                            ) => {
                                should_submit_action.set(true);
                                typeahead_selected.set(0);
                            }
                            Some(
                                crate::hooks::use_typeahead::SuggestionAcceptance::ClearSuggestions,
                            ) => {
                                autocomplete_dismissed_input_for_events
                                    .set(Some(current_input.clone()));
                                typeahead_selected.set(0);
                            }
                            None => {}
                        }
                        event.stop_propagation();
                        return;
                    }
                    None => {}
                }

                match code {
                    // `autocomplete:accept` fallback: Tab while suggestions or
                    // ghost text are active.
                    KeyCode::Tab
                        if !modifiers.contains(KeyModifiers::SHIFT)
                            && (!visible_suggestions.is_empty()
                                || scope.effective_ghost_text.is_some()) =>
                    {
                        match crate::hooks::use_typeahead::handle_autocomplete_accept(&scope) {
                            Some(crate::hooks::use_typeahead::SuggestionAcceptance::Replace {
                                input: value,
                                cursor_offset: offset,
                                dismiss,
                                submit_action,
                            }) => {
                                cursor_offset.set(offset);
                                input.set(value.clone());
                                if dismiss {
                                    autocomplete_dismissed_input_for_events.set(Some(value));
                                }
                                debug_assert!(!submit_action);
                                typeahead_selected.set(0);
                            }
                            Some(
                                crate::hooks::use_typeahead::SuggestionAcceptance::ClearSuggestions,
                            ) => {
                                autocomplete_dismissed_input_for_events
                                    .set(Some(current_input.clone()));
                                typeahead_selected.set(0);
                            }
                            Some(
                                crate::hooks::use_typeahead::SuggestionAcceptance::SubmitSuggestion
                                | crate::hooks::use_typeahead::SuggestionAcceptance::SubmitAction,
                            ) => {
                                debug_assert!(false, "handleTab never submits");
                            }
                            None => {}
                        }
                        event.stop_propagation();
                    }
                    // `autocomplete:previous` / `autocomplete:next` fallbacks.
                    KeyCode::Up if has_suggestions => {
                        let sel = typeahead_selected.get();
                        typeahead_selected.set(
                            crate::hooks::use_typeahead::handle_autocomplete_previous(
                                sel,
                                typeahead_count,
                            ),
                        );
                        event.stop_propagation();
                    }
                    KeyCode::Down if has_suggestions => {
                        let sel = typeahead_selected.get();
                        typeahead_selected.set(
                            crate::hooks::use_typeahead::handle_autocomplete_next(
                                sel,
                                typeahead_count,
                            ),
                        );
                        event.stop_propagation();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    });

    let (columns, terminal_rows) = hooks.use_terminal_size();
    let app_store_for_picker = app_store.clone();
    let mut notifications_for_picker = notifications.clone();
    let has_assistant_messages = props.has_assistant_messages;
    hooks.use_propagated_terminal_events(move |event| {
        let TerminalEvent::Key(KeyEvent {
            code,
            kind,
            modifiers,
            ..
        }) = event.event()
        else {
            return;
        };
        if *kind == KeyEventKind::Release {
            return;
        }

        if show_model_picker.get() {
            return;
        }

        if show_thinking_toggle.get() {
            let selected = thinking_toggle_focus.get() == 0;
            let current = app_store_for_picker
                .as_ref()
                .and_then(|store| store.get().thinking_enabled)
                .unwrap_or(true);
            match code {
                KeyCode::Esc => {
                    if thinking_confirmation.read().is_some() {
                        thinking_confirmation.set(None);
                    } else {
                        show_thinking_toggle.set(false);
                    }
                }
                KeyCode::Up | KeyCode::Down => {
                    if thinking_confirmation.read().is_none() {
                        thinking_toggle_focus
                            .set(1usize.saturating_sub(thinking_toggle_focus.get().min(1)));
                    }
                }
                KeyCode::Enter if modifiers.is_empty() => {
                    if thinking_confirmation.read().is_none()
                        && thinking_toggle_requires_confirmation(
                            current,
                            selected,
                            has_assistant_messages,
                        )
                    {
                        thinking_confirmation.set(Some(selected));
                    } else {
                        let value = thinking_confirmation.read().unwrap_or(selected);
                        if let Some(store) = app_store_for_picker.as_ref() {
                            store.replace_with(|state| state.thinking_enabled = Some(value));
                        }
                        {
                            let context = &mut notifications_for_picker;
                            context.add_notification(
                                crate::context::notifications::Notification::text(
                                    "thinking-toggled-hotkey",
                                    format!("Thinking {}", if value { "on" } else { "off" }),
                                    crate::context::notifications::NotificationPriority::Immediate,
                                ),
                            );
                        }
                        thinking_confirmation.set(None);
                        show_thinking_toggle.set(false);
                    }
                }
                _ => return,
            }
            event.stop_propagation();
        }
    });
    let text_input_columns = prompt_text_input_columns(columns);
    // Official main-screen/native-scrollback PromptInput does not cap the
    // input viewport. The 50% bottom-slot cap is fullscreen-only upstream.
    let max_visible_lines = None;

    let text_input_focused = !history_search_active
        && !local_command_ui_active
        && !prompt_dialog_active
        && footer_item_selected.is_none();
    // CC PromptInput.tsx:1663,2510-2511 owns a separate double-press timer
    // from useTextInput's clear-input timer. Decide eligibility from this
    // render's input, not the final text after an entire input batch edits it.
    let double_press_esc_from_empty = use_double_press(&mut hooks);
    let mut pending_show_message_selector = hooks.use_state(|| 0usize);
    let rewind_escape_active = text_input_focused
        // CC's useInput closure keeps these branches active for the entire
        // render batch, even after its first Escape closes help or pops queue.
        && !help_open.get()
        && !has_editable_queued_command
        && !props.messages.is_empty()
        && input.read().is_empty()
        && !props.is_loading;
    hooks.use_propagated_terminal_events(move |event| {
        if rewind_escape_active
            && matches!(event.event(), TerminalEvent::Key(KeyEvent {
                code: KeyCode::Esc, kind, ..
            }) if *kind != KeyEventKind::Release)
        {
            double_press_esc_from_empty.press();
            if double_press_esc_from_empty.take_triggered() {
                // HandlerMut is invoked during render; preserve each source
                // callback instead of collapsing two pairs in one input batch.
                pending_show_message_selector.set(pending_show_message_selector.get() + 1);
            }
            // TextInput must receive this Escape too: its independent timer
            // still owns clearing. Earlier help/footer/queue/dialog handlers
            // already consume the keys that belong to their own actions.
        }
    });
    let pending_shows = pending_show_message_selector.get();
    if pending_shows > 0 {
        pending_show_message_selector.set(0);
        for _ in 0..pending_shows {
            (props.on_show_message_selector)(());
        }
    }
    let mut text_input = use_text_input(
        &mut hooks,
        UseTextInputOptions {
            // Maps to: CC PromptInput.tsx:1148-1160 `onChange` — the work that
            // follows a keystroke, plus `trackAndSetInput → onInputChange`
            // (:382-388), runs in the input event. Doing it here instead of by
            // diffing during render is what keeps a typing frame to one
            // update pass: every `State` written during render forces the
            // settle pass to re-run the update (and this component is most
            // of it). The render-body emission near the end stays as the
            // fallback for programmatic edits; the parent ignores repeats.
            on_change: Handler::from({
                let app_store = app_store.clone();
                let on_input_change = props.on_input_change.clone();
                let on_input_state_change = props.on_input_state_change.clone();
                move |text: String| {
                    if prev_input.read().as_str() == text.as_str() {
                        return;
                    }
                    on_input_text_changed(
                        app_store.as_ref(),
                        &text,
                        prev_input,
                        typeahead_selection_reset,
                        autocomplete_dismissed_input,
                        help_open,
                    );
                    on_input_change(text.clone());
                    on_input_state_change(PromptInputTextUpdate {
                        revision: last_controlled_revision.get().unwrap_or_default(),
                        text,
                        mode: Some(input_mode.get()),
                        pasted_contents: Some(pasted_contents.read().clone()),
                        cursor_offset: Some(cursor_offset.get()),
                    });
                }
            }),
            on_clear_input: Handler::default(),
            on_history_reset: Handler::from({
                let arrow_history = arrow_history.clone();
                move |_| {
                    let mut history = arrow_history.clone();
                    history.reset();
                }
            }),
            escape_event_passthrough: false,
            select_navigation_passthrough: false,
            value: input,
            cursor_offset,
            inline_ghost_text: typeahead_inline_ghost_text.clone(),
            focus: text_input_focused,
            multiline: true,
            columns: text_input_columns,
            max_visible_lines,
            disable_cursor_movement_for_up_down_keys: has_suggestions,
            disable_escape_double_press: has_suggestions,
            // While a query is running, Escape belongs to the Repl-level
            // chat:cancel handler (CC CancelRequestHandler isActive gate);
            // the input yields Escape. Ctrl+C stays with double-press exit
            // (default Global binding app:exit — old semantics).
            cancel_passthrough: props.is_loading,
        },
    );

    // Deferred actions observe the input state after TextInput has processed
    // the same terminal-event batch. This avoids action handlers racing ahead
    // of character edits on iocraft's independent event subscriptions.
    if should_undo.get() {
        should_undo.set(false);
        let mut stack = undo_stack.read().clone();
        if let Some(snapshot) = stack.pop() {
            undo_stack.set(stack);
            last_edit_snapshot.set(snapshot.clone());
            input.set(snapshot.text);
            cursor_offset.set(snapshot.cursor_offset);
            pasted_contents.set(snapshot.pasted_contents);
        }
    }
    if should_stash.get() {
        should_stash.set(false);
        if input.read().trim().is_empty() {
            let stash = stashed_prompt.read().clone();
            if let Some(stash) = stash {
                input.set(stash.text);
                cursor_offset.set(stash.cursor_offset);
                pasted_contents.set(stash.pasted_contents);
                stashed_prompt.set(None);
            }
        } else {
            stashed_prompt.set(Some(PromptStash {
                text: input.read().clone(),
                cursor_offset: cursor_offset.get(),
                mode: input_mode.get(),
                pasted_contents: pasted_contents.read().clone(),
            }));
            input.set(String::new());
            cursor_offset.set(0);
            pasted_contents.set(std::collections::BTreeMap::new());
        }
    }
    if should_open_external_editor.get() {
        should_open_external_editor.set(false);
        is_external_editor_active.set(true);
        if editor_sender.try_send(input.read().clone()).is_err() {
            is_external_editor_active.set(false);
        }
    }
    if should_insert_newline.get() {
        should_insert_newline.set(false);
        let mut current = input.read().clone();
        let offset = crate::utils::cursor::clamp_cursor(&current, cursor_offset.get());
        current.insert(offset, '\n');
        input.set(current);
        cursor_offset.set(offset + 1);
    }

    // Deferred submit/cycle actions — props handlers live outside terminal
    // event closure lifetimes.
    if should_submit_suggestion.get() {
        should_submit_suggestion.set(false);
        let selected_suggestion = typeahead_suggestions
            .get(typeahead_selected.get() as usize)
            .cloned();
        let command_application = selected_suggestion.as_ref().and_then(|item| {
            crate::utils::suggestions::command_suggestions::apply_command_suggestion(
                item,
                true,
                commands.as_slice(),
            )
        });
        let text = command_application
            .as_ref()
            .map(|application| application.input.clone())
            .or_else(|| {
                selected_suggestion
                    .as_ref()
                    .map(|item| item.command_text.clone())
            })
            .unwrap_or_else(|| input.to_string());
        let requires_arguments = command_application
            .as_ref()
            .is_some_and(|application| !application.should_submit)
            || (command_application.is_none()
                && selected_suggestion
                    .as_ref()
                    .is_some_and(|suggestion| {
                        crate::utils::suggestions::command_suggestions::command_suggestion_requires_arguments(
                            suggestion,
                            commands.as_slice(),
                        )
                    }));
        if requires_arguments {
            let text = format!("{} ", text.trim_end());
            cursor_offset.set(text.len());
            input.set(text.clone());
            autocomplete_dismissed_input.set(Some(text));
            typeahead_selected.set(0);
        } else {
            let text = input_modes::prepend_mode_character_to_input(&text, input_mode.get());
            let submitted_pasted_contents = pasted_contents.read().clone();
            prompt_history::add_to_history_with_pasted(&text, &submitted_pasted_contents);
            arrow_history.reset();
            restore_stash_or_clear(
                input,
                cursor_offset,
                pasted_contents,
                stashed_prompt,
                input_mode,
            );
            typeahead_selected.set(0);
            submit_count += 1;
            (props.on_submit)(PromptSubmission {
                text,
                pasted_contents: submitted_pasted_contents,
                from_keybinding: false,
            });
        }
    }

    if should_submit_action.get() {
        should_submit_action.set(false);
        let raw_text = input.read().clone();
        let submitted_pasted_contents = pasted_contents.read().clone();
        let has_images = submitted_pasted_contents.values().any(|content| {
            matches!(
                content,
                input_paste::PastedContent::Image { data: Some(_), .. }
            )
        });
        let accepted_suggestion = raw_text.trim().is_empty()
            && !has_images
            && prompt_suggestion_state.shown_at > 0
            && prompt_suggestion_state.text.is_some();
        let text = if accepted_suggestion {
            prompt_suggestion_state.text.clone().unwrap_or_default()
        } else {
            raw_text
        };
        if !text.trim().is_empty() {
            let text = input_modes::prepend_mode_character_to_input(&text, input_mode.get());
            prompt_history::add_to_history_with_pasted(&text, &submitted_pasted_contents);
            arrow_history.reset();
            restore_stash_or_clear(
                input,
                cursor_offset,
                pasted_contents,
                stashed_prompt,
                input_mode,
            );
            typeahead_selected.set(0);
            submit_count += 1;
            if prompt_suggestion_state.text.is_some() {
                if let Some(store) = app_store.as_ref() {
                    store.replace_with(|state| {
                        if matches!(
                            state.speculation,
                            crate::state::app_state_store::SpeculationState::Active(_)
                        ) && accepted_suggestion
                        {
                            state.prompt_suggestion.accepted_at = current_time_millis();
                        } else {
                            state.prompt_suggestion =
                                crate::state::app_state_store::PromptSuggestionState::default();
                        }
                    });
                }
            }
            (props.on_submit)(PromptSubmission {
                text,
                pasted_contents: submitted_pasted_contents,
                from_keybinding: false,
            });
        }
    }

    if should_cycle_permission_mode.get() {
        should_cycle_permission_mode.set(false);
        (props.on_permission_mode_cycle)(());
    }

    if let Some(text) = history_search.take_pending_submit() {
        if !text.trim().is_empty() {
            let text = input_modes::prepend_mode_character_to_input(&text, input_mode.get());
            let submitted_pasted_contents = pasted_contents.read().clone();
            prompt_history::add_to_history_with_pasted(&text, &submitted_pasted_contents);
            arrow_history.reset();
            restore_stash_or_clear(
                input,
                cursor_offset,
                pasted_contents,
                stashed_prompt,
                input_mode,
            );
            typeahead_selected.set(0);
            submit_count += 1;
            (props.on_submit)(PromptSubmission {
                text,
                pasted_contents: submitted_pasted_contents,
                from_keybinding: false,
            });
        }
    }

    if let Some(text) = text_input.take_pending_submit() {
        if !text.trim().is_empty() {
            let text = input_modes::prepend_mode_character_to_input(&text, input_mode.get());
            let submitted_pasted_contents = pasted_contents.read().clone();
            prompt_history::add_to_history_with_pasted(&text, &submitted_pasted_contents);
            arrow_history.reset();
            restore_stash_or_clear(
                input,
                cursor_offset,
                pasted_contents,
                stashed_prompt,
                input_mode,
            );
            typeahead_selected.set(0);
            submit_count += 1;
            (props.on_submit)(PromptSubmission {
                text,
                pasted_contents: submitted_pasted_contents,
                from_keybinding: false,
            });
        }
    }

    if let Some(direction) = text_input.take_pending_history() {
        // CC PromptInput.tsx:582-596,1256,1278: logical-line gates remain
        // separate from TextInput's wrapped/logical cursor movement.
        let current = input.read().clone();
        let on_boundary_line = match direction {
            HistoryDirection::Up => current.find('\n').is_none_or(|i| cursor_offset.get() <= i),
            HistoryDirection::Down => current.rfind('\n').is_none_or(|i| cursor_offset.get() > i),
        };
        // Maps to: CC handleHistoryUp/Down — when multiple slash suggestions
        // are visible, ↑/↓ belong to autocomplete (not history). Draining the
        // pending event avoids TextInput's history path resetting selection.
        if typeahead_count > 1 || !on_boundary_line {
            let _ = direction;
        } else {
            let current_input = input.read().clone();
            let popped_queue = direction == HistoryDirection::Up
                && crate::utils::message_queue_manager::pop_all_editable(
                    &current_input,
                    cursor_offset.get(),
                )
                .is_some_and(|result| {
                    input.set(result.text);
                    cursor_offset.set(result.cursor_offset);
                    input_mode.set(PromptInputMode::Prompt);
                    if !result.images.is_empty() {
                        let mut restored = pasted_contents.read().clone();
                        for image in result.images {
                            restored.insert(image.id(), image);
                        }
                        pasted_contents.set(restored);
                    }
                    true
                });
            if popped_queue {
                typeahead_selected.set(0);
            } else {
                if input_mode.get() == PromptInputMode::Bash {
                    let with_mode = input_modes::prepend_mode_character_to_input(
                        &input.read(),
                        input_mode.get(),
                    );
                    input.set(with_mode);
                    cursor_offset.set(cursor_offset.get().saturating_add(1));
                }
                let reached_footer =
                    arrow_history.navigate(direction, input, cursor_offset, pasted_contents);
                if reached_footer && direction == HistoryDirection::Down && !footer_items.is_empty()
                {
                    let selected = match direction {
                        crate::hooks::use_text_input::HistoryDirection::Up => {
                            footer_items.last().copied()
                        }
                        crate::hooks::use_text_input::HistoryDirection::Down => {
                            footer_items.first().copied()
                        }
                    };
                    select_footer_item(selected);
                }
                let selected = input.read().clone();
                let history_mode = input_modes::get_mode_from_input(&selected);
                let value = input_modes::get_value_from_input(&selected);
                input_mode.set(match history_mode {
                    input_modes::HistoryMode::Prompt => PromptInputMode::Prompt,
                    input_modes::HistoryMode::Bash => PromptInputMode::Bash,
                });
                cursor_offset.set(cursor_offset.get().min(value.len()));
                input.set(value);
                typeahead_selected.set(0);
            }
        }
    }

    // Maps to: CC's controlled `input`/`onInputChange` pair. Emit after all
    // deferred edits/submits so the parent snapshot reflects this frame's
    // final value. The parent ignores unchanged values.
    (props.on_input_change)(input.read().clone());
    (props.on_input_state_change)(PromptInputTextUpdate {
        revision: last_controlled_revision.get().unwrap_or_default(),
        text: input.read().clone(),
        mode: Some(input_mode.get()),
        pasted_contents: Some(pasted_contents.read().clone()),
        cursor_offset: Some(cursor_offset.get()),
    });

    if text_input.exit.should_exit() {
        (props.on_exit)(());
    }

    // CC's dialog early returns unmount the TextInput child and its Escape
    // timer. Our inline hook remains retained: clear only that child timer
    // when replacing the input tree (L1 lifetime adaptation). Footer focus
    // and history search do not unmount it; PromptInput's rewind timer lives
    // above these branches and must survive. The external-editor placeholder
    // also replaces TextInput (CC PromptInput.tsx:2948-2965).
    let input_child_unmounted = show_quick_open.get()
        || show_global_search.get()
        || show_history_picker.get()
        || show_background_tasks.get()
        || show_teams_dialog.get()
        || show_bridge_dialog.get()
        || show_fast_mode_picker.get()
        || show_model_picker.get()
        || show_thinking_toggle.get()
        || is_external_editor_active.get();
    if input_child_unmounted && text_input.escape.is_pending() {
        text_input.escape.clear();
    }

    if show_quick_open.get() {
        let mut show_quick_open_for_done = show_quick_open;
        let input_for_insert = input;
        let cursor_for_insert = cursor_offset;
        return element! {
            QuickOpenDialog(
                on_done: move |_| show_quick_open_for_done.set(false),
                on_insert: move |text: String| insert_search_dialog_text(input_for_insert, cursor_for_insert, text),
                results: Vec::<String>::new(),
                previews: Vec::new(),
            )
        }
        .into_any();
    }

    if show_global_search.get() {
        let mut show_global_search_for_done = show_global_search;
        let input_for_insert = input;
        let cursor_for_insert = cursor_offset;
        return element! {
            GlobalSearchDialog(
                on_done: move |_| show_global_search_for_done.set(false),
                on_insert: move |text: String| insert_search_dialog_text(input_for_insert, cursor_for_insert, text),
                matches: Vec::new(),
                previews: Vec::new(),
            )
        }
        .into_any();
    }

    if show_history_picker.get() {
        let mut show_history_picker_for_cancel = show_history_picker;
        let mut show_history_picker_for_select = show_history_picker;
        let mut input_for_select = input;
        let mut cursor_for_select = cursor_offset;
        let initial_query = input.read().clone();
        return element! {
            HistorySearchDialog(
                initial_query: Some(initial_query),
                entries: Some(
                    prompt_history::get_history()
                        .into_iter()
                        .map(|entry| crate::components::history_search_dialog::TimestampedHistoryEntry::new(entry.display, entry.timestamp_ms).with_pasted_contents(entry.pasted_contents))
                        .collect(),
                ),
                on_select: move |entry: crate::utils::prompt_history::HistoryEntry| {
                    let history_mode = input_modes::get_mode_from_input(&entry.display);
                    let text = input_modes::get_value_from_input(&entry.display);
                    input_mode.set(match history_mode {
                        input_modes::HistoryMode::Prompt => PromptInputMode::Prompt,
                        input_modes::HistoryMode::Bash => PromptInputMode::Bash,
                    });
                    cursor_for_select.set(text.len());
                    input_for_select.set(text);
                    pasted_contents.set(entry.pasted_contents);
                    show_history_picker_for_select.set(false);
                },
                on_cancel: move |_| show_history_picker_for_cancel.set(false),
            )
        }
        .into_any();
    }

    if show_background_tasks.get() {
        let mut show_for_done = show_background_tasks;
        let app_store_for_tasks = hooks
            .try_use_context::<crate::state::store::AppStore>()
            .map(|store| store.clone());
        return element! {
            BackgroundTasksDialog(
                app_store: app_store_for_tasks,
                on_done: move |_| show_for_done.set(false),
            )
        }
        .into_any();
    }

    if show_teams_dialog.get() {
        let mut show_for_done = show_teams_dialog;
        return element! {
            TeamsDialog(
                data: teams_dialog_data,
                on_done: move |_| show_for_done.set(false),
                on_action: move |action: TeamsDialogAction| {
                    let agent_id = match action {
                        TeamsDialogAction::Kill(agent_id) => Some((agent_id, false)),
                        TeamsDialogAction::Shutdown(agent_id) => Some((agent_id, true)),
                        _ => None,
                    };
                    if let Some((agent_id, shutdown)) = agent_id {
                        if let Some(task) = crate::tasks::in_process_teammate_task::find_teammate_task_by_agent_id(&agent_id) {
                            if shutdown {
                                crate::tasks::in_process_teammate_task::request_teammate_shutdown(&task.task_id);
                            } else {
                                crate::tasks::in_process_teammate_task::kill_in_process_teammate(&task.task_id);
                            }
                        }
                    }
                },
            )
        }
        .into_any();
    }

    if show_bridge_dialog.get() {
        let mut show_for_done = show_bridge_dialog;
        let bridge_store = app_store.clone();
        return element! {
            BridgeDialog(
                snapshot: bridge_dialog_snapshot,
                on_done: move |_| show_for_done.set(false),
                on_disconnect: move |_forget: bool| {
                    if let Some(store) = bridge_store.as_ref() {
                        // Maps to: CC bridge.tsx:163-172 handleDisconnect —
                        // writes exactly enabled/explicit/outbound_only =
                        // false (config/analytics writes stay deferred
                        // seams). The extra connected/session_active/
                        // reconnecting + footer_selection clears are the
                        // Cometix stand-in for the unported useReplBridge
                        // transport teardown, which in CC clears those flags
                        // when the poll loop stops.
                        // B3 flip-audit: CC bridge.tsx:164-171 —
                        // `if (!prev.replBridgeEnabled) return prev` before
                        // the disconnecting spread.
                        store.set_state(|prev| {
                            if !prev.repl_bridge_enabled {
                                return crate::state::store::UpdateDecision::Same(());
                            }
                            let mut next = (**prev).clone();
                            next.repl_bridge_enabled = false;
                            next.repl_bridge_explicit = false;
                            next.repl_bridge_outbound_only = false;
                            next.repl_bridge_connected = false;
                            next.repl_bridge_session_active = false;
                            next.repl_bridge_reconnecting = false;
                            next.footer_selection = None;
                            crate::state::store::UpdateDecision::Replace {
                                next: std::sync::Arc::new(next),
                                result: (),
                            }
                        });
                    }
                    show_for_done.set(false);
                },
            )
        }
        .into_any();
    }

    if is_external_editor_active.get() {
        let theme = hooks.use_context::<Theme>();
        return element! {
            View(
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::CENTER,
                justify_content: JustifyContent::CENTER,
                border_style: BorderStyle::Round,
                border_color: prompt_border_color(input_mode.get(), &theme),
                border_left: false,
                border_right: false,
                width: 100pct,
            ) {
                Text(content: "Save and close editor to continue...".to_string(), dim: true, italic: true)
            }
        }
        .into_any();
    }

    if show_fast_mode_picker.get() {
        let mut show_for_done = show_fast_mode_picker;
        let store_for_done = app_store.clone();
        let mut notifications_for_done = notifications.clone();
        let initial_enabled = app_store
            .as_ref()
            .is_some_and(|store| store.get().fast_mode);
        let unavailable_reason = crate::utils::fast_mode::get_fast_mode_unavailable_reason();
        let unavailable = unavailable_reason.is_some();
        return element! {
            View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                FastModePicker(
                    initial_enabled: initial_enabled,
                    unavailable_reason: unavailable_reason,
                    on_done: move |selection: Option<bool>| {
                        show_for_done.set(false);
                        if let Some(enabled) = selection {
                            // Maps to: CC fast.tsx:35-58 applyFastMode —
                            // clearFastModeCooldown (:39), persist to
                            // userSettings (:40-42, `fastMode: enable ? true
                            // : undefined`; Null carries the undefined
                            // delete), then the setAppState toggle. P5-gap
                            // persist realized 2026-08-02 (P4 batch).
                            crate::utils::fast_mode::clear_fast_mode_cooldown();
                            let _ = crate::utils::settings::update_settings_for_source(
                                crate::utils::settings::SettingSource::User,
                                &serde_json::Map::from_iter([(
                                    "fastMode".to_string(),
                                    if enabled {
                                        serde_json::Value::Bool(true)
                                    } else {
                                        serde_json::Value::Null
                                    },
                                )]),
                            );
                            if let Some(store) = store_for_done.as_ref() {
                                store.replace_with(|state| {
                                    if enabled
                                        && !crate::utils::fast_mode::is_fast_mode_supported_by_model(
                                            state.main_loop_model.as_deref(),
                                        )
                                    {
                                        state.main_loop_model =
                                            Some(crate::utils::fast_mode::get_fast_mode_model());
                                        state.main_loop_model_for_session = None;
                                    }
                                    state.fast_mode = enabled;
                                });
                            }
                            {
                                let context = &mut notifications_for_done;
                                context.add_notification(crate::context::notifications::Notification::text(
                                    "fast-mode-toggled",
                                    format!("Fast mode {}", if enabled { "ON" } else { "OFF" }),
                                    crate::context::notifications::NotificationPriority::Immediate,
                                ).with_color(if enabled {
                                    crate::context::notifications::NotificationColor::FastMode
                                } else {
                                    crate::context::notifications::NotificationColor::Text
                                }));
                            }
                        } else if unavailable && initial_enabled {
                            // Source FastModePicker normalizes an unavailable
                            // previously-on session to OFF when it closes.
                            // Maps to: CC fast.tsx:100-104 handleCancel —
                            // gated by `if (initialFastMode)` (:102), then
                            // applyFastMode(false): clearFastModeCooldown
                            // (:39), persist `fastMode: undefined` (:40-42;
                            // Null carries the undefined delete), and the
                            // setAppState OFF write (:56). P4 batch
                            // (2026-08-02): SEAM realized — this branch now
                            // clears the cooldown and persists like CC.
                            crate::utils::fast_mode::clear_fast_mode_cooldown();
                            let _ = crate::utils::settings::update_settings_for_source(
                                crate::utils::settings::SettingSource::User,
                                &serde_json::Map::from_iter([(
                                    "fastMode".to_string(),
                                    serde_json::Value::Null,
                                )]),
                            );
                            if let Some(store) = store_for_done.as_ref() {
                                // In-updater guard kept: fast_mode cannot
                                // change while the picker is open, so this is
                                // the same `initialFastMode` gate.
                                store.set_state(|prev| {
                                    if !prev.fast_mode {
                                        return crate::state::store::UpdateDecision::Same(());
                                    }
                                    let mut next = (**prev).clone();
                                    next.fast_mode = false;
                                    crate::state::store::UpdateDecision::Replace {
                                        next: std::sync::Arc::new(next),
                                        result: (),
                                    }
                                });
                            }
                        }
                    },
                )
            }
        }
        .into_any();
    }

    if show_model_picker.get() {
        let mut notifications_for_model = notifications.clone();
        return element! {
            View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                ModelPicker(
                    initial: initial_model.clone(),
                    header_text: None,
                    session_model_label: None,
                    is_standalone_command: true,
                    skip_settings_write: false,
                    show_fast_mode_notice: show_fast_icon
                        && crate::utils::fast_mode::is_fast_mode_supported_by_model(
                            initial_model.as_deref(),
                        ),
                    show_fast_mode_available_hint: false,
                    fast_mode_is_on: fast_mode_on,
                    exit_pending: false,
                    exit_key_name: None,
                    on_select: move |selection: ModelPickerSelection| {
                        let effort_suffix = selection
                            .effort
                            .map(|effort| format!(" with {} effort", effort.label()))
                            .unwrap_or_default();
                        let suffix = if selection.fast_mode_disabled {
                            " · Fast mode OFF"
                        } else {
                            ""
                        };
                        notifications_for_model.add_notification(
                            crate::context::notifications::Notification::text(
                                "model-switched",
                                format!(
                                    "Model set to {}{effort_suffix}{suffix}",
                                    selection.display_label()
                                ),
                                crate::context::notifications::NotificationPriority::Immediate,
                            ),
                        );
                        show_model_picker.set(false);
                    },
                    on_cancel: move |_| {
                        show_model_picker.set(false);
                    },
                )
            }
        }
        .into_any();
    }

    if show_thinking_toggle.get() {
        let current = app_store
            .as_ref()
            .and_then(|store| store.get().thinking_enabled)
            .unwrap_or(true);
        return element! {
            View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                ThinkingToggle(
                    current_value: current,
                    focused_index: thinking_toggle_focus.get(),
                    confirmation_pending: *thinking_confirmation.read(),
                    is_mid_conversation: props.has_assistant_messages,
                    exit_pending: false,
                    exit_key_name: None,
                )
            }
        }
        .into_any();
    }

    let theme = hooks.use_context::<Theme>();
    let exit_hint = text_input.exit_hint().map(|s| s.to_string());
    let input_is_empty = input.read().is_empty();
    let use_brief_layout = use_app_state(&mut hooks, |state| {
        // Maps to: CC PromptInput `isBriefOnly && !viewingAgentTaskId`.
        state.is_brief_only && state.viewing_agent_task_id.is_none()
    });
    // Maps to: CC PromptInput `viewingAgentTaskId` → agent name/color lookup.
    let (effective_viewing_agent_name, viewing_agent_color_name) =
        use_app_state(&mut hooks, |state| {
            let id = state.viewing_agent_task_id.as_deref()?;
            state
                .tasks
                .get(id)
                .and_then(|task| task.as_in_process_teammate())
                .map(|task| (task.agent_name.clone(), task.color.clone()))
        })
        .map_or((None, None), |(name, color)| (Some(name), color));
    let input_for_placeholder = input.read().clone();
    let prompt_placeholder = use_prompt_input_placeholder::prompt_input_placeholder(
        use_prompt_input_placeholder::PromptPlaceholderInput {
            input: &input_for_placeholder,
            submit_count: submit_count.get(),
            viewing_agent_name: effective_viewing_agent_name.as_deref(),
            queued_commands: &queued_commands,
            queued_command_up_hint_count: crate::utils::config::load_global_config()
                .queued_command_up_hint_count
                .unwrap_or(0),
            prompt_suggestion_enabled: use_app_state(&mut hooks, |state| {
                state.prompt_suggestion_enabled
            }),
            // PROACTIVE/KAIROS are not present in the external build.
            proactive_active: false,
            example_command: Some(example_command.as_str()),
        },
    );
    let prompt_suggestion = visible_prompt_suggestion(
        &prompt_suggestion_state,
        &input_for_placeholder,
        props.is_loading,
        input_mode.get() == PromptInputMode::Prompt,
        has_suggestions,
        effective_viewing_agent_name.is_some(),
    );
    if prompt_suggestion_state.text.is_some()
        && prompt_suggestion_state.shown_at == 0
        && (props.is_loading || !input_for_placeholder.is_empty())
        && !prompt_suggestion_viewing_agent
    {
        if let Some(store) = app_store.as_ref() {
            // P3 §3a: render-body write — once the suggestion is already
            // reset, this must return `prev` untouched or each notify pass
            // schedules another frame after the B3 flip (unbounded render
            // loop).
            store.set_state(|prev| {
                if prev.prompt_suggestion
                    == crate::state::app_state_store::PromptSuggestionState::default()
                {
                    return crate::state::store::UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.prompt_suggestion =
                    crate::state::app_state_store::PromptSuggestionState::default();
                crate::state::store::UpdateDecision::Replace {
                    next: Arc::new(next),
                    result: (),
                }
            });
        }
    } else if prompt_suggestion.is_some() && prompt_suggestion_state.shown_at == 0 {
        if let Some(store) = app_store.as_ref() {
            // P3 §3a: render-body write — after shown_at is stamped the
            // guard must return `prev` untouched or each notify pass
            // schedules another frame after the B3 flip (unbounded render
            // loop).
            store.set_state(|prev| {
                if !(prev.prompt_suggestion.shown_at == 0 && prev.prompt_suggestion.text.is_some())
                {
                    return crate::state::store::UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.prompt_suggestion.shown_at = current_time_millis();
                crate::state::store::UpdateDecision::Replace {
                    next: Arc::new(next),
                    result: (),
                }
            });
        }
    }
    let placeholder = if input_mode.get() == PromptInputMode::Bash {
        Some("Enter shell command…".to_string())
    } else {
        prompt_suggestion.or(prompt_placeholder)
    };

    let render_suggestions = if !has_suggestions {
        Arc::clone(&empty_suggestions)
    } else {
        Arc::clone(&typeahead_suggestions)
    };
    let render_has_suggestions = !render_suggestions.is_empty();
    let notifications_context = notifications.clone();
    // Subscribed read (see the twin in prompt_input_footer.rs): the writer
    // returned by `use_notifications` has no subscription of its own.
    let has_notification = use_app_state(&mut hooks, |state| state.notifications.current.is_some());
    // Live bridge gates for the height budget — CC PromptInputFooter.tsx:241
    // feature('BRIDGE_MODE') + :255 isBridgeEnabled(); not AppState. The
    // entitlement closure is only evaluated past the cheap AppState gate
    // (auth-IO short-circuit documented at the callee).
    let bridge_feature_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::BridgeMode,
    );
    let status_indicator_count = use_app_state(&mut hooks, move |state| {
        prompt_input_footer::bridge_status_indicator_count_from_app(
            state,
            bridge_feature_enabled,
            crate::bridge::bridge_enabled::is_bridge_enabled,
        )
    });
    // Height computers read the scoped wake epoch: memory/sandbox/apiKeyHelper
    // visibility flips bump it, waking PromptInput to recompute height from
    // the live sources (both height sites read the same carrier — see
    // prompt_input_footer.rs Footer).
    let _footer_layout_epoch = footer_layout_wake.epoch();
    let verbose = use_app_state(&mut hooks, |state| state.verbose);
    // Maps to: CC Notifications.tsx:80-83 tokenUsage useMemo ([messages]
    // identity dep); PromptInput's height budget needs the same value.
    let token_usage = hooks.use_memo(
        {
            let messages = std::sync::Arc::clone(&props.messages);
            move || notifications::token_usage_from_messages(&messages)
        },
        std::sync::Arc::as_ptr(&props.messages) as usize,
    );
    // Captured by value for the same reason as the Footer's twin computation:
    // the subscription hook retains the selector, so it may not borrow props.
    let notification_row_count = {
        let debug = props.debug;
        let api_key_status = props.api_key_status;
        let auto_updater_result = props.auto_updater_result.clone();
        let auto_updating = is_auto_updating.get();
        let ide_selection = props.ide_selection.clone();
        use_app_state(&mut hooks, move |state| {
            notifications::direct_footer_row_count_from_app(
                state,
                has_notification,
                debug,
                api_key_status,
                token_usage,
                auto_updater_result.as_ref(),
                auto_updating,
                ide_selection.as_ref(),
            )
        })
    };
    // Maps to: CC StatusLine.tsx:195 `useAppState(s => s.statusLineText)` +
    // PromptInputFooter.tsx:136 `statusLineShouldDisplay(settings)`, both
    // re-derived from live AppState per render (no startup snapshot).
    let status_line_text = use_app_state(&mut hooks, |state| state.status_line_text.clone());
    let status_line_configured = use_app_state(&mut hooks, |state| {
        crate::components::status_line::status_line_should_display(&state.settings)
    });
    let rendered_lines = text_input.rendered_lines.clone();
    let base_input_state = BaseInputState::from(&text_input);
    // Maps to: PromptInput.tsx imageRefPositions + inside-chip snap useEffect.
    let displayed_value = input.read().clone();
    let image_ref_positions = image_ref_positions(&displayed_value);
    let cursor_for_image_snap = cursor_offset.get();
    let mut cursor_offset_for_image_snap = cursor_offset;
    let displayed_value_for_snap = displayed_value.clone();
    let image_ref_positions_for_snap = image_ref_positions.clone();
    hooks.use_effect(
        move || {
            let snapped = snap_cursor_inside_image_ref(
                &displayed_value_for_snap,
                cursor_for_image_snap,
                &image_ref_positions_for_snap,
            );
            if snapped != cursor_for_image_snap {
                cursor_offset_for_image_snap.set(snapped);
            }
        },
        (displayed_value.clone(), cursor_for_image_snap),
    );
    // Maps to: CC PromptInput.tsx:768-776#slashCommandTriggers. Command
    // validity is independent of mode, fuzzy suggestions and dropdown state.
    let slash_command_triggers = hooks.use_memo(
        {
            let value = displayed_value.clone();
            let commands = Arc::clone(&commands);
            move || {
                crate::utils::suggestions::command_suggestions::find_slash_command_positions(&value)
                    .into_iter()
                    .filter(|pos| {
                        crate::commands::has_command(&value[pos.start + 1..pos.end], &commands)
                    })
                    .collect::<Vec<_>>()
            }
        },
        (displayed_value.clone(), Arc::as_ptr(&commands) as usize),
    );
    let input_highlights = combined_highlights(
        &displayed_value,
        cursor_offset.get(),
        &slash_command_triggers,
    );
    let history_query = history_search.query.read().clone();
    let history_failed_match = history_search.failed_match.get();
    let input_row_height = rendered_lines.len().max(1);
    let terminal_focus = hooks.use_terminal_focus();
    let editor_for_external_hint = std::env::var("EDITOR")
        .ok()
        .or_else(|| std::env::var("VISUAL").ok());
    let external_editor_hint = external_editor_hint_notification_from_state(
        input_row_height > 1,
        false,
        external_editor_api_key_status(props.api_key_status),
        editor_for_external_hint.as_deref(),
    );
    let external_editor_hint_text = external_editor_hint
        .as_ref()
        .map(|notification| notification.text.clone())
        .unwrap_or_default();
    let should_reset_external_editor_hint = input_row_height <= 1;
    let mut notifications_context_for_hint = notifications_context;
    let mut external_editor_hint_signature_for_effect = external_editor_hint_signature;
    hooks.use_effect(
        move || {
            if let Some(notification) = external_editor_hint {
                if external_editor_hint_signature_for_effect.read().as_deref()
                    != Some(notification.text.as_str())
                {
                    {
                        let context = &mut notifications_context_for_hint;
                        let signature = notification.text.clone();
                        context.add_notification(notification);
                        external_editor_hint_signature_for_effect.set(Some(signature));
                    }
                }
            } else if should_reset_external_editor_hint
                && external_editor_hint_signature_for_effect.read().is_some()
            {
                external_editor_hint_signature_for_effect.set(None);
            }
        },
        (should_reset_external_editor_hint, external_editor_hint_text),
    );
    let show_vim_insert_footer =
        vim_enabled && vim_mode.get() == VimMode::Insert && !history_search_active;
    let footer_height = if render_has_suggestions {
        // Must be the same count SuggestionList renders (CC :199-201); a
        // reserved height that disagrees with the rows drawn leaves ghost rows.
        render_suggestions
            .len()
            .min(prompt_input_footer_suggestions::max_visible_items(
                terminal_rows,
                false,
            ))
    } else if help_open.get() {
        PROMPT_INPUT_HELP_MENU_HEIGHT
    } else {
        let suppress_hint = !input_is_empty || status_line_configured || history_search_active;
        // The pill predicate MUST be the same one the Footer component uses
        // to render its body row (footer_has_items, fed by the exact prop
        // expressions at the Footer mount below) — the old local `.max(1)`
        // band-aid diverged (no bash mode, its own vim condition) and, with a
        // status line occupying the budget's first row, left the tasks-pill
        // row unbudgeted: the prompt input row collapsed.
        prompt_input_footer::footer_height_for(
            status_line_configured,
            exit_hint.is_some(),
            history_search_active,
            suppress_hint,
            props.is_loading,
            props.permission_mode,
            notification_row_count,
            status_indicator_count,
            prompt_input_footer::footer_has_items(
                input_mode.get(),
                show_vim_insert_footer,
                background_task_items.len(),
                teams_dialog_data
                    .as_ref()
                    .map(|data| data.teammates.len())
                    .unwrap_or(0),
            ),
        )
    };
    // Match CC PromptInput's natural-height behavior. In iocraft inline
    // main-screen, leaving this root unconstrained lets terminal-size syncs
    // combine with sibling `Messages` percent-width rows and stretch the prompt
    // to hundreds of blank rows. The height is derived from the same visible
    // subcomponents we render below: the bordered input box plus the
    // footer/suggestion rows.
    let queue_preview_commands = queued_commands
        .iter()
        .filter(|command| {
            crate::utils::message_queue_manager::is_queued_command_editable(command)
                && !prompt_input_queued_commands::is_idle_notification(&command.value)
        })
        .cloned()
        .collect::<Vec<_>>();
    let queue_height =
        if effective_viewing_agent_name.is_some() || queue_preview_commands.is_empty() {
            0
        } else {
            1 + prompt_input_queued_commands::process_queued_commands(&queue_preview_commands)
                .iter()
                .map(|command| {
                    command
                        .value
                        .lines()
                        .map(|line| {
                            UnicodeWidthStr::width(line)
                                .div_ceil(text_input_columns.max(1))
                                .max(1)
                        })
                        .sum::<usize>()
                        .max(1)
                })
                .sum::<usize>()
        };
    let input_box_height = input_row_height + 2;
    let stash_height = usize::from(stashed_prompt.read().is_some());
    let prompt_height = queue_height + stash_height + input_box_height + footer_height;
    // Fill viewing fields from AppState (not REPL dual props).
    let mut swarm_banner_input = props.swarm_banner_input.clone();
    if effective_viewing_agent_name.is_some() {
        swarm_banner_input.viewed_teammate_name = effective_viewing_agent_name.clone();
        swarm_banner_input.viewed_teammate_color = viewing_agent_color_name.clone();
    }
    let swarm_banner = use_swarm_banner::swarm_banner(&swarm_banner_input);
    let teammate_prompt_color = swarm_banner_input
        .teammate_color
        .as_deref()
        .and_then(parse_agent_color_name);
    let viewed_prompt_color = viewing_agent_color_name
        .as_deref()
        .and_then(parse_agent_color_name);
    let prompt_input_row = element! {
        View(
            flex_direction: FlexDirection::Row,
            width: 100pct,
            height: input_row_height as u32,
            overflow: Overflow::Hidden,
        ) {
            PromptInputModeIndicator(
                mode: input_mode.get(),
                is_loading: props.is_loading,
                viewing_agent_name: effective_viewing_agent_name.clone(),
                viewing_agent_color: viewed_prompt_color,
                teammate_color: teammate_prompt_color,
            )
            View(
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0f32,
                flex_shrink: 1.0f32,
                min_height: 1u32,
                height: input_row_height as u32,
                overflow: Overflow::Hidden,
            ) {
                #(if vim_enabled {
                    element! {
                        VimTextInput(
                            managed_input_state: Some(base_input_state.clone()),
                            value: Some(input),
                            cursor_offset: Some(cursor_offset),
                            focus: text_input_focused,
                            multiline: true,
                            columns: text_input_columns,
                            show_cursor: true,
                            placeholder: placeholder.clone(),
                            highlights: input_highlights.clone(),
                            argument_hint: typeahead_argument_hint.clone(),
                            initial_mode: Some(vim_mode.get()),
                            current_mode: vim_mode.get(),
                        )
                    }.into_any()
                } else {
                    element! {
                        BaseTextInput(
                            input_state: base_input_state,
                            value: input.to_string(),
                            focus: text_input_focused,
                            show_cursor: true,
                            terminal_focus: terminal_focus,
                            cursor_offset: cursor_offset.get(),
                            placeholder: placeholder,
                            argument_hint: typeahead_argument_hint.clone(),
                            highlights: input_highlights,
                        )
                    }.into_any()
                })
            }
        }
    }
    .into_any();
    let fast_border_text =
        build_fast_border_text(show_fast_icon, fast_icon_hint, fast_mode_cooldown, &theme);
    let prompt_input_box = if let Some(banner) = swarm_banner {
        let banner_color = theme.color(banner.bg_color);
        let banner_width = UnicodeWidthStr::width(banner.text.as_str());
        let leading = "─".repeat(usize::from(columns).saturating_sub(banner_width + 4));
        element! {
            View(flex_direction: FlexDirection::Column, width: 100pct, height: input_box_height as u32) {
                // Maps to: CC PromptInput.tsx:2990-3003. A color-only banner
                // keeps the colored border without mounting an empty name tag.
                #(if banner.text.is_empty() {
                    element! {
                        Text(content: "─".repeat(usize::from(columns)), color: banner_color, wrap: TextWrap::NoWrap)
                    }.into_any()
                } else {
                    element! {
                        View(flex_direction: FlexDirection::Row, height: 1u32, overflow: Overflow::Hidden) {
                            Text(content: leading, color: banner_color, wrap: TextWrap::NoWrap)
                            Text(content: format!(" {} ", banner.text), color: theme.inverse_text, background_color: banner_color, wrap: TextWrap::NoWrap)
                            Text(content: "──".to_string(), color: banner_color, wrap: TextWrap::NoWrap)
                        }
                    }.into_any()
                })
                #(prompt_input_row)
                Text(content: "─".repeat(usize::from(columns)), color: banner_color, wrap: TextWrap::NoWrap)
            }
        }.into_any()
    } else {
        element! {
            View(
                flex_direction: FlexDirection::Column,
                width: 100pct,
                height: input_box_height as u32,
                border_style: BorderStyle::Round,
                border_color: prompt_border_color(input_mode.get(), &theme),
                border_left: false,
                border_right: false,
                border_top: true,
                border_bottom: true,
                border_text: fast_border_text,
                overflow: Overflow::Hidden,
            ) {
                #(prompt_input_row)
            }
        }
        .into_any()
    };

    // Official PromptInput wraps with `marginTop={briefOwnsGap ? 0 : 1}`.
    // Keep margin on an outer shell so a fixed inner height + Overflow::Hidden
    // cannot collapse the blank row under LogoHeader on empty startup.
    let prompt_margin_top = if use_brief_layout { 0u32 } else { 1u32 };
    element! {
        View(
            flex_direction: FlexDirection::Column,
            width: 100pct,
            margin_top: prompt_margin_top,
        ) {
            View(
                flex_direction: FlexDirection::Column,
                width: 100pct,
                height: prompt_height as u32,
                overflow: Overflow::Hidden,
            ) {
            PromptInputQueuedCommands(
                commands: Some(queued_commands.clone()),
                viewing_agent: effective_viewing_agent_name.is_some(),
                use_brief_layout: use_brief_layout,
            )
            PromptInputStashNotice(has_stash: stashed_prompt.read().is_some())
            #(prompt_input_box)

            #(if render_has_suggestions {
                // Maps to: CC `PromptInputFooter.tsx:149` — the suggestions
                // branch wraps the list in `<Box paddingX={2}>`. The padding is
                // the footer's, not the list's; the list still measures against
                // the full terminal width, as CC does.
                element! {
                    View(padding_left: 2u32, padding_right: 2u32) {
                        SuggestionList(
                            items: render_suggestions,
                            selected: typeahead_selected.get(),
                            max_column_width,
                        )
                    }
                }.into_any()
            } else if help_open.get() {
                element! {
                    PromptInputHelpMenu(
                        dim_color: true,
                        fixed_width: true,
                        padding_x: 2u32,
                    )
                }.into_any()
            } else {
                element! {
                    // Narrow FooterLayoutWake provider: only the Footer
                    // subtree (Footer / Notifications / MemoryUsageIndicator /
                    // SandboxPromptFooterHint) can reach the wake — layout
                    // flips wake PromptInput, not the whole tree.
                    ContextProvider(value: Context::owned(footer_layout_wake.clone())) {
                        // Maps to: CC PromptInput.tsx:3046-3056 — PromptInputFooter
                        // receives autoUpdaterResult/isAutoUpdating plus the
                        // onAutoUpdaterResult/onChangeIsUpdating callbacks.
                        prompt_input_footer::Footer(
                            api_key_status: props.api_key_status,
                            auto_updater_result: props.auto_updater_result.clone(),
                            is_auto_updating: is_auto_updating.get(),
                            on_auto_updater_result: props.on_auto_updater_result.clone(),
                            on_change_is_updating: on_change_is_updating.clone(),
                            debug: props.debug,
                            // Maps to: CC PromptInput.tsx:3069 `ideSelection={ideSelection}`.
                            ide_selection: props.ide_selection.clone(),
                            messages: props.messages.clone(),
                            exit_hint: exit_hint,
                            suppress_hint: !input_is_empty,
                            is_searching: history_search_active,
                            history_query: history_query,
                            history_failed_match: history_failed_match,
                            is_loading: props.is_loading,
                            permission_mode: props.permission_mode,
                            status_indicator_count: status_indicator_count,
                            // Maps to: CC BackgroundTaskStatus.tsx:199-208 —
                            // the pill renders getPillLabel(runningTasks);
                            // count and label come from the same
                            // post-exclusion set.
                            background_task_count: pill_tasks.len(),
                            background_tasks_label: crate::tasks::pill_label::get_pill_label(&pill_tasks),
                            teammate_count: teams_dialog_data.as_ref().map(|data| data.teammates.len()).unwrap_or(0),
                            vim_mode: show_vim_insert_footer.then(|| "INSERT".to_string()),
                            mode: input_mode.get(),
                            is_pasting: false,
                        )
                    }
                }.into_any()
            })
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::notifications::{Notification, NotificationPriority, NotificationsState};
    use crate::utils::cursor::Cursor;
    use crate::utils::env_utils::EnvVarGuard;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn key_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, code);
        event.modifiers = modifiers;
        TerminalEvent::Key(event)
    }

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

    #[test]
    fn shell_badge_matches_official_background_and_terminal_gates() {
        use crate::state::app_state_store::TaskState;
        use crate::tasks::local_shell_task::guards::LocalShellTaskState;
        let shell = LocalShellTaskState {
            id: "shell-badge-fixture".into(),
            task_type: "local_bash".into(),
            status: "running".into(),
            description: "inspect files".into(),
            command: "ls -la".into(),
            result: None,
            notified: false,
            shell_command: None,
            last_reported_total_lines: 0,
            is_backgrounded: false,
            agent_id: None,
            tool_use_id: None,
            kind: None,
            start_time_ms: 1234,
            end_time_ms: None,
        };
        for (status, backgrounded, expected) in [
            ("running", false, 0),
            ("pending", false, 0),
            ("running", true, 1),
            ("pending", true, 1),
            ("completed", true, 0),
            ("failed", true, 0),
            ("killed", true, 0),
        ] {
            let mut current = shell.clone();
            current.status = status.into();
            current.is_backgrounded = backgrounded;
            let tasks = std::collections::BTreeMap::from([(
                current.id.clone(),
                std::sync::Arc::new(TaskState::LocalShell(current)),
            )]);
            let items =
                crate::components::tasks::background_tasks_dialog::background_task_items(&tasks);
            let shells = items
                .into_iter()
                .filter(|item| item.category == BackgroundTaskCategory::Shell)
                .collect::<Vec<_>>();
            // CC tasks/types.ts:37-46 + BackgroundTaskStatus.tsx:49-57:
            // foreground progress registration is never a background badge.
            assert_eq!(
                shells.len(),
                expected,
                "{status}, backgrounded={backgrounded}"
            );
            let pills = prompt_pill_tasks(&shells);
            assert_eq!(pills.len(), expected);
            if expected == 1 {
                assert_eq!(crate::tasks::pill_label::get_pill_label(&pills), "1 shell");
                assert_eq!(shells[0].start_time, 1234);
            }
        }
    }

    #[test]
    fn pill_tasks_project_items_and_apply_the_ant_panel_exclusion() {
        use crate::tasks::pill_label::PillTask;
        fn agent_item(id: &str, agent_type: &str) -> BackgroundTasksDialogItem {
            BackgroundTasksDialogItem {
                id: id.to_string(),
                category: BackgroundTaskCategory::LocalAgent,
                label: id.to_string(),
                status: TaskStatus::Running,
                start_time: 0,
                row: None,
                detail: BackgroundTaskDetailData::Agent(AsyncAgentDetailData {
                    agent_type: Some(agent_type.to_string()),
                    ..Default::default()
                }),
                team_name: None,
            }
        }
        let shell = BackgroundTasksDialogItem {
            id: "shell-1".to_string(),
            category: BackgroundTaskCategory::Shell,
            label: "sleep".to_string(),
            status: TaskStatus::Running,
            start_time: 0,
            row: None,
            detail: BackgroundTaskDetailData::Unsupported,
            team_name: None,
        };
        let items = vec![
            shell,
            agent_item("agent-main", "main-session"),
            agent_item("agent-panel", "worker"),
        ];
        let pills = prompt_pill_tasks(&items);
        assert!(matches!(
            pills[0],
            PillTask::LocalBash { is_monitor: false }
        ));
        if cfg!(feature = "anthropic_internal") {
            // CC BackgroundTaskStatus.tsx:49-57 — ant builds exclude panel
            // agents (agentType !== 'main-session') from the pill only.
            assert_eq!(pills.len(), 2);
        } else {
            assert_eq!(pills.len(), 3);
        }

        // Gate difference (CC useBackgroundTaskNavigation.ts:81-83): the
        // dialog-open gate has NO panel-agent exclusion — it reads the raw
        // item set (`has_background_tasks = !background_task_items.is_empty()`),
        // so a panel-agent-only set still opens the dialog in ant builds even
        // though the pill set is empty. In external builds the two sets are
        // identical.
        let panel_only = vec![agent_item("agent-panel", "worker")];
        let panel_only_pills = prompt_pill_tasks(&panel_only);
        if cfg!(feature = "anthropic_internal") {
            assert!(
                panel_only_pills.is_empty(),
                "pill set applies the ant exclusion"
            );
        } else {
            assert_eq!(panel_only_pills.len(), 1);
        }
        assert!(
            !panel_only.is_empty(),
            "the dialog-open gate's set is unexcluded in both builds"
        );
    }

    #[component]
    fn PromptWithNotification(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let notifications = hooks.use_state(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.notifications = std::sync::Arc::new(NotificationsState {
                current: Some(Notification::text(
                    "demo",
                    "Prompt notification visible",
                    NotificationPriority::Medium,
                )),
                queue: Vec::new(),
            });
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(notifications.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        View(width: 80u32) {
                            PromptInput(
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[derive(Default, Props)]
    struct PromptFastIconHarnessProps {
        enabled: bool,
    }

    #[component]
    fn PromptFastIconHarness(
        props: &PromptFastIconHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let enabled = props.enabled;
        let store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.fast_mode = enabled;
            crate::state::store::AppStore::new(initial, None)
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        View(width: 80u32) {
                            PromptInput(on_submit: move |_| {}, on_exit: move |_| {})
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn PromptWithClipboardImage() -> impl Into<AnyElement<'static>> {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        View(width: 80u32) {
                            PromptInput(
                                clipboard_image_override: Some(crate::utils::image_paste::ClipboardImage {
                                    base64: "AAAA".to_string(),
                                    media_type: "image/png".to_string(),
                                    dimensions: None,
                                }),
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn PromptWithStatusIndicators(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = hooks.use_const(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.verbose = true;
            crate::state::store::AppStore::new(initial, None)
        });
        // CC: tokenUsage derives from the messages prop (Notifications.tsx:80-83).
        let messages = hooks.use_const(|| {
            Arc::new(vec![crate::types::message::Message::Assistant(
                crate::types::message::AssistantMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::AssistantContent::Text("hi".into())],
                    model: Some("claude".into()),
                    stop_reason: None,
                    usage: Some(crate::types::message::TokenUsage {
                        input_tokens: 100,
                        output_tokens: 23,
                        ..Default::default()
                    }),
                },
            )])
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(width: 80u32) {
                            PromptInput(
                                debug: true,
                                messages: messages.clone(),
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn PromptWithMissingApiKey(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = hooks.use_const(|| {
            crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            )
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        View(width: 80u32) {
                            PromptInput(
                                api_key_status: VerificationStatus::Missing,
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn PromptWithLiveCommandProps(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        let use_updated = tick.get() > 0;
        if tick.get() < 2 {
            tick += 1;
        } else {
            system.exit();
        }
        let initial = hooks.use_const(|| {
            Arc::new(vec![crate::commands::Command::from_mcp_prompt(
                crate::services::mcp::client::McpPromptCommandSnapshot {
                    name: "mcp__old__prompt".to_string(),
                    description: "Old prompt".to_string(),
                    has_user_specified_description: true,
                    user_facing_name: "old:prompt (MCP)".to_string(),
                    arg_names: Vec::new(),
                    source: "mcp",
                },
            )])
        });
        let updated = hooks.use_const(|| {
            Arc::new(vec![crate::commands::Command::from_mcp_prompt(
                crate::services::mcp::client::McpPromptCommandSnapshot {
                    name: "mcp__new__prompt".to_string(),
                    description: "New prompt".to_string(),
                    has_user_specified_description: true,
                    user_facing_name: "new:prompt (MCP)".to_string(),
                    arg_names: Vec::new(),
                    source: "mcp",
                },
            )])
        });
        let commands = if use_updated { updated } else { initial };

        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(width: 80u32) {
                            PromptInput(
                                commands: Some(commands.clone()),
                                initial_input: Some("/".to_string()),
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn PromptWithExternalEditorHint(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = hooks.use_const(|| {
            crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            )
        });

        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        View(width: 40u32) {
                            PromptInput(
                                api_key_status: VerificationStatus::Valid,
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn build_fast_border_text_matches_official_top_end_shape_and_hint() {
        let theme = *theme::current();
        assert!(build_fast_border_text(false, true, false, &theme).is_none());

        let border = build_fast_border_text(true, true, false, &theme).expect("border text");
        assert_eq!(border.position, BorderTextPosition::Top);
        assert_eq!(border.align, BorderTextAlign::End);
        assert_eq!(border.offset, 0);
        assert!(border.content.starts_with(' '));
        assert!(border.content.ends_with(' '));
        assert!(
            border
                .content
                .contains(crate::constants::figures::LIGHTNING_BOLT)
        );
        assert!(border.content.contains("\x1b[2m/fast\x1b[0m"));

        let cooldown = build_fast_border_text(true, false, true, &theme).expect("cooldown");
        assert!(cooldown.content.contains("\x1b[2m"));
        assert!(!cooldown.content.contains("/fast"));
    }

    #[test]
    fn model_selection_disables_fast_mode_for_unsupported_model_like_official() {
        let mut state = crate::state::app_state_store::AppState::default();
        state.fast_mode = true;
        state.main_loop_model = Some("opus".to_string());
        assert!(apply_prompt_model_selection(
            &mut state,
            Some("sonnet".to_string()),
            "sonnet",
            ModelEffortLevel::High,
            false,
        ));
        assert!(!state.fast_mode);
        assert_eq!(state.main_loop_model.as_deref(), Some("sonnet"));
        assert!(state.main_loop_model_for_session.is_none());
    }

    #[test]
    fn fast_icon_condition_matches_official_feature_user_availability_gate() {
        assert!(!should_show_fast_icon(false, true, true, false));
        assert!(!should_show_fast_icon(true, false, true, false));
        assert!(!should_show_fast_icon(true, true, false, false));
        assert!(should_show_fast_icon(true, true, true, false));
        assert!(should_show_fast_icon(true, true, false, true));
    }

    #[test]
    fn fast_icon_renders_once_in_top_right_border_only_when_fast_mode_is_on() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_FAST_MODE");

        let off = element! { PromptFastIconHarness(enabled: false) }.render(Some(80));
        assert!(
            !off.to_string()
                .contains(crate::constants::figures::LIGHTNING_BOLT)
        );

        let on = element! { PromptFastIconHarness(enabled: true) }.render(Some(80));
        let lines = canvas_lines(&on);
        let icon_rows = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains(crate::constants::figures::LIGHTNING_BOLT))
            .collect::<Vec<_>>();
        assert_eq!(icon_rows.len(), 1, "canvas=\n{}", lines.join("\n"));
        let (row, line) = icon_rows[0];
        assert!(
            row <= 2,
            "icon must be in the top prompt border: canvas=\n{}",
            lines.join("\n")
        );
        assert!(line.contains('─'), "icon row must be a border row: {line}");
        let icon_column = line
            .find(crate::constants::figures::LIGHTNING_BOLT)
            .expect("fast icon");
        assert!(icon_column >= 70, "icon must be right-aligned: {line}");
    }

    #[test]
    fn prompt_input_shift_tab_invokes_permission_mode_cycle_callback() {
        let current_theme = *theme::current();
        let cycle_count = Arc::new(AtomicUsize::new(0));
        let cycle_count_for_handler = cycle_count.clone();
        let mut key_event = KeyEvent::new(KeyEventKind::Press, KeyCode::Tab);
        key_event.modifiers = KeyModifiers::SHIFT;
        let events = vec![TerminalEvent::Key(key_event)];

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(move || element! {
                                View(width: 80u32) {
                                    PromptInput(
                                        on_submit: move |_| {},
                                        on_exit: move |_| {},
                                        on_permission_mode_cycle: {
                                            let cycle_count_for_handler = cycle_count_for_handler.clone();
                                            move |_| {
                                                cycle_count_for_handler.fetch_add(1, Ordering::SeqCst);
                                            }
                                        },
                                    )
                                }
                            }.into_any()),
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(events)).with_size(80, 20),
            ));
            for _ in 0..4 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(cycle_count.load(Ordering::SeqCst), 1);
    }

    fn prompt_after_events(is_local_command_ui_active: bool, events: Vec<TerminalEvent>) -> String {
        prompt_with_initial_after_events(None, None, None, None, is_local_command_ui_active, events)
    }

    fn prompt_with_initial_after_events(
        initial_input: Option<String>,
        controlled_input: Option<PromptInputTextUpdate>,
        insert_text: Option<PromptInputTextUpdate>,
        vim_enabled_override: Option<bool>,
        is_local_command_ui_active: bool,
        events: Vec<TerminalEvent>,
    ) -> String {
        let current_theme = *theme::current();
        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        // PromptInput reads AppState and raises notifications, both
                        // of which are strict at the source. A harness that skips
                        // the Provider is not a lighter test — it is a different
                        // component tree than the one that ships.
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(move || element! {
                                View(width: 80u32) {
                                    PromptInput(
                                        initial_input: initial_input.clone(),
                                        controlled_input: controlled_input.clone(),
                                        insert_text: insert_text.clone(),
                                        vim_enabled_override: vim_enabled_override,
                                        on_submit: move |_| {},
                                        on_exit: move |_| {},
                                        is_local_command_ui_active: is_local_command_ui_active,
                                    )
                                }
                            }.into_any()),
                        )
                    }
                }
            };
            let max_canvases = events.len() * 4 + 10;
            let delayed_events = stream::unfold(events.into_iter(), |mut events| async move {
                let event = std::iter::Iterator::next(&mut events)?;
                futures_timer::Delay::new(Duration::from_millis(30)).await;
                Some((event, events))
            });
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(delayed_events).with_size(80, 20),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= max_canvases {
                    break;
                }
            }
            canvases
        });
        canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n")
    }

    fn prompt_with_clipboard_image_after_events(events: Vec<TerminalEvent>) -> String {
        let canvases = futures::executor::block_on(async move {
            let mut app = crate::keybindings::keybinding_provider_setup::test_keybinding_root(
                element!(PromptWithClipboardImage).into_any(),
            );
            let delayed_events = stream::unfold(events.into_iter(), |mut events| async move {
                let event = std::iter::Iterator::next(&mut events)?;
                futures_timer::Delay::new(Duration::from_millis(40)).await;
                Some((event, events))
            });
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(delayed_events).with_size(80, 20),
            ));
            let mut canvases = Vec::new();
            while let Some(canvas) = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(150)).await;
                None
            })
            .await
            {
                canvases.push(canvas);
                if canvases.len() >= 10 {
                    break;
                }
            }
            canvases
        });
        canvas_lines(canvases.last().expect("clipboard image render")).join("\n")
    }

    fn prompt_with_notification_after_events(events: Vec<TerminalEvent>) -> String {
        let current_theme = *theme::current();
        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    PromptWithNotification
                }
            };
            let delayed_events = stream::unfold(events.into_iter(), |mut events| async move {
                let event = std::iter::Iterator::next(&mut events)?;
                futures_timer::Delay::new(Duration::from_millis(30)).await;
                Some((event, events))
            });
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(delayed_events).with_size(80, 20),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 10 {
                    break;
                }
            }
            canvases
        });
        canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n")
    }

    fn prompt_with_external_editor_after_events(events: Vec<TerminalEvent>) -> String {
        let canvases = futures::executor::block_on(async move {
            let mut app = element!(PromptWithExternalEditorHint);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(events)).with_size(40, 20),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 12 {
                    break;
                }
            }
            canvases
        });
        canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n")
    }

    fn drive_prompt_with_store(store: crate::state::store::AppStore, events: Vec<TerminalEvent>) {
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        crate::state::app_state::AppStateProvider(
                            prebuilt_store: Some(store),
                            children: crate::state::app_state::ProviderChildren::new(|| element! {
                                PromptInput(
                                    on_submit: move |_| {},
                                    on_exit: move |_| {},
                                )
                            }.into_any()),
                        )
                    }
                }
            };
            let delayed_events = stream::unfold(events.into_iter(), |mut events| async move {
                let event = std::iter::Iterator::next(&mut events)?;
                futures_timer::Delay::new(Duration::from_millis(30)).await;
                Some((event, events))
            });
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(delayed_events).with_size(90, 24),
            ));
            while crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await
            .is_some()
            {}
        });
    }

    #[test]
    fn prompt_suggestion_visibility_matches_official_empty_idle_gate() {
        let state = crate::state::app_state_store::PromptSuggestionState {
            text: Some("run the tests".to_string()),
            ..Default::default()
        };
        assert_eq!(
            visible_prompt_suggestion(&state, "", false, true, false, false).as_deref(),
            Some("run the tests")
        );
        assert!(visible_prompt_suggestion(&state, "x", false, true, false, false).is_none());
        assert!(visible_prompt_suggestion(&state, "", true, true, false, false).is_none());
        assert!(visible_prompt_suggestion(&state, "", false, true, true, false).is_none());
    }

    #[test]
    fn empty_enter_accepts_shown_prompt_suggestion_and_resets_state() {
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.prompt_suggestion_enabled = true;
        initial.prompt_suggestion = crate::state::app_state_store::PromptSuggestionState {
            text: Some("run the tests".to_string()),
            prompt_id: Some("user_intent".to_string()),
            shown_at: 1,
            accepted_at: 0,
            generation_request_id: None,
        };
        let store = crate::state::store::AppStore::new(initial, None);
        let submitted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let submitted_for_handler = submitted.clone();
        futures::executor::block_on(async {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        crate::state::app_state::AppStateProvider(
                            prebuilt_store: Some(store.clone()),
                            children: crate::state::app_state::ProviderChildren::new(move || {
                                let submitted_for_handler = submitted_for_handler.clone();
                                element! {
                                    PromptInput(
                                        on_submit: move |submission: PromptSubmission| {
                                            submitted_for_handler.lock().unwrap().push(submission.text);
                                        },
                                        on_exit: move |_| {},
                                    )
                                }
                                .into_any()
                            }),
                        )
                    }
                }
            };
            let events = stream::unfold(Some(key(KeyCode::Enter)), |event| async move {
                let event = event?;
                futures_timer::Delay::new(Duration::from_millis(30)).await;
                Some((event, None))
            });
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(90, 24),
            ));
            for _ in 0..10 {
                if crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await
                .is_none()
                {
                    break;
                }
            }
        });

        assert_eq!(submitted.lock().unwrap().as_slice(), ["run the tests"]);
        assert!(store.get().prompt_suggestion.text.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normal_typing_aborts_active_speculation() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        let cache = Arc::new(crate::utils::forked_agent::CacheSafeParams {
            system_prompt: Vec::new(),
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context,
            fork_context_messages: Arc::new(Vec::new()),
        });
        let abort = crate::tool::AbortController::default();
        store.replace_with(|state| {
            state.speculation = crate::state::app_state_store::SpeculationState::Active(
                crate::state::app_state_store::ActiveSpeculationState {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    abort_controller: abort.clone(),
                    start_time: current_time_millis(),
                    messages: Arc::new(std::sync::Mutex::new(Vec::new())),
                    written_paths: Arc::new(std::sync::Mutex::new(Default::default())),
                    boundary: None,
                    suggestion_length: 4,
                    tool_use_count: 0,
                    is_pipelined: false,
                    cache_safe_params: cache,
                    pipelined_suggestion: None,
                },
            );
        });
        let mut app = element! {
            ContextProvider(value: Context::owned(
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
            )) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(store.clone()),
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            PromptInput(on_submit: move |_| {}, on_exit: move |_| {})
                        }.into_any()),
                    )
                }
            }
        };
        let events = stream::unfold(
            vec![(key(KeyCode::Char('x')), 30), (key(KeyCode::Right), 80)].into_iter(),
            |mut events| async move {
                let (event, delay) = events.next()?;
                tokio::time::sleep(Duration::from_millis(delay)).await;
                Some((event, events))
            },
        );
        let mut render_loop =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(90, 24),
            ));
        for _ in 0..12 {
            if crate::utils::race(render_loop.next(), async {
                tokio::time::sleep(Duration::from_millis(120)).await;
                None
            })
            .await
            .is_none()
            {
                break;
            }
        }

        assert!(abort.is_aborted());
        assert!(matches!(
            store.get().speculation,
            crate::state::app_state_store::SpeculationState::Idle
        ));
    }

    #[test]
    fn prompt_input_model_and_thinking_actions_update_runtime_owner_state() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let model_store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        drive_prompt_with_store(
            model_store.clone(),
            vec![
                key_with_modifiers(KeyCode::Char('p'), KeyModifiers::ALT),
                key(KeyCode::Down),
                key(KeyCode::Enter),
            ],
        );
        assert!(model_store.get().main_loop_model.is_some());

        let max_effort_store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        drive_prompt_with_store(
            max_effort_store.clone(),
            vec![
                key_with_modifiers(KeyCode::Char('p'), KeyModifiers::ALT),
                key(KeyCode::Down),
                key(KeyCode::Right),
                key(KeyCode::Enter),
            ],
        );
        assert_eq!(
            max_effort_store.get().effort_value,
            Some(crate::utils::effort::EffortValue::Named("max".to_string())),
            "ModelPicker High → Max on Sonnet 4.6 must apply Max to AppState"
        );
        assert_eq!(
            max_effort_store.get().main_loop_model.as_deref(),
            Some("sonnet")
        );

        let thinking_store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        drive_prompt_with_store(
            thinking_store.clone(),
            vec![
                key_with_modifiers(KeyCode::Char('t'), KeyModifiers::ALT),
                key(KeyCode::Down),
                key(KeyCode::Enter),
            ],
        );
        assert_eq!(thinking_store.get().thinking_enabled, Some(false));

        let fast_store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        drive_prompt_with_store(
            fast_store.clone(),
            vec![
                key_with_modifiers(KeyCode::Char('o'), KeyModifiers::ALT),
                key(KeyCode::Tab),
                key(KeyCode::Enter),
            ],
        );
        assert!(fast_store.get().fast_mode);
    }

    #[test]
    fn prompt_input_chat_actions_open_model_and_thinking_boundaries() {
        let model = prompt_after_events(
            false,
            vec![key_with_modifiers(KeyCode::Char('p'), KeyModifiers::ALT)],
        );
        assert!(model.contains("Select model"), "canvas=\n{model}");

        let thinking = prompt_after_events(
            false,
            vec![key_with_modifiers(KeyCode::Char('t'), KeyModifiers::ALT)],
        );
        assert!(
            thinking.contains("Toggle thinking mode"),
            "canvas=\n{thinking}"
        );

        let fast = prompt_after_events(
            false,
            vec![key_with_modifiers(KeyCode::Char('o'), KeyModifiers::ALT)],
        );
        assert!(
            fast.contains("Fast mode (research preview)"),
            "canvas=\n{fast}"
        );
    }

    #[test]
    fn prompt_input_image_paste_action_inserts_live_image_chip() {
        let paste_key = if cfg!(windows) {
            key_with_modifiers(KeyCode::Char('v'), KeyModifiers::ALT)
        } else {
            key_with_modifiers(KeyCode::Char('v'), KeyModifiers::CONTROL)
        };
        let text = prompt_with_clipboard_image_after_events(vec![
            paste_key,
            key(KeyCode::F(24)),
            key(KeyCode::Char('x')),
            key(KeyCode::F(24)),
        ]);
        assert!(text.contains("[Image #1] x"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_applies_controlled_and_cursor_insertion_revisions_once() {
        let text = prompt_with_initial_after_events(
            Some("draft".to_string()),
            Some(PromptInputTextUpdate {
                revision: 1,
                text: "owner".to_string(),
                ..Default::default()
            }),
            Some(PromptInputTextUpdate {
                revision: 2,
                text: "voice".to_string(),
                ..Default::default()
            }),
            None,
            false,
            vec![key(KeyCode::F(24)), key(KeyCode::F(24))],
        );
        assert!(text.contains("❯ owner voice"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_history_suppression_matches_official_navigation_and_reactivation() {
        // CC PromptInput.tsx:1245-1286,1508 and useTypeahead.tsx:684-688:
        // history suppresses the suggestion state, not just the dropdown.
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skip = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        prompt_history::add_to_history("/help");
        prompt_history::add_to_history("/resume");
        assert_eq!(prompt_history::get_history()[0].display, "/resume");

        let older = prompt_after_events(false, vec![key(KeyCode::Up), key(KeyCode::Up)]);
        assert!(older.contains("❯ /help"), "canvas=\n{older}");
        let newer = prompt_after_events(
            false,
            vec![
                key(KeyCode::Up),
                key(KeyCode::Up),
                key(KeyCode::Down),
                key(KeyCode::Down),
            ],
        );
        assert!(newer.contains("❯ /resume"), "canvas=\n{newer}");

        let draft = prompt_with_initial_after_events(
            Some("saved draft".into()),
            None,
            None,
            Some(false),
            false,
            vec![
                key(KeyCode::Up),
                key(KeyCode::Up),
                key(KeyCode::Down),
                key(KeyCode::Down),
            ],
        );
        assert!(draft.contains("❯ saved draft"), "canvas=\n{draft}");

        let suggestions = prompt_after_events(
            false,
            vec![
                key(KeyCode::Up),
                key(KeyCode::Down),
                key(KeyCode::Down),
                key(KeyCode::Char('/')),
            ],
        );
        let fresh = prompt_after_events(false, vec![key(KeyCode::Char('/'))]);
        assert_eq!(
            suggestions, fresh,
            "returning to the draft restores the fresh slash dropdown"
        );
    }

    #[test]
    fn prompt_input_arrows_matches_official_cursor_before_history() {
        // CC useTextInput.ts:269-315 and Cursor.ts:353-407,511-551.
        // Use the real PromptInput + default keybinding runtime: the old
        // top-level history registration intercepted Up before this edit.
        let up = prompt_with_initial_after_events(
            Some("abcd\nefgh".into()),
            None,
            None,
            Some(false),
            false,
            vec![
                key(KeyCode::Left),
                key(KeyCode::Left),
                key(KeyCode::Up),
                key(KeyCode::Char('X')),
            ],
        );
        assert!(up.contains("abXcd") && up.contains("efgh"), "canvas=\n{up}");
        let down = prompt_with_initial_after_events(
            Some("abcd\nefgh".into()),
            None,
            None,
            Some(false),
            false,
            vec![
                key(KeyCode::Left),
                key(KeyCode::Left),
                key(KeyCode::Up),
                key(KeyCode::Down),
                key(KeyCode::Char('X')),
            ],
        );
        assert!(
            down.contains("abcd") && down.contains("efXgh"),
            "canvas=\n{down}"
        );
        let boundary = prompt_with_initial_after_events(
            Some("abcd".into()),
            None,
            None,
            Some(false),
            false,
            vec![
                key(KeyCode::Left),
                key(KeyCode::Up),
                key(KeyCode::Char('X')),
            ],
        );
        assert!(boundary.contains("Xabcd"), "canvas=\n{boundary}");
    }

    #[test]
    fn prompt_input_vim_normal_mode_navigation_and_delete_are_live() {
        let text = prompt_with_initial_after_events(
            Some("abc".to_string()),
            None,
            None,
            Some(true),
            false,
            vec![
                key(KeyCode::Esc),
                key(KeyCode::Char('h')),
                key(KeyCode::Char('x')),
            ],
        );
        assert!(text.contains("❯ ab"), "canvas=\n{text}");
        assert!(!text.contains("-- NORMAL --"), "canvas=\n{text}");
        assert!(!text.contains("abc"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_vim_delete_operator_removes_logical_line() {
        let text = prompt_with_initial_after_events(
            Some("one\ntwo".to_string()),
            None,
            None,
            Some(true),
            false,
            vec![
                key(KeyCode::Esc),
                key(KeyCode::Char('d')),
                key(KeyCode::Char('d')),
            ],
        );
        assert!(text.contains("❯ one"), "canvas=\n{text}");
        assert!(!text.contains("two"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_undo_action_restores_previous_edit_snapshot() {
        let text = prompt_with_initial_after_events(
            Some("draft".to_string()),
            None,
            None,
            None,
            false,
            vec![
                key(KeyCode::Char('x')),
                key_with_modifiers(KeyCode::Char('_'), KeyModifiers::CONTROL),
                key(KeyCode::F(24)),
            ],
        );
        assert!(text.contains("❯ draft"), "canvas=\n{text}");
        assert!(!text.contains("❯ draftx"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_stash_action_stashes_and_restores_prompt() {
        let stashed = prompt_with_initial_after_events(
            Some("draft".to_string()),
            None,
            None,
            None,
            false,
            vec![
                key_with_modifiers(KeyCode::Char('s'), KeyModifiers::CONTROL),
                key(KeyCode::F(24)),
            ],
        );
        assert!(
            stashed.contains("Stashed (auto-restores after submit)"),
            "canvas=\n{stashed}"
        );
        assert!(!stashed.contains("❯ draft"), "canvas=\n{stashed}");

        let restored = prompt_with_initial_after_events(
            Some("draft".to_string()),
            None,
            None,
            None,
            false,
            vec![
                key_with_modifiers(KeyCode::Char('s'), KeyModifiers::CONTROL),
                key(KeyCode::F(24)),
                key_with_modifiers(KeyCode::Char('s'), KeyModifiers::CONTROL),
                key(KeyCode::F(24)),
            ],
        );
        assert!(restored.contains("❯ draft"), "canvas=\n{restored}");
        assert!(
            !restored.contains("Stashed (auto-restores after submit)"),
            "canvas=\n{restored}"
        );
    }

    #[test]
    fn prompt_input_search_keybindings_open_live_local_dialogs() {
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;

        let quick = prompt_after_events(
            false,
            vec![key_with_modifiers(KeyCode::Char('p'), ctrl_shift)],
        );
        assert!(quick.contains("Quick Open"), "canvas=\n{quick}");
        assert!(quick.contains("Start typing to search"), "canvas=\n{quick}");

        let global = prompt_after_events(
            false,
            vec![key_with_modifiers(KeyCode::Char('f'), ctrl_shift)],
        );
        assert!(global.contains("Global Search"), "canvas=\n{global}");
        assert!(global.contains("Type to search"), "canvas=\n{global}");

        let history = prompt_after_events(
            false,
            vec![key_with_modifiers(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
            )],
        );
        assert!(history.contains("Search prompts"), "canvas=\n{history}");
        if prompt_history::get_history().is_empty() {
            assert!(history.contains("No history yet"), "canvas=\n{history}");
        } else {
            assert!(!history.contains("No history yet"), "canvas=\n{history}");
        }
    }

    #[test]
    fn prompt_input_search_keybindings_are_suppressed_for_local_command_ui() {
        let text = prompt_after_events(
            true,
            vec![key_with_modifiers(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )],
        );
        assert!(!text.contains("Quick Open"), "canvas=\n{text}");
        assert!(text.contains(PROMPT_CHAR), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_wires_footer_status_indicators_from_context() {
        let text = element!(PromptWithStatusIndicators)
            .render(Some(80))
            .to_string();

        assert!(text.contains("Debug mode"), "canvas=\n{text}");
        assert!(text.contains("123 tokens"), "canvas=\n{text}");
    }

    #[test]
    fn slash_highlights_matches_official_validity_in_prompt_and_bash() {
        // CC PromptInput.tsx:768-776 and commands.ts:688-702: exact internal
        // name, display name or alias; no hidden, mode or dropdown gate.
        let current_theme = *theme::current();
        let mut help = crate::commands::help::command();
        help.aliases = vec!["h".into()];
        help.user_facing_name = Some("help-visible".into());
        let mut hidden = crate::commands::help::command();
        hidden.name = "secret".into();
        hidden.is_hidden = true;
        let commands = Arc::new(vec![help, hidden]);
        for mode in [PromptInputMode::Prompt, PromptInputMode::Bash] {
            for (value, expected_tokens) in [
                ("/sdasdasdasdasd tail", vec![]),
                ("/printf %s RWF_CONTINUED_D_0909", vec![]),
                ("/hel tail", vec![]),
                ("/HELP tail", vec![]),
                ("src/help (/help) /123 /_help / tail", vec![]),
                ("/help tail", vec!["/help"]),
                ("/help-visible tail", vec!["/help-visible"]),
                ("/h tail", vec!["/h"]),
                ("/secret tail", vec!["/secret"]),
                ("ask /help, /h tail", vec!["/help", "/h"]),
                ("/help/file tail", vec!["/help"]),
                ("/help:unknown tail", vec![]),
            ] {
                let commands = Arc::clone(&commands);
                let canvas = element! {
                    ContextProvider(value: Context::owned(current_theme)) {
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(move || element! {
                                View(width: 120u32) {
                                    PromptInput(
                                        commands: Some(Arc::clone(&commands)),
                                        controlled_input: Some(PromptInputTextUpdate {
                                            revision: 1, text: value.into(), mode: Some(mode),
                                            cursor_offset: Some(value.len()), ..Default::default()
                                        }),
                                        vim_enabled_override: Some(false),
                                        on_submit: move |_| {}, on_exit: move |_| {},
                                    )
                                }
                            }.into_any()),
                        )
                    }
                }.render(Some(120));
                let lines = canvas_lines(&canvas);
                let (row, column) = lines
                    .iter()
                    .enumerate()
                    .find_map(|(row, line)| {
                        line.find(value).map(|column| {
                            (row, unicode_width::UnicodeWidthStr::width(&line[..column]))
                        })
                    })
                    .unwrap_or_else(|| panic!("input missing: {value:?}, canvas={lines:?}"));
                // These cases are ASCII, so input bytes and canvas columns agree.
                for offset in 0..value.len() {
                    let expected = expected_tokens.iter().any(|token| {
                        let start = value.rfind(*token).unwrap();
                        (start..start + token.len()).contains(&offset)
                    });
                    let color = canvas
                        .resolved_text_style(column + offset, row)
                        .and_then(|style| style.color);
                    assert_eq!(
                        color == Some(current_theme.color(ThemeColorKey::Suggestion)),
                        expected,
                        "mode={mode:?} input={value:?} offset={offset} color={color:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn slash_highlights_matches_official_utf16_and_priority() {
        // CC PromptInput.tsx:904-912 and JS string offsets; slash priority 5
        // must remain below the overlapping /btw warning highlight (15).
        let value = "😀 /help";
        let highlights = combined_highlights(value, value.len(), &[5..10]);
        let command = highlights
            .iter()
            .find(|h| h.color == Some(ThemeColorKey::Suggestion))
            .unwrap();
        assert_eq!((command.start, command.end, command.priority), (3, 8, 5));
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                shimmered_input::HighlightedInput(
                    text: "/btw".to_string(), highlights: combined_highlights("/btw", 4, &[0..4]),
                )
            }
        }
        .render(Some(40));
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().color,
            Some(current_theme.color(ThemeColorKey::Warning))
        );
    }

    #[test]
    fn prompt_highlights_cover_commands_image_chips_and_thinking_keywords() {
        let value = "/help [Image #1] ultrathink";
        let positions = image_ref_positions(value);
        assert_eq!(positions.len(), 1);
        let (chip_start, chip_end) = positions[0];
        // Cursor parked after `]` — chip must stay plain (official default).
        let at_end = combined_highlights(value, chip_end, &[0..5]);
        assert!(at_end.iter().any(|highlight| {
            highlight.start == 0
                && highlight.end == 5
                && highlight.color == Some(ThemeColorKey::Suggestion)
        }));
        assert!(
            !at_end.iter().any(|highlight| highlight.inverse),
            "image chip is not inverted unless cursor is at chip.start"
        );
        assert_eq!(
            at_end
                .iter()
                .filter(|highlight| highlight.shimmer_color.is_some())
                .count(),
            "ultrathink".len()
        );

        // Cursor at chip.start — selected/invert state for backspace.
        let selected = combined_highlights(value, chip_start, &[0..5]);
        let chip = selected
            .iter()
            .find(|highlight| highlight.inverse)
            .expect("selected image chip highlight");
        assert_eq!(chip.color, None);
        assert_eq!(chip.priority, 8);
        assert_eq!(chip.start, value[..chip_start].encode_utf16().count());
        assert_eq!(chip.end, value[..chip_end].encode_utf16().count());

        assert_eq!(
            snap_cursor_inside_image_ref(value, chip_start + 2, &positions),
            chip_start
        );
        assert_eq!(
            snap_cursor_inside_image_ref(value, chip_end - 2, &positions),
            chip_end
        );
    }

    #[test]
    fn prompt_input_text_columns_match_borderless_prompt_prefix_width() {
        assert_eq!(prompt_text_input_columns(80), 77);
        assert_eq!(prompt_text_input_columns(2), 1);

        let full_width_text = "a".repeat(76);
        let wrapped_text = "a".repeat(77);
        assert_eq!(
            Cursor::from_text(full_width_text, prompt_text_input_columns(80), 76)
                .render_lines(None)
                .len(),
            1,
            "CC passes columns - 3 and Cursor reserves one more cursor cell"
        );
        assert_eq!(
            Cursor::from_text(wrapped_text, prompt_text_input_columns(80), 77)
                .render_lines(None)
                .len(),
            2,
            "the 77th text cell should wrap after CC's outer and cursor reservations"
        );
    }

    #[test]
    fn prompt_input_uses_full_width_border_box_like_official() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        View(width: 125u32) {
                            PromptInput(
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
        .render(Some(125));

        assert_eq!(canvas.width(), 125);
        let border_rows = (0..canvas.height())
            .filter(|row| {
                canvas.cell(0, *row).and_then(|cell| cell.text()) == Some("─")
                    && canvas.cell(124, *row).and_then(|cell| cell.text()) == Some("─")
            })
            .collect::<Vec<_>>();

        assert!(
            border_rows.len() >= 2,
            "prompt input should render full-width top and bottom borders"
        );
        assert_eq!(
            canvas
                .cell(0, border_rows[0] + 1)
                .and_then(|cell| cell.text()),
            Some(PROMPT_CHAR),
            "the prompt indicator should start at the border content edge"
        );
    }

    #[test]
    fn prompt_input_renders_live_swarm_banner_projection() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        View(width: 80u32) {
                            PromptInput(
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                                swarm_banner_input: use_swarm_banner::SwarmBannerInput {
                                    standalone_name: Some("reviewer".to_string()),
                                    standalone_color: Some("purple".to_string()),
                                    ..Default::default()
                                },
                            )
                        }
                    }.into_any()),
                )
            }
        }
        .render(Some(80));
        let text = canvas.to_string();
        assert!(text.contains(" reviewer "), "canvas=\n{text}");
        assert!(text.lines().filter(|line| line.contains('─')).count() >= 2);
    }

    #[test]
    fn prompt_input_color_only_banner_matches_official_empty_text_border() {
        // CC useSwarmBanner.ts:127-133 preserves color-only metadata;
        // PromptInput.tsx:2990-3003 alone decides whether to render the tag.
        // A whitespace name is truthy in JS and must retain its padded tag.
        for name in [None, Some(""), Some("reviewer"), Some(" ")] {
            let current_theme = *theme::current();
            let canvas = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            View(width: 80u32) {
                                PromptInput(
                                    swarm_banner_input: use_swarm_banner::SwarmBannerInput {
                                        standalone_name: name.map(str::to_string),
                                        standalone_color: Some("pink".to_string()),
                                        ..Default::default()
                                    },
                                )
                            }
                        }.into_any()),
                    )
                }
            }
            .render(Some(80));
            let text = canvas.to_string();
            let borders = text
                .lines()
                .enumerate()
                .filter(|(_, line)| line.starts_with('─'))
                .collect::<Vec<_>>();
            assert_eq!(borders.len(), 2, "name={name:?}, canvas=\n{text}");
            let (row, top) = borders[0];
            assert_eq!(borders[1].1, "─".repeat(80));
            if name.is_none_or(str::is_empty) {
                assert_eq!(top, "─".repeat(80), "name={name:?}");
                for column in 0..80 {
                    let style = canvas.resolved_text_style(column, row).unwrap();
                    assert_eq!(
                        style.color,
                        Some(current_theme.color(ThemeColorKey::AgentPink))
                    );
                    assert_eq!(canvas.cell(column, row).unwrap().background_color, None);
                }
            } else {
                assert!(top.contains(&format!(" {} ", name.unwrap())));
                assert!(top.ends_with("──"));
            }
        }
    }

    #[test]
    fn prompt_input_refreshes_suggestions_when_live_merged_commands_change() {
        let canvases = futures::executor::block_on(
            element!(PromptWithLiveCommandProps)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(80, 20))
                .collect::<Vec<_>>(),
        );
        let rendered = canvases
            .last()
            .expect("mock render should produce a final canvas")
            .to_string();

        assert!(
            rendered.contains("/new:prompt (MCP)"),
            "canvas=\n{rendered}"
        );
        assert!(
            !rendered.contains("/old:prompt (MCP)"),
            "canvas=\n{rendered}"
        );
    }

    #[test]
    fn prompt_input_renders_notifications_in_native_document_flow() {
        let text = element!(PromptWithNotification)
            .render(Some(80))
            .to_string();

        assert!(
            text.contains("Prompt notification visible"),
            "canvas=\n{text}"
        );
        assert!(text.contains(PROMPT_CHAR), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_passes_repl_api_key_status_through_footer_matches_official() {
        let text = element!(PromptWithMissingApiKey)
            .render(Some(80))
            .to_string();

        assert!(
            text.contains("Not logged in · Run /login"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn prompt_input_suppresses_notifications_while_suggestions_own_footer() {
        let text = prompt_with_notification_after_events(vec![key(KeyCode::Char('/'))]);

        assert!(
            !text.contains("Prompt notification visible"),
            "suggestions should hide the visible notification row; canvas=\n{text}"
        );
        assert!(
            text.contains('/'),
            "the prompt input should still render the typed suggestion trigger; canvas=\n{text}"
        );
    }

    #[test]
    fn prompt_input_escape_pops_all_editable_queued_commands() {
        let _lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::message_queue_manager::enqueue(
            crate::utils::message_queue_manager::QueuedCommand::new("queued edit", "prompt"),
        );
        let text = prompt_after_events(false, vec![key(KeyCode::Esc), key(KeyCode::F(24))]);
        assert!(text.contains("❯ queued edit"), "canvas=\n{text}");
        assert!(crate::utils::message_queue_manager::get_command_queue().is_empty());
    }

    // Drive the actual PromptInput, AppState and keybinding owners. Each Vec
    // is one event batch; successive batches allow a render in between.
    fn rewind_escape_after_batches(
        initial_input: &str,
        has_messages: bool,
        is_loading: bool,
        batches: Vec<Vec<TerminalEvent>>,
    ) -> (usize, String) {
        let (opened, frames) = rewind_escape_frames_after_batches(
            initial_input,
            has_messages,
            is_loading,
            batches,
            None,
        );
        (opened, frames.last().cloned().unwrap_or_default())
    }

    fn rewind_escape_frames_after_batches(
        initial_input: &str,
        has_messages: bool,
        is_loading: bool,
        batches: Vec<Vec<TerminalEvent>>,
        queued_text: Option<&str>,
    ) -> (usize, Vec<String>) {
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        if let Some(text) = queued_text {
            crate::utils::message_queue_manager::enqueue(
                crate::utils::message_queue_manager::QueuedCommand::new(text, "prompt"),
            );
        }
        let opened = Arc::new(AtomicUsize::new(0));
        let opened_for_handler = Arc::clone(&opened);
        let input = initial_input.to_string();
        let messages = Arc::new(if has_messages {
            vec![crate::types::message::Message::User(
                crate::utils::messages::create_user_message("previous message".into()),
            )]
        } else {
            Vec::new()
        });
        let frames = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(move || {
                                let opened = Arc::clone(&opened_for_handler);
                                element! {
                                    PromptInput(
                                        initial_input: Some(input.clone()),
                                        messages: Arc::clone(&messages),
                                        is_loading: is_loading,
                                        vim_enabled_override: Some(false),
                                        on_show_message_selector: move |_| {
                                            opened.fetch_add(1, Ordering::SeqCst);
                                        },
                                        on_submit: move |_| {},
                                        on_exit: move |_| {},
                                    )
                                }.into_any()
                            }),
                        )
                    }
                }
            };
            let events = stream::unfold(batches.into_iter(), |mut batches| async move {
                let batch = batches.next()?;
                futures_timer::Delay::new(Duration::from_millis(30)).await;
                Some((batch, batches))
            })
            .flat_map(stream::iter);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(90, 24),
            ));
            let mut frames = Vec::new();
            while let Some(canvas) = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(120)).await;
                None
            })
            .await
            {
                frames.push(canvas.to_string());
            }
            frames
        });
        (opened.load(Ordering::SeqCst), frames)
    }

    #[test]
    fn prompt_rewind_escape_matches_official_empty_idle_messages_gate() {
        // CC PromptInput.tsx:2510: all three conditions belong to the
        // empty-input rewind owner, independently of TextInput's clear owner.
        for (has_messages, loading, expected) in
            [(false, false, 0), (true, true, 0), (true, false, 1)]
        {
            let (opened, _) = rewind_escape_after_batches(
                "",
                has_messages,
                loading,
                vec![vec![key(KeyCode::Esc)], vec![key(KeyCode::Esc)]],
            );
            assert_eq!(
                opened, expected,
                "messages={has_messages}, loading={loading}"
            );
        }
    }

    #[test]
    fn prompt_rewind_escape_matches_official_each_pair_in_one_batch() {
        // CC useDoublePress invokes onDoublePress synchronously for each
        // pair; REPL's callback toggles twice, so multiplicity must survive
        // the Rust HandlerMut render bridge.
        let (opened, _) = rewind_escape_after_batches(
            "",
            true,
            false,
            vec![vec![
                key(KeyCode::Esc),
                key(KeyCode::Esc),
                key(KeyCode::Esc),
                key(KeyCode::Esc),
            ]],
        );
        assert_eq!(opened, 2);
    }

    #[test]
    fn prompt_rewind_escape_matches_official_help_gate_for_entire_batch() {
        let (opened, frames) = rewind_escape_frames_after_batches(
            "",
            true,
            false,
            vec![
                vec![key(KeyCode::Char('?'))],
                vec![key(KeyCode::Esc), key(KeyCode::Esc), key(KeyCode::Esc)],
            ],
            None,
        );
        assert!(
            frames.iter().any(|frame| frame.contains("! for bash mode")),
            "help never opened: {frames:?}"
        );
        assert!(
            !frames.last().unwrap().contains("! for bash mode"),
            "help never closed: {frames:?}"
        );
        assert_eq!(
            opened, 0,
            "closing help must not reinterpret the rest of its input batch"
        );
    }

    #[test]
    fn prompt_rewind_escape_matches_official_queue_gate_for_entire_batch() {
        let (opened, _) = rewind_escape_frames_after_batches(
            "",
            true,
            false,
            vec![vec![
                key(KeyCode::Esc),
                key(KeyCode::Esc),
                key(KeyCode::Esc),
            ]],
            Some("queued edit"),
        );
        assert_eq!(
            opened, 0,
            "popping queue must not reinterpret the rest of its input batch"
        );
    }

    #[test]
    fn prompt_rewind_escape_matches_official_separate_nonempty_clear_owner() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        // The initial non-empty render is essential: CC Ink batches this
        // chunk, so PromptInput's captured input stays non-empty throughout.
        let (opened, text) = rewind_escape_after_batches(
            "x",
            true,
            false,
            vec![vec![
                key(KeyCode::Esc),
                key(KeyCode::Esc),
                key(KeyCode::Backspace),
            ]],
        );
        assert_eq!(opened, 0, "a clear must not turn into rewind: {text}");
        assert!(
            !text.contains("❯ x"),
            "the text clear must still execute: {text}"
        );
    }

    #[test]
    fn prompt_rewind_escape_matches_official_timer_survives_other_keys() {
        // useDoublePress.ts has no non-Escape reset. Editing back to empty
        // during the window does not erase the first eligible empty Escape.
        let (opened, _) = rewind_escape_after_batches(
            "",
            true,
            false,
            vec![
                vec![key(KeyCode::Esc)],
                vec![key(KeyCode::Char('x'))],
                vec![key(KeyCode::Backspace)],
                vec![key(KeyCode::Esc)],
            ],
        );
        assert_eq!(opened, 1);
    }

    #[test]
    fn prompt_rewind_escape_matches_official_bash_mode_exit_falls_through() {
        // CC PromptInput.tsx:2459-2465 changes mode but does not return.
        let (opened, _) = rewind_escape_after_batches(
            "",
            true,
            false,
            vec![
                vec![key(KeyCode::Char('!'))],
                vec![key(KeyCode::Esc)],
                vec![key(KeyCode::Esc)],
            ],
        );
        assert_eq!(opened, 1);
    }

    #[test]
    fn prompt_rewind_escape_matches_official_model_picker_child_unmount() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        // Each batch waits 30ms so the picker really mounts and unmounts;
        // all four/five keypresses remain within the 800ms double-press window.
        for additional_escape in [false, true] {
            let mut batches = vec![
                vec![key(KeyCode::Esc)],
                vec![key_with_modifiers(KeyCode::Char('p'), KeyModifiers::ALT)],
                vec![key(KeyCode::Esc)],
                vec![key(KeyCode::Esc)],
            ];
            if additional_escape {
                batches.push(vec![key(KeyCode::Esc)]);
            }
            let (opened, frames) =
                rewind_escape_frames_after_batches("x", true, false, batches, None);
            assert!(
                frames.iter().any(|frame| frame.contains("Select model")),
                "picker never mounted: {frames:?}"
            );
            let final_frame = frames.last().unwrap();
            assert!(
                !final_frame.contains("Select model"),
                "picker never closed: {frames:?}"
            );
            assert_eq!(
                final_frame.contains("❯ x"),
                !additional_escape,
                "{frames:?}"
            );
            assert_eq!(opened, 0);
        }
    }

    #[test]
    fn prompt_input_queues_external_editor_hint_when_wrapped() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _editor_guard = EnvVarGuard::set("EDITOR", "code --wait");
        let _visual_guard = EnvVarGuard::unset("VISUAL");
        let long_input = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let events = long_input
            .chars()
            .map(|ch| key(KeyCode::Char(ch)))
            .collect();
        let text = prompt_with_external_editor_after_events(events);

        assert!(
            text.contains("ctrl+g to edit in VS Code"),
            "wrapped prompt should enqueue the official external-editor hint; canvas=\n{text}"
        );
    }

    #[test]
    fn external_editor_status_mapping_keeps_auth_errors_suppressed() {
        assert_eq!(
            external_editor_api_key_status(VerificationStatus::Valid),
            ApiKeyVerificationStatus::Valid
        );
        assert_eq!(
            external_editor_api_key_status(VerificationStatus::Invalid),
            ApiKeyVerificationStatus::Invalid
        );
        assert_eq!(
            external_editor_api_key_status(VerificationStatus::Missing),
            ApiKeyVerificationStatus::Missing
        );
        assert_eq!(
            external_editor_api_key_status(VerificationStatus::Loading),
            ApiKeyVerificationStatus::Unknown
        );
        assert_eq!(
            external_editor_api_key_status(VerificationStatus::Error),
            ApiKeyVerificationStatus::Unknown
        );
    }

    #[test]
    fn prompt_help_question_mark_renders_in_prompt_footer_not_panel() {
        let text = prompt_after_events(false, vec![key(KeyCode::Char('?'))]);

        assert!(text.contains("! for bash mode"), "canvas=\n{text}");
        assert!(
            text.contains("ctrl + o for verbose output"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("? for shortcuts"), "canvas=\n{text}");
        assert!(!text.contains("Esc to close"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_help_esc_restores_normal_footer_hint() {
        let text = prompt_after_events(false, vec![key(KeyCode::Char('?')), key(KeyCode::Esc)]);

        assert!(!text.contains("! for bash mode"), "canvas=\n{text}");
        assert!(text.contains("? for shortcuts"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_input_stays_visible_but_unfocused_for_active_local_command_ui() {
        let events = vec![key(KeyCode::Char('a')), key(KeyCode::Char('b'))];
        let focused = prompt_after_events(false, events.clone());
        let unfocused = prompt_after_events(true, events);

        assert!(
            focused.contains("ab"),
            "normal PromptInput should edit text; canvas=\n{focused}"
        );
        assert!(
            unfocused.contains(PROMPT_CHAR),
            "PromptInput should remain mounted for immediate local command UI; canvas=\n{unfocused}"
        );
        assert!(
            !unfocused.contains("ab"),
            "active local command UI should prevent PromptInput text editing; canvas=\n{unfocused}"
        );
    }
}
