//! Maps to: CC `components/EffortPicker.tsx` (`Qsm` / 2.1.241 `NSg`).
//!
//! Official `NSg` owns the slider chrome, `handleKeyDown` (`left`/`right` no
//! wrap, `return`, `escape`), and cache-break (`pKe`). Ripple bleed stays
//! unavailable with the unported workflow runtime. `/effort <level>` confirm
//! (`BSg`) starts this same component on the cache-break dialog.

use crate::commands::effort::effort::{
    apply_effort_command, execute_effort_for_model, normalize_effort_picker_arg,
};
use crate::components::cache_break_confirm_dialog::CacheBreakConfirmDialog;
use crate::components::design_system::byline::Byline;
use crate::components::design_system::keyboard_shortcut_hint::{
    KeyboardShortcutHint, KeyboardShortcutHintStyleContext,
};
use crate::components::design_system::pane::Pane;
use crate::hooks::use_main_loop_model::use_main_loop_model;
use crate::state::app_state::use_app_state_maybe_outside_of_provider;
use crate::state::store::AppStore;
use crate::utils::effort::{EffortValue, get_displayed_effort_level, get_effort_env_override};
use crate::utils::theme::Theme;
use crate::utils::thinking::get_rainbow_color;
use crate::utils::ultracode::{
    XHIGH_EFFORT_WARNING, get_eligible_effort_levels, has_org_restricted_higher_effort,
    is_launch_effort_pinned, is_ultracode_active, is_ultracode_available,
    should_confirm_effort_cache_break,
};
use iocraft::prelude::*;

