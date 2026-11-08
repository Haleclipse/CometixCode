//! Maps to: CC `components/RemoteEnvironmentDialog.tsx`.
//!
//! Safety boundary: official `RemoteEnvironmentDialog` fetches environments via
//! Claude.ai OAuth (`getEnvironmentSelectionInfo`) and writes
//! `remote.defaultEnvironmentId` with `updateSettingsForSource('localSettings',
//! ...)` when a selection is made. Nyan preserves the official component
//! branches, visible copy, source suffix logic, selection callbacks, and loading
//! states from an explicit `EnvironmentSelectionInfo` snapshot. OAuth/network
//! calls and settings writes are deferred to the remote runtime/settings slice.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::components::design_system::loading_state::LoadingState;
use crate::constants::figures::MAIN_SYMBOLS;
use crate::utils::settings::{SettingSource, get_setting_source_name};
use crate::utils::teleport::environment_selection::EnvironmentSelectionInfo;
use crate::utils::teleport::environments::EnvironmentResource;
use iocraft::prelude::*;

pub const REMOTE_ENVIRONMENT_DIALOG_TITLE: &str = "Select Remote Environment";
pub const REMOTE_ENVIRONMENT_SETUP_HINT: &str = "Configure environments at: https://claude.ai/code";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteEnvironmentLoadingState {
    #[default]
    Loading,
    Updating,
    Ready,
}

#[derive(Default, Props)]
pub struct RemoteEnvironmentDialogProps<'a> {
    /// Maps to official `onDone(message?: string)`. `None` means no message.
    pub on_done: HandlerMut<'a, Option<String>>,
    pub loading_state: RemoteEnvironmentLoadingState,
    pub info: EnvironmentSelectionInfo,
    pub error: Option<String>,
}

/// Maps to: CC `RemoteEnvironmentDialog.tsx` `sourceSuffix` construction.
pub fn remote_environment_source_suffix(source: Option<SettingSource>) -> String {
    match source {
        Some(source) if source != SettingSource::Local => {
            format!(" (from {} settings)", get_setting_source_name(source))
        }
        _ => String::new(),
    }
}

/// Maps to: CC `RemoteEnvironmentDialog.tsx` `EnvironmentLabel`.
pub fn remote_environment_label(environment: &EnvironmentResource) -> String {
    format!(
        "{} Using {} ({})",
        MAIN_SYMBOLS.tick, environment.name, environment.environment_id
    )
}

/// Maps to: CC `RemoteEnvironmentDialog.tsx` selection success message.
pub fn remote_environment_success_message(environment: &EnvironmentResource) -> String {
    format!(
        "Set default remote environment to {} ({})",
        environment.name, environment.environment_id
    )
}

/// Maps to: CC `RemoteEnvironmentDialog.tsx` multiple-environment `Select`
/// options.
pub fn remote_environment_options(environments: &[EnvironmentResource]) -> Vec<SelectOptionData> {
    environments
        .iter()
        .map(|environment| SelectOptionData {
            label: format!("{} ({})", environment.name, environment.environment_id),
            value: environment.environment_id.clone(),
            ..SelectOptionData::default()
        })
        .collect()
}

fn selected_index(environments: &[EnvironmentResource], selected_id: &str) -> usize {
    environments
        .iter()
        .position(|environment| environment.environment_id == selected_id)
        .unwrap_or(0)
}

