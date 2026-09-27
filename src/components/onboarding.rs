//! Maps to: CC `components/Onboarding.tsx`:45-281.
//!
//! This is the main onboarding shell: official step ordering, WelcomeV2
//! header, and the steps themselves. Each step owns its keys, as in CC: the
//! theme step's ThemePicker, the api-key step's `ApproveApiKey` (which
//! records the answer in `customApiKeyResponses`), and the terminal-setup
//! step's Select; Onboarding's own confirm:yes / confirm:no bindings sit
//! in a carrier ahead of the step. OAuth and preflight remain safe
//! boundaries: no network, OAuth browser launch, keychain write, or
//! analytics; those stay with their dedicated slices (`ConsoleOAuthFlow`,
//! `PreflightStep`). Terminal setup delegates to the canonical
//! `terminalSetup` installer.

use crate::components::approve_api_key::ApproveApiKey;
use crate::components::console_oauth_flow::{ConsoleOAuthFlow, ConsoleOAuthMode, OAuthStatus};
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::logo_v2::welcome_v2::WelcomeV2;
use crate::components::press_enter_to_continue::PressEnterToContinue;
use crate::components::theme_picker::ThemePicker;
use crate::components::ui::{OrderedList, OrderedListItem};
use crate::constants::product;
use crate::utils::env;
use crate::utils::preflight_checks::PreflightStep;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnboardingStepId {
    Preflight,
    Theme,
    ApiKey,
    OAuth,
    Security,
    TerminalSetup,
}

impl OnboardingStepId {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Theme => "theme",
            Self::ApiKey => "api-key",
            Self::OAuth => "oauth",
            Self::Security => "security",
            Self::TerminalSetup => "terminal-setup",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OnboardingFlowConfig {
    pub oauth_enabled: bool,
    pub api_key_needing_approval: Option<String>,
    pub skip_oauth: bool,
    pub offer_terminal_setup: bool,
}

/// Maps to: CC `components/Onboarding.tsx`:150-230 step construction.
pub(crate) fn onboarding_step_ids(config: &OnboardingFlowConfig) -> Vec<OnboardingStepId> {
    let mut steps = Vec::new();
    if config.oauth_enabled {
        steps.push(OnboardingStepId::Preflight);
    }
    steps.push(OnboardingStepId::Theme);
    if config.api_key_needing_approval.is_some() {
        steps.push(OnboardingStepId::ApiKey);
    }
    if config.oauth_enabled && !config.skip_oauth {
        steps.push(OnboardingStepId::OAuth);
    }
    steps.push(OnboardingStepId::Security);
    if config.offer_terminal_setup {
        steps.push(OnboardingStepId::TerminalSetup);
    }
    steps
}

pub(crate) fn terminal_setup_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Yes, use recommended settings".to_string(),
            value: "install".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No, maybe later with /terminal-setup".to_string(),
            value: "no".to_string(),
            ..SelectOptionData::default()
        },
    ]
}

pub(crate) fn terminal_setup_recommended_settings(terminal: &str) -> &'static str {
    if terminal == "Apple_Terminal" {
        "Option+Enter for newlines and visual bell"
    } else {
        "Shift+Enter for newlines"
    }
}

fn step_from_initial(
    steps: &[OnboardingStepId],
    initial_step: Option<OnboardingStepId>,
    initial_step_index: usize,
) -> usize {
    if let Some(step) = initial_step {
        return steps
            .iter()
            .position(|candidate| *candidate == step)
            .unwrap_or(0);
    }
    initial_step_index.min(steps.len().saturating_sub(1))
}

#[derive(Props)]
pub(crate) struct OnboardingProps<'a> {
    pub on_done: HandlerMut<'a, ()>,
    pub oauth_enabled: bool,
    pub api_key_needing_approval: Option<String>,
    pub offer_terminal_setup: bool,
    pub initial_step: Option<OnboardingStepId>,
    pub initial_step_index: usize,
    pub terminal_name: Option<String>,
}

impl Default for OnboardingProps<'_> {
    fn default() -> Self {
        Self {
            on_done: HandlerMut::default(),
            oauth_enabled: false,
            api_key_needing_approval: None,
            offer_terminal_setup: false,
            initial_step: None,
            initial_step_index: 0,
            terminal_name: None,
        }
    }
}

