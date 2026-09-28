//! Maps to: CC `components/sandbox/SandboxSettings.tsx`.
//!
//! The `/sandbox` panel: CC's `Tabs` (the header starts focused, ↓ hands
//! focus to a tab's Select), a Mode tab and an Overrides tab that own their
//! Selects, and the Config and Dependencies tabs. Settings writes go through
//! `utils/sandbox/sandbox_adapter.rs`; no sandbox runtime initialization or
//! command wrapping happens in this UI component.

use super::sandbox_config_tab::SandboxConfigTab;
use super::sandbox_dependencies_tab::SandboxDependenciesTab;
use super::sandbox_overrides_tab::SandboxOverridesTab;
use crate::components::custom_select::select::SelectOptionLabel;
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::pane::Pane;
use crate::components::design_system::tabs::{Tab, Tabs, use_tab_header_focus};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::utils::sandbox::sandbox_adapter::{
    SandboxDependencyCheck, SandboxSettingsUpdate, are_sandbox_settings_locked_by_policy,
    are_unsandboxed_commands_allowed, check_dependencies_readonly, get_sandbox_enabled_setting,
    is_auto_allow_bash_if_sandboxed_enabled, is_platform_in_enabled_list, is_supported_platform,
    set_sandbox_settings,
};
use crate::utils::settings::get_initial_settings;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    AutoAllow,
    Regular,
    Disabled,
}

