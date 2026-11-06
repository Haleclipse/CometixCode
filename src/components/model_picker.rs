//! Maps to: CC `components/ModelPicker.tsx`:54-363.
//!
//! Official ModelPicker owns options, focus, effort cycling, Select chords,
//! and optional fast-mode notices. Callers only supply `initial` plus
//! `onSelect` / `onCancel`. Persist + `AppState` writes stay in
//! [`apply_model_picker_selection`] unless `skipSettingsWrite`.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::pane::Pane;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybindings};
use crate::state::store::AppStore;
use crate::utils::effort::Effort;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub(crate) const MODEL_NO_PREFERENCE: &str = "__NO_PREFERENCE__";
pub(crate) const MODEL_PICKER_VISIBLE_COUNT: usize = 10;
pub(crate) const MODEL_PICKER_HEADER_TEXT: &str = "Switch between Claude models. Applies to this session and future nyan sessions. For other/previous model names, specify with --model.";
pub(crate) use crate::utils::fast_mode::FAST_MODE_MODEL_DISPLAY;
// Maps to: CC `components/ModelPicker.tsx:20` importing `type EffortLevel`
// from `utils/effort.js`.
pub(crate) use crate::utils::effort::ModelEffortLevel;

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn model_option(
    label: &str,
    value: &str,
    description: &str,
    max_description_chars: usize,
) -> SelectOptionData {
    SelectOptionData {
        label: label.to_string(),
        value: value.to_string(),
        description: Some(truncate_chars(description, max_description_chars)),
        dim_description: true,
        disabled: false,
        input: None,
    }
}

/// Profile-specific picker entries are added in the profile selector stage.
fn gateway_model_options(
    _max_description_chars: usize,
    _builtin: &[SelectOptionData],
) -> Vec<SelectOptionData> {
    Vec::new()
}

pub(crate) fn model_picker_options(max_description_chars: usize) -> Vec<SelectOptionData> {
    let mut options = vec![
        // @nyan offset: the official picker lists Sonnet and Opus twice, once
        // plain and once with 1M context. A gateway serves one model per family,
        // so only the 1M entry is kept — the plain one is the same model with a
        // smaller window.
        model_option(
            "Sonnet (1M context)",
            "sonnet[1m]",
            "Sonnet 4.6 for long sessions",
            max_description_chars,
        ),
        model_option(
            "Opus (1M context)",
            "opus[1m]",
            "Opus 4.6 for long sessions",
            max_description_chars,
        ),
        model_option(
            "Haiku",
            "haiku",
            "Haiku 4.5 · Fastest for quick answers",
            max_description_chars,
        ),
    ];
    let extra = gateway_model_options(max_description_chars, &options);
    options.extend(extra);
    options
}

fn next_index(index: usize, count: usize) -> usize {
    if count == 0 { 0 } else { (index + 1) % count }
}

fn previous_index(index: usize, count: usize) -> usize {
    if count == 0 {
        0
    } else if index == 0 {
        count - 1
    } else {
        index - 1
    }
}

fn next_page_index(index: usize, count: usize, visible_count: usize) -> usize {
    if count == 0 {
        0
    } else {
        index.saturating_add(visible_count.max(1)).min(count - 1)
    }
}

fn previous_page_index(index: usize, visible_count: usize) -> usize {
    index.saturating_sub(visible_count.max(1))
}

pub(crate) fn focused_index_for_initial(
    options: &[SelectOptionData],
    initial: Option<&str>,
) -> usize {
    let Some(initial) = initial.map(str::trim).filter(|value| !value.is_empty()) else {
        return 0;
    };
    if initial.eq_ignore_ascii_case(MODEL_NO_PREFERENCE)
        || initial.eq_ignore_ascii_case("Default (recommended)")
        || initial.eq_ignore_ascii_case("default")
    {
        return 0;
    }
    options
        .iter()
        .position(|option| {
            option.value.eq_ignore_ascii_case(initial) || option.label.eq_ignore_ascii_case(initial)
        })
        .unwrap_or(0)
}

pub(crate) fn visible_from_index(
    focused_index: usize,
    count: usize,
    visible_count: usize,
) -> usize {
    if count == 0 {
        return 0;
    }
    let visible_count = visible_count.max(1).min(count);
    focused_index
        .saturating_add(1)
        .saturating_sub(visible_count)
        .min(count.saturating_sub(visible_count))
}

/// Maps to: CC `components/ModelPicker.tsx` initializing effort from
/// `AppState.effortValue` / `sessionEffort`.
pub(crate) fn model_effort_from_value(value: Option<&Effort>) -> ModelEffortLevel {
    value.copied().unwrap_or(Effort::Medium)
}

