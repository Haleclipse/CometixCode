//! iTerm2 split-pane setup prompt.
//!
//! Maps to: CC `utils/swarm/It2SetupPrompt.tsx`.
//!
//! This module keeps the official UI boundary under `utils/swarm/` rather than
//! moving it into AgentTool. The prompt owns only the interactive setup state;
//! installation, verification, and persisted preference flags remain in
//! `utils/swarm/backends/it2_setup.rs`, matching CC's `backends/it2Setup.ts`.

use crate::components::custom_select::select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::pane::Pane;
use crate::components::spinner::Spinner;
use crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings;
use crate::utils::swarm::backends::it2_setup::{
    PythonPackageManager, detect_python_package_manager, get_python_api_instructions, install_it2,
    mark_it2_setup_complete, set_prefer_tmux_over_iterm2, verify_it2_setup,
};
use iocraft::prelude::*;
use std::time::Duration;

/// Maps to: CC `It2SetupPrompt` `onDone(result)` result literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum It2SetupPromptResult {
    Installed,
    UseTmux,
    Cancelled,
}

impl It2SetupPromptResult {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::UseTmux => "use-tmux",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Cross-thread request emitted by Rust's `setToolJSX` equivalent for the
/// official `React.createElement(It2SetupPrompt, ...)` path.
///
/// Maps to: CC `spawnMultiAgent.ts` `context.setToolJSX({ jsx:
/// <It2SetupPrompt ...>, shouldHidePromptInput: true })`.
#[derive(Clone)]
pub struct It2SetupPromptRequest {
    pub tmux_available: bool,
    pub responder: async_channel::Sender<It2SetupPromptResult>,
}

impl std::fmt::Debug for It2SetupPromptRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("It2SetupPromptRequest")
            .field("tmux_available", &self.tmux_available)
            .finish_non_exhaustive()
    }
}

impl PartialEq for It2SetupPromptRequest {
    fn eq(&self, other: &Self) -> bool {
        self.tmux_available == other.tmux_available
    }
}

impl Eq for It2SetupPromptRequest {}

impl It2SetupPromptRequest {
    pub fn respond(&self, result: It2SetupPromptResult) {
        let _ = self.responder.try_send(result);
    }
}

/// Maps to: CC `SetupStep` union in `It2SetupPrompt.tsx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum It2SetupStep {
    Initial,
    Installing,
    InstallFailed,
    VerifyApi,
    ApiInstructions,
    Verifying,
    Success,
    Failed,
}

