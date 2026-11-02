//! Maps to: CC `components/ShowInIDEPrompt.tsx`.
//!
//! Safety boundary: official code asks runtime IDE helpers whether the current
//! terminal is VS Code and uses `getCwd()` for symlink display. Nyan accepts
//! explicit snapshot props and emits callback results only.

use crate::components::custom_select::select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::pane::Pane;
use iocraft::prelude::*;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum ShowInIDEPermissionOptionType {
    AcceptOnce,
    Reject,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShowInIDEPermissionOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
    /// Maps to CC `PermissionOption.type`.
    pub option_type: ShowInIDEPermissionOptionType,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShowInIDEChange {
    pub option: ShowInIDEPermissionOption,
    pub input: String,
    pub feedback: Option<String>,
}

#[derive(Default, Props)]
pub struct ShowInIDEPromptProps<'a> {
    pub file_path: String,
    pub input: String,
    pub options: Vec<ShowInIDEPermissionOption>,
    pub ide_name: String,
    pub symlink_target: Option<String>,
    pub cwd: String,
    pub reject_feedback: String,
    pub accept_feedback: String,
    pub focused_option: String,
    pub yes_input_mode: bool,
    pub no_input_mode: bool,
    pub is_supported_vscode_terminal: bool,
    pub on_change: HandlerMut<'a, ShowInIDEChange>,
    pub set_focused_option: HandlerMut<'a, String>,
    pub on_input_mode_toggle: HandlerMut<'a, String>,
}

/// Maps to the symlink warning branch in CC `ShowInIDEPrompt`.
pub fn show_in_ide_symlink_warning(symlink_target: &str, cwd: &str) -> String {
    let outside = cwd.is_empty() || Path::new(symlink_target).strip_prefix(cwd).is_err();
    if outside {
        format!("This will modify {symlink_target} (outside working directory) via a symlink")
    } else {
        format!("Symlink target: {symlink_target}")
    }
}