impl SandboxMode {
    fn value(self) -> &'static str {
        match self {
            Self::AutoAllow => "auto-allow",
            Self::Regular => "regular",
            Self::Disabled => "disabled",
        }
    }

    fn from_value(value: &str) -> Option<Self> {
        match value {
            "auto-allow" => Some(Self::AutoAllow),
            "regular" => Some(Self::Regular),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

pub fn current_sandbox_mode(enabled: bool, auto_allow: bool) -> SandboxMode {
    if !enabled {
        SandboxMode::Disabled
    } else if auto_allow {
        SandboxMode::AutoAllow
    } else {
        SandboxMode::Regular
    }
}

/// Maps to: CC `SandboxSettings.tsx:48-70`. The labels carry the
/// `(current)` suffix as text; `current_indicator_labels` renders it green.
pub fn sandbox_mode_options(current_mode: SandboxMode) -> Vec<SelectOptionData> {
    [
        (SandboxMode::AutoAllow, "Sandbox BashTool, with auto-allow"),
        (
            SandboxMode::Regular,
            "Sandbox BashTool, with regular permissions",
        ),
        (SandboxMode::Disabled, "No Sandbox"),
    ]
    .into_iter()
    .map(|(mode, label)| SelectOptionData {
        label: if mode == current_mode {
            format!("{label} (current)")
        } else {
            label.to_string()
        },
        description: None,
        dim_description: true,
        value: mode.value().to_string(),
        disabled: false,
        input: None,
    })
    .collect()
}

/// Maps to: CC `SandboxSettings.tsx:46`
/// `color('success', theme)('(current)')`, a coloured span inside the
/// current option's label string.
fn current_indicator_labels(
    options: &[SelectOptionData],
    current_value: &str,
    theme: &Theme,
) -> BTreeMap<String, SelectOptionLabel> {
    options
        .iter()
        .filter(|option| option.value == current_value)
        .map(|option| {
            let base = option.label.trim_end_matches(" (current)");
            let mut indicator = StyledSegment::new("(current)");
            indicator.styles.color = Some(theme.success);
            (
                option.value.clone(),
                vec![StyledSegment::new(format!("{base} ")), indicator],
            )
        })
        .collect()
}

pub fn sandbox_mode_completion_and_update(
    mode: SandboxMode,
) -> (&'static str, SandboxSettingsUpdate) {
    match mode {
        SandboxMode::AutoAllow => (
            "✓ Sandbox enabled with auto-allow for bash commands",
            SandboxSettingsUpdate {
                enabled: Some(true),
                auto_allow_bash_if_sandboxed: Some(true),
                allow_unsandboxed_commands: None,
            },
        ),
        SandboxMode::Regular => (
            "✓ Sandbox enabled with regular bash permissions",
            SandboxSettingsUpdate {
                enabled: Some(true),
                auto_allow_bash_if_sandboxed: Some(false),
                allow_unsandboxed_commands: None,
            },
        ),
        SandboxMode::Disabled => (
            "○ Sandbox disabled",
            SandboxSettingsUpdate {
                enabled: Some(false),
                auto_allow_bash_if_sandboxed: Some(false),
                allow_unsandboxed_commands: None,
            },
        ),
    }
}

#[derive(Default, Props)]
pub struct SandboxSettingsProps<'a> {
    pub dep_check: Option<SandboxDependencyCheck>,
    /// CC `onComplete(result)`; `None` is `onComplete(undefined, { display:
    /// 'skip' })`.
    pub on_complete: HandlerMut<'a, Option<String>>,
}

/// Maps to: CC `SandboxSettings.tsx:25-160`.
#[component]
pub fn SandboxSettings<'a>(
    props: &mut SandboxSettingsProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let settings = get_initial_settings();
    // CC's `/sandbox` checks the dependencies once and passes `depCheck` in
    // (sandbox-toggle.tsx:39, :70); without one, probe once per mount rather
    // than on every render.
    let dep_check = hooks.use_const({
        let provided = props.dep_check.clone();
        move || provided.unwrap_or_else(check_dependencies_readonly)
    });
    // CC :30-45. `SandboxManager.isSandboxingEnabled()` is
    // `sandbox_adapter::is_sandboxing_enabled`, spelled out so this
    // component's dependency check stands in for the probe it repeats.
    let current_enabled = is_supported_platform()
        && dep_check.errors.is_empty()
        && is_platform_in_enabled_list()
        && get_sandbox_enabled_setting(&settings);
    let current_auto_allow = is_auto_allow_bash_if_sandboxed_enabled(&settings);
    let has_warnings = !dep_check.warnings.is_empty();
    let allow_all_unix_sockets = settings
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.pointer("/network/allowAllUnixSockets"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let show_socket_warning = has_warnings && !allow_all_unix_sockets;
    let current_mode = current_sandbox_mode(current_enabled, current_auto_allow);

    // The tabs report from their own renders; the result reaches this
    // component's `onComplete` on its next one.
    let pending_result = hooks.use_state(|| None::<Option<String>>);
    let result = pending_result.read().clone();
    if let Some(result) = result {
        let mut pending_result = pending_result;
        pending_result.set(None);
        (props.on_complete)(result);
    }
    // The first result of a render stands: Enter and Esc read in one chunk
    // must not let the cancel overwrite the choice already written.
    let complete = Handler::from(move |result: Option<String>| {
        let mut pending_result = pending_result;
        if pending_result.read().is_none() {
            pending_result.set(Some(result));
        }
    });

    // CC :100-105.
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        ContextName::Settings,
        || true,
        {
            let complete = complete.clone();
            move || {
                complete(None);
                true
            }
        },
    );

    // CC :72-98 `handleSelect`.
    let handle_select = Handler::from({
        let complete = complete.clone();
        move |mode: SandboxMode| {
            // `setSandboxSettings` logs a failed write and resolves anyway.
            let (message, update) = sandbox_mode_completion_and_update(mode);
            set_sandbox_settings(update);
            complete(Some(message.to_string()));
        }
    });
    let mode_tab = element! {
        Tab(title: "Mode".to_string()) {
            SandboxModeTab(
                show_socket_warning: show_socket_warning,
                current_mode: Some(current_mode),
                on_select: handle_select,
                on_complete: complete.clone(),
            )
        }
    };
    // CC `SandboxOverridesTab.tsx:18-20` reads these from SandboxManager;
    // this component reads the same adapter once per render and hands them
    // down.
    let overrides_tab = element! {
        Tab(title: "Overrides".to_string()) {
            SandboxOverridesTab(
                is_enabled: current_enabled,
                is_locked: are_sandbox_settings_locked_by_policy(),
                current_allow_unsandboxed: are_unsandboxed_commands_allowed(&settings),
                on_complete: complete.clone(),
            )
        }
    };
    let config_tab = element! {
        Tab(title: "Config".to_string()) {
            SandboxConfigTab(settings: settings.clone(), dep_check: dep_check.clone())
        }
    };
    let dependencies_tab = || {
        element! {
            Tab(title: "Dependencies".to_string()) {
                SandboxDependenciesTab(dep_check: dep_check.clone())
            }
        }
    };

    // CC :130-151: missing required dependencies leave only the
    // Dependencies tab; missing optional ones add it after Mode.
    let tabs = if !dep_check.errors.is_empty() {
        vec![dependencies_tab()]
    } else {
        let mut tabs = vec![mode_tab];
        if has_warnings {
            tabs.push(dependencies_tab());
        }
        tabs.push(overrides_tab);
        tabs.push(config_tab);
        tabs
    };

    // CC :153-159.
    element! {
        Pane(color: Some(theme.permission)) {
            Tabs(
                title: Some("Sandbox:".to_string()),
                color: Some(theme.permission),
                default_tab: Some("Mode".to_string()),
            ) {
                #(tabs)
            }
        }
    }
}