/// Maps to: CC `components/Onboarding.tsx`:45-281 `Onboarding(...)`.
#[component]
pub(crate) fn Onboarding<'a>(
    props: &mut OnboardingProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    // CC Onboarding.tsx:49 `const [theme, setTheme] = useTheme()`.
    let (theme_name, theme_control) =
        crate::components::design_system::theme_provider::use_theme(&hooks);
    let terminal_name = props
        .terminal_name
        .clone()
        .or_else(|| env::get().terminal.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let base_config = OnboardingFlowConfig {
        oauth_enabled: props.oauth_enabled,
        api_key_needing_approval: props.api_key_needing_approval.clone(),
        skip_oauth: false,
        offer_terminal_setup: props.offer_terminal_setup,
    };
    let base_steps = onboarding_step_ids(&base_config);
    let initial_index =
        step_from_initial(&base_steps, props.initial_step, props.initial_step_index);

    let mut current_step_index = hooks.use_state(|| initial_index);
    let skip_oauth = hooks.use_state(|| false);
    // CC calls `onDone()` from `goToNextStep`; this component's `on_done`
    // cannot be held by the step callbacks, so they set this and the next
    // render calls it.
    let mut should_done = hooks.use_state(|| false);
    // CC :77 `exitState`, relayed from `OnboardingExitBinding`.
    let exit_state = hooks.use_state(crate::hooks::use_exit::ExitKeyState::default);

    let config = OnboardingFlowConfig {
        oauth_enabled: props.oauth_enabled,
        api_key_needing_approval: props.api_key_needing_approval.clone(),
        skip_oauth: skip_oauth.get(),
        offer_terminal_setup: props.offer_terminal_setup,
    };
    let steps = onboarding_step_ids(&config);
    let bounded_index = current_step_index.get().min(steps.len().saturating_sub(1));
    if current_step_index.get() != bounded_index {
        current_step_index.set(bounded_index);
    }
    let current_step = steps.get(bounded_index).copied();
    // CC :57-70 `goToNextStep`. Like CC's, each render's closure holds that
    // render's step index, so a second call from one step lands on the
    // same next step.
    let go_to_next_step = {
        let step_count = steps.len();
        move || {
            let (mut current_step_index, mut should_done) = (current_step_index, should_done);
            if bounded_index + 1 < step_count {
                current_step_index.set(bounded_index + 1);
            } else {
                should_done.set(true);
            }
        }
    };
    // CC :72-75 `handleThemeSelection`.
    let handle_theme_selection = {
        let theme_control = theme_control.clone();
        move |setting: crate::utils::theme::ThemeSetting| {
            theme_control.set_theme_setting(setting);
            go_to_next_step();
        }
    };
    // CC :143-148 `handleApiKeyDone`: an approved key skips OAuth, then
    // `goToNextStep()`. CC lands on its skipped OAuth step, whose
    // SkippableStep moves on; the port drops that step from `steps`, so
    // the next index is already the step after it.
    let handle_api_key_done = move |approved: bool| {
        if approved {
            let mut skip_oauth = skip_oauth;
            skip_oauth.set(true);
        }
        go_to_next_step();
    };

    // Maps to: CC Onboarding.tsx:207-216, Select onChange's
    // setupTerminal(theme).catch(...).finally(goToNextStep), the
    // `goToNextStep` of the render that chose to install.
    // A3/A6: dropping the UI waiter does not cancel the installation promise.
    let install_terminal = hooks.use_async_handler({
        move |()| {
            async move {
                if let Some(runtime) =
                    crate::utils::process_runtime::runtime_handle_for_detached_work()
                {
                    let _ = runtime
                        .spawn(async move {
                            crate::commands::terminal_setup::terminal_setup::setup_terminal(
                                theme_name,
                            )
                            .await
                        })
                        .await;
                } else {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        "Process runtime unavailable for terminal setup",
                    ));
                }
                go_to_next_step();
            }
        }
    });

    if should_done.get() {
        should_done.set(false);
        (props.on_done)(());
    }

    hooks.use_propagated_terminal_events({
        let steps = steps.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
                return;
            };
            if *kind == KeyEventKind::Release {
                return;
            }
            let index = current_step_index.get().min(steps.len().saturating_sub(1));
            let Some(step) = steps.get(index).copied() else {
                return;
            };

            let mut go_next = || {
                if index + 1 < steps.len() {
                    current_step_index.set(index + 1);
                } else {
                    should_done.set(true);
                }
            };

            match step {
                // ThemePicker owns the theme step's keys (Onboarding.tsx:80-90).
                OnboardingStepId::Theme => {}
                OnboardingStepId::Security => {}
                OnboardingStepId::TerminalSetup => {}
                // ApproveApiKey owns its keys (Onboarding.tsx:156-165).
                OnboardingStepId::ApiKey => {}
                OnboardingStepId::Preflight | OnboardingStepId::OAuth => {
                    if matches!(code, KeyCode::Enter | KeyCode::Tab | KeyCode::Esc) {
                        go_next();
                        event.stop_propagation();
                    }
                }
            }
        }
    });

    let body = match current_step {
        Some(OnboardingStepId::Preflight) => render_preflight_step(theme),
        Some(OnboardingStepId::Theme) => render_theme_step(handle_theme_selection),
        Some(OnboardingStepId::ApiKey) => render_api_key_step(
            props
                .api_key_needing_approval
                .as_deref()
                .unwrap_or_default(),
            handle_api_key_done,
        ),
        Some(OnboardingStepId::OAuth) => render_oauth_step(),
        Some(OnboardingStepId::Security) => render_security_step(theme),
        Some(OnboardingStepId::TerminalSetup) => render_terminal_setup_step(
            theme,
            &terminal_name,
            install_terminal,
            go_to_next_step,
            exit_state.get().pending,
            exit_state.get().key_name,
        ),
        None => element! { Fragment }.into_any(),
    };

    // CC :273-277.
    let exit_notice = exit_state.get().pending.then(|| {
        format!(
            "Press {} again to exit",
            exit_state.get().key_name.unwrap_or("Ctrl-C")
        )
    });

    element! {
        View(flex_direction: FlexDirection::Column) {
            OnboardingExitBinding(exit_state: Some(exit_state))
            OnboardingStepBindings(
                current_step,
                // CC :236-246: continuing from the last step is `onDone()`,
                // which `goToNextStep()` also is there.
                handle_security_continue: Handler::from(move |()| go_to_next_step()),
                handle_terminal_setup_skip: Handler::from(move |()| go_to_next_step()),
            )
            WelcomeV2(
                theme_name: Some(theme_name),
                version: Some(product::VERSION.to_string()),
                apple_terminal: Some(terminal_name == "Apple_Terminal"),
            )
            View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                #(body)
                #(exit_notice.map(|notice| element! {
                    View(padding: 1u32) {
                        Text(content: notice, color: theme.inactive)
                    }
                }))
            }
        }
    }
}

