//! Maps to: CC `components/ModelPicker.tsx`:54-363.
//!
//! Official ModelPicker owns options, focus, effort cycling, Select chords,
//! and optional fast-mode notices. Callers only supply `initial` plus
//! `onSelect` / `onCancel`. Persist + `AppState` writes stay in
//! [`apply_model_picker_selection`] unless `skipSettingsWrite`.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::pane::Pane;
use crate::constants::figures::EFFORT_LOW;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybindings};
use crate::state::store::AppStore;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub(crate) const MODEL_NO_PREFERENCE: &str = "__NO_PREFERENCE__";
pub(crate) const MODEL_PICKER_VISIBLE_COUNT: usize = 10;
pub(crate) const MODEL_PICKER_HEADER_TEXT: &str = "Switch between Claude models. Applies to this session and future Claude Code sessions. For other/previous model names, specify with --model.";
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemapTier {
    Default,
    Sonnet,
    Sonnet1M,
    Opus,
    Opus1M,
    Haiku,
}

impl RemapTier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Default => "Default (默认)",
            Self::Sonnet => "Sonnet",
            Self::Sonnet1M => "Sonnet (1M context)",
            Self::Opus => "Opus",
            Self::Opus1M => "Opus (1M context)",
            Self::Haiku => "Haiku",
        }
    }

    pub fn env_key(&self) -> &'static str {
        match self {
            Self::Default | Self::Sonnet => "ANTHROPIC_DEFAULT_SONNET_MODEL",
            Self::Sonnet1M => "ANTHROPIC_DEFAULT_SONNET_1M_MODEL",
            Self::Opus => "ANTHROPIC_DEFAULT_OPUS_MODEL",
            Self::Opus1M => "ANTHROPIC_DEFAULT_OPUS_1M_MODEL",
            Self::Haiku => "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        }
    }
}

pub(crate) fn get_candidate_models_for_active_channel(
    active_id: &str,
) -> Vec<crate::services::proxy_channel::types::DiscoveredModel> {
    use crate::services::proxy_channel::types::DiscoveredModel;

    if let Some(cache) = crate::services::proxy_channel::load_channel_models_cache(active_id) {
        if !cache.models.is_empty() {
            return cache.models;
        }
    }
    vec![
        DiscoveredModel {
            id: "claude-3-7-sonnet".to_string(),
            name: "Claude 3.7 Sonnet".to_string(),
            description: Some("Anthropic flagship".to_string()),
        },
        DiscoveredModel {
            id: "claude-3-5-sonnet".to_string(),
            name: "Claude 3.5 Sonnet".to_string(),
            description: None,
        },
        DiscoveredModel {
            id: "claude-3-opus".to_string(),
            name: "Claude 3 Opus".to_string(),
            description: None,
        },
        DiscoveredModel {
            id: "claude-3-5-haiku".to_string(),
            name: "Claude 3.5 Haiku".to_string(),
            description: None,
        },
        DiscoveredModel {
            id: "gemini-2.5-pro".to_string(),
            name: "Gemini 2.5 Pro".to_string(),
            description: None,
        },
        DiscoveredModel {
            id: "gemini-2.5-flash".to_string(),
            name: "Gemini 2.5 Flash".to_string(),
            description: None,
        },
        DiscoveredModel {
            id: "gpt-4o".to_string(),
            name: "GPT-4o".to_string(),
            description: None,
        },
        DiscoveredModel {
            id: "deepseek-chat".to_string(),
            name: "DeepSeek Chat (V3)".to_string(),
            description: None,
        },
    ]
}