#[derive(Default, Props)]
struct SandboxModeTabProps {
    show_socket_warning: bool,
    current_mode: Option<SandboxMode>,
    on_select: Handler<SandboxMode>,
    on_complete: Handler<Option<String>>,
}

/// Maps to: CC `SandboxSettings.tsx:162-211`. The options are built here
/// from the current mode (CC builds them in SandboxSettings, :48-70).
#[component]
fn SandboxModeTab(props: &SandboxModeTabProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let focus = use_tab_header_focus(&mut hooks);
    let current_mode = props.current_mode.unwrap_or(SandboxMode::Disabled);
    let options = sandbox_mode_options(current_mode);
    let option_labels = current_indicator_labels(&options, current_mode.value(), &theme);
    // CC :186-192 `<Select options onChange onCancel onUpFromFirstItem
    // isDisabled={headerFocused}>`: the default five-row viewport.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: focus.header_focused,
            has_on_cancel: true,
            has_on_up_from_first_item: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    if let Some(mode) = events
        .take_accepted()
        .as_deref()
        .and_then(SandboxMode::from_value)
    {
        (props.on_select)(mode);
    }
    if events.take_cancelled() {
        (props.on_complete)(None);
    }
    if events.take_up_from_first_item() {
        focus.focus_header();
    }
    let navigation = state.navigation.snapshot();

    let mut auto_allow_heading = StyledSegment::new("Auto-allow mode:");
    auto_allow_heading.styles.bold = Some(true);
    auto_allow_heading.styles.color = Some(theme.inactive);
    let mut auto_allow_body = StyledSegment::new(
        " Commands will try to run in the sandbox automatically, and attempts to run outside of the sandbox fallback to regular permissions. Explicit ask/deny rules are always respected.",
    );
    auto_allow_body.styles.color = Some(theme.inactive);
    let mut learn_more = StyledSegment::new("Learn more: ");
    learn_more.styles.color = Some(theme.inactive);
    let mut link = link_segment(
        "https://code.claude.com/docs/en/sandboxing".to_string(),
        Some("code.claude.com/docs/en/sandboxing".to_string()),
        None,
        None,
    );
    link.styles.color = Some(theme.inactive);

    // CC :175-209.
    element! {
        View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
            #(props.show_socket_warning.then(|| element! {
                View(margin_bottom: 1u32) {
                    Text(
                        content: "Cannot block unix domain sockets (see Dependencies tab)".to_string(),
                        color: theme.warning,
                        wrap: TextWrap::Wrap,
                    )
                }
            }))
            View(margin_bottom: 1u32) {
                Text(content: "Configure Mode:".to_string(), weight: Weight::Bold, wrap: TextWrap::Wrap)
            }
            Select(
                is_disabled: focus.header_focused,
                options: options,
                option_labels: option_labels,
                selected_value: state.committed_value(),
                focused_index: navigation.focused_index().unwrap_or(0),
                visible_option_count: navigation.visible_option_count,
                visible_from_index: navigation.visible_from_index,
                layout: SelectLayout::Compact,
            )
            View(flex_direction: FlexDirection::Column, margin_top: 1u32, gap: 1) {
                Text(segments: Some(vec![auto_allow_heading, auto_allow_body]), wrap: TextWrap::Wrap)
                Text(segments: Some(vec![learn_more, link]), wrap: TextWrap::Wrap)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    type Results = Arc<Mutex<Vec<Option<String>>>>;

    #[derive(Default, Props)]
    struct SandboxHarnessProps {
        results: Option<Results>,
    }

    /// The panel under the keybinding runtime and theme it mounts with, and
    /// a line counting key presses (so each key produces a frame) and
    /// listing the `onComplete` results.
    #[component]
    fn SandboxHarness(props: &SandboxHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            KeybindingRuntime::with_default_bindings(),
        );
        let results = props.results.clone().unwrap_or_default();
        let completed = results.clone();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        SandboxSettings(
                            dep_check: Some(SandboxDependencyCheck::default()),
                            on_complete: move |result: Option<String>| {
                                completed.lock().unwrap().push(result);
                            },
                        )
                        ResultsEcho(results: Some(results))
                    }
                }
            }
        }
    }

    #[component]
    fn ResultsEcho(props: &SandboxHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let results = props.results.clone().unwrap_or_default();
        let results = results.lock().unwrap().len();
        element! { Text(content: format!("keys={} results={results}", keys.get())) }
    }

    /// Sends one key per frame; returns the mount frame and the frame after
    /// each key, and the `onComplete` results.
    fn drive(keys_in_order: Vec<KeyCode>) -> (Vec<String>, Vec<Option<String>>) {
        let results: Results = Arc::new(Mutex::new(Vec::new()));
        let echo = results.clone();
        let frames = futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(SandboxHarness(results: Some(echo)));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 40),
            ));
            let mut frames = Vec::new();
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                if !text.contains(&format!("keys={} ", frames.len())) {
                    continue;
                }
                frames.push(text);
                let Some(code) = keys_in_order.get(frames.len() - 1) else {
                    break;
                };
                keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code)))
                    .await
                    .unwrap();
            }
            frames
        });
        let results = results.lock().unwrap().clone();
        (frames, results)
    }

    fn focused_row(frame: &str) -> String {
        frame
            .lines()
            .find(|line| line.contains('❯'))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    #[test]
    fn sandbox_mode_options_and_completion_match_official_copy() {
        let options = sandbox_mode_options(SandboxMode::AutoAllow);
        assert_eq!(
            options[0].label,
            "Sandbox BashTool, with auto-allow (current)"
        );
        assert_eq!(
            options[1].label,
            "Sandbox BashTool, with regular permissions"
        );
        assert_eq!(options[2].label, "No Sandbox");
        assert_eq!(
            sandbox_mode_completion_and_update(SandboxMode::Regular).0,
            "✓ Sandbox enabled with regular bash permissions"
        );
    }

    #[test]
    fn sandbox_settings_renders_mode_tabs_and_copy() {
        let text = element!(SandboxHarness).render(Some(120)).to_string();
        assert!(text.contains("Sandbox:"), "canvas=\n{text}");
        assert!(text.contains("Mode"), "canvas=\n{text}");
        assert!(text.contains("Overrides"), "canvas=\n{text}");
        assert!(text.contains("Config"), "canvas=\n{text}");
        assert!(text.contains("Configure Mode:"), "canvas=\n{text}");
        // CC's Select shows its indexes (hideIndexes defaults to false).
        assert!(
            text.contains("1. Sandbox BashTool, with auto-allow"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Learn more: code.claude.com/docs/en/sandboxing"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn sandbox_settings_escape_completes_with_skip_none() {
        // CC SandboxSettings.tsx:100-105 confirm:no → onComplete(undefined,
        // { display: 'skip' }). The trailing key's frame shows it settled.
        let (_, results) = drive(vec![KeyCode::Esc, KeyCode::F(12)]);
        assert_eq!(results, vec![None]);
    }

    #[test]
    fn sandbox_mode_values_round_trip() {
        for mode in [SandboxMode::AutoAllow, SandboxMode::Regular, SandboxMode::Disabled] {
            assert_eq!(SandboxMode::from_value(mode.value()), Some(mode));
        }
    }

    #[test]
    fn sandbox_mode_select_waits_for_down_from_the_focused_header() {
        // CC Tabs.tsx:125 starts with the header focused and the Select
        // disabled (:191 `isDisabled={headerFocused}`): Enter selects
        // nothing. ↓ hands focus to the Select, whose focus starts on the
        // first option; ↓ moves it and Enter selects, reporting the mode's
        // message (:83-88) whether or not the write lands, as CC's
        // setSandboxSettings never rejects. Writes stay off in this test.
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _no_write = crate::utils::env_utils::EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        let (frames, results) = drive(vec![
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::F(12),
        ]);
        assert!(
            focused_row(&frames[3]).contains("Sandbox BashTool, with regular permissions"),
            "{}",
            frames[3]
        );
        assert_eq!(
            results,
            vec![Some("✓ Sandbox enabled with regular bash permissions".to_string())]
        );
    }

    #[test]
    fn sandbox_escape_from_the_focused_select_completes_once() {
        // With the Select focused, Esc is both select:cancel (:189
        // onCancel) and SandboxSettings' confirm:no (:100-105); the panel
        // completes with one skip.
        let (_, results) = drive(vec![KeyCode::Down, KeyCode::Esc, KeyCode::F(12)]);
        assert_eq!(results, vec![None]);
    }

    #[test]
    fn sandbox_tab_keys_switch_tabs_and_up_returns_to_the_header() {
        // CC Tabs.tsx:152-161: → switches tabs from the focused header. On
        // the Mode tab ↓ enters the Select and ↑ on its first option gives
        // focus back (:190 onUpFromFirstItem={focusHeader}), so → switches
        // again.
        let (frames, _) = drive(vec![
            KeyCode::Down,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Right,
        ]);
        // → from the focused Select stays on Mode (no navFromContent).
        assert!(frames[2].contains("Configure Mode:"), "{}", frames[2]);
        assert!(!frames[4].contains("Configure Mode:"), "{}", frames[4]);
    }
}