#[derive(Default, Props)]
struct OnboardingExitBindingProps {
    exit_state: Option<State<crate::hooks::use_exit::ExitKeyState>>,
}

/// Native placement carrier for CC `Onboarding.tsx:77`
/// `useExitOnCtrlCDWithKeybindings()`, like Settings' `SettingsExitBinding`:
/// a zero-size first child, so the hook sees Ctrl+C/Ctrl+D before a step's
/// ThemePicker (whose own exit is a no-op), as CC's listener order gives it
/// whenever Onboarding mounted before the picker (the OAuth preflight step
/// comes first). Without OAuth CC mounts the picker in the same commit, and
/// its child-first effects let the picker's no-op take the keys; the port
/// keeps Onboarding's exit there too. The state goes back to Onboarding for
/// its "again to exit" lines.
#[component]
fn OnboardingExitBinding(
    props: &OnboardingExitBindingProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let state = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    if let Some(mut target) = props.exit_state {
        if target.get() != state {
            target.set(state);
        }
    }
    element! { View(width: 0u32, height: 0u32) }
}

#[derive(Default, Props)]
struct OnboardingStepBindingsProps {
    current_step: Option<OnboardingStepId>,
    /// CC `handleSecurityContinue`.
    handle_security_continue: Handler<()>,
    /// CC `handleTerminalSetupSkip`.
    handle_terminal_setup_skip: Handler<()>,
}

/// Native placement carrier for CC `Onboarding.tsx:234-266`
/// `handleSecurityContinue` / `handleTerminalSetupSkip` and their
/// `useKeybindings`. CC registers them when Onboarding mounts, before the
/// security and terminal-setup steps mount their content, so confirm:no
/// takes Esc and n ahead of the terminal-setup Select's cancel. A zero-size
/// child ahead of the step keeps that order.
#[component]
fn OnboardingStepBindings(
    props: &OnboardingStepBindingsProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let current_step = props.current_step;
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // CC :248-256.
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:yes",
        crate::keybindings::types::ContextName::Confirmation,
        move || current_step == Some(OnboardingStepId::Security),
        {
            let handle_security_continue = props.handle_security_continue.clone();
            move || {
                handle_security_continue(());
                true
            }
        },
    );
    // CC :258-266.
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        crate::keybindings::types::ContextName::Confirmation,
        move || current_step == Some(OnboardingStepId::TerminalSetup),
        {
            let handle_terminal_setup_skip = props.handle_terminal_setup_skip.clone();
            move || {
                handle_terminal_setup_skip(());
                true
            }
        },
    );
    element! { View(width: 0u32, height: 0u32) }
}