pub(crate) fn model_picker_options(max_description_chars: usize) -> Vec<SelectOptionData> {
    if let Some(active_id) = crate::services::proxy_channel::get_active_channel_id() {
        let cfg = crate::services::proxy_channel::load_channel_config(&active_id);
        let sonnet_mapping = cfg
            .as_ref()
            .and_then(|c| c.env.get("ANTHROPIC_DEFAULT_SONNET_MODEL"))
            .cloned()
            .or_else(|| std::env::var("ANTHROPIC_DEFAULT_SONNET_MODEL").ok());
        let sonnet_1m_mapping = cfg
            .as_ref()
            .and_then(|c| c.env.get("ANTHROPIC_DEFAULT_SONNET_1M_MODEL"))
            .cloned()
            .or_else(|| std::env::var("ANTHROPIC_DEFAULT_SONNET_1M_MODEL").ok())
            .or_else(|| sonnet_mapping.as_ref().map(|s| format!("{s}[1m]")));

        let opus_mapping = cfg
            .as_ref()
            .and_then(|c| c.env.get("ANTHROPIC_DEFAULT_OPUS_MODEL"))
            .cloned()
            .or_else(|| std::env::var("ANTHROPIC_DEFAULT_OPUS_MODEL").ok());
        let opus_1m_mapping = cfg
            .as_ref()
            .and_then(|c| c.env.get("ANTHROPIC_DEFAULT_OPUS_1M_MODEL"))
            .cloned()
            .or_else(|| std::env::var("ANTHROPIC_DEFAULT_OPUS_1M_MODEL").ok())
            .or_else(|| opus_mapping.as_ref().map(|s| format!("{s}[1m]")));

        let haiku_mapping = cfg
            .as_ref()
            .and_then(|c| c.env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL"))
            .cloned()
            .or_else(|| std::env::var("ANTHROPIC_DEFAULT_HAIKU_MODEL").ok());

        vec![
            model_option(
                "Default (recommended)",
                MODEL_NO_PREFERENCE,
                &format!(
                    "{} · [修改模型 (按 M / Tab)]",
                    sonnet_mapping.as_deref().unwrap_or("Sonnet (默认)")
                ),
                max_description_chars,
            ),
            model_option(
                "Sonnet",
                "sonnet",
                &format!(
                    "{} · [修改模型 (按 M / Tab)]",
                    sonnet_mapping.as_deref().unwrap_or("Sonnet 4.6")
                ),
                max_description_chars,
            ),
            model_option(
                "Sonnet (1M context)",
                "sonnet[1m]",
                &format!(
                    "{} · [修改模型 (按 M / Tab)]",
                    sonnet_1m_mapping.as_deref().unwrap_or("Sonnet 4.6 (1M)")
                ),
                max_description_chars,
            ),
            model_option(
                "Opus",
                "opus",
                &format!(
                    "{} · [修改模型 (按 M / Tab)]",
                    opus_mapping.as_deref().unwrap_or("Opus 4.6")
                ),
                max_description_chars,
            ),
            model_option(
                "Opus (1M context)",
                "opus[1m]",
                &format!(
                    "{} · [修改模型 (按 M / Tab)]",
                    opus_1m_mapping.as_deref().unwrap_or("Opus 4.6 (1M)")
                ),
                max_description_chars,
            ),
            model_option(
                "Haiku",
                "haiku",
                &format!(
                    "{} · [修改模型 (按 M / Tab)]",
                    haiku_mapping.as_deref().unwrap_or("Haiku 4.5")
                ),
                max_description_chars,
            ),
        ]
    } else {
        vec![
            model_option(
                "Default (recommended)",
                MODEL_NO_PREFERENCE,
                "Use the default model (currently Sonnet 4.6)",
                max_description_chars,
            ),
            model_option(
                "Sonnet",
                "sonnet",
                "Sonnet 4.6 · Best for everyday tasks",
                max_description_chars,
            ),
            model_option(
                "Sonnet (1M context)",
                "sonnet[1m]",
                "Sonnet 4.6 for long sessions",
                max_description_chars,
            ),
            model_option(
                "Opus",
                "opus",
                "Opus 4.6 · Most capable for complex work",
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
        ]
    }
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

/// Maps to: CC `components/ModelPicker.tsx`:324-330 `resolveOptionModel(...)`.
fn resolve_option_model(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if value == MODEL_NO_PREFERENCE {
        return Some(crate::utils::model::model::get_default_main_loop_model());
    }
    Some(crate::utils::model::model::parse_user_specified_model(
        value,
    ))
}

/// Maps to: CC `utils/effort.ts#modelSupportsEffort` as consumed by
/// `components/ModelPicker.tsx`:122-124.
pub(crate) fn model_supports_effort(value: &str) -> bool {
    resolve_option_model(value)
        .as_deref()
        .is_some_and(crate::utils::effort::model_supports_effort)
}

/// Maps to: CC `utils/effort.ts#modelSupportsMaxEffort` as consumed by
/// `components/ModelPicker.tsx`:125-127.
pub(crate) fn model_supports_max_effort(value: &str) -> bool {
    resolve_option_model(value)
        .as_deref()
        .is_some_and(crate::utils::effort::model_supports_max_effort)
}

fn model_effort_level_from_name(level: &str) -> Option<ModelEffortLevel> {
    Some(match level {
        "low" => ModelEffortLevel::Low,
        "medium" => ModelEffortLevel::Medium,
        "high" => ModelEffortLevel::High,
        "xhigh" => ModelEffortLevel::Xhigh,
        "max" => ModelEffortLevel::Max,
        _ => return None,
    })
}

fn effort_level_from_label(level: &str) -> ModelEffortLevel {
    model_effort_level_from_name(level).unwrap_or(ModelEffortLevel::High)
}

/// Maps to: CC `components/ModelPicker.tsx` initializing effort from
/// `AppState.effortValue` / `sessionEffort`.
pub(crate) fn model_effort_from_value(
    value: Option<&crate::utils::effort::EffortValue>,
) -> ModelEffortLevel {
    value
        .map(crate::utils::effort::convert_effort_value_to_level)
        .map(effort_level_from_label)
        .unwrap_or(ModelEffortLevel::High)
}

/// Maps to: CC `components/ModelPicker.tsx`:132-133
/// `getDefaultEffortLevelForOption(...)`.
pub(crate) fn default_effort_level_for_option(value: &str) -> ModelEffortLevel {
    resolve_option_model(value)
        .as_deref()
        .and_then(crate::utils::effort::get_default_effort_for_model)
        .as_ref()
        .map(crate::utils::effort::convert_effort_value_to_level)
        .map(effort_level_from_label)
        .unwrap_or(ModelEffortLevel::High)
}

pub(crate) fn displayed_effort(effort: ModelEffortLevel, focused_value: &str) -> ModelEffortLevel {
    if effort == ModelEffortLevel::Max && !model_supports_max_effort(focused_value) {
        ModelEffortLevel::High
    } else if effort == ModelEffortLevel::Xhigh && !model_supports_xhigh_effort(focused_value) {
        ModelEffortLevel::High
    } else {
        effort
    }
}

/// Maps to: CC `utils/ultracode.ts#modelSupportsXhighEffort` as consumed by
/// ModelPicker effort cycling.
pub(crate) fn model_supports_xhigh_effort(value: &str) -> bool {
    resolve_option_model(value)
        .as_deref()
        .is_some_and(crate::utils::effort::model_supports_xhigh_effort)
}

/// Maps to: CC `components/ModelPicker.tsx` `cycleEffortLevel` / `lmg`.
///
/// Official cycle starts from `getEligibleEffortLevels` (`mQe` / `AU`) and
/// then drops `max` / `xhigh` unless the focused model supports them.
pub(crate) fn cycle_effort(
    current: ModelEffortLevel,
    direction: KeyCode,
    focused_value: &str,
) -> ModelEffortLevel {
    if !model_supports_effort(focused_value) {
        return current;
    }
    let include_max = model_supports_max_effort(focused_value);
    let include_xhigh = model_supports_xhigh_effort(focused_value);
    let resolved = resolve_option_model(focused_value).unwrap_or_default();
    let levels: Vec<ModelEffortLevel> =
        crate::utils::ultracode::get_eligible_effort_levels(&resolved)
            .into_iter()
            .filter(|level| {
                (*level != "max" || include_max) && (*level != "xhigh" || include_xhigh)
            })
            .filter_map(model_effort_level_from_name)
            .collect();
    let levels = if levels.is_empty() {
        vec![
            ModelEffortLevel::Low,
            ModelEffortLevel::Medium,
            ModelEffortLevel::High,
        ]
    } else {
        levels
    };
    let normalized_current = displayed_effort(current, focused_value);
    let current_index = levels
        .iter()
        .position(|level| *level == normalized_current)
        .unwrap_or_else(|| {
            levels
                .iter()
                .position(|level| *level == ModelEffortLevel::High)
                .unwrap_or(0)
        });
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
    selected_value: &str,
    has_toggled: bool,
) -> Option<ModelEffortLevel> {
    if !has_toggled || !model_supports_effort(selected_value) {
        return None;
    }
    Some(effort)
}

/// Maps to: CC ModelPicker `handleSelect` / `bt` — persist persistable
/// levels (`w5e`) and write `AppState.effortValue` / `sessionEffort`.
/// @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
pub(crate) fn apply_model_picker_selection(
    state: &mut crate::state::app_state_store::AppState,
    model: Option<String>,
    selected_value: &str,
    picked_effort: ModelEffortLevel,
    has_toggled_effort: bool,
) -> bool {
    let fast_mode_disabled = crate::utils::fast_mode::is_fast_mode_enabled()
        && state.fast_mode
        && !crate::utils::fast_mode::is_fast_mode_supported_by_model(model.as_deref());
    state.main_loop_model = model;
    state.main_loop_model_for_session = None;
    if fast_mode_disabled {
        state.fast_mode = false;
    }
    let picked = crate::utils::effort::EffortValue::Named(picked_effort.label().to_string());
    let model_default = crate::utils::effort::EffortValue::Named(
        default_effort_level_for_option(selected_value)
            .label()
            .to_string(),
    );
    let prior_persisted = crate::utils::settings::get_settings_for_source(
        crate::utils::settings::SettingSource::User,
    )
    .and_then(|settings| settings.effort_level);
    let effort_level = crate::utils::effort::resolve_picker_effort_persistence(
        Some(&picked),
        &model_default,
        prior_persisted.as_deref(),
        has_toggled_effort,
    );
    if let Some(value) = effort_level.as_ref() {
        if let Some(persistable) = crate::utils::effort::to_persistable_effort(Some(value)) {
            let _ = crate::utils::settings::update_settings_for_source(
                crate::utils::settings::SettingSource::User,
                &serde_json::Map::from_iter([(
                    "effortLevel".to_string(),
                    serde_json::Value::String(persistable),
                )]),
            );
        }
    }
    state.effort_value = effort_level;
    if has_toggled_effort && model_supports_effort(selected_value) {
        // An explicit picker adjustment has the same session semantics as an
        // explicit `/effort <level>`: it exits ultracode and lets the chosen
        // level override launch-pinned model defaults.
        crate::utils::ultracode::unpin_launch_effort();
        state.ultracode = false;
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

impl Default for ModelEffortLevel {
    fn default() -> Self {
        Self::High
    }
}

fn focused_model_label(options: &[SelectOptionData], focused_index: usize) -> Option<&str> {
    options
        .get(focused_index)
        .map(|option| option.label.as_str())
}

fn render_remap_tier_picker(
    tier: RemapTier,
    candidates: &[crate::services::proxy_channel::types::DiscoveredModel],
    focused_index: usize,
    theme: Theme,
) -> AnyElement<'static> {
    let count = candidates.len();
    let focused = focused_index.min(count.saturating_sub(1));
    let visible_count = 8usize.min(count.max(1));
    let visible_from = visible_from_index(focused, count, visible_count);

    let options: Vec<SelectOptionData> = candidates
        .iter()
        .map(|m| SelectOptionData {
            label: m.name.clone(),
            value: m.id.clone(),
            description: m.description.clone(),
            dim_description: true,
            disabled: false,
            input: None,
        })
        .collect();

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(margin_bottom: 1u32, flex_direction: FlexDirection::Column) {
                Text(
                    content: format!("配置 [{}] 档位对应模型", tier.label()),
                    color: theme.remember,
                    weight: Weight::Bold,
                )
                Text(
                    content: "从代理渠道拉取的可用远端模型列表中点选绑定：".to_string(),
                    dim: true,
                )
            }
            View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                Select(
                    is_disabled: false,
                    hide_indexes: false,
                    visible_option_count: visible_count,
                    options: options,
                    focused_index: focused,
                    selected_value: None::<String>,
                    visible_from_index: visible_from,
                    layout: SelectLayout::Compact,
                )
            }
            Text(
                content: "↑/↓ 移动选择 · Enter 确认绑定并生效 · Esc 取消返回".to_string(),
                dim: true,
                italic: true,
            )
        }
    }
    .into_any()
}

fn render_model_picker_content(props: &ModelPickerView, theme: Theme) -> AnyElement<'static> {
    let count = props.options.len();
    let focused_index = props.focused_index.min(count.saturating_sub(1));
    let focused_value = props
        .options
        .get(focused_index)
        .map(|option| option.value.as_str())
        .unwrap_or(MODEL_NO_PREFERENCE);
    let focused_label = focused_model_label(&props.options, focused_index);
    let visible_option_count = count.min(MODEL_PICKER_VISIBLE_COUNT).max(1);
    let visible_from = props
        .visible_from_index
        .min(count.saturating_sub(visible_option_count));
    let hidden_count = count.saturating_sub(visible_option_count);
    let display_effort = displayed_effort(props.effort, focused_value);
    let focused_default_effort = default_effort_level_for_option(focused_value);
    let supports_effort = model_supports_effort(focused_value);
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
                    #(if supports_effort {
                        element! {
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
                        }.into_any()
                    } else {
                        element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: EFFORT_LOW.to_string(), color: theme.subtle, wrap: TextWrap::NoWrap)
                                Text(content: format!(" Effort not supported{}", focused_label.map(|label| format!(" for {label}")).unwrap_or_default()), color: theme.subtle, wrap: TextWrap::NoWrap)
                            }
                        }.into_any()
                    })
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
    let mut remap_tier = hooks.use_state(|| Option::<RemapTier>::None);
    let mut remap_focus = hooks.use_state(|| 0usize);
    let skip_settings_write = props.skip_settings_write;
    let event_options = options.clone();

    let active_channel_id = crate::services::proxy_channel::get_active_channel_id();
    let candidate_models = active_channel_id
        .as_deref()
        .map(get_candidate_models_for_active_channel)
        .unwrap_or_default();
    let candidate_count = candidate_models.len();
    let event_candidate_models = candidate_models.clone();
    let event_active_channel_id = active_channel_id.clone();

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

        // Sub-view: In remap mode (Screen 2)
        if let Some(tier) = remap_tier.get() {
            let remap_idx = remap_focus.get().min(candidate_count.saturating_sub(1));
            match code {
                KeyCode::Esc => {
                    remap_tier.set(None);
                }
                KeyCode::Down => {
                    remap_focus.set(next_index(remap_idx, candidate_count));
                }
                KeyCode::Up => {
                    remap_focus.set(previous_index(remap_idx, candidate_count));
                }
                KeyCode::Enter => {
                    if let Some(chosen) = event_candidate_models.get(remap_idx) {
                        if let Some(active_id) = event_active_channel_id.as_deref() {
                            let env_key = tier.env_key();
                            let _ = crate::services::proxy_channel::update_channel_env(
                                active_id,
                                &[(env_key, &chosen.id)],
                            );
                            crate::utils::process_env::set(env_key, &chosen.id);
                            unsafe {
                                std::env::set_var(env_key, &chosen.id);
                            }
                        }
                    }
                    remap_tier.set(None);
                }
                _ => {}
            }
            event.stop_propagation();
            return;
        }

        // Main view: In standard model picker (Screen 1)
        let focused = focused_index.get().min(option_count.saturating_sub(1));
        match code {
            KeyCode::Esc => should_close.set(true),
            KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Tab => {
                if event_active_channel_id.is_some() {
                    if let Some(option) = event_options.get(focused) {
                        let tier = match option.value.as_str() {
                            MODEL_NO_PREFERENCE => Some(RemapTier::Default),
                            "sonnet" => Some(RemapTier::Sonnet),
                            "sonnet[1m]" => Some(RemapTier::Sonnet1M),
                            "opus" => Some(RemapTier::Opus),
                            "opus[1m]" => Some(RemapTier::Opus1M),
                            "haiku" => Some(RemapTier::Haiku),
                            _ => None,
                        };
                        if let Some(t) = tier {
                            remap_tier.set(Some(t));
                            remap_focus.set(0);
                        }
                    }
                }
            }
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
    let content = if let Some(tier) = remap_tier.get() {
        render_remap_tier_picker(tier, &candidate_models, remap_focus.get(), theme)
    } else {
        render_model_picker_content(
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
        )
    };

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
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");

        let options = model_picker_options(120);
        assert_eq!(options[0].label, "Default (recommended)");
        assert_eq!(options[0].value, MODEL_NO_PREFERENCE);
        assert_eq!(
            options[0].description.as_deref(),
            Some("Use the default model (currently Sonnet 4.6)")
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::High, KeyCode::Right, "opus"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::Xhigh, KeyCode::Right, "opus"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::High, KeyCode::Right, "sonnet"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::Xhigh, KeyCode::Right, "sonnet"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            displayed_effort(ModelEffortLevel::Max, "sonnet"),
            ModelEffortLevel::Max
        );
        assert!(model_supports_effort(MODEL_NO_PREFERENCE));
        assert!(model_supports_effort("sonnet"));
        assert!(model_supports_effort("opus"));
        assert!(!model_supports_effort("haiku"));
        assert!(model_supports_max_effort("opus"));
        assert!(model_supports_max_effort("sonnet"));
        assert!(!model_supports_max_effort("haiku"));
        assert_eq!(
            selected_picker_effort(ModelEffortLevel::Max, "sonnet", true),
            Some(ModelEffortLevel::Max)
        );
        assert_eq!(
            selected_picker_effort(ModelEffortLevel::Max, "sonnet", false),
            None
        );

        let mut state = crate::state::app_state_store::AppState::default();
        state.ultracode = true;
        apply_model_picker_selection(
            &mut state,
            Some("sonnet".to_string()),
            "sonnet",
            ModelEffortLevel::Max,
            true,
        );
        assert_eq!(
            state.effort_value,
            Some(crate::utils::effort::EffortValue::Named("max".to_string()))
        );
        assert!(!state.ultracode);
    }

    #[test]
    fn model_picker_renders_unsupported_effort_for_haiku_like_official() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");

        let text = render_picker(ModelPickerProps {
            initial: Some("haiku".to_string()),
            ..ModelPickerProps::default()
        });

        assert!(
            text.contains("Effort not supported for Haiku"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("High effort"),
            "Haiku should not show adjustable effort; canvas=\n{text}"
        );
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
        assert!(text.contains("Default (recommended)"), "canvas=\n{text}");
        assert!(text.contains("Sonnet 4.6 · Best"), "canvas=\n{text}");
        assert!(text.contains("● High effort (default)"), "canvas=\n{text}");
        assert!(text.contains("← → to adjust"), "canvas=\n{text}");
        assert_eq!(ModelEffortLevel::Xhigh.symbol(), "◉");
        assert_eq!(ModelEffortLevel::Xhigh.title_label(), "xHigh");
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
