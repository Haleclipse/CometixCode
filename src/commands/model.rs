//! `/model` local command UI.
//! Maps to official `src/commands/model/model.tsx`:30-337.
//!
//! @nyan: `/model` takes a model profile ref (`profile`, `@main`, `@light`,
//! `@heavy`, optionally `:effort`) and switches this session to it. An
//! unknown ref is an error and changes nothing. With no argument it opens the
//! profile picker ([`ModelPicker`]).

use crate::components::model_picker::{ModelPicker, ModelPickerResult, apply_session_profile};
use crate::state::store::AppStore;
use crate::utils::model::model::{NO_MAIN_PROFILE_PROMPT, get_main_loop_model, render_model_name};
use crate::utils::model::profile::ModelChoice;
use iocraft::prelude::*;

/// Maps to: CC `commands/model/index.ts:8-10` `get description()`.
pub fn description() -> String {
    format!(
        "Set the AI model for nyan (currently {})",
        render_model_name(&get_main_loop_model())
    )
}

/// The session's profile: the one it switched to, else `selection.main`.
fn current_profile_label() -> String {
    crate::bootstrap::state::get_session_profile()
        .or_else(|| {
            crate::utils::settings::get_initial_settings()
                .nyan
                .as_deref()
                .and_then(|nyan| nyan.selection.as_ref())
                .and_then(|selection| selection.main.clone())
        })
        .map(|profile| {
            let model = get_main_loop_model();
            if model.is_empty() {
                profile
            } else {
                format!("{profile} ({model})")
            }
        })
        .unwrap_or_else(|| NO_MAIN_PROFILE_PROMPT.to_string())
}

/// Maps to: CC `ModelPickerWrapper` `handleCancel`.
pub fn handle_cancel_output() -> String {
    match crate::bootstrap::state::get_session_profile() {
        None if !crate::utils::model::model::has_main_profile() => {
            NO_MAIN_PROFILE_PROMPT.to_string()
        }
        _ => format!("Kept model as {}", current_profile_label()),
    }
}

/// What `/model <args>` does without opening the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelArgs {
    /// Print this text; nothing changes.
    Message(String),
    /// Switch the session to this profile (`None` is `selection.main`).
    Switch {
        profile: Option<String>,
        effort: Option<crate::utils::effort::Effort>,
    },
}

pub fn parse_args(args: &str) -> Option<ModelArgs> {
    let args = args.trim();
    if matches!(args, "help" | "-h" | "--help") {
        return Some(ModelArgs::Message(
            "Run /model to open the model selection menu, or /model <profile>[:effort] to switch this session to a configured profile (@main, @light and @heavy name the configured tiers)."
                .to_string(),
        ));
    }
    if matches!(
        args,
        "list"
            | "show"
            | "display"
            | "current"
            | "view"
            | "get"
            | "check"
            | "describe"
            | "print"
            | "version"
            | "about"
            | "status"
            | "?"
    ) {
        return Some(ModelArgs::Message(format!(
            "Current model: {}",
            current_profile_label()
        )));
    }
    if args.is_empty() {
        return None;
    }
    let settings = crate::utils::settings::get_initial_settings();
    let nyan = settings.nyan.as_deref();
    Some(
        match crate::utils::model::profile::resolve_model_ref(nyan, args) {
            Ok((ModelChoice::Profile(profile), effort)) => ModelArgs::Switch {
                profile: Some(profile),
                effort,
            },
            // `inherit` and a tier with nothing configured are the session
            // itself: only an explicit `:effort` changes anything.
            Ok((ModelChoice::Inherit, effort)) => ModelArgs::Switch {
                profile: crate::bootstrap::state::get_session_profile(),
                effort,
            },
            Err(error) => ModelArgs::Message(error.to_string()),
        },
    )
}

/// Apply a [`ModelArgs::Switch`] to the session and return the message.
///
/// Effort follows design §5.2: an explicit `:effort`, else the new profile's
/// configured effort, else the model default — never the previous session's.
pub fn apply_switch(
    store: Option<&AppStore>,
    profile: Option<String>,
    effort: Option<crate::utils::effort::Effort>,
) -> String {
    let settings = crate::utils::settings::get_initial_settings();
    let Some(profile) = profile.or_else(|| {
        settings
            .nyan
            .as_deref()
            .and_then(|nyan| nyan.selection.as_ref())
            .and_then(|selection| selection.main.clone())
    }) else {
        return NO_MAIN_PROFILE_PROMPT.to_string();
    };
    match store {
        Some(store) => store.replace_with(|state| apply_session_profile(state, &profile, effort)),
        // No store: the effort rides the next request through the recorded
        // switch; the profile's own effort applies when `effort` is `None`.
        None => {
            crate::components::model_picker::switch_session_profile(&profile, effort);
        }
    }
    ModelPickerResult::Session { profile, effort }.message()
}