/// Maps to: CC `Onboarding.tsx:80-90` `themeStep`.
fn render_theme_step(
    handle_theme_selection: impl FnMut(crate::utils::theme::ThemeSetting) + Send + Sync + 'static,
) -> AnyElement<'static> {
    element! {
        View(margin_left: 1u32, margin_right: 1u32) {
            ThemePicker(
                on_theme_select: handle_theme_selection,
                show_intro_text: true,
                help_text: "To change this later, run /theme".to_string(),
                hide_esc_to_cancel: true,
                skip_exit_handling: true,
            )
        }
    }
    .into_any()
}

/// Maps to: CC `Onboarding.tsx:92-124` `securityStep`; `<Text dimColor>`
/// is the theme's `inactive` colour.
fn render_security_step(theme: Theme) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32, padding_left: 1u32) {
            Text(content: "Security notes:".to_string(), weight: Weight::Bold)
            View(flex_direction: FlexDirection::Column, width: 70u32) {
                OrderedList() {
                    OrderedListItem() {
                        Text(content: "Claude can make mistakes".to_string())
                        Text(content: "You should always review Claude's responses, especially when\nrunning code.\n".to_string(), color: theme.inactive, wrap: TextWrap::Wrap)
                    }
                    OrderedListItem() {
                        Text(content: "Due to prompt injection risks, only use it with code you trust".to_string())
                        Text(content: "For more details see:\nhttps://code.claude.com/docs/en/security".to_string(), color: theme.inactive, wrap: TextWrap::Wrap)
                    }
                }
            }
            PressEnterToContinue()
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct TerminalSetupSelectProps<'a> {
    /// CC `onChange(value)`.
    on_change: HandlerMut<'a, String>,
    /// CC `onCancel`.
    on_cancel: HandlerMut<'a, ()>,
}

/// L1 (inline Select state carrier, PORTING.md): CC writes this Select
/// inline in the terminal-setup step (Onboarding.tsx:196-218), so it mounts
/// only while that step shows; the port's Select only renders, and its
/// state lives here in its place. An accept and a cancel in one poll report
/// the accept first.
#[component]
fn TerminalSetupSelect<'a>(
    props: &mut TerminalSetupSelectProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let options = terminal_setup_options();
    // No default value, no focus value: the default five-row viewport,
    // focus on the first option.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            values: options.iter().map(|option| option.value.clone()).collect(),
            visible_option_count: Some(5),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            has_on_cancel: true,
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
    // Both are taken every render, so neither is left to fire later.
    let accepted = events.take_accepted();
    let cancelled = events.take_cancelled();
    if let Some(value) = accepted {
        (props.on_change)(value);
    }
    if cancelled {
        (props.on_cancel)(());
    }
    let navigation = state.navigation.snapshot();
    element! {
        Select(
            is_disabled: false,
            hide_indexes: false,
            visible_option_count: navigation.visible_option_count,
            options: options,
            focused_index: navigation.focused_index().unwrap_or(0),
            // CC select:accept commits the focused option, and the compact
            // layout ticks it while the install runs.
            selected_value: state.committed_value(),
            visible_from_index: navigation.visible_from_index,
            layout: SelectLayout::Compact,
        )
    }
}

/// Maps to: CC `Onboarding.tsx:181-230`.
fn render_terminal_setup_step(
    theme: Theme,
    terminal_name: &str,
    install_terminal: Handler<()>,
    go_to_next_step: impl Fn() + Copy + Send + Sync + 'static,
    exit_pending: bool,
    exit_key_name: Option<&str>,
) -> AnyElement<'static> {
    let footer = if exit_pending {
        format!(
            "Press {} again to exit",
            exit_key_name
                .filter(|value| !value.is_empty())
                .unwrap_or("Ctrl-C")
        )
    } else {
        "Enter to confirm · Esc to skip".to_string()
    };

    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32, padding_left: 1u32) {
            Text(content: "Use Claude Code's terminal setup?".to_string(), weight: Weight::Bold)
            View(flex_direction: FlexDirection::Column, width: 70u32, row_gap: 1u32) {
                Text(
                    content: format!(
                        "For the optimal coding experience, enable the recommended settings\nfor your terminal: {}",
                        terminal_setup_recommended_settings(terminal_name),
                    ),
                    wrap: TextWrap::Wrap,
                )
                // CC :207-217.
                TerminalSetupSelect(
                    on_change: move |value: String| {
                        if value == "install" {
                            install_terminal(());
                        } else {
                            go_to_next_step();
                        }
                    },
                    on_cancel: move |_| go_to_next_step(),
                )
                // CC :219 `<Text dimColor>`.
                Text(content: footer, color: theme.inactive)
            }
        }
    }
    .into_any()
}

