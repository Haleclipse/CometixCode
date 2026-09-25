//! Maps to: CC `components/Spinner.tsx` + `components/Spinner/`.
//! The main-screen port keeps the official component seams (glyph, glimmer
//! message, animation row responsibilities) while preserving native terminal
//! scrollback. No fullscreen, alternate screen, real model, or real tool work is
//! introduced here.

pub mod flashing_char;
pub mod glimmer_message;
pub mod glyph;
pub mod shimmer_char;
pub mod spinner_animation_row;
pub mod teammate_select_hint;
pub mod teammate_spinner_line;
pub mod teammate_spinner_tree;
pub mod teammate_tree;
pub mod use_shimmer_animation;
pub mod use_stalled_animation;
pub mod utils;

pub use glyph::SpinnerGlyph;
pub use teammate_tree::{
    TeammateMessageBlockSnapshot, TeammateMessageSnapshot, TeammateRecentActivity,
    TeammateSpinnerColor, TeammateSpinnerTask, TeammateSpinnerTree, TeammateTaskSnapshot,
};

use crate::components::message_response::MessageResponse;
use crate::constants::figures::{DOWN_ARROW, TEARDROP_ASTERISK, UP_ARROW};
use crate::utils::config::GlobalConfig;
use crate::utils::format::format_number;
use crate::utils::settings::SettingsJson;
use crate::utils::theme::Theme;
use glimmer_message::GlimmerMessage;
use iocraft::prelude::*;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use teammate_tree::running_teammate_spinner_tasks_from_app_tasks;
use unicode_width::UnicodeWidthStr;

const THINKING_DELAY_MS: u64 = 3_000;
const THINKING_GLOW_PERIOD_MS: u64 = 2_000;
const PLAIN_SPINNER_FRAME_MS: u64 = 120;

