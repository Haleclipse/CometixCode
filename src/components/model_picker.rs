//! Maps to: CC `components/ModelPicker.tsx`:54-363.
//!
//! @nyan: the picker lists configured model profiles only (design §5.1): the
//! session's current profile first, the configured default (`selection.main`)
//! second, then the rest by name. ←/→ adjusts effort, Enter switches this
//! session's profile, Ctrl+S makes the focused profile the default. With no
//! profile configured it only shows a configuration hint. CC's model aliases,
//! "no preference" row and Fast mode notices are gone.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::pane::Pane;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybindings};
use crate::state::store::AppStore;
use crate::utils::effort::Effort;
use crate::utils::settings::nyan::NyanSettings;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub(crate) const MODEL_PICKER_VISIBLE_COUNT: usize = 10;
pub(crate) const MODEL_PICKER_HEADER_TEXT: &str = "Switch between model profiles. Enter applies to this session; Ctrl+S makes it the default for future sessions.";
pub(crate) const NO_PROFILES_HINT: &str = "No model profiles configured. Add profiles under settings.nyan.models and pick the default with settings.nyan.selection.main.";

/// One picker row: a configured profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileRow {
    pub name: String,
    /// The provider model id, or empty when the profile does not resolve.
    pub model: String,
    /// The profile's configured effort; `None` is the model default.
    pub effort: Option<Effort>,
}