impl Default for It2SetupStep {
    fn default() -> Self {
        Self::Initial
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum It2SetupAction {
    Install,
    RetryInstall,
    Verify,
    RetryVerify,
    UseTmux,
    Cancel,
}

impl It2SetupAction {
    fn from_value(value: &str, step: It2SetupStep) -> Option<Self> {
        match (step, value) {
            (It2SetupStep::Initial, "install") => Some(Self::Install),
            (It2SetupStep::InstallFailed, "retry") => Some(Self::RetryInstall),
            (It2SetupStep::Failed, "retry") => Some(Self::RetryVerify),
            (_, "tmux") => Some(Self::UseTmux),
            (_, "cancel") => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[derive(Default, Props)]
pub struct It2SetupPromptProps<'a> {
    pub on_done: HandlerMut<'a, It2SetupPromptResult>,
    pub tmux_available: bool,
}

/// Maps to: CC `renderInitialPrompt()` option construction.
pub fn initial_prompt_options(
    package_manager: Option<PythonPackageManager>,
    tmux_available: bool,
) -> Vec<SelectOptionData> {
    let mut options = vec![SelectOptionData {
        label: "Install it2 now".to_string(),
        value: "install".to_string(),
        description: Some(match package_manager {
            Some(pm) => format!("Uses {} to install the it2 CLI tool", pm.official_name()),
            None => "Requires Python (uvx, pipx, or pip)".to_string(),
        }),
        ..Default::default()
    }];

    if tmux_available {
        options.push(SelectOptionData {
            label: "Use tmux instead".to_string(),
            value: "tmux".to_string(),
            description: Some("Opens teammates in a separate tmux session".to_string()),
            ..Default::default()
        });
    }

    options.push(SelectOptionData {
        label: "Cancel".to_string(),
        value: "cancel".to_string(),
        description: Some("Skip teammate spawning for now".to_string()),
        ..Default::default()
    });

    options
}

/// Maps to: CC `renderInstallFailed()` option construction.
pub fn install_failed_options(tmux_available: bool) -> Vec<SelectOptionData> {
    retry_options(
        tmux_available,
        "Retry the installation",
        "Falls back to tmux for teammate panes",
    )
}

/// Maps to: CC `renderFailed()` option construction.
pub fn verify_failed_options(tmux_available: bool) -> Vec<SelectOptionData> {
    retry_options(
        tmux_available,
        "Verify the connection again",
        "Falls back to tmux for teammate panes",
    )
}

fn retry_options(
    tmux_available: bool,
    retry_description: &str,
    tmux_description: &str,
) -> Vec<SelectOptionData> {
    let mut options = vec![SelectOptionData {
        label: "Try again".to_string(),
        value: "retry".to_string(),
        description: Some(retry_description.to_string()),
        ..Default::default()
    }];

    if tmux_available {
        options.push(SelectOptionData {
            label: "Use tmux instead".to_string(),
            value: "tmux".to_string(),
            description: Some(tmux_description.to_string()),
            ..Default::default()
        });
    }

    options.push(SelectOptionData {
        label: "Cancel".to_string(),
        value: "cancel".to_string(),
        description: Some("Skip teammate spawning for now".to_string()),
        ..Default::default()
    });

    options
}

/// Maps to: CC `renderInstallFailed()` manual install copy.
pub fn manual_install_command(package_manager: Option<PythonPackageManager>) -> &'static str {
    match package_manager {
        Some(PythonPackageManager::Uvx) => "uv tool install it2",
        Some(PythonPackageManager::Pipx) => "pipx install it2",
        Some(PythonPackageManager::Pip) | None => "pip install --user it2",
    }
}

fn select_window_start(focused_index: usize, visible_count: usize, total: usize) -> usize {
    if total <= visible_count {
        0
    } else {
        focused_index
            .saturating_sub(visible_count.saturating_sub(1))
            .min(total - visible_count)
    }
}

fn options_for_step(
    step: It2SetupStep,
    package_manager: Option<PythonPackageManager>,
    tmux_available: bool,
) -> Vec<SelectOptionData> {
    match step {
        It2SetupStep::Initial => initial_prompt_options(package_manager, tmux_available),
        It2SetupStep::InstallFailed => install_failed_options(tmux_available),
        It2SetupStep::Failed => verify_failed_options(tmux_available),
        _ => Vec::new(),
    }
}

fn start_install(
    package_manager: Option<PythonPackageManager>,
    mut step: State<It2SetupStep>,
    mut error: State<Option<String>>,
) {
    let Some(package_manager) = package_manager else {
        error.set(Some(
            "No Python package manager found (uvx, pipx, or pip)".to_string(),
        ));
        step.set(It2SetupStep::Failed);
        return;
    };

    step.set(It2SetupStep::Installing);
    tokio::spawn(async move {
        let result = install_it2(package_manager).await;
        if result.success {
            step.set(It2SetupStep::ApiInstructions);
        } else {
            error.set(Some(
                result
                    .error
                    .unwrap_or_else(|| "Installation failed".to_string()),
            ));
            step.set(It2SetupStep::InstallFailed);
        }
    });
}

fn start_verify(
    mut step: State<It2SetupStep>,
    mut error: State<Option<String>>,
    mut pending_done: State<Option<It2SetupPromptResult>>,
) {
    step.set(It2SetupStep::Verifying);
    tokio::spawn(async move {
        let result = verify_it2_setup().await;
        if result.success {
            let _ = mark_it2_setup_complete();
            step.set(It2SetupStep::Success);
            tokio::time::sleep(Duration::from_millis(1500)).await;
            pending_done.set(Some(It2SetupPromptResult::Installed));
        } else {
            error.set(Some(
                result
                    .error
                    .unwrap_or_else(|| "Verification failed".to_string()),
            ));
            step.set(It2SetupStep::Failed);
        }
    });
}

fn render_select(options: Vec<SelectOptionData>, focused_index: usize) -> AnyElement<'static> {
    let visible_count = options.len().max(1);
    let focused_index = focused_index.min(options.len().saturating_sub(1));
    let start = select_window_start(focused_index, visible_count, options.len());
    element! {
        Select(
            options: options,
            focused_index: focused_index,
            visible_option_count: visible_count,
            visible_from_index: start,
            hide_indexes: false,
            layout: SelectLayout::Expanded,
        )
    }
    .into_any()
}

fn render_initial_prompt(
    package_manager: Option<PythonPackageManager>,
    tmux_available: bool,
    focused_index: usize,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let options = initial_prompt_options(package_manager, tmux_available);
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: "To use native iTerm2 split panes for teammates, you need the it2 CLI tool.".to_string(), wrap: TextWrap::Wrap)
            Text(content: "This enables teammates to appear as split panes within your current window.".to_string(), color: theme.inactive, dim: true, wrap: TextWrap::Wrap)
            View(margin_top: 1u32) {
                #(Some(render_select(options, focused_index)))
            }
        }
    }
    .into_any()
}