const FULL_TRACK_WIDTH: usize = 42;
const MIN_TRACK_WIDTH: usize = 14;
const TRIANGLE_POSITIONS: &[usize] = &[1, 10, 20, 30, 40];
const LABEL_SPACERS: &[usize] = &[5, 5, 5, 6];
const DEFAULT_FOCUS_INDEX: usize = 3;
const SHIMMER_HIGHLIGHT: Color = Color::Rgb {
    r: 0xd0,
    g: 0xb4,
    b: 0xff,
};
const ULTRACODE_VIOLET: Color = Color::Rgb {
    r: 140,
    g: 80,
    b: 240,
};
const ANIMATION_TICK_MS: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderLevelColor {
    Warning,
    Success,
    Permission,
    AutoAcceptShimmer,
    RainbowAnimated,
    VioletRipple,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliderLevel {
    pub value: &'static str,
    pub label: &'static str,
    pub color: SliderLevelColor,
}

const SLIDER_LEVELS: &[SliderLevel] = &[
    SliderLevel {
        value: "low",
        label: "low",
        color: SliderLevelColor::Warning,
    },
    SliderLevel {
        value: "medium",
        label: "medium",
        color: SliderLevelColor::Success,
    },
    SliderLevel {
        value: "high",
        label: "high",
        color: SliderLevelColor::Permission,
    },
    SliderLevel {
        value: "xhigh",
        label: "xhigh",
        color: SliderLevelColor::AutoAcceptShimmer,
    },
    SliderLevel {
        value: "max",
        label: "max",
        color: SliderLevelColor::RainbowAnimated,
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliderGeometry {
    pub levels: Vec<SliderLevel>,
    pub width: usize,
    pub triangle_positions: Vec<usize>,
    pub label_starts: Vec<usize>,
    pub spacers: Vec<usize>,
    pub track_chars: String,
    pub accent_start: Option<usize>,
    pub sublabel: Option<(String, usize)>,
    pub cap_note: Option<String>,
}

fn repeat_safe(ch: char, count: usize) -> String {
    std::iter::repeat_n(ch, count).collect()
}

fn compute_label_starts(levels: &[SliderLevel], spacers: &[usize]) -> Vec<usize> {
    levels
        .iter()
        .enumerate()
        .map(|(idx, _)| {
            levels
                .iter()
                .take(idx)
                .enumerate()
                .map(|(i, level)| level.label.len() + spacers[i])
                .sum()
        })
        .collect()
}

fn build_base_geometry(level_count: usize) -> (Vec<SliderLevel>, usize, Vec<usize>, Vec<usize>) {
    let count = level_count.clamp(1, SLIDER_LEVELS.len());
    let levels = SLIDER_LEVELS[..count].to_vec();
    let spacers = LABEL_SPACERS[..count.saturating_sub(1)].to_vec();
    let triangle_positions = TRIANGLE_POSITIONS[..count].to_vec();
    if count == SLIDER_LEVELS.len() {
        return (levels, FULL_TRACK_WIDTH, triangle_positions, spacers);
    }
    let starts = compute_label_starts(&levels, &spacers);
    let width = starts
        .last()
        .copied()
        .unwrap_or(0)
        .saturating_add(levels.last().map(|level| level.label.len()).unwrap_or(0))
        .max(MIN_TRACK_WIDTH);
    (levels, width, triangle_positions, spacers)
}

/// Maps to: CC `getSliderGeometry` (`Qac` / 2.1.241 `wbs`).
pub fn get_slider_geometry(model: &str) -> SliderGeometry {
    let (base_levels, base_width, base_triangles, base_spacers) =
        build_base_geometry(get_eligible_effort_levels(model).len());
    let cap_note = has_org_restricted_higher_effort(model)
        .then(|| "Higher effort levels are restricted by your organization.".to_string());
    if is_ultracode_available(Some(model)) {
        let divider_col = base_width + 3;
        let mut levels = base_levels;
        levels.push(SliderLevel {
            value: "ultracode",
            label: "ultracode",
            color: SliderLevelColor::VioletRipple,
        });
        let ultracode_label_start = divider_col + 4;
        let mut spacers = base_spacers;
        spacers.push(ultracode_label_start.saturating_sub(base_width));
        let mut triangle_positions = base_triangles;
        triangle_positions.push(divider_col + 8);
        return SliderGeometry {
            label_starts: compute_label_starts(&levels, &spacers),
            levels,
            width: divider_col + 17,
            triangle_positions,
            spacers,
            track_chars: format!(
                "{}┆{}",
                repeat_safe('─', base_width + 1),
                repeat_safe('─', 18)
            ),
            accent_start: Some(base_width + 2),
            sublabel: Some(("xhigh + workflows".to_string(), divider_col)),
            cap_note,
        };
    }
    SliderGeometry {
        label_starts: compute_label_starts(&base_levels, &base_spacers),
        levels: base_levels,
        width: base_width,
        triangle_positions: base_triangles,
        spacers: base_spacers,
        track_chars: repeat_safe('─', base_width),
        accent_start: None,
        sublabel: None,
        cap_note,
    }
}

/// Values currently on the slider, including ultracode when the official gate is on.
pub fn slider_values(model: &str) -> Vec<&'static str> {
    get_slider_geometry(model)
        .levels
        .into_iter()
        .map(|level| level.value)
        .collect()
}

/// Maps to: CC `hAc` initial slider index.
pub fn initial_focus_index(
    geometry: &SliderGeometry,
    model: &str,
    effort: Option<&EffortValue>,
    ultracode: bool,
) -> usize {
    if is_ultracode_active(model, effort, ultracode)
        && let Some(index) = geometry
            .levels
            .iter()
            .position(|level| level.value == "ultracode")
    {
        return index;
    }
    let effective = match get_effort_env_override() {
        Some(None) => None,
        Some(Some(value)) => Some(value),
        None => (!is_launch_effort_pinned(model))
            .then(|| effort.cloned())
            .flatten(),
    };
    if let Some(EffortValue::Named(level)) = &effective
        && let Some(index) = geometry
            .levels
            .iter()
            .position(|entry| entry.value == level.as_str())
    {
        return index;
    }
    let displayed = get_displayed_effort_level(model, effective.as_ref());
    geometry
        .levels
        .iter()
        .position(|level| level.value == displayed)
        .unwrap_or(DEFAULT_FOCUS_INDEX.min(geometry.levels.len().saturating_sub(1)))
}

fn prefers_reduced_motion() -> bool {
    crate::utils::settings::get_initial_settings()
        .prefers_reduced_motion
        .unwrap_or(false)
}

fn theme_color(theme: &Theme, color: SliderLevelColor) -> Option<Color> {
    match color {
        SliderLevelColor::Warning => Some(theme.warning),
        SliderLevelColor::Success => Some(theme.success),
        SliderLevelColor::Permission => Some(theme.permission),
        SliderLevelColor::AutoAcceptShimmer => Some(theme.auto_accept),
        SliderLevelColor::RainbowAnimated => None,
        SliderLevelColor::VioletRipple => Some(ULTRACODE_VIOLET),
    }
}

fn rainbow_label(text: &str, theme: &Theme, tick: u64) -> AnyElement<'static> {
    let shift = tick as usize;
    element! {
        View(flex_direction: FlexDirection::Row) {
            #(text.chars().enumerate().map(|(index, ch)| {
                element! {
                    Text(
                        content: ch.to_string(),
                        color: get_rainbow_color(theme, index + shift, false),
                        weight: Weight::Bold,
                    )
                }
            }).collect::<Vec<_>>())
        }
    }
    .into_any()
}