fn render_preflight_step(_theme: Theme) -> AnyElement<'static> {
    element! {
        PreflightStep(
            result: None,
            is_checking: true,
            show_spinner: true,
        )
    }
    .into_any()
}

/// Maps to: CC `Onboarding.tsx:156-165`.
fn render_api_key_step(
    custom_api_key_truncated: &str,
    handle_api_key_done: impl FnMut(bool) + Send + Sync + 'static,
) -> AnyElement<'static> {
    element! {
        ApproveApiKey(
            custom_api_key_truncated: custom_api_key_truncated.to_string(),
            on_done: handle_api_key_done,
        )
    }
    .into_any()
}

fn render_oauth_step() -> AnyElement<'static> {
    element! {
        ConsoleOAuthFlow(
            oauth_status: OAuthStatus::Idle,
            mode: ConsoleOAuthMode::Login,
            starting_message: None,
            force_login_method: None,
            show_paste_prompt: false,
            url_copied: false,
            pasted_code: String::new(),
            cursor_offset: 0usize,
            text_input_columns: 60usize,
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn render_onboarding_text(props: OnboardingProps<'static>) -> String {
        let oauth_enabled = props.oauth_enabled;
        let api_key_needing_approval = props.api_key_needing_approval;
        let offer_terminal_setup = props.offer_terminal_setup;
        let initial_step = props.initial_step;
        let initial_step_index = props.initial_step_index;
        let terminal_name = props.terminal_name;
        // Onboarding renders inside its setup dialog's AppStateProvider
        // (interactiveHelpers.tsx:127, `interactive_helpers::SetupScreensHost`).
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Onboarding(
                            oauth_enabled: oauth_enabled,
                            api_key_needing_approval: api_key_needing_approval.clone(),
                            offer_terminal_setup: offer_terminal_setup,
                            initial_step: initial_step,
                            initial_step_index: initial_step_index,
                            terminal_name: terminal_name.clone(),
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn onboarding_step_ids_match_official_order_and_gates() {
        assert_eq!(
            onboarding_step_ids(&OnboardingFlowConfig::default()),
            vec![OnboardingStepId::Theme, OnboardingStepId::Security]
        );
        assert_eq!(
            onboarding_step_ids(&OnboardingFlowConfig {
                oauth_enabled: true,
                api_key_needing_approval: Some("abc".to_string()),
                skip_oauth: false,
                offer_terminal_setup: true,
            })
            .into_iter()
            .map(OnboardingStepId::as_str)
            .collect::<Vec<_>>(),
            vec![
                "preflight",
                "theme",
                "api-key",
                "oauth",
                "security",
                "terminal-setup",
            ]
        );
        assert!(
            !onboarding_step_ids(&OnboardingFlowConfig {
                oauth_enabled: true,
                api_key_needing_approval: None,
                skip_oauth: true,
                offer_terminal_setup: false,
            })
            .contains(&OnboardingStepId::OAuth)
        );
    }

    #[test]
    fn onboarding_security_step_renders_welcome_security_notes_and_continue() {
        let text = render_onboarding_text(OnboardingProps {
            initial_step: Some(OnboardingStepId::Security),
            terminal_name: Some("xterm".to_string()),
            ..OnboardingProps::default()
        });

        assert!(text.contains("Welcome to Claude Code"), "canvas=\n{text}");
        assert!(text.contains("Security notes:"), "canvas=\n{text}");
        assert!(text.contains("Claude can make mistakes"), "canvas=\n{text}");
        assert!(
            text.contains("Due to prompt injection risks, only use it with code you trust"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("https://code.claude.com/docs/en/security"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Press Enter to continue…"), "canvas=\n{text}");
    }

    #[test]
    fn onboarding_theme_step_saves_the_theme_and_moves_to_the_next_step() {
        // CC Onboarding.tsx:72-75 `handleThemeSelection`: `setTheme`, then
        // `goToNextStep` (Theme → Security by default). Frame-driven: each key
        // bumps the echo's count, so each is followed by a frame.
        use crate::components::design_system::theme_provider::{
            ThemeProvider, ThemeSaveHandler, use_theme,
        };
        use crate::utils::theme::{ThemeName, ThemeSetting};

        #[component]
        fn ProviderEcho(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let (current, value) = use_theme(&hooks);
            let mut keys = hooks.use_state(|| 0usize);
            hooks.use_terminal_events(move |event| {
                if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                    keys.set(keys.get() + 1);
                }
            });
            element! {
                Text(content: format!(
                    "keys={} provider setting={} current={}",
                    keys.get(),
                    value.theme_setting().setting_value(),
                    current.setting_value(),
                ))
            }
        }

        #[component]
        fn ThemeStepHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
            element! {
                ContextProvider(value: Context::owned(runtime)) {
                    ThemeProvider(
                        initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                        on_theme_save: Some(Arc::new(|_| {}) as ThemeSaveHandler),
                    ) {
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(|| element! {
                                View(flex_direction: FlexDirection::Column) {
                                    Onboarding(
                                        initial_step: Some(OnboardingStepId::Theme),
                                        terminal_name: Some("xterm".to_string()),
                                    )
                                    ProviderEcho
                                }
                            }.into_any()),
                        )
                    }
                }
            }
        }

        let frames = futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(ThemeStepHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 40),
            ));
            let batches = [key(KeyCode::Down), key(KeyCode::Enter)];
            let mut frames = Vec::new();
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                assert!(text.contains("provider setting="), "the echo stopped rendering:\n{text}");
                if !text.contains(&format!("keys={} ", frames.len())) {
                    continue;
                }
                frames.push(text);
                let Some(next) = batches.get(frames.len() - 1) else {
                    break;
                };
                keys.send(next.clone()).await.unwrap();
            }
            frames
        });
        assert_eq!(frames.len(), 3, "frames={frames:#?}");
        assert!(frames[1].contains("provider setting=dark current=light"), "{}", frames[1]);
        assert!(frames[2].contains("provider setting=light current=light"), "{}", frames[2]);
        assert!(frames[2].contains("Security notes:"), "{}", frames[2]);
    }

    #[test]
    fn onboarding_api_key_answer_skips_oauth_only_when_approved() {
        // CC Onboarding.tsx:143-148 `handleApiKeyDone`: approving skips the
        // OAuth step; rejecting goes on to it. The dialog owns its keys and
        // opens on "No". Terminal setup follows security, so a second step
        // would show. Writes stay off: the answer's config save is a dry
        // run here.
        use crate::components::design_system::theme_provider::{ThemeProvider, ThemeSaveHandler};
        use crate::utils::theme::{ThemeName, ThemeSetting};

        #[component]
        fn KeyEcho(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let mut keys = hooks.use_state(|| 0usize);
            hooks.use_terminal_events(move |event| {
                if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                    keys.set(keys.get() + 1);
                }
            });
            element! { Text(content: format!("keys={} pressed", keys.get())) }
        }

        #[component]
        fn ApiKeyStepHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
            element! {
                ContextProvider(value: Context::owned(runtime)) {
                    ThemeProvider(
                        initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                        on_theme_save: Some(Arc::new(|_| {}) as ThemeSaveHandler),
                    ) {
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(|| element! {
                                View(flex_direction: FlexDirection::Column) {
                                    Onboarding(
                                        oauth_enabled: true,
                                        api_key_needing_approval: Some("abcd1234".to_string()),
                                        offer_terminal_setup: true,
                                        initial_step: Some(OnboardingStepId::ApiKey),
                                        terminal_name: Some("xterm".to_string()),
                                    )
                                    KeyEcho
                                }
                            }.into_any()),
                        )
                    }
                }
            }
        }

        // Each batch in one go once the previous one has a frame, then an
        // unbound F12 for the step to settle.
        let run = |batches: Vec<Vec<KeyCode>>| {
            futures::executor::block_on(async move {
                let (keys, events) = async_channel::unbounded();
                let mut app = element!(ApiKeyStepHarness);
                let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(events).with_size(120, 40),
                ));
                let mut batches = batches;
                batches.push(vec![KeyCode::F(12)]);
                let mut next_batch = 0;
                let mut sent = 0;
                let mut last = String::new();
                while let Some(canvas) = render_loop.next().await {
                    last = canvas.to_string();
                    if !last.contains(&format!("keys={sent} ")) {
                        continue;
                    }
                    let Some(batch) = batches.get(next_batch) else {
                        break;
                    };
                    for code in batch {
                        keys.send(key(*code)).await.unwrap();
                    }
                    sent += batch.len();
                    next_batch += 1;
                }
                last
            })
        };

        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _no_write = crate::utils::env_utils::EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        let approved = run(vec![vec![KeyCode::Up], vec![KeyCode::Enter]]);
        assert!(approved.contains("Security notes:"), "{approved}");
        let rejected = run(vec![vec![KeyCode::Enter]]);
        assert!(rejected.contains("Select login method:"), "{rejected}");
        // An answer and Esc in one read are one answer, one step.
        let rejected = run(vec![vec![KeyCode::Enter, KeyCode::Esc]]);
        assert!(rejected.contains("Select login method:"), "{rejected}");
        let approved = run(vec![vec![KeyCode::Up], vec![KeyCode::Enter, KeyCode::Esc]]);
        assert!(approved.contains("Security notes:"), "{approved}");
    }

    #[derive(Default, Props)]
    struct StepHarnessProps {
        initial_step: Option<OnboardingStepId>,
        /// Unbinds Esc in the Confirmation context.
        unbind_confirmation_escape: bool,
    }

    /// Onboarding with terminal setup offered, from `initial_step`, under
    /// the keybinding runtime and theme it mounts with, and a line counting
    /// key presses (so each read produces a frame) and `onDone` calls.
    #[component]
    fn StepHarness(props: &StepHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        use crate::components::design_system::theme_provider::{ThemeProvider, ThemeSaveHandler};
        use crate::utils::theme::{ThemeName, ThemeSetting};
        let unbind_confirmation_escape = props.unbind_confirmation_escape;
        let runtime = hooks.use_const(|| {
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            if unbind_confirmation_escape {
                bindings.push(crate::keybindings::types::ParsedBinding {
                    chord: crate::keybindings::parser::parse_chord("escape"),
                    action: None,
                    context: crate::keybindings::types::ContextName::Confirmation,
                });
            }
            crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings)
        });
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks, runtime,
        );
        let mut keys = hooks.use_state(|| 0usize);
        let done = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let initial_step = props.initial_step;
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ThemeProvider(
                    initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                    on_theme_save: Some(Arc::new(|_| {}) as ThemeSaveHandler),
                ) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            View(flex_direction: FlexDirection::Column) {
                                Onboarding(
                                    offer_terminal_setup: true,
                                    initial_step,
                                    terminal_name: Some("xterm".to_string()),
                                    on_done: move |_| {
                                        let mut done = done;
                                        done.set(done.get() + 1);
                                    },
                                )
                                Text(content: format!("keys={} done={}", keys.get(), done.get()))
                            }
                        }.into_any()),
                    )
                }
            }
        }
    }

    /// Sends each batch in one go once the previous one has a frame, then an
    /// unbound F12 to settle; returns the last frame.
    fn run_steps(
        initial_step: OnboardingStepId,
        unbind_confirmation_escape: bool,
        batches: Vec<Vec<KeyCode>>,
    ) -> String {
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(StepHarness(
                initial_step: Some(initial_step),
                unbind_confirmation_escape,
            ));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 40),
            ));
            let mut batches = batches;
            batches.push(vec![KeyCode::F(12)]);
            let mut next_batch = 0;
            let mut sent = 0;
            let mut last = String::new();
            while let Some(canvas) = render_loop.next().await {
                last = canvas.to_string();
                if !last.contains(&format!("keys={sent} ")) {
                    continue;
                }
                let Some(batch) = batches.get(next_batch) else {
                    break;
                };
                for code in batch {
                    keys.send(key(*code)).await.unwrap();
                }
                sent += batch.len();
                next_batch += 1;
            }
            last
        })
    }

    #[test]
    fn onboarding_terminal_setup_confirm_no_sees_escape_before_the_select() {
        // CC Onboarding.tsx:258-265 registers confirm:no when Onboarding
        // mounts, ahead of the terminal-setup Select, which registers no
        // active context. With Esc unbound in Confirmation, that unbound
        // match stops Esc (useKeybinding.ts:182-186) before the Select's
        // cancel; n still skips.
        let escaped = run_steps(OnboardingStepId::TerminalSetup, true, vec![vec![KeyCode::Esc]]);
        assert!(escaped.contains("done=0"), "{escaped}");
        assert!(escaped.contains("Use Claude Code's terminal setup?"), "{escaped}");
        let declined = run_steps(OnboardingStepId::TerminalSetup, true, vec![vec![KeyCode::Char('n')]]);
        assert!(declined.contains("done=1"), "{declined}");
    }

    #[test]
    fn onboarding_keys_in_one_read_advance_from_the_step_they_were_rendered_at() {
        // CC :57-70: `handleSecurityContinue` closes over the render's
        // `currentStepIndex`, so two Enters read together both go to the
        // step after security; the second does not also leave terminal
        // setup.
        let shown = run_steps(OnboardingStepId::Security, false, vec![vec![KeyCode::Enter, KeyCode::Enter]]);
        assert!(shown.contains("done=0"), "{shown}");
        assert!(shown.contains("Use Claude Code's terminal setup?"), "{shown}");
    }

    #[test]
    fn onboarding_theme_step_uses_official_theme_picker_intro() {
        let text = render_onboarding_text(OnboardingProps {
            initial_step: Some(OnboardingStepId::Theme),
            terminal_name: Some("xterm".to_string()),
            ..OnboardingProps::default()
        });

        assert!(text.contains("Let's get started."), "canvas=\n{text}");
        assert!(
            text.contains("Choose the text style that looks best with your terminal"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("To change this later, run /theme"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Esc to cancel"), "canvas=\n{text}");
    }

    #[test]
    fn onboarding_oauth_step_delegates_to_console_oauth_flow_idle_state() {
        let text = render_onboarding_text(OnboardingProps {
            oauth_enabled: true,
            initial_step: Some(OnboardingStepId::OAuth),
            terminal_name: Some("xterm".to_string()),
            ..OnboardingProps::default()
        });

        assert!(
            text.contains("Claude Code can be used with your Claude subscription"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Select login method:"), "canvas=\n{text}");
        assert!(
            text.contains("Claude account with subscription"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn onboarding_terminal_setup_step_renders_official_copy_and_options() {
        let text = render_onboarding_text(OnboardingProps {
            initial_step: Some(OnboardingStepId::TerminalSetup),
            offer_terminal_setup: true,
            terminal_name: Some("Apple_Terminal".to_string()),
            ..OnboardingProps::default()
        });

        assert!(
            text.contains("Use Claude Code's terminal setup?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Option+Enter for newlines and visual bell"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Yes, use recommended settings"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("No, maybe later with /terminal-setup"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to skip"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn onboarding_security_enter_invokes_done_without_external_side_effects() {
        let done_count = Arc::new(Mutex::new(0usize));
        let done_for_handler = Arc::clone(&done_count);
        let current_theme = *theme::current();

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        Onboarding(
                        initial_step: Some(OnboardingStepId::Security),
                        on_done: move |_| {
                            *done_for_handler.lock().expect("done mutex") += 1;
                        },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(120, 40),
                ),
            );
            for _ in 0..8 {
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

        assert_eq!(*done_count.lock().expect("done mutex"), 1);
    }
    // CC Onboarding.tsx:208-218: select install awaits settlement; refusal and
    // cancellation advance without calling the installer. Real keybinding
    // contexts and the production file installer are required in this fixture.
    fn run_terminal_onboarding(keys: Vec<KeyCode>, fail_write: bool) -> (usize, bool) {
        use crate::utils::env_utils::EnvVarGuard;
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("terminal-onboarding-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _vars = [
            EnvVarGuard::set("TERM", "xterm-256color"),
            EnvVarGuard::set("TERM_PROGRAM", "alacritty"),
            EnvVarGuard::unset("CURSOR_TRACE_ID"),
            EnvVarGuard::unset("VSCODE_GIT_ASKPASS_MAIN"),
            EnvVarGuard::set("XDG_CONFIG_HOME", root.join("xdg")),
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.join("config")),
            EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1"),
        ];
        if fail_write {
            std::fs::write(root.join("xdg"), "not a directory").unwrap();
        }
        crate::utils::config::clear_global_config_cache_for_testing();
        let done = Arc::new(Mutex::new(Vec::new()));
        let output = root.join("xdg/alacritty/alacritty.toml");
        let done_callback = done.clone();
        let callback_path = output.clone();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        Onboarding(
                            initial_step: Some(OnboardingStepId::TerminalSetup),
                            offer_terminal_setup: true,
                            on_done: move |_| { done_callback.lock().unwrap().push(callback_path.exists()); },
                        )
                    }
                }
            };
            let events = stream::iter(keys.into_iter().map(key));
            let mut frames = Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events).with_size(120,40)));
            let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
            while let Ok(Some(_)) = tokio::time::timeout_at(deadline, frames.next()).await {
                if !done.lock().unwrap().is_empty() { break; }
            }
            // The callback must execute exactly once for a single action.
            let observed = done.lock().unwrap();
            (observed.len(), observed.first().copied().unwrap_or(false))
        });
        crate::utils::config::clear_global_config_cache_for_testing();
        let _ = std::fs::remove_dir_all(root);
        result
    }

    #[test]
    fn onboarding_terminal_install_matches_official_completion_after_write() {
        assert_eq!(
            run_terminal_onboarding(vec![KeyCode::Enter], false),
            (1, true)
        );
    }

    #[test]
    fn onboarding_terminal_install_matches_official_rejection_still_advances() {
        assert_eq!(
            run_terminal_onboarding(vec![KeyCode::Enter], true),
            (1, false)
        );
    }

    #[test]
    fn onboarding_terminal_skip_matches_official_no_installer() {
        assert_eq!(
            run_terminal_onboarding(vec![KeyCode::Down, KeyCode::Enter], false),
            (1, false)
        );
    }

    #[test]
    fn onboarding_terminal_escape_matches_official_single_cancel() {
        assert_eq!(
            run_terminal_onboarding(vec![KeyCode::Esc], false),
            (1, false)
        );
    }
}