/// Profiles in picker order: `current` first, `default` second, then the rest
/// by name. Either may be absent or name a profile that is not configured.
pub(crate) fn profile_rows(
    nyan: Option<&NyanSettings>,
    current: Option<&str>,
    default: Option<&str>,
) -> Vec<ProfileRow> {
    let mut names = crate::utils::model::profile::profile_names(nyan).collect::<Vec<_>>();
    names.sort_unstable();
    for leading in [default, current].into_iter().flatten() {
        if let Some(index) = names.iter().position(|name| *name == leading) {
            let name = names.remove(index);
            names.insert(0, name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let settings =
                nyan.and_then(|nyan| crate::utils::model::profile::model_profile(nyan, name));
            ProfileRow {
                name: name.to_string(),
                model: nyan
                    .and_then(|nyan| crate::utils::model::profile::resolve_profile(nyan, name).ok())
                    .map(|profile| profile.model)
                    .unwrap_or_default(),
                effort: settings.and_then(|profile| profile.effort),
            }
        })
        .collect()
}

fn row_option(row: &ProfileRow, default: Option<&str>) -> SelectOptionData {
    let mut description = row.model.clone();
    if default == Some(row.name.as_str()) {
        description.push_str(if description.is_empty() {
            "default"
        } else {
            " · default"
        });
    }
    SelectOptionData {
        label: row.name.clone(),
        value: row.name.clone(),
        description: Some(description),
        dim_description: true,
        disabled: false,
        input: None,
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

/// Maps to: CC `components/ModelPicker.tsx` `cycleEffortLevel`.
///
/// Cycles through the four unified effort levels: `off < medium < high < max`.
/// The model default (`None`) starts the cycle from medium.
pub(crate) fn cycle_effort(current: Option<Effort>, direction: KeyCode) -> Option<Effort> {
    let levels = Effort::ALL;
    let index = levels
        .iter()
        .position(|level| Some(*level) == current)
        .unwrap_or(1);
    match direction {
        KeyCode::Right => Some(levels[(index + 1) % levels.len()]),
        KeyCode::Left => Some(levels[(index + levels.len() - 1) % levels.len()]),
        _ => current,
    }
}

/// The effort a row starts at: the session's for the current profile, else
/// the profile's own (design §5.2: a switch never carries the old effort).
fn initial_row_effort(
    row: &ProfileRow,
    current: Option<&str>,
    session: Option<Effort>,
) -> Option<Effort> {
    if current == Some(row.name.as_str()) {
        session.or(row.effort)
    } else {
        row.effort
    }
}

/// Switch the session to `profile` at `effort` (Enter). `effort` is `None`
/// when the user did not adjust it: the profile's own effort applies.
pub(crate) fn apply_session_profile(
    state: &mut crate::state::app_state_store::AppState,
    profile: &str,
    effort: Option<Effort>,
) {
    state.main_loop_profile = switch_session_profile(profile, effort);
    state.effort_value = effort;
}

/// The one session switch every path shares (picker Enter, `/model`, headless
/// `set_model`): moves the session to `profile`, stops the warmer and records
/// the switch. Returns the stored profile — `None` when `profile` is
/// `selection.main`, so a session back on the default keeps following it.
///
/// The caller sets the session effort to `effort` (design §5.2): `None` is the
/// profile's own effort, resolved at request time, never the previous one.
pub(crate) fn switch_session_profile(profile: &str, effort: Option<Effort>) -> Option<String> {
    let main = crate::utils::settings::get_initial_settings()
        .nyan
        .as_deref()
        .and_then(|nyan| nyan.selection.as_ref())
        .and_then(|selection| selection.main.clone());
    let stored = (main.as_deref() != Some(profile)).then(|| profile.to_string());
    crate::bootstrap::state::set_session_profile(stored.clone());
    // @nyan (compaction C12): prompt cache is per-model, so a switch must stop
    // a warmer that is still refreshing the previous model's prefix.
    crate::services::cache_warmer::invalidate();
    let _ = crate::utils::session_storage::save_session_profile(stored.as_deref(), effort);
    stored
}

/// What Ctrl+S reports: the new default, and the project override that still
/// wins in this directory, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetDefaultOutcome {
    pub profile: String,
    pub project_main: Option<String>,
}

/// Make `profile` the default (Ctrl+S): user settings `nyan.selection.main`
/// and that profile's `effort` (`None` removes it, the model default).
pub(crate) fn set_default_profile(
    profile: &str,
    effort: Option<Effort>,
) -> anyhow::Result<SetDefaultOutcome> {
    use crate::utils::settings::SettingSource;
    let updates = serde_json::Map::from_iter([(
        "nyan".to_string(),
        serde_json::json!({
            "selection": {"main": profile},
            "models": {profile: {"effort": effort.map(Effort::as_str)}},
        }),
    )]);
    crate::utils::settings::update_settings_for_source(SettingSource::User, &updates)?;
    let project_main = [SettingSource::Local, SettingSource::Project]
        .into_iter()
        .find_map(|source| {
            crate::utils::settings::get_settings_for_source(source)?
                .nyan?
                .selection?
                .main
        });
    Ok(SetDefaultOutcome {
        profile: profile.to_string(),
        project_main,
    })
}

pub(crate) fn set_default_message(outcome: &SetDefaultOutcome) -> String {
    match outcome.project_main.as_deref() {
        Some(project) if project != outcome.profile => format!(
            "Default model set to {}. This project still uses {project}.",
            outcome.profile
        ),
        _ => format!("Default model set to {}", outcome.profile),
    }
}

/// Maps to: CC `ModelPicker`'s effort `useKeybindings` registration, plus the
/// nyan `modelPicker:setDefault` action.
fn use_model_picker_keybindings<Decrease, Increase, SetDefault>(
    hooks: &mut Hooks,
    is_active: bool,
    mut on_decrease: Decrease,
    mut on_increase: Increase,
    mut on_set_default: SetDefault,
) where
    Decrease: FnMut() + Send + 'static,
    Increase: FnMut() + Send + 'static,
    SetDefault: FnMut() + Send + 'static,
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
        (
            "modelPicker:setDefault".to_string(),
            Box::new(move || {
                on_set_default();
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

/// What the picker hands back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModelPickerResult {
    /// Enter: the session now runs on `profile`.
    Session {
        profile: String,
        effort: Option<Effort>,
    },
    /// Ctrl+S: `profile` is the configured default.
    Default(Result<SetDefaultOutcome, String>),
}

impl ModelPickerResult {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Session { profile, effort } => {
                let mut message = format!("Set model to {profile}");
                if let Some(effort) = effort {
                    message.push_str(&crate::utils::effort::get_effort_suffix("", Some(effort)));
                }
                message
            }
            Self::Default(Ok(outcome)) => set_default_message(outcome),
            Self::Default(Err(error)) => format!("Failed to save the default model: {error}"),
        }
    }
}

#[derive(Default, Props)]
pub(crate) struct ModelPickerProps<'a> {
    pub is_standalone_command: bool,
    pub on_select: HandlerMut<'a, ModelPickerResult>,
    pub on_cancel: HandlerMut<'a, ()>,
}

struct ModelPickerView {
    options: Vec<SelectOptionData>,
    focused_index: usize,
    selected_value: Option<String>,
    effort: Option<Effort>,
    is_standalone_command: bool,
}

fn render_model_picker_content(props: &ModelPickerView, theme: Theme) -> AnyElement<'static> {
    let count = props.options.len();
    if count == 0 {
        return element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "Select model".to_string(), color: theme.remember, weight: Weight::Bold)
                Text(content: NO_PROFILES_HINT.to_string(), dim: true)
                #(props.is_standalone_command.then(|| element! {
                    Text(content: "Esc to exit".to_string(), dim: true, italic: true)
                }))
            }
        }
        .into_any();
    }
    let focused_index = props.focused_index.min(count - 1);
    let visible_option_count = count.min(MODEL_PICKER_VISIBLE_COUNT);
    let visible_from = visible_from_index(focused_index, count, visible_option_count);
    let hidden_count = count.saturating_sub(visible_option_count);
    let (symbol, effort_label) = match props.effort {
        Some(effort) => (effort.symbol(), format!(" {} effort", effort.title_label())),
        None => (Effort::Medium.symbol(), " Model default effort".to_string()),
    };

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Column) {
                View(margin_bottom: 1u32, flex_direction: FlexDirection::Column) {
                    Text(content: "Select model".to_string(), color: theme.remember, weight: Weight::Bold)
                    Text(content: MODEL_PICKER_HEADER_TEXT.to_string(), dim: true)
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
                    #((hidden_count > 0).then(|| element! {
                        View(padding_left: 3u32) {
                            Text(content: format!("and {hidden_count} more…"), dim: true)
                        }
                    }))
                }
                View(margin_bottom: 1u32, flex_direction: FlexDirection::Row) {
                    Text(content: symbol.to_string(), color: theme.claude, wrap: TextWrap::NoWrap)
                    Text(content: format!("{effort_label} "), dim: true, wrap: TextWrap::NoWrap)
                    Text(content: "← → to adjust".to_string(), color: theme.subtle, wrap: TextWrap::NoWrap)
                }
            }
            #(props.is_standalone_command.then(|| element! {
                Text(content: "Enter to confirm · Ctrl+S to set as default · Esc to exit".to_string(), dim: true, italic: true)
            }))
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
    let app_store = hooks
        .try_use_context::<AppStore>()
        .map(|store| store.clone());
    let settings = crate::utils::settings::get_initial_settings();
    let nyan = settings.nyan.as_deref();
    let default = nyan
        .and_then(|nyan| nyan.selection.as_ref())
        .and_then(|selection| selection.main.clone());
    let snapshot = app_store.as_ref().map(|store| store.get());
    let current = snapshot
        .as_ref()
        .and_then(|state| state.main_loop_profile.clone())
        .or_else(|| default.clone());
    let session_effort = snapshot.as_ref().and_then(|state| state.effort_value);
    let rows = profile_rows(nyan, current.as_deref(), default.as_deref());
    let options = rows
        .iter()
        .map(|row| row_option(row, default.as_deref()))
        .collect::<Vec<_>>();
    let row_count = rows.len();
    let visible_count = MODEL_PICKER_VISIBLE_COUNT.min(row_count.max(1));

    let mut focused_index = hooks.use_state(|| 0usize);
    let initial_effort = rows
        .first()
        .and_then(|row| initial_row_effort(row, current.as_deref(), session_effort));
    let mut effort = hooks.use_state(move || initial_effort);
    let mut has_toggled_effort = hooks.use_state(|| false);
    let mut should_close = hooks.use_state(|| false);
    let mut pending = hooks.use_state(|| Option::<ModelPickerResult>::None);

    let row_effort = {
        let rows = rows.clone();
        let current = current.clone();
        move |index: usize| {
            rows.get(index)
                .and_then(|row| initial_row_effort(row, current.as_deref(), session_effort))
        }
    };

    use_model_picker_keybindings(
        &mut hooks,
        row_count > 0,
        move || {
            effort.set(cycle_effort(effort.get(), KeyCode::Left));
            has_toggled_effort.set(true);
        },
        move || {
            effort.set(cycle_effort(effort.get(), KeyCode::Right));
            has_toggled_effort.set(true);
        },
        {
            let rows = rows.clone();
            move || {
                if let Some(row) = rows.get(focused_index.get()) {
                    let effort = if has_toggled_effort.get() {
                        effort.get()
                    } else {
                        row.effort
                    };
                    pending.set(Some(ModelPickerResult::Default(
                        set_default_profile(&row.name, effort).map_err(|error| error.to_string()),
                    )));
                }
            }
        },
    );

    let event_rows = rows.clone();
    let event_current = current.clone();
    hooks.use_propagated_terminal_events(move |event| {
        let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
            return;
        };
        if *kind == KeyEventKind::Release {
            return;
        }
        let focused = focused_index.get().min(row_count.saturating_sub(1));
        let next = match code {
            KeyCode::Esc => {
                should_close.set(true);
                None
            }
            KeyCode::Down => Some(next_index(focused, row_count)),
            KeyCode::Up => Some(previous_index(focused, row_count)),
            KeyCode::PageDown => Some(next_page_index(focused, row_count, visible_count)),
            KeyCode::PageUp => Some(previous_page_index(focused, visible_count)),
            KeyCode::Enter => {
                if let Some(row) = event_rows.get(focused) {
                    // An untouched effort is the profile's own (or, staying
                    // on the current profile, the session's).
                    let picked = if has_toggled_effort.get() {
                        effort.get()
                    } else if event_current.as_deref() == Some(row.name.as_str()) {
                        session_effort
                    } else {
                        None
                    };
                    if let Some(store) = app_store.as_ref() {
                        store.replace_with(|state| apply_session_profile(state, &row.name, picked));
                    }
                    pending.set(Some(ModelPickerResult::Session {
                        profile: row.name.clone(),
                        effort: picked,
                    }));
                }
                None
            }
            _ => None,
        };
        if let Some(next) = next {
            focused_index.set(next);
            if !has_toggled_effort.get() {
                effort.set(row_effort(next));
            }
        }
        event.stop_propagation();
    });

    if should_close.get() {
        should_close.set(false);
        (props.on_cancel)(());
    }
    let result = { pending.read().clone() };
    if let Some(result) = result {
        pending.set(None);
        (props.on_select)(result);
    }

    let content = render_model_picker_content(
        &ModelPickerView {
            options,
            focused_index: focused_index.get(),
            selected_value: current,
            effort: effort.get(),
            is_standalone_command: props.is_standalone_command,
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
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn nyan(json: serde_json::Value) -> NyanSettings {
        serde_json::from_value(json).unwrap()
    }

    fn three_profiles() -> serde_json::Value {
        serde_json::json!({
            "providers": {"p": {"kind": "anthropic", "protocol": "anthropicMessages"}},
            "models": {
                "zeta": {"provider": "p", "model": "claude-haiku-4-5",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}},
                "main": {"provider": "p", "model": "claude-sonnet-4-6", "effort": "high",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}},
                "alpha": {"provider": "p", "model": "claude-opus-4-6", "effort": "max",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}},
                "beta": {"provider": "p", "model": "claude-opus-4-6",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}}
            },
            "selection": {"main": "main"}
        })
    }

    #[test]
    fn rows_put_the_current_profile_first_and_the_default_second() {
        let nyan = nyan(three_profiles());
        let names = |current: Option<&str>| {
            profile_rows(Some(&nyan), current, Some("main"))
                .into_iter()
                .map(|row| row.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(Some("zeta")), ["zeta", "main", "alpha", "beta"]);
        // On the default: it is first, the rest follow by name.
        assert_eq!(names(Some("main")), ["main", "alpha", "beta", "zeta"]);
        // A current profile that is gone does not reorder anything else.
        assert_eq!(names(Some("gone")), ["main", "alpha", "beta", "zeta"]);
        let rows = profile_rows(Some(&nyan), None, Some("main"));
        assert_eq!(rows[0].model, "claude-sonnet-4-6");
        assert_eq!(rows[0].effort, Some(Effort::High));
        assert!(profile_rows(None, None, None).is_empty());
    }

    #[test]
    fn effort_cycles_through_the_four_tiers_from_the_model_default() {
        assert_eq!(cycle_effort(None, KeyCode::Right), Some(Effort::High));
        assert_eq!(cycle_effort(None, KeyCode::Left), Some(Effort::Off));
        assert_eq!(
            cycle_effort(Some(Effort::Max), KeyCode::Right),
            Some(Effort::Off)
        );
        assert_eq!(
            cycle_effort(Some(Effort::Off), KeyCode::Left),
            Some(Effort::Max)
        );
    }

    #[test]
    fn a_row_starts_at_its_own_effort_and_only_the_current_one_keeps_the_session_effort() {
        let row = |name: &str, effort| ProfileRow {
            name: name.to_string(),
            model: String::new(),
            effort,
        };
        assert_eq!(
            initial_row_effort(
                &row("main", Some(Effort::High)),
                Some("main"),
                Some(Effort::Off)
            ),
            Some(Effort::Off)
        );
        assert_eq!(
            initial_row_effort(
                &row("alpha", Some(Effort::Max)),
                Some("main"),
                Some(Effort::Off)
            ),
            Some(Effort::Max)
        );
        assert_eq!(
            initial_row_effort(&row("beta", None), Some("main"), Some(Effort::Off)),
            None
        );
    }

    #[test]
    fn the_session_switch_routes_through_the_profile_and_follows_main_when_back_on_it() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::settings::settings_cache::install_test_settings(
            serde_json::from_value(serde_json::json!({"nyan": three_profiles()})).unwrap(),
        );
        let dir = std::env::temp_dir().join(format!("nyan-picker-switch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _write = EnvVarGuard::set("NYAN_WRITE_ENABLED", "1");
        let original_session = crate::bootstrap::state::get_session_id();
        crate::bootstrap::state::switch_session("picker-switch", Some(dir.clone()));
        let mut state = crate::state::app_state_store::AppState::default();
        apply_session_profile(&mut state, "alpha", None);
        assert_eq!(state.main_loop_profile.as_deref(), Some("alpha"));
        assert_eq!(state.effort_value, None);
        apply_session_profile(&mut state, "main", Some(Effort::Off));
        assert_eq!(state.main_loop_profile, None);
        assert_eq!(state.effort_value, Some(Effort::Off));
        // The default is recorded as `null` too, so resume keeps following it.
        let events = std::fs::read_to_string(dir.join("picker-switch.jsonl")).unwrap();
        let profiles: Vec<serde_json::Value> = events
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["profile"].clone())
            .collect();
        assert_eq!(
            profiles,
            [serde_json::json!("alpha"), serde_json::Value::Null]
        );
        crate::bootstrap::state::switch_session(original_session, None);
        crate::bootstrap::state::set_session_profile(None);
        let _ = std::fs::remove_dir_all(dir);
        crate::utils::settings::settings_cache::reset_settings_cache();
    }

    #[test]
    fn the_set_default_message_names_a_project_override() {
        let outcome = |project: Option<&str>| SetDefaultOutcome {
            profile: "alpha".to_string(),
            project_main: project.map(str::to_string),
        };
        assert_eq!(
            set_default_message(&outcome(None)),
            "Default model set to alpha"
        );
        assert_eq!(
            set_default_message(&outcome(Some("alpha"))),
            "Default model set to alpha"
        );
        assert_eq!(
            set_default_message(&outcome(Some("beta"))),
            "Default model set to alpha. This project still uses beta."
        );
    }
}