fn shimmer_label(text: &str, theme: &Theme, tick: u64, reduced: bool) -> AnyElement<'static> {
    let cycle = text.chars().count() + 4;
    let highlight = if reduced {
        None
    } else {
        Some((tick as usize) % cycle)
    };
    element! {
        View(flex_direction: FlexDirection::Row) {
            #(text.chars().enumerate().map(|(index, ch)| {
                let is_highlight = highlight == Some(index);
                let is_adjacent = highlight.is_some_and(|at| index + 1 == at || at + 1 == index);
                element! {
                    Text(
                        content: ch.to_string(),
                        color: if is_highlight { SHIMMER_HIGHLIGHT } else { theme.auto_accept },
                        weight: if is_highlight || is_adjacent { Weight::Bold } else { Weight::Normal },
                    )
                }
            }).collect::<Vec<_>>())
        }
    }
    .into_any()
}

fn slider_level_label(
    level: &SliderLevel,
    selected: bool,
    theme: &Theme,
    tick: u64,
    reduced: bool,
) -> AnyElement<'static> {
    if !selected {
        if level.color == SliderLevelColor::VioletRipple {
            return element! {
                Text(content: level.label.to_string(), color: ULTRACODE_VIOLET)
            }
            .into_any();
        }
        return element! {
            Text(content: level.label.to_string(), dim: true)
        }
        .into_any();
    }
    match level.color {
        SliderLevelColor::VioletRipple => element! {
            Text(
                content: level.label.to_string(),
                weight: Weight::Bold,
                color: Color::Rgb { r: 255, g: 255, b: 255 },
                background_color: ULTRACODE_VIOLET,
            )
        }
        .into_any(),
        SliderLevelColor::RainbowAnimated => rainbow_label(level.label, theme, tick),
        SliderLevelColor::AutoAcceptShimmer => shimmer_label(level.label, theme, tick, reduced),
        color => element! {
            Text(
                content: level.label.to_string(),
                weight: Weight::Bold,
                color: theme_color(theme, color),
            )
        }
        .into_any(),
    }
}

fn track_row(geometry: &SliderGeometry, selected_index: usize) -> AnyElement<'static> {
    let triangle_col = geometry
        .triangle_positions
        .get(selected_index)
        .copied()
        .unwrap_or(0)
        .min(geometry.track_chars.chars().count().saturating_sub(1));
    let chars: Vec<char> = geometry.track_chars.chars().collect();
    let accent_start = geometry.accent_start.unwrap_or(chars.len());
    let left: String = chars.iter().take(triangle_col).collect();
    let right: String = chars.iter().skip(triangle_col + 1).collect();
    let left_plain_end = left.chars().count().min(accent_start);
    let left_plain: String = left.chars().take(left_plain_end).collect();
    let left_accent: String = left.chars().skip(left_plain_end).collect();
    let triangle_in_accent = triangle_col >= accent_start;
    let right_plain_len = accent_start.saturating_sub(triangle_col + 1);
    let right_plain: String = right.chars().take(right_plain_len).collect();
    let right_accent: String = right.chars().skip(right_plain_len).collect();

    element! {
        View(flex_direction: FlexDirection::Row) {
            Text(content: left_plain, dim: true)
            #(if !left_accent.is_empty() {
                Some(element! { Text(content: left_accent, color: ULTRACODE_VIOLET) })
            } else {
                None
            })
            Text(
                content: "▲".to_string(),
                weight: Weight::Bold,
                color: triangle_in_accent.then_some(ULTRACODE_VIOLET),
            )
            Text(content: right_plain, dim: true)
            #(if !right_accent.is_empty() {
                Some(element! { Text(content: right_accent, color: ULTRACODE_VIOLET) })
            } else {
                None
            })
        }
    }
    .into_any()
}