/// Maps to: CC `components/ModelPicker.tsx`:132-133
/// `getDefaultEffortLevelForOption(...)`.
pub(crate) fn default_effort_level_for_option(_value: &str) -> ModelEffortLevel {
    Effort::Medium
}

pub(crate) fn displayed_effort(effort: ModelEffortLevel, _focused_value: &str) -> ModelEffortLevel {
    effort
}

/// Maps to: CC `components/ModelPicker.tsx` `cycleEffortLevel` / `lmg`.
///
/// Cycles through the four unified effort levels: `off < medium < high < max`.
pub(crate) fn cycle_effort(
    current: ModelEffortLevel,
    direction: KeyCode,
    _focused_value: &str,
) -> ModelEffortLevel {
    let levels = Effort::ALL;
    let current_index = levels
        .iter()
        .position(|level| *level == current)
        .unwrap_or(1);
    match direction {
        KeyCode::Right => levels[(current_index + 1) % levels.len()],
        KeyCode::Left => levels[(current_index + levels.len() - 1) % levels.len()],
        _ => current,
    }
}

/// Maps to: CC `components/ModelPicker.tsx` `handleSelect` selected-effort
/// gate (`hasToggledEffort && modelSupportsEffort`).
pub(crate) fn selected_picker_effort(
    effort: ModelEffortLevel,
    _selected_value: &str,
    has_toggled: bool,
) -> Option<ModelEffortLevel> {
    if !has_toggled {
        return None;
    }
    Some(effort)
}

/// Enter only changes the current session.
pub(crate) fn apply_model_picker_selection(
    state: &mut crate::state::app_state_store::AppState,
    model: Option<String>,
    _selected_value: &str,
    picked_effort: ModelEffortLevel,
    has_toggled_effort: bool,
) -> bool {
    let fast_mode_disabled = crate::utils::fast_mode::is_fast_mode_enabled()
        && state.fast_mode
        && !crate::utils::fast_mode::is_fast_mode_supported_by_model(model.as_deref());
    state.main_loop_model_for_session = model.clone();
    let _ = crate::utils::session_storage::save_session_model(model.as_deref());
    if fast_mode_disabled {
        state.fast_mode = false;
    }
    if has_toggled_effort {
        state.effort_value = Some(picked_effort);
        let _ = crate::utils::session_storage::save_session_effort(picked_effort);
    }
    fast_mode_disabled
}

