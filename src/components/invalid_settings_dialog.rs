//! Maps to: CC `components/InvalidSettingsDialog.tsx`.
//!
//! The official dialog lets the caller decide whether `onExit` terminates the
//! process and whether `onContinue` skips invalid files. This component mirrors
//! the render/selection boundary only.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::components::validation_errors_list::ValidationErrorsList;
use crate::utils::settings::ValidationError;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidSettingsChoice {
    Exit,
    Continue,
}

impl InvalidSettingsChoice {
    fn value(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Continue => "continue",
        }
    }
}

/// Maps to: CC `components/InvalidSettingsDialog.tsx` `Select` options.
pub fn invalid_settings_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Exit and fix manually".to_string(),
            value: InvalidSettingsChoice::Exit.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Continue without these settings".to_string(),
            value: InvalidSettingsChoice::Continue.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

fn choice_from_value(value: &str) -> InvalidSettingsChoice {
    match value {
        "continue" => InvalidSettingsChoice::Continue,
        _ => InvalidSettingsChoice::Exit,
    }
}

#[derive(Default, Props)]
pub struct InvalidSettingsDialogProps<'a> {
    pub settings_errors: Vec<ValidationError>,
    pub on_continue: HandlerMut<'a, ()>,
    pub on_exit: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/InvalidSettingsDialog.tsx` `InvalidSettingsDialog`.
#[component]
pub fn InvalidSettingsDialog<'a>(
    props: &mut InvalidSettingsDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<InvalidSettingsChoice>::None);
    let options = invalid_settings_options();
    let option_count = options.len().max(1);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_choice = pending_choice;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(option) = options.get(focused_index.get()) {
                        pending_choice.set(Some(choice_from_value(&option.value)));
                    }
                }
                _ => {}
            }
        }
    });

    let choice = { *pending_choice.read() };
    if let Some(choice) = choice {
        pending_choice.set(None);
        match choice {
            InvalidSettingsChoice::Exit => (props.on_exit)(()),
            InvalidSettingsChoice::Continue => (props.on_continue)(()),
        }
    }

    element! {
        Dialog(
            title: "Settings Error".to_string(),
            color: Some(theme.warning),
            on_cancel: move |_| {
                pending_choice.set(Some(InvalidSettingsChoice::Exit));
            },
        ) {
            ValidationErrorsList(errors: props.settings_errors.clone())
            Text(
                content: "Files with errors are skipped entirely, not just the invalid settings.".to_string(),
                color: theme.inactive,
                wrap: TextWrap::Wrap,
            )
            Select(
                options: options,
                focused_index: focused_index.get().min(option_count - 1),
                visible_option_count: option_count,
                layout: SelectLayout::CompactVertical,
                hide_indexes: true,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn validation_error(path: &str) -> ValidationError {
        ValidationError {
            file: Some("settings.json".to_string()),
            path: path.to_string(),
            message: "Invalid setting".to_string(),
            expected: None,
            invalid_value: None,
            suggestion: None,
        }
    }

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn invalid_settings_options_match_official_copy_and_order() {
        let options = invalid_settings_options();

        assert_eq!(options[0].label, "Exit and fix manually");
        assert_eq!(options[0].value, "exit");
        assert_eq!(options[1].label, "Continue without these settings");
        assert_eq!(options[1].value, "continue");
    }

    #[test]
    fn invalid_settings_dialog_renders_official_title_errors_notice_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                InvalidSettingsDialog(settings_errors: vec![validation_error("model")])
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Settings Error"), "canvas=\n{text}");
        assert!(text.contains("settings.json"), "canvas=\n{text}");
        assert!(text.contains("model: Invalid setting"), "canvas=\n{text}");
        assert!(
            text.contains("Files with errors are skipped entirely, not just the invalid settings."),
            "canvas=\n{text}"
        );
        assert!(text.contains("Exit and fix manually"), "canvas=\n{text}");
        assert!(
            text.contains("Continue without these settings"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn invalid_settings_dialog_default_enter_calls_exit() {
        let exited = Arc::new(Mutex::new(0usize));
        let continued = Arc::new(Mutex::new(0usize));
        let exited_for_handler = Arc::clone(&exited);
        let continued_for_handler = Arc::clone(&continued);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    InvalidSettingsDialog(
                        settings_errors: vec![validation_error("model")],
                        on_exit: move |_| {
                            *exited_for_handler.lock().expect("exited mutex") += 1;
                        },
                        on_continue: move |_| {
                            *continued_for_handler.lock().expect("continued mutex") += 1;
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(100, 24),
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

        assert_eq!(*exited.lock().expect("exited mutex"), 1);
        assert_eq!(*continued.lock().expect("continued mutex"), 0);
    }

    #[test]
    fn invalid_settings_dialog_down_enter_calls_continue() {
        let exited = Arc::new(Mutex::new(0usize));
        let continued = Arc::new(Mutex::new(0usize));
        let exited_for_handler = Arc::clone(&exited);
        let continued_for_handler = Arc::clone(&continued);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    InvalidSettingsDialog(
                        settings_errors: vec![validation_error("model")],
                        on_exit: move |_| {
                            *exited_for_handler.lock().expect("exited mutex") += 1;
                        },
                        on_continue: move |_| {
                            *continued_for_handler.lock().expect("continued mutex") += 1;
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Down),
                        key(KeyCode::Enter),
                    ]))
                    .with_size(100, 24),
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

        assert_eq!(*exited.lock().expect("exited mutex"), 0);
        assert_eq!(*continued.lock().expect("continued mutex"), 1);
    }
}