fn labels_row(
    geometry: &SliderGeometry,
    selected_index: usize,
    theme: &Theme,
    tick: u64,
    reduced: bool,
) -> AnyElement<'static> {
    let labels_end = geometry
        .label_starts
        .last()
        .copied()
        .unwrap_or(0)
        .saturating_add(
            geometry
                .levels
                .last()
                .map(|level| level.label.len())
                .unwrap_or(0),
        );
    let trailing = geometry.width.saturating_sub(labels_end);
    element! {
        View(flex_direction: FlexDirection::Row) {
            #(geometry.levels.iter().enumerate().flat_map(|(index, level)| {
                let mut items = Vec::new();
                if index > 0 {
                    items.push(element! {
                        Text(content: repeat_safe(' ', geometry.spacers[index - 1]))
                    }.into_any());
                }
                items.push(slider_level_label(
                    level,
                    selected_index == index,
                    theme,
                    tick,
                    reduced,
                ));
                items
            }).collect::<Vec<_>>())
            #(if trailing > 0 {
                Some(element! { Text(content: repeat_safe(' ', trailing)) })
            } else {
                None
            })
        }
    }
    .into_any()
}

#[derive(Default, Props)]
pub struct EffortPickerProps<'a> {
    pub args: Option<String>,
    pub has_conversation_messages: bool,
    pub on_cancel_args: HandlerMut<'a, String>,
    pub on_close: HandlerMut<'a, ()>,
    pub on_select: HandlerMut<'a, String>,
}

fn apply_picker_effort(value: &str, store: Option<&AppStore>, model: &str) -> String {
    match store {
        Some(store) => apply_effort_command(value, store),
        None => execute_effort_for_model(value, model).message,
    }
}