/// Maps to: CC `ModelPicker`'s effort `useKeybindings` registration.
pub(crate) fn use_model_picker_effort_keybindings<Decrease, Increase>(
    hooks: &mut Hooks,
    is_active: bool,
    mut on_decrease: Decrease,
    mut on_increase: Increase,
) where
    Decrease: FnMut() + Send + 'static,
    Increase: FnMut() + Send + 'static,
{
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let handlers: KeybindingHandlers = vec![
        (
            "modelPicker:decreaseEffort".to_string(),
            Box::new(move || {
                on_decrease();
                true
            }),
        ),
        (
            "modelPicker:increaseEffort".to_string(),
            Box::new(move || {
                on_increase();
                true
            }),
        ),
    ];
    use_keybindings(
        hooks,
        runtime,
        handlers,
        ContextName::ModelPicker,
        move || is_active,
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelPickerSelection {
    pub value: String,
    pub label: String,
    pub effort: Option<ModelEffortLevel>,
    pub fast_mode_disabled: bool,
}

impl ModelPickerSelection {
    pub(crate) fn display_label(&self) -> &str {
        if self.value == MODEL_NO_PREFERENCE {
            "Default (recommended)"
        } else {
            &self.label
        }
    }
}

#[derive(Default, Props)]
pub(crate) struct ModelPickerProps<'a> {
    pub initial: Option<String>,
    pub header_text: Option<String>,
    pub session_model_label: Option<String>,
    pub is_standalone_command: bool,
    pub skip_settings_write: bool,
    pub show_fast_mode_notice: bool,
    pub show_fast_mode_available_hint: bool,
    pub fast_mode_is_on: bool,
    pub exit_pending: bool,
    pub exit_key_name: Option<String>,
    pub on_select: HandlerMut<'a, ModelPickerSelection>,
    pub on_cancel: HandlerMut<'a, ()>,
}

struct ModelPickerView {
    options: Vec<SelectOptionData>,
    focused_index: usize,
    selected_value: Option<String>,
    visible_from_index: usize,
    effort: ModelEffortLevel,
    header_text: Option<String>,
    session_model_label: Option<String>,
    is_standalone_command: bool,
    show_fast_mode_notice: bool,
    show_fast_mode_available_hint: bool,
    fast_mode_is_on: bool,
    exit_pending: bool,
    exit_key_name: Option<String>,
}

fn render_model_picker_content(props: &ModelPickerView, theme: Theme) -> AnyElement<'static> {
    let count = props.options.len();
    let focused_index = props.focused_index.min(count.saturating_sub(1));
    let focused_value = props
        .options
        .get(focused_index)
        .map(|option| option.value.as_str())
        .unwrap_or(MODEL_NO_PREFERENCE);
    let visible_option_count = count.min(MODEL_PICKER_VISIBLE_COUNT).max(1);
    let visible_from = props
        .visible_from_index
        .min(count.saturating_sub(visible_option_count));
    let hidden_count = count.saturating_sub(visible_option_count);
    let display_effort = displayed_effort(props.effort, focused_value);
    let focused_default_effort = default_effort_level_for_option(focused_value);
    let header_text = props
        .header_text
        .clone()
        .unwrap_or_else(|| MODEL_PICKER_HEADER_TEXT.to_string());
    let exit_footer = if props.exit_pending {
        format!(
            "Press {} again to exit",
            props
                .exit_key_name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or("Ctrl-C")
        )
    } else {
        "Enter to confirm · Esc to exit".to_string()
    };

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Column) {
                View(margin_bottom: 1u32, flex_direction: FlexDirection::Column) {
                    Text(content: "Select model".to_string(), color: theme.remember, weight: Weight::Bold)
                    Text(content: header_text, dim: true)
                    #(props.session_model_label.as_ref().map(|label| element! {
                        Text(content: format!("Currently using {label} for this session (set by plan mode). Selecting a model will undo this."), dim: true)
                    }))
                }
                View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                    Select(
                        is_disabled: false,
                        hide_indexes: false,
                        visible_option_count: visible_option_count,
                        options: props.options.clone(),
                        focused_index: focused_index,
                        selected_value: props.selected_value.clone(),
                        visible_from_index: visible_from,
                        layout: SelectLayout::Compact,
                    )
                    #(if hidden_count > 0 {
                        Some(element! {
                            View(padding_left: 3u32) {
                                Text(content: format!("and {hidden_count} more…"), dim: true)
                            }
                        })
                    } else {
                        None
                    })
                }
                View(margin_bottom: 1u32, flex_direction: FlexDirection::Column) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: display_effort.symbol().to_string(), color: theme.claude, wrap: TextWrap::NoWrap)
                        Text(content: format!(" {} effort", display_effort.title_label()), dim: true, wrap: TextWrap::NoWrap)
                        #(if display_effort == focused_default_effort {
                            Some(element! { Text(content: " (default) ".to_string(), dim: true, wrap: TextWrap::NoWrap) })
                        } else {
                            Some(element! { Text(content: " ".to_string(), dim: true, wrap: TextWrap::NoWrap) })
                        })
                        Text(content: "← → to adjust".to_string(), color: theme.subtle, wrap: TextWrap::NoWrap)
                    }
                }
                #(if props.show_fast_mode_notice {
                    Some(element! {
                        View(margin_bottom: 1u32) {
                            Text(content: format!("Fast mode is ON and available with {FAST_MODE_MODEL_DISPLAY} only (/fast). Switching to other models turn off fast mode."), dim: true)
                        }
                    })
                } else if props.show_fast_mode_available_hint && !props.fast_mode_is_on {
                    Some(element! {
                        View(margin_bottom: 1u32) {
                            Text(content: format!("Use /fast to turn on Fast mode ({FAST_MODE_MODEL_DISPLAY} only)."), dim: true)
                        }
                    })
                } else {
                    None
                })
            }
            #(if props.is_standalone_command {
                Some(element! { Text(content: exit_footer, dim: true, italic: true) })
            } else {
                None
            })
        }
    }
    .into_any()
}