fn render_installing(
    package_manager: Option<PythonPackageManager>,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let pm = package_manager
        .map(|pm| pm.official_name().to_string())
        .unwrap_or_else(|| "Python".to_string());
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: format!(" Installing it2 using {pm}…"), wrap: TextWrap::NoWrap)
            }
            Text(content: "This may take a moment.".to_string(), dim: true, wrap: TextWrap::NoWrap)
        }
    }
    .into_any()
}

fn render_install_failed(
    package_manager: Option<PythonPackageManager>,
    error: Option<String>,
    tmux_available: bool,
    focused_index: usize,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let options = install_failed_options(tmux_available);
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: "Installation failed".to_string(), color: theme.error, wrap: TextWrap::NoWrap)
            #(error.map(|message| element! { Text(content: message, dim: true, wrap: TextWrap::Wrap) }))
            Text(content: format!("You can try installing manually: {}", manual_install_command(package_manager)), dim: true, wrap: TextWrap::Wrap)
            View(margin_top: 1u32) {
                #(Some(render_select(options, focused_index)))
            }
        }
    }
    .into_any()
}

fn render_api_instructions(theme: crate::utils::theme::Theme) -> AnyElement<'static> {
    let instructions = get_python_api_instructions();
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: "✓ it2 installed successfully".to_string(), color: theme.success, wrap: TextWrap::NoWrap)
            View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                #(instructions.into_iter().map(|line| element! { Text(content: line.to_string(), wrap: TextWrap::Wrap) }))
            }
            View(margin_top: 1u32) {
                Text(content: "Press Enter when ready to verify…".to_string(), dim: true, wrap: TextWrap::NoWrap)
            }
        }
    }
    .into_any()
}

fn render_verifying(theme: crate::utils::theme::Theme) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Row) {
            Spinner
            Text(content: " Verifying it2 can communicate with iTerm2…".to_string(), wrap: TextWrap::NoWrap)
        }
    }
    .into_any()
}

fn render_success(theme: crate::utils::theme::Theme) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "✓ iTerm2 split pane support is ready".to_string(), color: theme.success, wrap: TextWrap::NoWrap)
            Text(content: "Teammates will now appear as split panes.".to_string(), dim: true, wrap: TextWrap::NoWrap)
        }
    }
    .into_any()
}

fn render_failed(
    error: Option<String>,
    tmux_available: bool,
    focused_index: usize,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let options = verify_failed_options(tmux_available);
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: "Verification failed".to_string(), color: theme.error, wrap: TextWrap::NoWrap)
            #(error.map(|message| element! { Text(content: message, dim: true, wrap: TextWrap::Wrap) }))
            Text(content: "Make sure:".to_string(), wrap: TextWrap::NoWrap)
            View(flex_direction: FlexDirection::Column, padding_left: 2u32) {
                Text(content: "· Python API is enabled in iTerm2 preferences".to_string(), wrap: TextWrap::NoWrap)
                Text(content: "· You may need to restart iTerm2 after enabling".to_string(), wrap: TextWrap::NoWrap)
            }
            View(margin_top: 1u32) {
                #(Some(render_select(options, focused_index)))
            }
        }
    }
    .into_any()
}