/// Maps to: CC `components/RemoteEnvironmentDialog.tsx`.
#[component]
pub fn RemoteEnvironmentDialog<'a>(
    props: &mut RemoteEnvironmentDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let mut pending_done = hooks.use_state(|| Option::<Option<String>>::None);
    let mut pending_select = hooks.use_state(|| Option::<String>::None);
    let environments = props.info.available_environments.clone();
    let initial_focus = props
        .info
        .selected_environment
        .as_ref()
        .map(|environment| selected_index(&environments, &environment.environment_id))
        .unwrap_or(0);
    let mut focused_index = hooks.use_state(move || initial_focus);
    let options = remote_environment_options(&environments);
    let ready = props.loading_state == RemoteEnvironmentLoadingState::Ready
        && props.error.as_deref().unwrap_or_default().is_empty();
    let has_selected_environment = props.info.selected_environment.is_some();
    let single_active = ready && has_selected_environment && environments.len() == 1;
    let multiple_active = ready && has_selected_environment && environments.len() > 1;
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:yes",
        crate::keybindings::types::ContextName::Confirmation,
        move || single_active,
        move || {
            pending_done.set(Some(None));
            true
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "select:previous",
        crate::keybindings::types::ContextName::Select,
        move || multiple_active,
        move || {
            focused_index.set(focused_index.get().saturating_sub(1));
            true
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "select:next",
        crate::keybindings::types::ContextName::Select,
        move || multiple_active,
        {
            let option_count = options.len().max(1);
            move || {
                focused_index.set((focused_index.get() + 1).min(option_count - 1));
                true
            }
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "select:accept",
        crate::keybindings::types::ContextName::Select,
        move || multiple_active,
        {
            let options = options.clone();
            move || {
                if let Some(option) = options.get(focused_index.get()) {
                    pending_select.set(Some(option.value.clone()));
                }
                true
            }
        },
    );

    let done_message = {
        let pending = pending_done.read();
        pending.clone()
    };
    if let Some(message) = done_message {
        pending_done.set(None);
        (props.on_done)(message);
    }

    match props.loading_state {
        RemoteEnvironmentLoadingState::Loading => {
            return element! {
                Dialog(
                    title: REMOTE_ENVIRONMENT_DIALOG_TITLE.to_string(),
                    hide_input_guide: true,
                    on_cancel: move |_| pending_done.set(Some(None)),
                ) {
                    LoadingState(message: "Loading environments…".to_string())
                }
            }
            .into_any();
        }
        RemoteEnvironmentLoadingState::Updating | RemoteEnvironmentLoadingState::Ready => {}
    }

    if let Some(error) = props.error.clone().filter(|error| !error.is_empty()) {
        return element! {
            Dialog(
                title: REMOTE_ENVIRONMENT_DIALOG_TITLE.to_string(),
                on_cancel: move |_| pending_done.set(Some(None)),
            ) {
                Text(content: format!("Error: {error}"), color: theme.error)
            }
        }
        .into_any();
    }

    let Some(selected_environment) = props.info.selected_environment.clone() else {
        return element! {
            Dialog(
                title: REMOTE_ENVIRONMENT_DIALOG_TITLE.to_string(),
                subtitle: Some(REMOTE_ENVIRONMENT_SETUP_HINT.to_string()),
                on_cancel: move |_| pending_done.set(Some(None)),
            ) {
                Text(content: "No remote environments available.".to_string())
            }
        }
        .into_any();
    };

    let default_value = selected_environment.environment_id.clone();
    let option_count = options.len().max(1);
    if environments.len() == 1 {
        return element! {
            Dialog(
                title: REMOTE_ENVIRONMENT_DIALOG_TITLE.to_string(),
                subtitle: Some(REMOTE_ENVIRONMENT_SETUP_HINT.to_string()),
                on_cancel: move |_| pending_done.set(Some(None)),
            ) {
                Text(content: remote_environment_label(&selected_environment), wrap: TextWrap::NoWrap)
            }
        }
        .into_any();
    }

    let selected_id = {
        let pending = pending_select.read();
        pending.clone()
    };
    if let Some(selected_id) = selected_id {
        pending_select.set(None);
        let message = environments
            .iter()
            .find(|environment| environment.environment_id == selected_id)
            .map(remote_environment_success_message)
            .unwrap_or_else(|| "Error: Selected environment not found".to_string());
        (props.on_done)(Some(message));
    }

    let source_suffix = remote_environment_source_suffix(props.info.selected_environment_source);
    let subtitle = format!(
        "Currently using: {}{source_suffix}",
        selected_environment.name
    );

    element! {
        Dialog(
            title: REMOTE_ENVIRONMENT_DIALOG_TITLE.to_string(),
            subtitle: Some(subtitle),
            hide_input_guide: true,
            on_cancel: move |_| pending_done.set(Some(None)),
        ) {
            Text(content: REMOTE_ENVIRONMENT_SETUP_HINT.to_string(), dim: true, wrap: TextWrap::NoWrap)
            #(if props.loading_state == RemoteEnvironmentLoadingState::Updating {
                Some(element! {
                    LoadingState(message: "Updating…".to_string())
                }.into_any())
            } else {
                Some(element! {
                    Select(
                        options: options,
                        focused_index: focused_index.get().min(option_count - 1),
                        selected_value: Some(default_value),
                        visible_option_count: option_count,
                        layout: SelectLayout::CompactVertical,
                        hide_indexes: true,
                    )
                }.into_any())
            })
            View(flex_direction: FlexDirection::Row) {
                Byline {
                    KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "select".to_string())
                    ConfigurableShortcutHint(
                        action: "confirm:no".to_string(),
                        context: "Confirmation".to_string(),
                        fallback: "Esc".to_string(),
                        description: "cancel".to_string(),
                    )
                }
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::teleport::environment_selection::get_environment_selection_info_from_snapshot;
    use crate::utils::teleport::environments::EnvironmentKind;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn env(kind: EnvironmentKind, id: &str, name: &str) -> EnvironmentResource {
        EnvironmentResource::new(kind, id, name)
    }

    fn info(environments: Vec<EnvironmentResource>) -> EnvironmentSelectionInfo {
        get_environment_selection_info_from_snapshot(environments, None, &[])
    }

    #[test]
    fn remote_environment_helpers_match_official_copy() {
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        assert_eq!(
            remote_environment_source_suffix(Some(SettingSource::User)),
            " (from user settings)"
        );
        assert_eq!(
            remote_environment_source_suffix(Some(SettingSource::Local)),
            ""
        );
        assert_eq!(
            remote_environment_label(&cloud),
            "✔ Using Cloud (env_cloud)"
        );
        assert_eq!(
            remote_environment_success_message(&cloud),
            "Set default remote environment to Cloud (env_cloud)"
        );
        assert_eq!(
            remote_environment_options(&[cloud])[0].label,
            "Cloud (env_cloud)"
        );
    }

    #[test]
    fn remote_environment_dialog_renders_loading_error_and_empty_branches() {
        let loading = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteEnvironmentDialog(loading_state: RemoteEnvironmentLoadingState::Loading)
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            loading.contains("Select Remote Environment"),
            "canvas=\n{loading}"
        );
        assert!(
            loading.contains("Loading environments…"),
            "canvas=\n{loading}"
        );

        let error = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteEnvironmentDialog(
                    loading_state: RemoteEnvironmentLoadingState::Ready,
                    error: Some("network failed".to_string()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(error.contains("Error: network failed"), "canvas=\n{error}");

        let empty = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteEnvironmentDialog(
                    loading_state: RemoteEnvironmentLoadingState::Ready,
                    info: EnvironmentSelectionInfo::default(),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            empty.contains("Configure environments at: https://claude.ai/code"),
            "canvas=\n{empty}"
        );
        assert!(
            empty.contains("No remote environments available."),
            "canvas=\n{empty}"
        );
    }

    #[test]
    fn remote_environment_dialog_renders_single_environment_branch() {
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteEnvironmentDialog(
                    loading_state: RemoteEnvironmentLoadingState::Ready,
                    info: info(vec![cloud]),
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("✔ Using Cloud (env_cloud)"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Configure environments at: https://claude.ai/code"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn remote_environment_dialog_renders_multiple_environment_selection_and_updating() {
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        let byoc = env(EnvironmentKind::Byoc, "env_byoc", "BYOC");
        let selection = get_environment_selection_info_from_snapshot(
            vec![cloud, byoc],
            Some("env_byoc"),
            &[(SettingSource::Project, Some("env_byoc".to_string()))],
        );

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteEnvironmentDialog(
                    loading_state: RemoteEnvironmentLoadingState::Ready,
                    info: selection.clone(),
                )
            }
        }
        .render(Some(160))
        .to_string();

        assert!(
            text.contains("Currently using: BYOC (from project settings)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Cloud (env_cloud)"), "canvas=\n{text}");
        // CC design-system/ListItem.tsx:179 uses installed figures.tick: U+2714,
        // confirmed with the actual Node dependency; the old U+2713 was not its output.
        assert!(text.contains("BYOC (env_byoc) ✔"), "canvas=\n{text}");
        assert!(text.contains("Enter to select"), "canvas=\n{text}");
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");

        let updating = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteEnvironmentDialog(
                    loading_state: RemoteEnvironmentLoadingState::Updating,
                    info: selection,
                )
            }
        }
        .render(Some(160))
        .to_string();
        assert!(updating.contains("Updating…"), "canvas=\n{updating}");
    }

    #[test]
    fn remote_environment_dialog_selects_focused_environment_callback_only() {
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        let byoc = env(EnvironmentKind::Byoc, "env_byoc", "BYOC");
        let selection = get_environment_selection_info_from_snapshot(vec![cloud, byoc], None, &[]);
        let completions = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let completions_for_handler = Arc::clone(&completions);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        RemoteEnvironmentDialog(
                        loading_state: RemoteEnvironmentLoadingState::Ready,
                        info: selection,
                        on_done: move |message| {
                            completions_for_handler.lock().expect("completions mutex").push(message);
                        },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Down),
                        key(KeyCode::Enter),
                    ]))
                    .with_size(140, 24),
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

        assert_eq!(
            completions.lock().expect("completions mutex").as_slice(),
            &[Some(
                "Set default remote environment to BYOC (env_byoc)".to_string()
            )]
        );
    }

    #[test]
    fn remote_environment_dialog_escape_cancels_without_message() {
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        let byoc = env(EnvironmentKind::Byoc, "env_byoc", "BYOC");
        let completions = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let completions_for_handler = Arc::clone(&completions);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        RemoteEnvironmentDialog(
                            loading_state: RemoteEnvironmentLoadingState::Ready,
                            info: info(vec![cloud, byoc]),
                            on_done: move |message| {
                                completions_for_handler.lock().expect("completions mutex").push(message);
                            },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Esc)]))
                        .with_size(140, 24),
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

        assert_eq!(
            completions.lock().expect("completions mutex").as_slice(),
            &[None]
        );
    }
}