/// Maps to: CC `components/ModelPicker.tsx`:54-321 `ModelPicker(...)`.
#[component]
pub(crate) fn ModelPicker<'a>(
    props: &mut ModelPickerProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks
        .try_use_context::<Theme>()
        .map(|theme| *theme)
        .unwrap_or_else(|| *crate::utils::theme::current());
    let (columns, _) = hooks.use_terminal_size();
    let max_description_chars = usize::from(columns).saturating_sub(10).max(1);
    let options = model_picker_options(max_description_chars);
    let option_count = options.len();
    let visible_count = MODEL_PICKER_VISIBLE_COUNT.min(option_count.max(1));
    let initial_focus = focused_index_for_initial(&options, props.initial.as_deref());
    let app_store = hooks
        .try_use_context::<AppStore>()
        .map(|store| store.clone());
    let initial_effort_value = app_store
        .as_ref()
        .and_then(|store| store.get().effort_value.clone());
    let has_app_effort = initial_effort_value.is_some();
    let initial_effort = model_effort_from_value(initial_effort_value.as_ref());
    let mut focused_index = hooks.use_state(move || initial_focus);
    let mut effort = hooks.use_state(|| initial_effort);
    let has_toggled_effort = hooks.use_state(|| false);
    let mut should_close = hooks.use_state(|| false);
    let mut pending_selection = hooks.use_state(|| Option::<ModelPickerSelection>::None);
    let skip_settings_write = props.skip_settings_write;
    let event_options = options.clone();

    use_model_picker_effort_keybindings(
        &mut hooks,
        true,
        {
            let options = options.clone();
            let mut effort = effort;
            let mut has_toggled_effort = has_toggled_effort;
            move || {
                let focused = focused_index.get().min(options.len().saturating_sub(1));
                let value = options
                    .get(focused)
                    .map(|option| option.value.as_str())
                    .unwrap_or(MODEL_NO_PREFERENCE);
                effort.set(cycle_effort(effort.get(), KeyCode::Left, value));
                has_toggled_effort.set(true);
            }
        },
        {
            let options = options.clone();
            let mut effort = effort;
            let mut has_toggled_effort = has_toggled_effort;
            move || {
                let focused = focused_index.get().min(options.len().saturating_sub(1));
                let value = options
                    .get(focused)
                    .map(|option| option.value.as_str())
                    .unwrap_or(MODEL_NO_PREFERENCE);
                effort.set(cycle_effort(effort.get(), KeyCode::Right, value));
                has_toggled_effort.set(true);
            }
        },
    );

    hooks.use_propagated_terminal_events(move |event| {
        let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
            return;
        };
        if *kind == KeyEventKind::Release {
            return;
        }
        let focused = focused_index.get().min(option_count.saturating_sub(1));
        match code {
            KeyCode::Esc => should_close.set(true),
            KeyCode::Down => {
                let next = next_index(focused, option_count);
                focused_index.set(next);
                if !has_toggled_effort.get()
                    && !has_app_effort
                    && let Some(option) = event_options.get(next)
                {
                    effort.set(default_effort_level_for_option(&option.value));
                }
            }
            KeyCode::Up => {
                let next = previous_index(focused, option_count);
                focused_index.set(next);
                if !has_toggled_effort.get()
                    && !has_app_effort
                    && let Some(option) = event_options.get(next)
                {
                    effort.set(default_effort_level_for_option(&option.value));
                }
            }
            KeyCode::PageDown => {
                let next = next_page_index(focused, option_count, visible_count);
                focused_index.set(next);
                if !has_toggled_effort.get()
                    && !has_app_effort
                    && let Some(option) = event_options.get(next)
                {
                    effort.set(default_effort_level_for_option(&option.value));
                }
            }
            KeyCode::PageUp => {
                let next = previous_page_index(focused, visible_count);
                focused_index.set(next);
                if !has_toggled_effort.get()
                    && !has_app_effort
                    && let Some(option) = event_options.get(next)
                {
                    effort.set(default_effort_level_for_option(&option.value));
                }
            }
            KeyCode::Enter => {
                if let Some(option) = event_options.get(focused) {
                    let selected_effort = selected_picker_effort(
                        effort.get(),
                        &option.value,
                        has_toggled_effort.get(),
                    );
                    let mut fast_mode_disabled = false;
                    if !skip_settings_write && let Some(store) = app_store.as_ref() {
                        let model =
                            (option.value != MODEL_NO_PREFERENCE).then(|| option.value.clone());
                        fast_mode_disabled = store.replace_with(|state| {
                            apply_model_picker_selection(
                                state,
                                model,
                                &option.value,
                                effort.get(),
                                has_toggled_effort.get(),
                            )
                        });
                    }
                    pending_selection.set(Some(ModelPickerSelection {
                        value: option.value.clone(),
                        label: option.label.clone(),
                        effort: selected_effort,
                        fast_mode_disabled,
                    }));
                }
            }
            _ => {}
        }
        event.stop_propagation();
    });

    if should_close.get() {
        should_close.set(false);
        (props.on_cancel)(());
    }
    let selected = { pending_selection.read().clone() };
    if let Some(selection) = selected {
        pending_selection.set(None);
        (props.on_select)(selection);
    }

    let focused = focused_index.get().min(option_count.saturating_sub(1));
    let content = render_model_picker_content(
        &ModelPickerView {
            options,
            focused_index: focused,
            selected_value: props
                .initial
                .clone()
                .or_else(|| Some(MODEL_NO_PREFERENCE.to_string())),
            visible_from_index: visible_from_index(focused, option_count, visible_count),
            effort: effort.get(),
            header_text: props.header_text.clone(),
            session_model_label: props.session_model_label.clone(),
            is_standalone_command: props.is_standalone_command,
            show_fast_mode_notice: props.show_fast_mode_notice,
            show_fast_mode_available_hint: props.show_fast_mode_available_hint,
            fast_mode_is_on: props.fast_mode_is_on,
            exit_pending: props.exit_pending,
            exit_key_name: props.exit_key_name.clone(),
        },
        theme,
    );

    if props.is_standalone_command {
        element! {
            Pane(color: theme.permission) {
                #(content)
            }
        }
        .into_any()
    } else {
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_picker(props: ModelPickerProps) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ModelPicker(
                    initial: props.initial,
                    header_text: props.header_text,
                    session_model_label: props.session_model_label,
                    is_standalone_command: props.is_standalone_command,
                    skip_settings_write: props.skip_settings_write,
                    show_fast_mode_notice: props.show_fast_mode_notice,
                    show_fast_mode_available_hint: props.show_fast_mode_available_hint,
                    fast_mode_is_on: props.fast_mode_is_on,
                    exit_pending: props.exit_pending,
                    exit_key_name: props.exit_key_name,
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn model_picker_options_and_effort_cycle_match_official_shape() {
        let options = model_picker_options(120);
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            ["sonnet[1m]", "opus[1m]", "haiku"]
        );
        assert_eq!(options[0].label, "Sonnet (1M context)");
        assert_eq!(
            cycle_effort(ModelEffortLevel::High, KeyCode::Right, "opus"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::Max, KeyCode::Right, "opus"),
            ModelEffortLevel::Off
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::High, KeyCode::Right, "sonnet"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::Max, KeyCode::Right, "sonnet"),
            ModelEffortLevel::Off
        );
        assert_eq!(
            displayed_effort(ModelEffortLevel::Max, "sonnet"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            selected_picker_effort(ModelEffortLevel::Max, "sonnet", true),
            Some(ModelEffortLevel::Max)
        );
        assert_eq!(
            selected_picker_effort(ModelEffortLevel::Max, "sonnet", false),
            None
        );

        let mut state = crate::state::app_state_store::AppState::default();
        apply_model_picker_selection(
            &mut state,
            Some("sonnet".to_string()),
            "sonnet",
            ModelEffortLevel::Max,
            true,
        );
        assert_eq!(state.effort_value, Some(Effort::Max));
    }

    #[test]
    fn model_picker_keeps_the_unified_effort_control_for_any_model() {
        let text = render_picker(ModelPickerProps {
            initial: Some("haiku".to_string()),
            ..ModelPickerProps::default()
        });

        assert!(text.contains("effort"), "canvas=\n{text}");
        assert!(text.contains("← → to adjust"), "canvas=\n{text}");
    }

    #[test]
    fn model_picker_renders_official_header_select_effort_and_footer() {
        let text = render_picker(ModelPickerProps {
            is_standalone_command: true,
            ..ModelPickerProps::default()
        });
        assert!(text.contains("Select model"), "canvas=\n{text}");
        assert!(
            text.contains("Switch between Claude models."),
            "canvas=\n{text}"
        );
        assert!(text.contains("specify with --model."), "canvas=\n{text}");
        assert!(text.contains("Sonnet (1M context)"), "canvas=\n{text}");
        assert!(
            text.contains("Sonnet 4.6 for long sessions"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("◐ Medium effort (default)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("← → to adjust"), "canvas=\n{text}");
        assert_eq!(ModelEffortLevel::Off.symbol(), "○");
        assert_eq!(ModelEffortLevel::Off.title_label(), "Off");
        assert_eq!(ModelEffortLevel::Max.symbol(), "◈");
        assert_eq!(ModelEffortLevel::Max.title_label(), "Max");
        assert!(
            text.contains("Enter to confirm · Esc to exit"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn model_picker_renders_plan_mode_and_fast_mode_notices() {
        let text = render_picker(ModelPickerProps {
            session_model_label: Some("Opus".to_string()),
            show_fast_mode_notice: true,
            ..ModelPickerProps::default()
        });
        assert!(
            text.contains("Currently using Opus for this session"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Fast mode is ON and available with Opus 4.6 only (/fast)."),
            "canvas=\n{text}"
        );
    }
}