pub fn show_in_ide_basename(file_path: &str) -> String {
    Path::new(file_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(file_path)
        .to_string()
}

fn trim_feedback(feedback: &str) -> Option<String> {
    let trimmed = feedback.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[component]
pub fn ShowInIDEPrompt<'a>(
    props: &mut ShowInIDEPromptProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let mut pending_change = hooks.use_state(|| Option::<ShowInIDEChange>::None);
    let mut pending_focus = hooks.use_state(|| Option::<String>::None);
    let mut pending_toggle = hooks.use_state(|| Option::<String>::None);

    let focused_index = props
        .options
        .iter()
        .position(|option| option.value == props.focused_option)
        .unwrap_or(0);
    let focused_option = props
        .options
        .get(focused_index)
        .cloned()
        .unwrap_or_else(|| ShowInIDEPermissionOption {
            value: "no".to_string(),
            label: "No".to_string(),
            description: None,
            option_type: ShowInIDEPermissionOptionType::Reject,
        });
    let input = props.input.clone();
    let reject_feedback = props.reject_feedback.clone();
    let accept_feedback = props.accept_feedback.clone();
    let options_for_input = props.options.clone();
    let focused_value = focused_option.value.clone();
    hooks.use_terminal_events({
        let mut pending_change = pending_change;
        let mut pending_focus = pending_focus;
        let mut pending_toggle = pending_toggle;
        move |event| {
            let TerminalEvent::Key(key) = event else {
                return;
            };
            if key.kind == KeyEventKind::Release {
                return;
            }
            match key.code {
                KeyCode::Enter => {
                    let option = focused_option.clone();
                    let feedback = match option.option_type {
                        ShowInIDEPermissionOptionType::Reject => trim_feedback(&reject_feedback),
                        ShowInIDEPermissionOptionType::AcceptOnce => {
                            trim_feedback(&accept_feedback)
                        }
                        ShowInIDEPermissionOptionType::Other => None,
                    };
                    pending_change.set(Some(ShowInIDEChange {
                        option,
                        input: input.clone(),
                        feedback,
                    }));
                }
                KeyCode::Esc => {
                    pending_change.set(Some(ShowInIDEChange {
                        option: ShowInIDEPermissionOption {
                            value: "no".to_string(),
                            label: "No".to_string(),
                            description: None,
                            option_type: ShowInIDEPermissionOptionType::Reject,
                        },
                        input: input.clone(),
                        feedback: None,
                    }));
                }
                KeyCode::Tab => pending_toggle.set(Some(focused_value.clone())),
                KeyCode::Up => {
                    if !options_for_input.is_empty() {
                        let next = focused_index.saturating_sub(1);
                        pending_focus.set(Some(options_for_input[next].value.clone()));
                    }
                }
                KeyCode::Down if !options_for_input.is_empty() => {
                    let next = (focused_index + 1).min(options_for_input.len() - 1);
                    pending_focus.set(Some(options_for_input[next].value.clone()));
                }
                _ => {}
            }
        }
    });

    let pending = { pending_change.read().clone() };
    if let Some(change) = pending {
        pending_change.set(None);
        (props.on_change)(change);
    }
    let focus = { pending_focus.read().clone() };
    if let Some(value) = focus {
        pending_focus.set(None);
        (props.set_focused_option)(value);
    }
    let toggle = { pending_toggle.read().clone() };
    if let Some(value) = toggle {
        pending_toggle.set(None);
        (props.on_input_mode_toggle)(value);
    }

    let select_options: Vec<SelectOptionData> = props
        .options
        .iter()
        .map(|option| SelectOptionData {
            label: option.label.clone(),
            description: option.description.clone(),
            dim_description: false,
            value: option.value.clone(),
            disabled: false,
            input: None,
        })
        .collect();
    let basename = show_in_ide_basename(&props.file_path);
    let show_tab_hint = (props.focused_option == "yes" && !props.yes_input_mode)
        || (props.focused_option == "no" && !props.no_input_mode);

    element! {
        Pane(color: Some(theme.permission)) {
            View(flex_direction: FlexDirection::Column, gap: 1u32) {
                Text(content: format!("Opened changes in {} ⧉", props.ide_name), color: theme.permission, weight: Weight::Bold)
                #(props.symlink_target.as_ref().map(|target| element! {
                    Text(content: show_in_ide_symlink_warning(target, &props.cwd), color: theme.warning)
                }))
                #(if props.is_supported_vscode_terminal {
                    Some(element! { Text(content: "Save file to continue…".to_string(), dim: true) })
                } else { None })
                View(flex_direction: FlexDirection::Column) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Do you want to make this edit to ".to_string())
                        Text(content: basename, weight: Weight::Bold)
                        Text(content: "?".to_string())
                    }
                    Select(
                        options: select_options,
                        focused_index: focused_index,
                        selected_value: Some(props.focused_option.clone()),
                        layout: SelectLayout::Compact,
                    )
                }
                View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                    Text(content: if show_tab_hint { "Esc to cancel · Tab to amend".to_string() } else { "Esc to cancel".to_string() }, dim: true)
                }
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
    use std::time::Duration;

    fn options() -> Vec<ShowInIDEPermissionOption> {
        vec![
            ShowInIDEPermissionOption {
                value: "yes".to_string(),
                label: "Yes".to_string(),
                description: Some("Accept once".to_string()),
                option_type: ShowInIDEPermissionOptionType::AcceptOnce,
            },
            ShowInIDEPermissionOption {
                value: "no".to_string(),
                label: "No".to_string(),
                description: Some("Reject".to_string()),
                option_type: ShowInIDEPermissionOptionType::Reject,
            },
        ]
    }

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn show_in_ide_prompt_renders_symlink_vscode_and_hint_copy() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ShowInIDEPrompt(
                    file_path: "/repo/src/main.rs".to_string(),
                    input: "input".to_string(),
                    options: options(),
                    ide_name: "VS Code".to_string(),
                    symlink_target: Some("/outside/file.rs".to_string()),
                    cwd: "/repo".to_string(),
                    focused_option: "yes".to_string(),
                    is_supported_vscode_terminal: true,
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Opened changes in VS Code ⧉"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("outside working directory"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Save file to continue…"), "canvas=\n{text}");
        assert!(
            text.contains("make this edit to main.rs?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Esc to cancel · Tab to amend"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn show_in_ide_symlink_warning_matches_official_branches() {
        assert_eq!(
            show_in_ide_symlink_warning("/repo/target.rs", "/repo"),
            "Symlink target: /repo/target.rs"
        );
        assert_eq!(
            show_in_ide_symlink_warning("/tmp/target.rs", "/repo"),
            "This will modify /tmp/target.rs (outside working directory) via a symlink"
        );
    }

    #[test]
    fn show_in_ide_enter_uses_trimmed_accept_feedback() {
        let results = Arc::new(Mutex::new(Vec::<ShowInIDEChange>::new()));
        let results_for_handler = results.clone();

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    ShowInIDEPrompt(
                        file_path: "/repo/src/main.rs".to_string(),
                        input: "payload".to_string(),
                        options: options(),
                        ide_name: "VS Code".to_string(),
                        focused_option: "yes".to_string(),
                        accept_feedback: "  looks good  ".to_string(),
                        on_change: move |change: ShowInIDEChange| results_for_handler.lock().unwrap().push(change),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::iter(vec![key(
                        KeyCode::Enter,
                    )]))
                    .with_size(120, 24),
                ),
            );
            for _ in 0..4 {
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

        let captured = results.lock().unwrap().clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].option.option_type,
            ShowInIDEPermissionOptionType::AcceptOnce
        );
        assert_eq!(captured[0].input, "payload");
        assert_eq!(captured[0].feedback.as_deref(), Some("looks good"));
    }
}