/// Maps to: CC `components/EffortPicker.tsx#EffortPicker` (`NSg`).
/// Official `NSg` reads effort (`fk`), `cacheMissAckedAtOutputTokens`,
/// `ultracode` via `useAppState` (`Ht`), and the model via `useMainLoopModel`
/// (`wC`). Confirm applies through `W8t`/`Zvr` (`Zi()` + persist) and then
/// `onDone(message)`. Callers only supply host completion plus `/effort
/// <level>` confirm args and the `getMessages` emptiness flag.
#[component]
pub fn EffortPicker<'a>(
    props: &mut EffortPickerProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks
        .try_use_context::<Theme>()
        .map(|theme| *theme)
        .unwrap_or_else(|| *crate::utils::theme::current());
    let main_loop_model =
        use_app_state_maybe_outside_of_provider(&mut hooks, |state| state.main_loop_model.clone())
            .flatten();
    let main_loop_model_for_session =
        use_app_state_maybe_outside_of_provider(&mut hooks, |state| {
            state.main_loop_model_for_session.clone()
        })
        .flatten();
    let effort =
        use_app_state_maybe_outside_of_provider(&mut hooks, |state| state.effort_value.clone())
            .flatten();
    let ultracode = use_app_state_maybe_outside_of_provider(&mut hooks, |state| state.ultracode)
        .unwrap_or(false);
    let acked_output_tokens = use_app_state_maybe_outside_of_provider(&mut hooks, |state| {
        state.cache_miss_acked_at_output_tokens
    })
    .unwrap_or(-1);
    let model = use_main_loop_model(
        main_loop_model.as_deref(),
        main_loop_model_for_session.as_deref(),
    );
    let store = hooks
        .try_use_context::<AppStore>()
        .map(|store| store.clone());
    let geometry = get_slider_geometry(&model);
    let option_count = geometry.levels.len();
    let initial = initial_focus_index(&geometry, &model, effort.as_ref(), ultracode);
    let mut pending_confirm = hooks.use_state(|| props.args.clone());
    let mut confirmed = hooks.use_state(|| false);
    let mut cancelled = hooks.use_state(|| false);
    let mut focused_index = hooks.use_state(move || initial);
    let mut should_close = hooks.use_state(|| false);
    let mut pending_selection = hooks.use_state(|| Option::<String>::None);
    let tick = hooks.use_state(|| 0u64);
    let exit_state = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    let values: Vec<String> = geometry
        .levels
        .iter()
        .map(|level| level.value.to_string())
        .collect();

    hooks.use_propagated_terminal_events(move |event| {
        if pending_confirm.read().is_some() {
            return;
        }
        let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
            return;
        };
        if *kind == KeyEventKind::Release {
            return;
        }
        let focused = focused_index.get().min(option_count.saturating_sub(1));
        match code {
            KeyCode::Left => {
                focused_index.set(focused.saturating_sub(1));
            }
            KeyCode::Right => {
                focused_index.set((focused + 1).min(option_count.saturating_sub(1)));
            }
            KeyCode::Enter => {
                if let Some(value) = values.get(focused) {
                    pending_selection.set(Some(value.clone()));
                }
            }
            KeyCode::Esc => {
                should_close.set(true);
            }
            _ => return,
        }
        event.stop_propagation();
    });

    let focused = focused_index.get().min(option_count.saturating_sub(1));
    let selected_value = geometry
        .levels
        .get(focused)
        .map(|level| level.value)
        .unwrap_or("high");
    let reduced = prefers_reduced_motion();
    let needs_animation = !reduced
        && geometry.levels.get(focused).is_some_and(|level| {
            matches!(
                level.color,
                SliderLevelColor::RainbowAnimated | SliderLevelColor::AutoAcceptShimmer
            )
        });
    hooks.use_interval(
        {
            let mut tick = tick;
            move || tick.set(tick.get().wrapping_add(1))
        },
        needs_animation.then(|| std::time::Duration::from_millis(ANIMATION_TICK_MS)),
    );

    if should_close.get() {
        should_close.set(false);
        (props.on_close)(());
    }
    if cancelled.get() {
        cancelled.set(false);
        pending_confirm.set(None);
        if props.args.is_some() {
            let current = effort
                .as_ref()
                .map(EffortValue::as_str)
                .unwrap_or_else(|| "auto".into());
            (props.on_cancel_args)(format!("Kept effort level as {current}"));
        }
    }
    if confirmed.get() {
        confirmed.set(false);
        let value = pending_confirm.read().clone();
        pending_confirm.set(None);
        if let Some(value) = value {
            let message = apply_picker_effort(&value, store.as_ref(), &model);
            (props.on_select)(message);
        }
    }
    let selected = { pending_selection.read().clone() };
    if let Some(value) = selected {
        pending_selection.set(None);
        let new_effort = normalize_effort_picker_arg(&value, &model).flatten();
        if should_confirm_effort_cache_break(
            new_effort.as_ref(),
            effort.as_ref(),
            &model,
            acked_output_tokens,
            props.has_conversation_messages,
        ) {
            pending_confirm.set(Some(value));
        } else {
            let message = apply_picker_effort(&value, store.as_ref(), &model);
            (props.on_select)(message);
        }
    }
    let pending = pending_confirm.read().clone();
    if let Some(value) = pending {
        return element! {
            CacheBreakConfirmDialog(
                effort: normalize_effort_picker_arg(&value, &model).flatten(),
                on_confirm: move |_| confirmed.set(true),
                on_cancel: move |_| cancelled.set(true),
            )
        }
        .into_any();
    }

    let faster_smarter_gap = geometry.width.saturating_sub(6 + 7);
    let warning = (selected_value == "max").then_some(XHIGH_EFFORT_WARNING);
    let tick_value = tick.get();

    element! {
        Pane {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "Effort".to_string(), weight: Weight::Bold)
                View(height: 1u32) {}
                View(
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::CENTER,
                    width: 100pct,
                ) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Faster".to_string())
                        Text(content: repeat_safe(' ', faster_smarter_gap))
                        Text(content: "Smarter".to_string())
                    }
                    #(track_row(&geometry, focused))
                    #(labels_row(&geometry, focused, &theme, tick_value, reduced))
                    #(geometry.sublabel.as_ref().map(|(text, start)| {
                        element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: repeat_safe(' ', *start))
                                Text(content: text.clone(), dim: true)
                            }
                        }
                    }))
                    #(geometry.cap_note.as_ref().map(|text| element! {
                        View { Text(content: text.clone(), dim: true, wrap: TextWrap::Wrap) }
                    }))
                    #(warning.map(|text| {
                        element! {
                            View {
                                Text(content: text.to_string(), dim: true, wrap: TextWrap::Wrap)
                            }
                        }
                    }))
                }
                View(height: 1u32) {}
                #(if exit_state.pending {
                    element! {
                        Text(
                            content: format!(
                                "Press {} again to exit",
                                exit_state.key_name.unwrap_or("Ctrl-C"),
                            ),
                            dim: true,
                        )
                    }
                    .into_any()
                } else {
                    element! {
                        ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext {
                            dim: true,
                            italic: false,
                        })) {
                            Byline {
                                KeyboardShortcutHint(shortcut: "←/→".to_string(), action: "adjust".to_string())
                                KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                                KeyboardShortcutHint(shortcut: "Esc".to_string(), action: "cancel".to_string())
                            }
                        }
                    }
                    .into_any()
                })
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use crate::utils::theme;

    #[test]
    fn slider_geometry_matches_official_five_stop_ladder() {
        let geometry = get_slider_geometry("claude-opus-4-7");
        let values: Vec<_> = geometry.levels.iter().map(|level| level.value).collect();
        assert_eq!(values, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(geometry.width, 42);
        assert_eq!(geometry.triangle_positions, vec![1usize, 10, 20, 30, 40]);
        assert_eq!(geometry.label_starts, vec![0usize, 8, 19, 28, 39]);
        assert_eq!(geometry.spacers, vec![5usize, 5, 5, 6]);
        assert_eq!(geometry.track_chars.chars().count(), 42);
        assert!(!values.contains(&"ultracode"));
    }

    #[test]
    fn initial_focus_follows_named_effort_then_displayed_default() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");
        let geometry = get_slider_geometry("claude-opus-4-7");
        assert_eq!(
            initial_focus_index(
                &geometry,
                "claude-opus-4-7",
                Some(&EffortValue::Named("xhigh".into())),
                false
            ),
            3
        );
        assert_eq!(
            initial_focus_index(&geometry, "claude-opus-4-7", None, false),
            3
        );
    }

    #[test]
    fn picker_renders_official_horizontal_slider_not_a_list() {
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.main_loop_model = Some("claude-opus-4-7".to_string());
        initial.effort_value = Some(EffortValue::Named("high".into()));
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    initial_state: Some(initial),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        EffortPicker
                    }.into_any()),
                )
            }
        }
        .render(Some(80));
        let text = canvas.to_string();

        assert!(text.contains("Effort"), "canvas=\n{text}");
        assert!(
            text.contains("Faster") && text.contains("Smarter"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("low     medium     high     xhigh      max"),
            "canvas=\n{text}"
        );
        assert!(text.contains('▲'), "canvas=\n{text}");
        assert!(
            text.contains("←/→ to adjust")
                && text.contains("Enter to confirm")
                && text.contains("Esc to cancel"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("1.") && !text.contains("minimal overhead"),
            "must not render the list adapter; canvas=\n{text}"
        );
    }
}