#[derive(Default, Props)]
pub struct ModelPickerWrapperProps<'a> {
    pub on_close: HandlerMut<'a, ()>,
    pub on_select: HandlerMut<'a, String>,
}

#[component]
pub fn ModelPickerWrapper<'a>(
    props: &mut ModelPickerWrapperProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut pending_output = hooks.use_state(|| Option::<String>::None);

    let selected = { pending_output.read().clone() };
    if let Some(output) = selected {
        pending_output.set(None);
        (props.on_select)(output);
    }

    element! {
        ModelPicker(
            is_standalone_command: true,
            on_select: move |result: ModelPickerResult| {
                pending_output.set(Some(result.message()));
            },
            on_cancel: move |_| {
                pending_output.set(Some(handle_cancel_output()));
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::effort::Effort;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn ctrl(c: char) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(c));
        event.modifiers = KeyModifiers::CONTROL;
        TerminalEvent::Key(event)
    }

    const PROFILES: &str = r#"{
        "nyan": {
            "providers": {"p": {"kind": "anthropic", "protocol": "anthropicMessages"}},
            "models": {
                "main": {"provider": "p", "model": "claude-sonnet-4-6", "effort": "high",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}},
                "deep": {"provider": "p", "model": "claude-opus-4-6", "effort": "max",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}},
                "qwen3:32b": {"provider": "p", "model": "qwen3:32b",
                    "capabilities": {"contextWindow": 32000, "maxOutputTokens": 8000}}
            },
            "selection": {"main": "main", "light": "qwen3:32b"}
        }
    }"#;

    /// A config dir whose user settings hold [`PROFILES`]; Ctrl+S writes here.
    struct Fixture {
        _lock: TestEnvGuard<'static>,
        _config: EnvVarGuard,
        root: std::path::PathBuf,
    }

    impl Fixture {
        fn new(user_settings: &str) -> Self {
            let lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let root =
                std::env::temp_dir().join(format!("nyan-model-cmd-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("settings.json"), user_settings).unwrap();
            let config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::bootstrap::state::set_session_profile(None);
            Self {
                _lock: lock,
                _config: config,
                root,
            }
        }

        fn user_settings(&self) -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(self.root.join("settings.json")).unwrap())
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            crate::bootstrap::state::set_session_profile(None);
            crate::utils::settings::settings_cache::reset_settings_cache();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn canvas_text(canvas: &Canvas) -> String {
        (0..canvas.height())
            .map(|y| {
                (0..canvas.width())
                    .map(|x| {
                        canvas
                            .cell(x, y)
                            .and_then(|cell| cell.text())
                            .unwrap_or(" ")
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_after(store: &AppStore, events: Vec<TerminalEvent>) -> (String, Vec<String>) {
        let selected = Arc::new(Mutex::new(Vec::<String>::new()));
        let selected_for_handler = Arc::clone(&selected);
        let current_theme = *theme::current();
        let keybinding_runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        let store = store.clone();
        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(keybinding_runtime)) {
                    ContextProvider(value: Context::owned(store)) {
                        ContextProvider(value: Context::owned(current_theme)) {
                            ModelPickerWrapper(
                                on_select: move |output: String| {
                                    selected_for_handler.lock().unwrap().push(output);
                                },
                            )
                        }
                    }
                }
            };
            // Spaced like a typist so each key lands on the state the
            // previous one left (keybinding and picker handlers both run).
            let delayed = stream::unfold(events.into_iter(), |mut events| async move {
                let event = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(30)).await;
                Some((event, events))
            });
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(delayed).with_size(110, 32),
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
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        });
        let text = canvases.last().map(canvas_text).unwrap_or_default();
        let selected = selected.lock().unwrap().clone();
        (text, selected)
    }

    fn store() -> AppStore {
        AppStore::new(crate::state::app_state_store::AppState::default(), None)
    }

    #[test]
    fn model_args_switch_to_a_profile_with_its_effort_or_report_an_unknown_one() {
        let _fixture = Fixture::new(PROFILES);
        let switch = |profile: &str, effort| ModelArgs::Switch {
            profile: Some(profile.to_string()),
            effort,
        };
        assert_eq!(parse_args("deep"), Some(switch("deep", None)));
        assert_eq!(
            parse_args("deep:off"),
            Some(switch("deep", Some(Effort::Off)))
        );
        assert_eq!(parse_args("@light"), Some(switch("qwen3:32b", None)));
        assert_eq!(
            parse_args("qwen3:32b:high"),
            Some(switch("qwen3:32b", Some(Effort::High)))
        );
        assert_eq!(parse_args(""), None);
        for unknown in ["sonnet", "opus[1m]", "anthropic/claude-opus-4-6", "default"] {
            let Some(ModelArgs::Message(message)) = parse_args(unknown) else {
                panic!("{unknown} must be refused");
            };
            assert!(
                message.starts_with(&format!("Unknown model {unknown:?}")),
                "{message}"
            );
        }
    }

    #[test]
    fn switching_follows_the_new_profiles_effort_not_the_previous_session_effort() {
        let _fixture = Fixture::new(PROFILES);
        let store = store();
        store.replace_with(|state| state.effort_value = Some(Effort::Off));
        assert_eq!(
            apply_switch(Some(&store), Some("deep".into()), None),
            "Set model to deep"
        );
        let state = store.get();
        assert_eq!(state.main_loop_profile.as_deref(), Some("deep"));
        // `None` lets the profile's own `max` apply at request time.
        assert_eq!(state.effort_value, None);
        assert_eq!(
            apply_switch(Some(&store), Some("main".into()), Some(Effort::Medium)),
            "Set model to main with medium effort"
        );
        let state = store.get();
        assert_eq!(state.main_loop_profile, None);
        assert_eq!(state.effort_value, Some(Effort::Medium));
    }

    #[test]
    fn the_picker_lists_profiles_current_first_and_default_second() {
        let _fixture = Fixture::new(PROFILES);
        let store = store();
        store.replace_with(|state| state.main_loop_profile = Some("qwen3:32b".into()));
        let (text, selected) = render_after(&store, Vec::new());
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle}:\n{text}"))
        };
        assert!(at("qwen3:32b") < at("main"), "{text}");
        assert!(at("main") < at("deep"), "{text}");
        assert!(text.contains("claude-sonnet-4-6 · default"), "{text}");
        assert!(text.contains("← → to adjust"), "{text}");
        assert!(!text.contains("Sonnet"), "{text}");
        assert!(selected.is_empty());
    }

    #[test]
    fn enter_switches_the_session_only_and_right_adjusts_the_effort() {
        let fixture = Fixture::new(PROFILES);
        let store = store();
        // main, deep, qwen3:32b — Down focuses deep (effort max), Right wraps to off.
        let (_, selected) = render_after(
            &store,
            vec![key(KeyCode::Down), key(KeyCode::Right), key(KeyCode::Enter)],
        );
        assert_eq!(selected, ["Set model to deep (thinking off)"]);
        let state = store.get();
        assert_eq!(state.main_loop_profile.as_deref(), Some("deep"));
        assert_eq!(state.effort_value, Some(Effort::Off));
        // Settings are untouched.
        assert_eq!(fixture.user_settings()["nyan"]["selection"]["main"], "main");
    }

    #[test]
    fn ctrl_s_writes_the_default_and_its_effort_to_user_settings() {
        let fixture = Fixture::new(PROFILES);
        let store = store();
        let (_, selected) = render_after(
            &store,
            vec![key(KeyCode::Down), key(KeyCode::Left), ctrl('s')],
        );
        assert_eq!(selected, ["Default model set to deep"]);
        let settings = fixture.user_settings();
        assert_eq!(settings["nyan"]["selection"]["main"], "deep");
        assert_eq!(settings["nyan"]["models"]["deep"]["effort"], "high");
        // The rest of the profile survives the patch.
        assert_eq!(
            settings["nyan"]["models"]["deep"]["model"],
            "claude-opus-4-6"
        );
        assert_eq!(store.get().main_loop_profile, None);
    }

    #[test]
    fn ctrl_s_reports_a_project_selection_that_still_wins() {
        let fixture = Fixture::new(PROFILES);
        let project = fixture.root.join("project");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(
            project.join(".claude/settings.json"),
            r#"{"nyan": {"selection": {"main": "qwen3:32b"}}}"#,
        )
        .unwrap();
        let _cwd = crate::utils::test_env::PinnedProjectDir::at(&project);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let outcome = crate::components::model_picker::set_default_profile("deep", None).unwrap();
        assert_eq!(
            crate::components::model_picker::set_default_message(&outcome),
            "Default model set to deep. This project still uses qwen3:32b."
        );
    }

    #[test]
    fn with_no_profiles_the_picker_only_shows_the_configuration_hint() {
        let _fixture = Fixture::new("{}");
        let (text, selected) = render_after(&store(), vec![key(KeyCode::Enter)]);
        assert!(text.contains("No model profiles configured."), "{text}");
        assert!(text.contains("settings.nyan.models"), "{text}");
        assert!(!text.contains("effort"), "{text}");
        assert!(selected.is_empty());
        assert_eq!(handle_cancel_output(), NO_MAIN_PROFILE_PROMPT);
    }

    #[test]
    fn esc_keeps_the_current_model() {
        let _fixture = Fixture::new(PROFILES);
        let (_, selected) = render_after(&store(), vec![key(KeyCode::Esc)]);
        assert_eq!(selected, ["Kept model as main (claude-sonnet-4-6)"]);
    }
}