/// Maps to: CC `components/Spinner.tsx:578` `Spinner()` — the plain inline
/// spinner (e.g. MessageSelector's "Summarizing…" row): a width-2 box whose
/// text-colored glyph advances every 120ms through the official frame cycle
/// (`SPINNER_FRAMES = [...DEFAULT_CHARACTERS, ...reverse]`); reduced motion
/// renders the static dot (CC :584-589 — no dim cycle, unlike SpinnerGlyph's
/// reduced-motion branch).
#[component]
pub fn Spinner(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks
        .try_use_context::<Theme>()
        .map(|theme| *theme)
        .unwrap_or_else(|| *crate::utils::theme::current());
    // CC reads `useSettings()`, whose provider wraps every screen. Here the
    // startup screens (main.rs's "Resuming conversation…", "Loading
    // commands…") mount outside `AppStateProvider` (Contract B), so the
    // setting is read provider-optionally and defaults to animated there.
    let reduced_motion = crate::state::app_state::use_app_state_maybe_outside_of_provider(
        &mut hooks,
        |state| state.settings.prefers_reduced_motion.unwrap_or(false),
    )
    .unwrap_or(false);
    let frame = hooks.use_animation_frame(if reduced_motion {
        None
    } else {
        Some(Duration::from_millis(PLAIN_SPINNER_FRAME_MS))
    });
    let glyph = if reduced_motion {
        "●".to_string()
    } else {
        // CC :593 — frame derived from synced time so all spinners align.
        utils::spinner_frame((frame.time_ms as u64 / PLAIN_SPINNER_FRAME_MS) as usize).to_string()
    };

    element! {
        View(width: 2u32, height: 1u32, flex_shrink: 0.0f32) {
            Text(content: glyph, color: theme.text, wrap: TextWrap::NoWrap)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpinnerMode {
    Requesting,
    Responding,
    ToolInput,
    ToolUse,
    Thinking,
}

impl Default for SpinnerMode {
    fn default() -> Self {
        // Maps to CC `REPL.tsx`: `streamMode` starts as `'responding'`.
        Self::Responding
    }
}

#[derive(Default, Props)]
pub struct SpinnerWithVerbProps {
    pub mode: SpinnerMode,
    pub message: String,
    /// Optional suffix appended to active thinking text.
    /// Maps to CC `getEffortSuffix(...)`, e.g. ` with high effort`.
    pub effort_suffix: Option<String>,
    pub reduced_motion: bool,
    pub response_length: usize,
    /// Ref-backed live response length. Maps to CC `responseLengthRef`, which
    /// updates per stream delta without forcing `REPL`/`Messages` to rerender.
    pub response_length_ref: Option<Ref<usize>>,
    pub has_active_tools: bool,
    pub spinner_suffix: Option<String>,
    /// Explicit tip override (tests / message rows). Production Spinner reads
    /// `AppState.spinner_tip` when this is `None` (CC REPL `useAppState`).
    pub spinner_tip: Option<String>,
    /// Maps to: CC Spinner `leaderIsIdle` prop — leader row idle while
    /// teammates still run (passed from REPL, not AppState).
    pub leader_is_idle: bool,
    pub next_task: Option<String>,
    pub budget_text: Option<String>,
    pub thinking_text: Option<String>,
    /// Optional readonly snapshot of official `btwUseCount` for time-based tips.
    pub btw_use_count: Option<u32>,
    pub override_color: Option<Color>,
    pub override_shimmer_color: Option<Color>,
    /// Freezes the animation clock for static message rendering and tests.
    pub disable_animation: bool,
    /// Deterministic elapsed time for mock/static spinner rows.
    pub time_ms_override: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StalledState {
    mount_time_ms: u64,
    last_token_time_ms: u64,
    last_active_tool_time_ms: u64,
    last_response_length: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ThinkingStatusState {
    #[default]
    Idle,
    Thinking {
        started_at_ms: u64,
    },
    HoldingThinking {
        duration_ms: u64,
        switch_at_ms: u64,
        clear_at_ms: u64,
    },
    Thought {
        duration_ms: u64,
        clear_at_ms: u64,
    },
}

fn default_message(mode: SpinnerMode) -> &'static str {
    match mode {
        SpinnerMode::Requesting => "Requesting",
        SpinnerMode::Responding => "Responding",
        SpinnerMode::ToolInput => "Waiting for input",
        SpinnerMode::ToolUse => "Using tools",
        SpinnerMode::Thinking => "Thinking",
    }
}

fn spinner_verb_candidates(settings: Option<&SettingsJson>) -> Vec<String> {
    crate::constants::spinner_verbs::get_spinner_verbs(settings)
}

fn choose_spinner_verb(settings: Option<&SettingsJson>) -> String {
    let candidates = spinner_verb_candidates(settings);
    if candidates.is_empty() {
        return default_message(SpinnerMode::Thinking).to_string();
    }

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as usize)
        .unwrap_or(0);
    candidates[seed % candidates.len()].clone()
}

fn next_thinking_status_state(
    state: ThinkingStatusState,
    mode: SpinnerMode,
    now_ms: u64,
) -> ThinkingStatusState {
    if mode == SpinnerMode::Thinking {
        return match state {
            ThinkingStatusState::Thinking { .. } => state,
            _ => ThinkingStatusState::Thinking {
                started_at_ms: now_ms,
            },
        };
    }

    match state {
        ThinkingStatusState::Idle => ThinkingStatusState::Idle,
        ThinkingStatusState::Thinking { started_at_ms } => {
            let duration_ms = now_ms.saturating_sub(started_at_ms);
            if duration_ms < 2_000 {
                ThinkingStatusState::HoldingThinking {
                    duration_ms,
                    switch_at_ms: started_at_ms.saturating_add(2_000),
                    clear_at_ms: started_at_ms.saturating_add(4_000),
                }
            } else {
                ThinkingStatusState::Thought {
                    duration_ms,
                    clear_at_ms: now_ms.saturating_add(2_000),
                }
            }
        }
        ThinkingStatusState::HoldingThinking {
            duration_ms,
            switch_at_ms,
            clear_at_ms,
        } => {
            if now_ms >= clear_at_ms {
                ThinkingStatusState::Idle
            } else if now_ms >= switch_at_ms {
                ThinkingStatusState::Thought {
                    duration_ms,
                    clear_at_ms,
                }
            } else {
                state
            }
        }
        ThinkingStatusState::Thought {
            duration_ms,
            clear_at_ms,
        } => {
            if now_ms >= clear_at_ms {
                ThinkingStatusState::Idle
            } else {
                ThinkingStatusState::Thought {
                    duration_ms,
                    clear_at_ms,
                }
            }
        }
    }
}

fn thinking_status_text(state: ThinkingStatusState) -> Option<String> {
    match state {
        ThinkingStatusState::Idle => None,
        ThinkingStatusState::Thinking { .. } | ThinkingStatusState::HoldingThinking { .. } => {
            Some("thinking".to_string())
        }
        ThinkingStatusState::Thought { duration_ms, .. } => Some(format!(
            "thought for {}s",
            1_u64.max((duration_ms.saturating_add(500)) / 1_000)
        )),
    }
}

fn spinner_mode_glyph(mode: SpinnerMode) -> &'static str {
    match mode {
        SpinnerMode::Requesting => UP_ARROW,
        SpinnerMode::Responding
        | SpinnerMode::ToolInput
        | SpinnerMode::ToolUse
        | SpinnerMode::Thinking => DOWN_ARROW,
    }
}

fn foregrounded_teammate_color(color: TeammateSpinnerColor, theme: &Theme) -> Color {
    match color {
        TeammateSpinnerColor::Red => theme.agent_red,
        TeammateSpinnerColor::Blue => theme.agent_blue,
        TeammateSpinnerColor::Green => theme.agent_green,
        TeammateSpinnerColor::Yellow => theme.agent_yellow,
        TeammateSpinnerColor::Purple => theme.agent_purple,
        TeammateSpinnerColor::Orange => theme.agent_orange,
        TeammateSpinnerColor::Pink => theme.agent_pink,
        TeammateSpinnerColor::Cyan => theme.agent_cyan,
    }
}

fn format_duration_ms(ms: u64) -> String {
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }

    let mut seconds = ((ms % 60_000) as f64 / 1000.0).round() as u64;
    let mut minutes_total = ms / 60_000;
    if seconds == 60 {
        seconds = 0;
        minutes_total += 1;
    }

    let days = minutes_total / (24 * 60);
    let hours = (minutes_total % (24 * 60)) / 60;
    let minutes = minutes_total % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

/// The animation row's clock interval; `None` freezes it. Reduced motion
/// still ticks once a second so wall-elapsed text refreshes (Cometix keeps
/// the timer live where CC relies on parent re-renders).
fn spinner_row_interval(mode: SpinnerMode, reduced_motion: bool, frozen: bool) -> Option<Duration> {
    if frozen {
        None
    } else if reduced_motion {
        Some(Duration::from_millis(1_000))
    } else if mode == SpinnerMode::Requesting {
        Some(Duration::from_millis(50))
    } else {
        Some(Duration::from_millis(100))
    }
}

fn compute_glimmer_index(mode: SpinnerMode, time_ms: u64, message_width: usize) -> isize {
    let glimmer_speed = if mode == SpinnerMode::Requesting {
        50
    } else {
        200
    };
    let cycle_length = message_width + 20;
    if cycle_length == 0 {
        return -100;
    }
    let cycle_position = (time_ms / glimmer_speed) as usize;
    if mode == SpinnerMode::Requesting {
        (cycle_position % cycle_length) as isize - 10
    } else {
        message_width as isize + 10 - (cycle_position % cycle_length) as isize
    }
}

fn spinner_stall_reset_activity(has_active_tools: bool, leader_is_idle: bool) -> bool {
    // Official SpinnerAnimationRow treats leader-idle teammate turns like active
    // tools so teammate foregrounding does not falsely trigger the stalled fade.
    has_active_tools || leader_is_idle
}

fn compute_stalled_intensity(
    state: StalledState,
    time_ms: u64,
    current_response_length: usize,
    has_active_tools: bool,
) -> f32 {
    let last_activity = state
        .last_token_time_ms
        .max(state.last_active_tool_time_ms)
        .max(state.mount_time_ms);
    let time_since_last_token = if has_active_tools {
        0
    } else if current_response_length > 0 {
        time_ms.saturating_sub(last_activity)
    } else {
        time_ms.saturating_sub(state.mount_time_ms)
    };

    if has_active_tools || time_since_last_token <= 3_000 {
        0.0
    } else {
        ((time_since_last_token - 3_000) as f32 / 2_000.0).clamp(0.0, 1.0)
    }
}

fn estimated_token_count_from_chars(char_count: usize) -> usize {
    // Maps to CC `SpinnerAnimationRow`: `Math.round(responseLength / 4)`.
    char_count.saturating_add(2) / 4
}

fn next_displayed_response_length(
    displayed_response_length: usize,
    current_response_length: usize,
    animation_paused: bool,
) -> usize {
    if animation_paused || current_response_length <= displayed_response_length {
        return current_response_length;
    }

    let gap = current_response_length - displayed_response_length;
    let increment = if gap < 70 {
        3
    } else if gap < 200 {
        8.max((gap.saturating_mul(15) + 99) / 100)
    } else {
        50
    };
    displayed_response_length
        .saturating_add(increment)
        .min(current_response_length)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SpinnerStatusPart {
    Text(String),
    Tokens {
        mode: SpinnerMode,
        token_count: String,
        has_running_teammates: bool,
    },
    Thinking(String),
}

#[derive(Default, Props)]
struct SpinnerStatusBylineProps {
    pub parts: Vec<SpinnerStatusPart>,
    pub thinking_color: Option<Color>,
    pub thinking_only: bool,
}

/// Maps to CC `SpinnerAnimationRow` status rendering with `Byline` parts.
#[component]
fn SpinnerStatusByline(
    props: &SpinnerStatusBylineProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let thinking_color = props.thinking_color.unwrap_or(theme.inactive);
    let mut children = Vec::<AnyElement<'static>>::new();

    if props.parts.is_empty() {
        return element! { View }.into_any();
    }

    if !props.thinking_only {
        children.push(
            element! { Text(content: "(".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap) }
                .into(),
        );
    }

    for (index, part) in props.parts.iter().enumerate() {
        if index > 0 {
            children.push(
                element! { Text(content: " · ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap) }
                    .into(),
            );
        }

        match part {
            SpinnerStatusPart::Text(text) => children.push(
                element! { Text(content: text.clone(), color: theme.inactive, wrap: TextWrap::NoWrap) }
                    .into(),
            ),
            SpinnerStatusPart::Tokens {
                mode,
                token_count,
                has_running_teammates,
            } => {
                if *has_running_teammates {
                    children.push(
                        element! { Text(content: format!("{token_count} tokens"), color: theme.inactive, wrap: TextWrap::NoWrap) }
                            .into(),
                    );
                } else {
                    children.push(
                        element! {
                            View(flex_direction: FlexDirection::Row) {
                                View(width: 2u32) {
                                    Text(content: spinner_mode_glyph(*mode).to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                                }
                                Text(content: format!("{token_count} tokens"), color: theme.inactive, wrap: TextWrap::NoWrap)
                            }
                        }
                        .into(),
                    );
                }
            }
            SpinnerStatusPart::Thinking(text) => {
                let content = if props.thinking_only {
                    format!("({text})")
                } else {
                    text.clone()
                };
                children.push(
                    element! { Text(content: content, color: thinking_color, wrap: TextWrap::NoWrap) }
                        .into(),
                );
            }
        }
    }

    if !props.thinking_only {
        children.push(
            element! { Text(content: ")".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap) }
                .into(),
        );
    }

    element! {
        View(flex_direction: FlexDirection::Row) {
            #(children.into_iter())
        }
    }
    .into_any()
}

fn tip_sessions_since_last_shown(tip_id: &str, global_config: Option<&GlobalConfig>) -> u64 {
    let Some(global_config) = global_config else {
        return u64::MAX;
    };
    let Some(last_shown) = global_config
        .tips_history
        .as_ref()
        .and_then(|history| history.get(tip_id))
    else {
        return u64::MAX;
    };
    if *last_shown == 0 {
        return u64::MAX;
    }
    global_config.num_startups.saturating_sub(*last_shown)
}

fn spinner_tips_override_tip(
    settings: &SettingsJson,
    global_config: Option<&GlobalConfig>,
) -> Option<String> {
    let override_settings = settings.spinner_tips_override.as_ref()?;
    let mut best: Option<(u64, String)> = None;
    for (index, tip) in override_settings.tips.iter().enumerate() {
        let tip = tip.trim();
        if tip.is_empty() {
            continue;
        }
        let sessions = tip_sessions_since_last_shown(&format!("custom-tip-{index}"), global_config);
        if best
            .as_ref()
            .is_none_or(|(best_sessions, _)| sessions > *best_sessions)
        {
            best = Some((sessions, tip.to_string()));
        }
    }
    best.map(|(_, tip)| tip)
}

fn spinner_tips_override_excludes_default(settings: &SettingsJson) -> bool {
    settings
        .spinner_tips_override
        .as_ref()
        .and_then(|override_settings| override_settings.exclude_default)
        .unwrap_or(false)
}

fn spinner_tip_fallback_from_settings(
    settings: Option<&SettingsJson>,
    global_config: Option<&GlobalConfig>,
    explicit_tip: Option<&str>,
    live_tips_enabled: bool,
) -> Option<String> {
    let explicit_tip = explicit_tip
        .map(str::trim)
        .filter(|tip| !tip.is_empty())
        .map(str::to_string);
    if !live_tips_enabled {
        return explicit_tip;
    }

    let Some(settings) = settings else {
        return explicit_tip;
    };
    let custom_tip = spinner_tips_override_tip(settings, global_config);
    if custom_tip.is_some()
        && (explicit_tip.is_none() || spinner_tips_override_excludes_default(settings))
    {
        custom_tip
    } else {
        explicit_tip
    }
}

fn effective_spinner_tip(
    elapsed_ms: u64,
    spinner_tip: Option<&str>,
    has_next_task: bool,
    time_based_tips_enabled: bool,
    btw_use_count: u32,
) -> Option<String> {
    if has_next_task {
        return None;
    }
    if time_based_tips_enabled && elapsed_ms > 1_800_000 {
        return Some(
            "Use /clear to start fresh when switching topics and free up context".to_string(),
        );
    }
    if time_based_tips_enabled && elapsed_ms > 30_000 && btw_use_count == 0 {
        return Some(
            "Use /btw to ask a quick side question without interrupting Claude's current work"
                .to_string(),
        );
    }
    spinner_tip.map(str::to_string)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SpinnerTasksSnapshot {
    current_verb: Option<String>,
    next_pending_subject: Option<String>,
}

fn non_empty_task_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn task_spinner_verb(task: &crate::utils::tasks::TaskRecord) -> Option<String> {
    non_empty_task_text(task.active_form.as_deref())
        .or_else(|| non_empty_task_text(Some(task.subject.as_str())))
}

fn find_next_pending_task<'a>(
    tasks: &'a [crate::utils::tasks::TaskRecord],
) -> Option<&'a crate::utils::tasks::TaskRecord> {
    let pending = tasks
        .iter()
        .filter(|task| task.status == "pending")
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return None;
    }

    let unresolved_ids = tasks
        .iter()
        .filter(|task| task.status != "completed")
        .map(|task| task.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    pending
        .iter()
        .copied()
        .find(|task| {
            !task
                .blocked_by
                .iter()
                .any(|id| unresolved_ids.contains(id.as_str()))
        })
        .or_else(|| pending.first().copied())
}

fn spinner_tasks_snapshot_from_tasks(
    tasks: &[crate::utils::tasks::TaskRecord],
) -> SpinnerTasksSnapshot {
    SpinnerTasksSnapshot {
        current_verb: tasks
            .iter()
            .find(|task| task.status != "pending" && task.status != "completed")
            .and_then(task_spinner_verb),
        next_pending_subject: find_next_pending_task(tasks)
            .and_then(|task| non_empty_task_text(Some(task.subject.as_str()))),
    }
}

fn spinner_tasks_snapshot_from_store() -> SpinnerTasksSnapshot {
    let task_list_id = crate::utils::tasks::get_task_list_id();
    let tasks = crate::utils::tasks::list_tasks(&task_list_id);
    spinner_tasks_snapshot_from_tasks(&tasks)
}

/// Row-level spinner used while a turn is in flight.
/// Maps to CC `SpinnerWithVerb` + `SpinnerAnimationRow` for the main-screen
/// path. App/task data remains mocked/read-only; this component only renders
/// visible chrome from explicit props and runtime display settings.
#[component]
pub fn SpinnerWithVerb(
    props: &SpinnerWithVerbProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    // Maps to CC `useStalledAnimation`: mutable timing values are refs and
    // must not schedule a render while being refreshed by the render itself.
    let mut stalled_state = hooks.use_ref(StalledState::default);
    // Refs, not States: CC's SpinnerAnimationRow keeps its thinking status
    // and token counter in `useRef` and steps them during render, so a step
    // never schedules a render of its own — the next one comes from the row
    // clock. As States they re-rendered on every step and, while the token
    // counter was catching up, that meant a frame every ~20ms.
    let mut thinking_status_state = hooks.use_ref(ThinkingStatusState::default);
    let settings_reduced_motion = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.settings.prefers_reduced_motion.unwrap_or(false)
    });
    // Maps to: CC settings via `useAppState(s => s.settings)` and
    // `getGlobalConfig()` cached direct reads.
    let runtime_settings_snapshot =
        crate::state::app_state::use_app_state(&mut hooks, |state| (*state.settings).clone());
    let runtime_effort_value =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.effort_value.clone());
    let time_based_tips_enabled = runtime_settings_snapshot
        .spinner_tips_enabled
        .unwrap_or(true);
    let runtime_global_config_snapshot = Some(crate::utils::config::load_global_config());
    let runtime_btw_use_count = runtime_global_config_snapshot
        .as_ref()
        .and_then(|config| config.btw_use_count)
        .unwrap_or(0);
    let runtime_settings_for_spinner_verb = runtime_settings_snapshot.clone();
    let spinner_verb =
        hooks.use_state(move || choose_spinner_verb(Some(&runtime_settings_for_spinner_verb)));
    let teammate_tasks = crate::state::app_state::use_app_state(&mut hooks, |state| {
        running_teammate_spinner_tasks_from_app_tasks(&state.tasks)
    });
    // Maps to: CC TeammateSpinnerTree / Spinner reading selection + focus from
    // AppState (`selectedIPAgentIndex`, `viewSelectionMode`,
    // `viewingAgentTaskId`, `showTeammateMessagePreview`, `expandedView`).
    // One hook per field, per CC `AppState.tsx:138-141`: "For multiple
    // independent fields, call the hook multiple times". The five-field tuple
    // this replaces was a CONSTRUCTED selector, which the same doc block
    // forbids at `:143-148` — under `Object.is` it would report a change every
    // render. Rust's `PartialEq` made it merely wasteful rather than wrong, but
    // it is still the shape the source rules out.
    let selected_ip_agent_index =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.selected_ip_agent_index);
    let view_selection_mode =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.view_selection_mode);
    let viewing_agent_task_id = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.viewing_agent_task_id.clone()
    });
    let show_teammate_message_preview =
        crate::state::app_state::use_app_state(&mut hooks, |state| {
            state.show_teammate_message_preview
        });
    let expanded_view =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.expanded_view);
    let is_in_selection_mode = view_selection_mode.is_selecting();
    let selected_index = is_in_selection_mode.then_some(selected_ip_agent_index);
    let has_running_teammates = !teammate_tasks.is_empty();
    let all_teammates_idle =
        has_running_teammates && teammate_tasks.iter().all(|task| task.is_idle);
    // Maps to: CC `showSpinnerTree = expandedView === 'teammates'`.
    let show_teammate_tree = matches!(
        expanded_view,
        crate::state::app_state_store::ExpandedView::Teammates
    ) && has_running_teammates;
    let foregrounded_teammate = viewing_agent_task_id
        .as_deref()
        .and_then(|id| teammate_tasks.iter().find(|task| task.id == id))
        .cloned();
    let foregrounded_idle_teammate = foregrounded_teammate
        .as_ref()
        .filter(|task| task.is_idle)
        .cloned();
    let foregrounded_active_teammate = foregrounded_teammate
        .as_ref()
        .filter(|task| !task.is_idle)
        .cloned();
    let teammate_token_count = if show_teammate_tree {
        0
    } else {
        teammate_tasks
            .iter()
            .map(|task| task.token_count)
            .sum::<usize>()
    };
    let task_snapshot = spinner_tasks_snapshot_from_store();
    let random_spinner_verb = spinner_verb.read().clone();
    let leader_verb = if !props.message.trim().is_empty() {
        props.message.clone()
    } else if let Some(current_verb) = &task_snapshot.current_verb {
        current_verb.clone()
    } else {
        random_spinner_verb.clone()
    };
    let tree_leader_verb = Some(leader_verb.clone());
    let leader_is_idle = props.leader_is_idle;
    let effective_next_task = props
        .next_task
        .clone()
        .or_else(|| task_snapshot.next_pending_subject.clone());
    let effective_effort_suffix = props.effort_suffix.clone().or_else(|| {
        let suffix = crate::utils::effort::get_effort_suffix(
            &crate::utils::model::model::get_main_loop_model(),
            runtime_effort_value.as_ref(),
        );
        (!suffix.is_empty()).then_some(suffix)
    });

    let reduced_motion = props.reduced_motion || settings_reduced_motion;
    // Maps to CC: animation clock (`useAnimationFrame`) vs wall-clock elapsed
    // (`Date.now() - loadingStartTimeRef`). `reduced_motion` pauses animation
    // only — elapsed must keep advancing.
    let anim_paused = reduced_motion || props.disable_animation || props.time_ms_override.is_some();
    // Still tick under reduced_motion (1s) so wall-elapsed UI can refresh;
    // official relies on parent re-renders, but we keep the timer live.
    // Row clock. CC 2.1.88 `SpinnerAnimationRow` ticks at a flat 50ms;
    // CC 2.1.280 (`chunk-dgxaxeme.js:841`) ticks at 50ms only while
    // requesting — the one mode whose glimmer steps every 50ms — and at
    // 100ms otherwise, since the glimmer then steps every 200ms and the
    // glyph every 120ms, so a 50ms clock rendered frames nothing moved in.
    // Adopted here (deliberate 2.1.280 alignment): with the streaming
    // preview coalesced at 100ms this is what puts streaming near CC's
    // ~9 frames/s. 280's `glimmerParked` prop (defaults to false) has no
    // 2.1.88 counterpart and is treated as false.
    let frame_interval = spinner_row_interval(
        props.mode,
        reduced_motion,
        props.time_ms_override.is_some() || props.disable_animation,
    );
    let frame = hooks.use_animation_frame(frame_interval);
    let loading_start = hooks.use_const(|| Instant::now());
    // Depend on the frame tick so each interval re-samples wall time.
    let _frame_tick = frame.time_ms;
    let elapsed_ms = props
        .time_ms_override
        .unwrap_or_else(|| loading_start.elapsed().as_millis() as u64);
    let anim_anchor_ms = hooks.use_state(|| frame.time_ms as u64);
    let anim_ms = if anim_paused {
        0
    } else {
        (frame.time_ms as u64).saturating_sub(anim_anchor_ms.get())
    };
    let next_thinking_status =
        next_thinking_status_state(thinking_status_state.get(), props.mode, elapsed_ms);
    if next_thinking_status != thinking_status_state.get() {
        thinking_status_state.set(next_thinking_status);
    }
    let current_response_length = props
        .response_length_ref
        .map(|response_length_ref| response_length_ref.get())
        .unwrap_or(props.response_length);
    // CC SpinnerAnimationRow.tsx:155-169 `tokenCounterRef` ("driven by 50ms
    // clock"): one increment per row tick, held in a ref.
    let mut displayed_response_length_state = hooks.use_ref(|| current_response_length);
    let token_animation_paused = anim_paused;
    let displayed_response_length = next_displayed_response_length(
        displayed_response_length_state.get(),
        current_response_length,
        token_animation_paused,
    );
    if displayed_response_length != displayed_response_length_state.get() {
        displayed_response_length_state.set(displayed_response_length);
    }

    let mut next_stalled = stalled_state.get();
    if next_stalled.mount_time_ms == 0 && elapsed_ms > 0 {
        next_stalled.mount_time_ms = elapsed_ms;
        next_stalled.last_token_time_ms = elapsed_ms;
    }
    if current_response_length > next_stalled.last_response_length {
        next_stalled.last_response_length = current_response_length;
        next_stalled.last_token_time_ms = elapsed_ms;
    }
    let has_stall_reset_activity =
        spinner_stall_reset_activity(props.has_active_tools, leader_is_idle);
    if has_stall_reset_activity {
        next_stalled.last_active_tool_time_ms = elapsed_ms;
    }
    if next_stalled != stalled_state.get() {
        stalled_state.set(next_stalled);
    }

    let stalled_intensity = if props.override_color.is_some() {
        0.0
    } else {
        compute_stalled_intensity(
            next_stalled,
            elapsed_ms,
            current_response_length,
            has_stall_reset_activity,
        )
    };

    if let Some(foregrounded_teammate) = foregrounded_idle_teammate {
        let idle_text = if all_teammates_idle {
            format!(
                "{TEARDROP_ASTERISK} Worked for {}",
                format_duration_ms(foregrounded_teammate.worked_for_ms.unwrap_or(0))
            )
        } else {
            format!("{TEARDROP_ASTERISK} Idle")
        };
        return element! {
            View(flex_direction: FlexDirection::Column, width: 100pct, align_items: AlignItems::FLEX_START) {
                View(flex_direction: FlexDirection::Row, flex_wrap: FlexWrap::Wrap, margin_top: 1u32, width: 100pct) {
                    Text(content: idle_text, color: theme.inactive, wrap: TextWrap::NoWrap)
                }
                #(if show_teammate_tree {
                    Some(element! {
                        TeammateSpinnerTree(
                            tasks: teammate_tasks.clone(),
                            selected_index: selected_index,
                            is_in_selection_mode: is_in_selection_mode,
                            all_idle: all_teammates_idle,
                            viewing_teammate_id: viewing_agent_task_id.clone(),
                            leader_verb: if leader_is_idle { None } else { tree_leader_verb.clone() },
                            leader_token_count: Some(estimated_token_count_from_chars(current_response_length)),
                            leader_idle_text: if leader_is_idle { Some("Idle".to_string()) } else { None },
                            show_preview: show_teammate_message_preview,
                        )
                    })
                } else { None })
            }
        };
    }

    if leader_is_idle && has_running_teammates && viewing_agent_task_id.is_none() {
        let idle_text = if all_teammates_idle {
            format!("{TEARDROP_ASTERISK} Idle")
        } else {
            format!("{TEARDROP_ASTERISK} Idle · teammates running")
        };
        return element! {
            View(flex_direction: FlexDirection::Column, width: 100pct, align_items: AlignItems::FLEX_START) {
                View(flex_direction: FlexDirection::Row, flex_wrap: FlexWrap::Wrap, margin_top: 1u32, width: 100pct) {
                    Text(content: idle_text, color: theme.inactive, wrap: TextWrap::NoWrap)
                }
                #(if show_teammate_tree {
                    Some(element! {
                        TeammateSpinnerTree(
                            tasks: teammate_tasks.clone(),
                            selected_index: selected_index,
                            is_in_selection_mode: is_in_selection_mode,
                            all_idle: all_teammates_idle,
                            viewing_teammate_id: viewing_agent_task_id.clone(),
                            leader_verb: None::<String>,
                            leader_token_count: Some(estimated_token_count_from_chars(current_response_length)),
                            leader_idle_text: Some("Idle".to_string()),
                            show_preview: show_teammate_message_preview,
                        )
                    })
                } else { None })
            }
        };
    }

    let bare_message = if let Some(foregrounded_task) = foregrounded_active_teammate.as_ref() {
        foregrounded_task
            .spinner_verb
            .as_deref()
            .and_then(|verb| non_empty_task_text(Some(verb)))
            // Maps to CC `SpinnerWithVerbInner`: foregrounded active teammates
            // fall back to the mount-stable random verb, not the leader verb.
            .unwrap_or_else(|| random_spinner_verb.clone())
    } else {
        leader_verb.clone()
    };
    let message = if bare_message.ends_with('…') || bare_message.ends_with("...") {
        bare_message
    } else {
        format!("{}…", bare_message)
    };
    let live_tip_scheduler_enabled =
        time_based_tips_enabled && !props.disable_animation && props.time_ms_override.is_none();
    // Maps to: CC Spinner `spinnerTip` from AppState, with prop override for
    // tests / static message rows.
    let app_spinner_tip =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.spinner_tip.clone());
    let tip_from_app_or_prop = props.spinner_tip.clone().or(app_spinner_tip);
    let spinner_tip = spinner_tip_fallback_from_settings(
        Some(&runtime_settings_snapshot),
        runtime_global_config_snapshot.as_ref(),
        tip_from_app_or_prop.as_deref(),
        live_tip_scheduler_enabled,
    );
    let effective_tip = effective_spinner_tip(
        elapsed_ms,
        spinner_tip.as_deref(),
        effective_next_task.is_some(),
        live_tip_scheduler_enabled,
        props.btw_use_count.unwrap_or(runtime_btw_use_count),
    );
    let message_color = props.override_color.unwrap_or(theme.claude);
    let shimmer_color = props.override_shimmer_color.unwrap_or(theme.claude_shimmer);
    let message_width = UnicodeWidthStr::width(message.as_str());
    let glimmer_index = if reduced_motion || stalled_intensity > 0.0 {
        -100
    } else {
        compute_glimmer_index(props.mode, anim_ms, message_width)
    };
    let flash_opacity = if reduced_motion || props.mode != SpinnerMode::ToolUse {
        0.0
    } else {
        (((anim_ms as f32 / 1000.0) * std::f32::consts::PI).sin() + 1.0) / 2.0
    };

    let (columns, _) = hooks.use_terminal_size();
    // Maps to CC `SpinnerAnimationRow` progressive width gating:
    // messageWidth = glimmerMessageWidth + 2, availableSpace = columns - messageWidth - 5.
    let available_width = (columns as usize).saturating_sub(message_width + 7);
    // Cometix Special modification:
    // Official gates timer/tokens behind `AppState.verbose`, running teammates,
    // or `SHOW_TOKENS_AFTER_MS` (30s). We always offer them immediately; width
    // gating and `token_count > 0` still apply. Do not thread `verbose` here.
    let wants_timer_and_tokens = true;
    let timer = format_duration_ms(elapsed_ms);
    let timer_width = UnicodeWidthStr::width(timer.as_str());
    let leader_token_count = estimated_token_count_from_chars(displayed_response_length);
    let token_count = foregrounded_active_teammate
        .as_ref()
        .map(|teammate| teammate.token_count)
        .unwrap_or_else(|| leader_token_count + teammate_token_count);
    let token_count_text = format_number(token_count as u64);
    let tokens_for_width = if has_running_teammates {
        format!("{token_count_text} tokens")
    } else {
        format!(
            "{} {token_count_text} tokens",
            spinner_mode_glyph(props.mode)
        )
    };
    let tokens_width = UnicodeWidthStr::width(tokens_for_width.as_str());
    let active_thinking_status = matches!(
        next_thinking_status,
        ThinkingStatusState::Thinking { .. } | ThinkingStatusState::HoldingThinking { .. }
    );
    let mut effective_thinking_text = props
        .thinking_text
        .clone()
        .or_else(|| thinking_status_text(next_thinking_status));
    if active_thinking_status {
        if let Some(text) = effective_thinking_text.as_mut() {
            if let Some(suffix) = effective_effort_suffix
                .as_deref()
                .filter(|suffix| !suffix.is_empty())
            {
                text.push_str(suffix);
            }
        }
    }
    let mut thinking_width = effective_thinking_text
        .as_deref()
        .map(UnicodeWidthStr::width)
        .unwrap_or(0);
    let wants_thinking = effective_thinking_text.is_some();
    let mut show_thinking = wants_thinking && available_width > thinking_width;
    if !show_thinking
        && active_thinking_status
        && effective_effort_suffix
            .as_deref()
            .is_some_and(|suffix| !suffix.is_empty())
        && available_width > UnicodeWidthStr::width("thinking")
    {
        effective_thinking_text = Some("thinking".to_string());
        thinking_width = UnicodeWidthStr::width("thinking");
        show_thinking = true;
    }
    let used_after_thinking = if show_thinking { thinking_width + 3 } else { 0 };
    let show_timer = wants_timer_and_tokens && available_width > used_after_thinking + timer_width;
    let used_after_timer = used_after_thinking + if show_timer { timer_width + 3 } else { 0 };
    let show_tokens = wants_timer_and_tokens
        && token_count > 0
        && available_width > used_after_timer + tokens_width;

    let spinner_suffix = props
        .spinner_suffix
        .as_deref()
        .map(str::trim)
        .filter(|suffix| !suffix.is_empty());
    let thinking_only = show_thinking
        && active_thinking_status
        && spinner_suffix.is_none()
        && !show_timer
        && !show_tokens;
    let mut status_parts = Vec::new();
    if let Some(suffix) = spinner_suffix {
        status_parts.push(SpinnerStatusPart::Text(suffix.to_string()));
    }
    if show_timer {
        status_parts.push(SpinnerStatusPart::Text(timer));
    }
    if show_tokens {
        status_parts.push(SpinnerStatusPart::Tokens {
            mode: props.mode,
            token_count: token_count_text,
            has_running_teammates,
        });
    }
    if show_thinking {
        if let Some(thinking_text) = &effective_thinking_text {
            status_parts.push(SpinnerStatusPart::Thinking(thinking_text.clone()));
        }
    }

    if effective_thinking_text.is_some() {
        let thinking_color = if reduced_motion || elapsed_ms < THINKING_DELAY_MS {
            theme.inactive
        } else {
            utils::interpolate_terminal_color(
                theme.inactive,
                theme.inactive_shimmer,
                utils::sine_opacity(anim_ms, THINKING_DELAY_MS, THINKING_GLOW_PERIOD_MS),
            )
        };
        let has_status = !status_parts.is_empty();
        return element! {
            View(flex_direction: FlexDirection::Column, width: 100pct, align_items: AlignItems::FLEX_START) {
                View(flex_direction: FlexDirection::Row, flex_wrap: FlexWrap::Wrap, margin_top: 1u32, width: 100pct) {
                    SpinnerGlyph(
                        frame: (anim_ms / 120) as usize,
                        color: Some(message_color),
                        reduced_motion: reduced_motion,
                        time_ms: anim_ms,
                        stalled_intensity: stalled_intensity,
                    )
                    GlimmerMessage(
                        message: message.clone(),
                        mode: props.mode,
                        message_color: Some(message_color),
                        glimmer_index: glimmer_index,
                        flash_opacity: flash_opacity,
                        shimmer_color: Some(shimmer_color),
                        stalled_intensity: stalled_intensity,
                    )
                    #({
                        let byline: Option<AnyElement<'static>> = if let Some(teammate) = &foregrounded_active_teammate {
                            Some(element! {
                                View(flex_direction: FlexDirection::Row) {
                                    Text(content: "(esc to interrupt ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                                    Text(content: teammate.agent_name.clone(), color: foregrounded_teammate_color(teammate.color, &theme), wrap: TextWrap::NoWrap)
                                    Text(content: ")".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                                }
                            }.into_any())
                        } else if has_status {
                            Some(element! {
                                SpinnerStatusByline(
                                    parts: status_parts.clone(),
                                    thinking_color: Some(thinking_color),
                                    thinking_only: thinking_only,
                                )
                            }.into_any())
                        } else { None };
                        byline
                    })
                }
                #(if show_teammate_tree {
                    Some(element! {
                        TeammateSpinnerTree(
                            tasks: teammate_tasks.clone(),
                            selected_index: selected_index,
                            is_in_selection_mode: is_in_selection_mode,
                            all_idle: all_teammates_idle,
                            viewing_teammate_id: viewing_agent_task_id.clone(),
                            leader_verb: tree_leader_verb.clone(),
                            leader_token_count: Some(estimated_token_count_from_chars(current_response_length)),
                            leader_idle_text: if leader_is_idle { Some("Idle".to_string()) } else { None },
                            show_preview: show_teammate_message_preview,
                        )
                    })
                } else { None })
                #(if let Some(budget) = &props.budget_text {
                    Some(element! { MessageResponse(content: budget.clone(), color: Some(theme.inactive)) })
                } else { None })
                #(if let Some(next) = &effective_next_task {
                    Some(element! { MessageResponse(content: format!("Next: {next}"), color: Some(theme.inactive)) })
                } else if let Some(tip) = &effective_tip {
                    Some(element! { MessageResponse(content: format!("Tip: {tip}"), color: Some(theme.inactive)) })
                } else { None })
            }
        };
    }

    let has_status = !status_parts.is_empty();
    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct, align_items: AlignItems::FLEX_START) {
            View(flex_direction: FlexDirection::Row, flex_wrap: FlexWrap::Wrap, margin_top: 1u32, width: 100pct) {
                SpinnerGlyph(
                    frame: (anim_ms / 120) as usize,
                    color: Some(message_color),
                    reduced_motion: reduced_motion,
                    time_ms: anim_ms,
                    stalled_intensity: stalled_intensity,
                )
                GlimmerMessage(
                    message: message.clone(),
                    mode: props.mode,
                    message_color: Some(message_color),
                    glimmer_index: glimmer_index,
                    flash_opacity: flash_opacity,
                    shimmer_color: Some(shimmer_color),
                    stalled_intensity: stalled_intensity,
                )
                #({
                    let byline: Option<AnyElement<'static>> = if let Some(teammate) = &foregrounded_active_teammate {
                        Some(element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "(esc to interrupt ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                                Text(content: teammate.agent_name.clone(), color: foregrounded_teammate_color(teammate.color, &theme), wrap: TextWrap::NoWrap)
                                Text(content: ")".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                            }
                        }.into_any())
                    } else if has_status {
                        Some(element! {
                            SpinnerStatusByline(
                                parts: status_parts.clone(),
                                thinking_color: Some(theme.inactive),
                                thinking_only: false,
                            )
                        }.into_any())
                    } else { None };
                    byline
                })
            }
            #(if show_teammate_tree {
                Some(element! {
                    TeammateSpinnerTree(
                        tasks: teammate_tasks.clone(),
                        selected_index: selected_index,
                        is_in_selection_mode: is_in_selection_mode,
                        all_idle: all_teammates_idle,
                        viewing_teammate_id: viewing_agent_task_id.clone(),
                        leader_verb: tree_leader_verb.clone(),
                        leader_token_count: Some(estimated_token_count_from_chars(current_response_length)),
                        leader_idle_text: if leader_is_idle { Some("Idle".to_string()) } else { None },
                        show_preview: show_teammate_message_preview,
                    )
                })
            } else { None })
            #(if let Some(budget) = &props.budget_text {
                Some(element! { MessageResponse(content: budget.clone(), color: Some(theme.inactive)) })
            } else { None })
            #(if let Some(next) = &effective_next_task {
                Some(element! { MessageResponse(content: format!("Next: {next}"), color: Some(theme.inactive)) })
            } else if let Some(tip) = &effective_tip {
                Some(element! { MessageResponse(content: format!("Tip: {tip}"), color: Some(theme.inactive)) })
            } else { None })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CC `Spinner()` (Spinner.tsx:578) advances its glyph every 120ms on the
    // shared clock. The startup screens used to mount a static
    // `SpinnerGlyph(frame: 0)` in its place, which never moved; this mounts
    // the real thing with no AppStateProvider, as main.rs's resume screen
    // does, and expects the glyph to change over ~500ms.
    #[component]
    fn PlainSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let done = hooks.use_state(|| false);
        let mut done_for_future = done;
        hooks.use_future(async move {
            futures_timer::Delay::new(Duration::from_millis(500)).await;
            done_for_future.set(true);
        });
        if done.get() {
            system.exit();
        }
        element! {
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: " Resuming conversation…")
            }
        }
    }

    #[test]
    fn plain_spinner_animates_without_an_app_state_provider() {
        use futures::StreamExt;
        let canvases: Vec<_> = futures::executor::block_on(
            element!(PlainSpinnerHarness)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let glyphs: std::collections::BTreeSet<String> = canvases
            .iter()
            .filter_map(|canvas| {
                canvas
                    .to_string()
                    .lines()
                    .next()
                    .and_then(|line| line.trim_start().chars().next())
                    .map(|glyph| glyph.to_string())
            })
            .collect();
        assert!(
            glyphs.len() >= 2,
            "the plain spinner should advance through the official frames: {glyphs:?}"
        );
    }

    #[test]
    fn spinner_row_clock_matches_cc_2_1_280_cadence() {
        // chunk-dgxaxeme.js:841: `Li(t ? null : l === "requesting" && !R ? 50 : 100)`.
        assert_eq!(
            spinner_row_interval(SpinnerMode::Requesting, false, false),
            Some(Duration::from_millis(50))
        );
        for mode in [SpinnerMode::Responding, SpinnerMode::Thinking, SpinnerMode::ToolUse] {
            assert_eq!(
                spinner_row_interval(mode, false, false),
                Some(Duration::from_millis(100)),
                "{mode:?}"
            );
        }
        assert_eq!(
            spinner_row_interval(SpinnerMode::Responding, true, false),
            Some(Duration::from_millis(1_000))
        );
        assert_eq!(spinner_row_interval(SpinnerMode::Requesting, false, true), None);
    }

    fn render_text(element: impl Into<AnyElement<'static>>) -> String {
        let canvas = element.into().render(None);
        canvas.to_string().trim().to_string()
    }

    #[test]
    fn spinner_with_verb_can_render_reduced_motion_main_screen_row() {
        let text = render_text(element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                // SpinnerWithVerb reads settings, effort, spinner_tip, tasks and
                // the teammate-selection fields. Default state is the fixture
                // for every test in this module: they assert on the verb row,
                // timer and token copy, all driven by props.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(message: "Thinking".to_string(), reduced_motion: true)
                    }.into_any()),
                )
            }
        });

        // Cometix Special modification: timer shows immediately (no verbose/30s gate).
        assert!(
            text.contains("● Thinking…"),
            "reduced-motion glyph + verb; canvas=\n{text}"
        );
        assert!(
            text.contains("0s") || text.contains("1s"),
            "immediate wall-elapsed timer on first paint; canvas=\n{text}"
        );
    }

    #[component]
    fn ReducedMotionSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let test_store = hooks.use_const(|| {
            let mut settings = crate::utils::settings::SettingsJson::default();
            settings.prefers_reduced_motion = Some(true);
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = std::sync::Arc::new(settings);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(message: "Thinking".to_string())
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_uses_runtime_reduced_motion_context() {
        let text = render_text(element!(ReducedMotionSpinnerHarness));

        assert!(
            text.contains("● Thinking…"),
            "settings prefers_reduced_motion; canvas=\n{text}"
        );
    }

    #[test]
    fn reduced_motion_keeps_wall_elapsed_timer_via_override() {
        // Anim clock is paused under reduced_motion; elapsed still comes from
        // wall/override so the immediate timer stays correct.
        let text = render_text(element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Waiting".to_string(),
                            reduced_motion: true,
                            response_length: 8_000usize,
                            time_ms_override: Some(5_000u64),
                        )
                    }.into_any()),
                )
            }
        });
        assert!(text.contains("● Waiting…"), "canvas=\n{text}");
        assert!(text.contains("5s"), "canvas=\n{text}");
        assert!(text.contains("tokens"), "canvas=\n{text}");
    }

    #[test]
    fn spinner_verb_candidates_match_official_settings_modes() {
        let mut replace_settings = SettingsJson::default();
        replace_settings.spinner_verbs =
            Some(crate::utils::settings::types::SpinnerVerbsSettings {
                mode: "replace".to_string(),
                verbs: vec!["Reviewing".to_string(), "".to_string()],
            });
        assert_eq!(
            spinner_verb_candidates(Some(&replace_settings)),
            vec!["Reviewing".to_string()]
        );

        let mut empty_replace_settings = SettingsJson::default();
        empty_replace_settings.spinner_verbs =
            Some(crate::utils::settings::types::SpinnerVerbsSettings {
                mode: "replace".to_string(),
                verbs: Vec::new(),
            });
        assert_eq!(
            spinner_verb_candidates(Some(&empty_replace_settings))[0],
            "Accomplishing"
        );

        let mut append_settings = SettingsJson::default();
        append_settings.spinner_verbs = Some(crate::utils::settings::types::SpinnerVerbsSettings {
            mode: "append".to_string(),
            verbs: vec!["Reviewing".to_string()],
        });
        let candidates = spinner_verb_candidates(Some(&append_settings));
        assert_eq!(candidates[0], "Accomplishing");
        assert_eq!(candidates.last().map(String::as_str), Some("Reviewing"));
    }

    #[component]
    fn ConfigSpinnerVerbHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut settings = SettingsJson::default();
        settings.spinner_verbs = Some(crate::utils::settings::types::SpinnerVerbsSettings {
            mode: "replace".to_string(),
            verbs: vec!["Reviewing".to_string()],
        });
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = std::sync::Arc::new(settings);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(reduced_motion: true)
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_reads_readonly_runtime_spinner_verbs_settings() {
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap().clear();
        let text = render_text(element!(ConfigSpinnerVerbHarness));

        // CC SpinnerAnimationRow.tsx:182 appends timerText (formatDuration(0) = "0s",
        // utils/format.ts:41) whenever width allows; tokens part stays hidden at 0.
        assert_eq!(text, "● Reviewing… (0s)");
    }

    fn seed_teammate_tasks(
        initial: &mut crate::state::app_state_store::AppState,
        snapshots: Vec<TeammateTaskSnapshot>,
        show_tree: bool,
    ) {
        for snapshot in snapshots {
            std::sync::Arc::make_mut(&mut initial.tasks).insert(
                snapshot.id.clone(),
                std::sync::Arc::new(crate::state::app_state_store::TaskState::InProcessTeammate(
                    snapshot,
                )),
            );
        }
        if show_tree {
            initial.expanded_view = crate::state::app_state_store::ExpandedView::Teammates;
        }
    }

    #[component]
    fn TeammateSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let snapshots = vec![TeammateTaskSnapshot {
            id: "task-alpha".to_string(),
            task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
            status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
            agent_name: "alpha".to_string(),
            color: Some("purple".to_string()),
            last_activity_description: Some("Reviewing session restore".to_string()),
            tool_use_count: 2,
            token_count: 1_200,
            messages: vec![TeammateMessageSnapshot {
                message_type: "assistant".to_string(),
                blocks: vec![TeammateMessageBlockSnapshot::Text {
                    text: "Checking preview line".to_string(),
                }],
            }],
            ..TeammateTaskSnapshot::default()
        }];
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            seed_teammate_tasks(&mut initial, snapshots, true);
            initial.show_teammate_message_preview = true;
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Thinking".to_string(),
                            reduced_motion: true,
                            response_length: 400usize,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_reads_app_tasks_for_tree() {
        let text = render_text(element!(TeammateSpinnerHarness));

        assert!(text.contains("Thinking…"), "canvas=\n{text}");
        assert!(text.contains("team-lead"), "canvas=\n{text}");
        assert!(text.contains("@alpha"), "canvas=\n{text}");
        assert!(
            text.contains("Reviewing session restore"),
            "canvas=\n{text}"
        );
        assert!(text.contains("2 tool uses"), "canvas=\n{text}");
        assert!(text.contains("1.2k tokens"), "canvas=\n{text}");
        assert!(text.contains("Checking preview line"), "canvas=\n{text}");
    }

    #[component]
    fn TeammateTokenSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let snapshots = vec![TeammateTaskSnapshot {
            id: "task-beta".to_string(),
            task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
            status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
            agent_name: "beta".to_string(),
            token_count: 1_200,
            ..TeammateTaskSnapshot::default()
        }];
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            seed_teammate_tasks(&mut initial, snapshots, false);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Thinking".to_string(),
                            reduced_motion: true,
                            response_length: 400usize,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_counts_runtime_teammate_tokens_without_leader_arrow() {
        let text = render_text(element!(TeammateTokenSpinnerHarness));

        assert!(text.contains("1.3k tokens"), "canvas=\n{text}");
        assert!(!text.contains("↓ 1.3k tokens"), "canvas=\n{text}");
    }

    #[component]
    fn LeaderIdleTeammateSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let snapshots = vec![TeammateTaskSnapshot {
            id: "task-gamma".to_string(),
            task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
            status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
            agent_name: "gamma".to_string(),
            last_activity_description: Some("Checking worker state".to_string()),
            token_count: 900,
            ..TeammateTaskSnapshot::default()
        }];
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            seed_teammate_tasks(&mut initial, snapshots, true);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Thinking".to_string(),
                            reduced_motion: true,
                            response_length: 400usize,
                            leader_is_idle: true,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_shows_static_leader_idle_when_teammates_run() {
        let text = render_text(element!(LeaderIdleTeammateSpinnerHarness));

        assert!(
            text.contains("✻ Idle · teammates running"),
            "canvas=\n{text}"
        );
        assert!(text.contains("team-lead"), "canvas=\n{text}");
        assert!(text.contains("@gamma"), "canvas=\n{text}");
        assert!(text.contains("Checking worker state"), "canvas=\n{text}");
        assert!(!text.contains("Thinking…"), "canvas=\n{text}");
    }

    #[component]
    fn ForegroundedIdleTeammateSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let snapshots = vec![
            TeammateTaskSnapshot {
                id: "task-delta".to_string(),
                task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
                status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
                agent_name: "delta".to_string(),
                is_idle: true,
                idle_text: Some("Idle".to_string()),
                worked_for_ms: Some(125_000),
                token_count: 300,
                ..TeammateTaskSnapshot::default()
            },
            TeammateTaskSnapshot {
                id: "task-epsilon".to_string(),
                task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
                status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
                agent_name: "epsilon".to_string(),
                is_idle: true,
                idle_text: Some("Idle".to_string()),
                token_count: 450,
                ..TeammateTaskSnapshot::default()
            },
        ];
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            seed_teammate_tasks(&mut initial, snapshots, true);
            initial.viewing_agent_task_id = Some("task-delta".to_string());
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Thinking".to_string(),
                            reduced_motion: true,
                            response_length: 400usize,
                            leader_is_idle: true,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_shows_static_foregrounded_idle_teammate() {
        let text = render_text(element!(ForegroundedIdleTeammateSpinnerHarness));

        assert!(text.contains("✻ Worked for 2m 5s"), "canvas=\n{text}");
        assert!(text.contains("team-lead: Idle"), "canvas=\n{text}");
        assert!(text.contains("@delta"), "canvas=\n{text}");
        assert!(text.contains("@epsilon"), "canvas=\n{text}");
        assert!(!text.contains("● Thinking"), "canvas=\n{text}");
    }

    #[component]
    fn ForegroundedActiveTeammateSpinnerHarness(
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let snapshots = vec![
            TeammateTaskSnapshot {
                id: "task-zeta".to_string(),
                task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
                status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
                agent_name: "zeta".to_string(),
                last_activity_description: Some("Hidden foreground activity".to_string()),
                spinner_verb: Some("Auditing teammate work".to_string()),
                token_count: 700,
                ..TeammateTaskSnapshot::default()
            },
            TeammateTaskSnapshot {
                id: "task-eta".to_string(),
                task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
                status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
                agent_name: "eta".to_string(),
                last_activity_description: Some("Checking parallel work".to_string()),
                token_count: 450,
                ..TeammateTaskSnapshot::default()
            },
        ];
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            seed_teammate_tasks(&mut initial, snapshots, true);
            initial.viewing_agent_task_id = Some("task-zeta".to_string());
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            // Maps to CC overrideMessage / leaderVerb (tree shows
                            // this while main spinner uses the foregrounded verb).
                            message: "Planning".to_string(),
                            reduced_motion: true,
                            response_length: 400usize,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_uses_foregrounded_active_teammate_verb() {
        let text = render_text(element!(ForegroundedActiveTeammateSpinnerHarness));

        assert!(text.contains("Auditing teammate work…"), "canvas=\n{text}");
        assert!(text.contains("team-lead: Planning…"), "canvas=\n{text}");
        assert!(text.contains("@zeta"), "canvas=\n{text}");
        assert!(text.contains("@eta"), "canvas=\n{text}");
        assert!(text.contains("Checking parallel work…"), "canvas=\n{text}");
        assert!(text.contains("(esc to interrupt zeta)"), "canvas=\n{text}");
        assert!(!text.contains("Thinking…"), "canvas=\n{text}");
        assert!(!text.contains("(0s"), "canvas=\n{text}");
        assert!(
            !text.contains("Hidden foreground activity…"),
            "foregrounded teammate activity is represented by the main spinner verb; canvas=\n{text}"
        );
    }

    #[component]
    fn ForegroundedActiveTeammateWithoutVerbSpinnerHarness(
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let snapshots = vec![TeammateTaskSnapshot {
            id: "task-theta".to_string(),
            task_type: teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
            status: teammate_tree::RUNNING_TASK_STATUS.to_string(),
            agent_name: "theta".to_string(),
            last_activity_description: Some("Activity belongs in the tree".to_string()),
            token_count: 300,
            ..TeammateTaskSnapshot::default()
        }];
        let test_store = hooks.use_const(move || {
            let mut settings = SettingsJson::default();
            settings.spinner_verbs = Some(crate::utils::settings::types::SpinnerVerbsSettings {
                mode: "replace".to_string(),
                verbs: vec!["Reviewing".to_string()],
            });
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = std::sync::Arc::new(settings);
            seed_teammate_tasks(&mut initial, snapshots, true);
            initial.viewing_agent_task_id = Some("task-theta".to_string());
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Leader fallback".to_string(),
                            reduced_motion: true,
                            response_length: 400usize,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_does_not_use_foregrounded_activity_as_main_verb() {
        let text = render_text(element!(
            ForegroundedActiveTeammateWithoutVerbSpinnerHarness
        ));

        assert!(text.contains("● Reviewing…"), "canvas=\n{text}");
        assert!(text.contains("@theta"), "canvas=\n{text}");
        assert!(
            !text.contains("Activity belongs in the tree…"),
            "official foregrounded teammate fallback uses spinnerVerb/random verb, not last activity; canvas=\n{text}"
        );
    }

    #[test]
    fn spinner_with_verb_prefers_current_task_active_form_and_shows_next_task() {
        let tasks = vec![
            crate::utils::tasks::TaskRecord {
                id: "1".to_string(),
                subject: "Blocked pending".to_string(),
                description: String::new(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: vec!["2".to_string()],
                metadata: None,
            },
            crate::utils::tasks::TaskRecord {
                id: "2".to_string(),
                subject: "Implement core".to_string(),
                description: String::new(),
                active_form: Some("Implementing core".to_string()),
                status: "in_progress".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            },
            crate::utils::tasks::TaskRecord {
                id: "3".to_string(),
                subject: "Write docs".to_string(),
                description: String::new(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            },
        ];

        let snapshot = spinner_tasks_snapshot_from_tasks(&tasks);

        assert_eq!(snapshot.current_verb.as_deref(), Some("Implementing core"));
        assert_eq!(snapshot.next_pending_subject.as_deref(), Some("Write docs"));
    }

    #[component]
    fn EffortSuffixSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let test_store = hooks.use_const(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.effort_value = Some(crate::utils::effort::EffortValue::Named(
                "medium".to_string(),
            ));
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            mode: SpinnerMode::Thinking,
                            message: "Thinking".to_string(),
                            reduced_motion: true,
                            time_ms_override: Some(3_100u64),
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_appends_runtime_effort_suffix_to_active_thinking() {
        let text = render_text(element!(EffortSuffixSpinnerHarness));

        assert!(
            text.contains("thinking with medium effort"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn spinner_status_line_can_show_timer_tokens_suffix_tip_and_budget() {
        let text = render_text(element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Working".to_string(),
                            response_length: 8_000usize,
                            time_ms_override: Some(1_500u64),
                            spinner_suffix: Some("esc to interrupt".to_string()),
                            spinner_tip: Some("Use /clear to start fresh".to_string()),
                            budget_text: Some("Target: 2.0k / 4.0k (50%)".to_string()),
                        )
                    }.into_any()),
                )
            }
        });

        assert!(text.contains("Working…"), "canvas=\n{text}");
        assert!(text.contains("esc to interrupt"), "canvas=\n{text}");
        assert!(text.contains("↓ 2.0k tokens"), "canvas=\n{text}");
        assert!(
            text.contains("Target: 2.0k / 4.0k (50%)"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Tip: Use /clear to start fresh"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn spinner_shows_timer_and_tokens_immediately_without_official_gate() {
        // Cometix Special modification: no verbose / 30s gate.
        let with_tokens = render_text(element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Waiting".to_string(),
                            response_length: 8_000usize,
                            time_ms_override: Some(1_500u64),
                            reduced_motion: true,
                        )
                    }.into_any()),
                )
            }
        });
        assert!(with_tokens.contains("Waiting…"), "canvas=\n{with_tokens}");
        assert!(with_tokens.contains("1s"), "canvas=\n{with_tokens}");
        assert!(
            with_tokens.contains("tokens"),
            "token counter should show immediately; canvas=\n{with_tokens}"
        );

        let no_tokens_yet = render_text(element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(
                            message: "Waiting".to_string(),
                            response_length: 0usize,
                            time_ms_override: Some(1_500u64),
                            reduced_motion: true,
                        )
                    }.into_any()),
                )
            }
        });
        assert!(no_tokens_yet.contains("1s"), "canvas=\n{no_tokens_yet}");
        assert!(
            !no_tokens_yet.contains("tokens"),
            "token counter stays hidden while count is 0; canvas=\n{no_tokens_yet}"
        );
    }

    #[test]
    fn effective_spinner_tip_matches_official_time_based_tip_order() {
        assert_eq!(
            effective_spinner_tip(30_001, Some("Custom tip"), false, true, 0).as_deref(),
            Some(
                "Use /btw to ask a quick side question without interrupting Claude's current work"
            )
        );
        assert_eq!(
            effective_spinner_tip(1_800_001, Some("Custom tip"), false, true, 0).as_deref(),
            Some("Use /clear to start fresh when switching topics and free up context")
        );
        assert_eq!(
            effective_spinner_tip(1_800_001, Some("Custom tip"), false, false, 0).as_deref(),
            Some("Custom tip")
        );
        assert_eq!(
            effective_spinner_tip(30_001, Some("Custom tip"), true, true, 0),
            None
        );
        assert_eq!(
            effective_spinner_tip(30_001, Some("Custom tip"), false, true, 1).as_deref(),
            Some("Custom tip")
        );
    }

    #[test]
    fn spinner_tip_fallback_reads_readonly_spinner_tips_override() {
        let mut settings = SettingsJson::default();
        settings.spinner_tips_override = Some(crate::utils::settings::types::SpinnerTipsOverride {
            exclude_default: None,
            tips: vec!["Custom operator tip".to_string()],
        });
        assert_eq!(
            spinner_tip_fallback_from_settings(Some(&settings), None, None, true).as_deref(),
            Some("Custom operator tip")
        );
        assert_eq!(
            spinner_tip_fallback_from_settings(Some(&settings), None, Some("Built-in tip"), true)
                .as_deref(),
            Some("Built-in tip")
        );
        assert_eq!(
            spinner_tip_fallback_from_settings(Some(&settings), None, None, false),
            None
        );

        settings
            .spinner_tips_override
            .as_mut()
            .expect("override")
            .exclude_default = Some(true);
        assert_eq!(
            spinner_tip_fallback_from_settings(Some(&settings), None, Some("Built-in tip"), true)
                .as_deref(),
            Some("Custom operator tip")
        );

        settings.spinner_tips_override = Some(crate::utils::settings::types::SpinnerTipsOverride {
            exclude_default: Some(true),
            tips: vec![
                "Recently shown".to_string(),
                "Oldest custom tip".to_string(),
            ],
        });
        let mut global_config = GlobalConfig::default();
        global_config.num_startups = 10;
        global_config.tips_history = Some(std::collections::HashMap::from([
            ("custom-tip-0".to_string(), 9),
            ("custom-tip-1".to_string(), 2),
        ]));
        assert_eq!(
            spinner_tip_fallback_from_settings(
                Some(&settings),
                Some(&global_config),
                Some("Built-in tip"),
                true,
            )
            .as_deref(),
            Some("Oldest custom tip")
        );

        global_config.tips_history = Some(std::collections::HashMap::from([
            ("custom-tip-0".to_string(), 0),
            ("custom-tip-1".to_string(), 9),
        ]));
        assert_eq!(
            spinner_tip_fallback_from_settings(
                Some(&settings),
                Some(&global_config),
                Some("Built-in tip"),
                true,
            )
            .as_deref(),
            Some("Recently shown")
        );
    }

    #[component]
    fn ConfigSpinnerTipHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut settings = SettingsJson::default();
        settings.spinner_tips_override = Some(crate::utils::settings::types::SpinnerTipsOverride {
            exclude_default: Some(true),
            tips: vec!["Custom operator tip".to_string()],
        });
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = std::sync::Arc::new(settings);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        SpinnerWithVerb(message: "Working".to_string(), reduced_motion: true)
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn spinner_with_verb_reads_readonly_runtime_spinner_tips_override() {
        let text = render_text(element!(ConfigSpinnerTipHarness));

        assert!(text.contains("Working…"), "canvas=\n{text}");
        assert!(text.contains("Tip: Custom operator tip"), "canvas=\n{text}");
    }

    #[test]
    fn thinking_status_tracks_official_minimum_and_duration_windows() {
        let thinking =
            next_thinking_status_state(ThinkingStatusState::Idle, SpinnerMode::Thinking, 100);
        assert_eq!(thinking_status_text(thinking).as_deref(), Some("thinking"));

        let holding = next_thinking_status_state(thinking, SpinnerMode::Responding, 1_000);
        assert_eq!(thinking_status_text(holding).as_deref(), Some("thinking"));

        let thought = next_thinking_status_state(holding, SpinnerMode::Responding, 2_100);
        assert_eq!(
            thinking_status_text(thought).as_deref(),
            Some("thought for 1s")
        );

        let cleared = next_thinking_status_state(thought, SpinnerMode::Responding, 4_200);
        assert_eq!(thinking_status_text(cleared), None);
    }

    #[test]
    fn token_indicator_uses_official_round_response_length_div_four() {
        assert_eq!(estimated_token_count_from_chars(0), 0);
        assert_eq!(estimated_token_count_from_chars(1), 0);
        assert_eq!(estimated_token_count_from_chars(2), 1);
        assert_eq!(estimated_token_count_from_chars(6), 2);
    }

    #[test]
    fn token_counter_animation_matches_official_increment_boundaries() {
        assert_eq!(next_displayed_response_length(100, 105, false), 103);
        assert_eq!(next_displayed_response_length(100, 250, false), 123);
        assert_eq!(next_displayed_response_length(100, 500, false), 150);
        assert_eq!(next_displayed_response_length(100, 500, true), 500);
        assert_eq!(next_displayed_response_length(500, 100, false), 100);
    }

    #[test]
    fn stalled_intensity_uses_theme_error_target_without_hardcoded_color_constant() {
        let state = StalledState {
            mount_time_ms: 1,
            last_token_time_ms: 1,
            last_active_tool_time_ms: 0,
            last_response_length: 10,
        };

        assert_eq!(compute_stalled_intensity(state, 3_000, 10, false), 0.0);
        assert_eq!(compute_stalled_intensity(state, 6_001, 10, false), 1.0);
        assert_eq!(compute_stalled_intensity(state, 6_001, 10, true), 0.0);
    }

    #[test]
    fn leader_idle_teammate_turn_resets_stalled_fade_like_official_spinner() {
        let state = StalledState {
            mount_time_ms: 1,
            last_token_time_ms: 1,
            last_active_tool_time_ms: 0,
            last_response_length: 10,
        };
        let has_stall_reset_activity = spinner_stall_reset_activity(false, true);

        assert!(has_stall_reset_activity);
        assert_eq!(
            compute_stalled_intensity(state, 6_001, 10, has_stall_reset_activity),
            0.0
        );
    }
}