/// Maps to: CC `utils/swarm/It2SetupPrompt.tsx#It2SetupPrompt`.
#[component]
pub fn It2SetupPrompt<'a>(
    props: &mut It2SetupPromptProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let mut step = hooks.use_state(It2SetupStep::default);
    let mut package_manager = hooks.use_state(|| Option::<PythonPackageManager>::None);
    let mut error = hooks.use_state(|| Option::<String>::None);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_action = hooks.use_state(|| Option::<It2SetupAction>::None);
    let mut pending_done = hooks.use_state(|| Option::<It2SetupPromptResult>::None);
    let exit_state = use_exit_on_ctrl_cd_with_keybindings(
        &mut hooks,
        !matches!(
            step.get(),
            It2SetupStep::Installing | It2SetupStep::Verifying
        ),
    );

    hooks.use_future({
        let mut package_manager = package_manager;
        async move {
            let detected = detect_python_package_manager().await;
            package_manager.set(detected);
        }
    });

    let done = { *pending_done.read() };
    if let Some(result) = done {
        pending_done.set(None);
        (props.on_done)(result);
    }

    let action = { *pending_action.read() };
    if let Some(action) = action {
        pending_action.set(None);
        match action {
            It2SetupAction::Install | It2SetupAction::RetryInstall => {
                error.set(None);
                focused_index.set(0);
                start_install(package_manager.get(), step, error);
            }
            It2SetupAction::Verify | It2SetupAction::RetryVerify => {
                error.set(None);
                focused_index.set(0);
                start_verify(step, error, pending_done);
            }
            It2SetupAction::UseTmux => {
                let _ = set_prefer_tmux_over_iterm2(true);
                pending_done.set(Some(It2SetupPromptResult::UseTmux));
            }
            It2SetupAction::Cancel => {
                pending_done.set(Some(It2SetupPromptResult::Cancelled));
            }
        }
    }

    let current_step = step.get();
    let current_pm = package_manager.get();
    let current_error = error.read().clone();
    let tmux_available = props.tmux_available;
    let current_options = options_for_step(current_step, current_pm, tmux_available);
    let total_options = current_options.len();
    if total_options > 0 && focused_index.get() >= total_options {
        focused_index.set(total_options.saturating_sub(1));
    }
    let current_focused = focused_index.get().min(total_options.saturating_sub(1));

    hooks.use_propagated_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_action = pending_action;
        let options = current_options.clone();
        move |event| {
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

            match code {
                KeyCode::Esc
                    if !matches!(
                        current_step,
                        It2SetupStep::Installing | It2SetupStep::Verifying | It2SetupStep::Success
                    ) =>
                {
                    pending_action.set(Some(It2SetupAction::Cancel));
                    event.stop_propagation();
                }
                KeyCode::Char('n') | KeyCode::Char('N')
                    if !modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) && !matches!(
                        current_step,
                        It2SetupStep::Installing | It2SetupStep::Verifying | It2SetupStep::Success
                    ) =>
                {
                    pending_action.set(Some(It2SetupAction::Cancel));
                    event.stop_propagation();
                }
                KeyCode::Enter => {
                    if current_step == It2SetupStep::ApiInstructions
                        || current_step == It2SetupStep::VerifyApi
                    {
                        pending_action.set(Some(It2SetupAction::Verify));
                        event.stop_propagation();
                        return;
                    }
                    if matches!(
                        current_step,
                        It2SetupStep::Initial | It2SetupStep::InstallFailed | It2SetupStep::Failed
                    ) {
                        if let Some(option) = options.get(focused_index.get()) {
                            if let Some(action) =
                                It2SetupAction::from_value(&option.value, current_step)
                            {
                                pending_action.set(Some(action));
                                event.stop_propagation();
                            }
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k')
                    if !modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) && !options.is_empty() =>
                {
                    focused_index.set(focused_index.get().saturating_sub(1));
                    event.stop_propagation();
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab
                    if !modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) && !options.is_empty() =>
                {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                    event.stop_propagation();
                }
                _ => {}
            }
        }
    });

    let content = match current_step {
        It2SetupStep::Initial => {
            render_initial_prompt(current_pm, tmux_available, current_focused, *theme)
        }
        It2SetupStep::Installing => render_installing(current_pm, *theme),
        It2SetupStep::InstallFailed => render_install_failed(
            current_pm,
            current_error,
            tmux_available,
            current_focused,
            *theme,
        ),
        It2SetupStep::VerifyApi | It2SetupStep::ApiInstructions => render_api_instructions(*theme),
        It2SetupStep::Verifying => render_verifying(*theme),
        It2SetupStep::Success => render_success(*theme),
        It2SetupStep::Failed => {
            render_failed(current_error, tmux_available, current_focused, *theme)
        }
    };

    let show_footer = !matches!(
        current_step,
        It2SetupStep::Installing | It2SetupStep::Verifying | It2SetupStep::Success
    );
    let footer_text = if exit_state.pending {
        format!(
            "Press {} again to exit",
            exit_state.key_name.unwrap_or("Ctrl-C")
        )
    } else {
        "Esc to cancel".to_string()
    };

    element! {
        Pane(color: Some(theme.permission)) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32, padding_bottom: 1u32) {
                Text(content: "iTerm2 Split Pane Setup".to_string(), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                #(Some(content))
                #(if show_footer {
                    Some(element! { Text(content: footer_text, dim: true, italic: true, wrap: TextWrap::NoWrap) })
                } else { None })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_prompt_options_match_official_tmux_branch() {
        let options = initial_prompt_options(Some(PythonPackageManager::Uvx), true);
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["install", "tmux", "cancel"]
        );
        assert_eq!(
            options[0].description.as_deref(),
            Some("Uses uvx to install the it2 CLI tool")
        );
        assert_eq!(
            options[1].description.as_deref(),
            Some("Opens teammates in a separate tmux session")
        );
    }

    #[test]
    fn retry_options_match_official_copy() {
        let install = install_failed_options(false);
        assert_eq!(
            install
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["retry", "cancel"]
        );
        assert_eq!(
            install[0].description.as_deref(),
            Some("Retry the installation")
        );

        let verify = verify_failed_options(true);
        assert_eq!(
            verify
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["retry", "tmux", "cancel"]
        );
        assert_eq!(
            verify[0].description.as_deref(),
            Some("Verify the connection again")
        );
    }

    #[test]
    fn manual_install_command_matches_official_package_manager_copy() {
        assert_eq!(
            manual_install_command(Some(PythonPackageManager::Uvx)),
            "uv tool install it2"
        );
        assert_eq!(
            manual_install_command(Some(PythonPackageManager::Pipx)),
            "pipx install it2"
        );
        assert_eq!(
            manual_install_command(Some(PythonPackageManager::Pip)),
            "pip install --user it2"
        );
        assert_eq!(manual_install_command(None), "pip install --user it2");
    }

    #[test]
    fn it2_setup_prompt_initial_render_contains_official_copy() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                It2SetupPrompt(tmux_available: true, on_done: move |_| {})
            }
        }
        .render(Some(90));
        let text = canvas.to_string();
        assert!(text.contains("iTerm2 Split Pane Setup"), "canvas=\n{text}");
        assert!(text.contains("Install it2 now"), "canvas=\n{text}");
        assert!(text.contains("Use tmux instead"), "canvas=\n{text}");
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_request_debug_and_equality_ignore_response_channel() {
        let (a_tx, _a_rx) = async_channel::bounded(1);
        let (b_tx, _b_rx) = async_channel::bounded(1);
        let a = It2SetupPromptRequest {
            tmux_available: true,
            responder: a_tx,
        };
        let b = It2SetupPromptRequest {
            tmux_available: true,
            responder: b_tx,
        };
        assert_eq!(a, b);
        assert!(format!("{a:?}").contains("tmux_available"));
    }
}
