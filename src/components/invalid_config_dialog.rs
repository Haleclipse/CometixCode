//! Maps to: CC `components/InvalidConfigDialog.tsx`.
//!
//! Safety boundary: official `showInvalidConfigDialog` renders an Ink app,
//! writes `defaultConfig` to disk for reset, and calls `process.exit(...)`.
//! Nyan ports the dialog, options, safe theme constant, and exported wrapper
//! boundary, but does not write files or terminate the process in this slice.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::errors::ConfigParseError;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Maps to: CC `components/InvalidConfigDialog.tsx` `SAFE_ERROR_THEME_NAME`.
pub const SAFE_ERROR_THEME_NAME: &str = "dark";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidConfigChoice {
    Exit,
    Reset,
}

impl InvalidConfigChoice {
    fn value(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Reset => "reset",
        }
    }
}

/// Maps to: CC `components/InvalidConfigDialog.tsx` `Select` options.
pub fn invalid_config_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Exit and fix manually".to_string(),
            value: InvalidConfigChoice::Exit.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Reset with default configuration".to_string(),
            value: InvalidConfigChoice::Reset.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

fn choice_from_value(value: &str) -> InvalidConfigChoice {
    match value {
        "reset" => InvalidConfigChoice::Reset,
        _ => InvalidConfigChoice::Exit,
    }
}

#[derive(Default, Props)]
pub struct InvalidConfigDialogProps<'a> {
    pub file_path: String,
    pub error_description: String,
    pub on_exit: HandlerMut<'a, ()>,
    pub on_reset: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/InvalidConfigDialog.tsx` `InvalidConfigDialog`.
#[component]
pub fn InvalidConfigDialog<'a>(
    props: &mut InvalidConfigDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<InvalidConfigChoice>::None);
    let options = invalid_config_options();
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
            InvalidConfigChoice::Exit => (props.on_exit)(()),
            InvalidConfigChoice::Reset => (props.on_reset)(()),
        }
    }

    element! {
        Dialog(
            title: "Configuration Error".to_string(),
            color: Some(theme.error),
            on_cancel: move |_| {
                pending_choice.set(Some(InvalidConfigChoice::Exit));
            },
        ) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(
                    content: format!("The configuration file at {} contains invalid JSON.", props.file_path),
                    wrap: TextWrap::Wrap,
                )
                Text(content: props.error_description.clone(), wrap: TextWrap::Wrap)
            }
            View(flex_direction: FlexDirection::Column) {
                Text(content: "Choose an option:".to_string(), weight: Weight::Bold)
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
}

/// Maps to: CC `components/InvalidConfigDialog.tsx` `showInvalidConfigDialog`.
///
/// Official behavior mounts the dialog, writes default config on reset, unmounts,
/// and exits the process. The Nyan safe wrapper records the same entrypoint
/// and returns without rendering, writing, or exiting until a dedicated startup
/// error runtime slice wires this into the app shell.
pub async fn show_invalid_config_dialog(_error: ConfigParseError) {}

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

    #[test]
    fn invalid_config_options_match_official_copy_and_order() {
        let options = invalid_config_options();

        assert_eq!(options[0].label, "Exit and fix manually");
        assert_eq!(options[0].value, "exit");
        assert_eq!(options[1].label, "Reset with default configuration");
        assert_eq!(options[1].value, "reset");
    }

    #[test]
    fn invalid_config_dialog_renders_official_title_body_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                InvalidConfigDialog(
                    file_path: "/tmp/state.json".to_string(),
                    error_description: "Unexpected token }".to_string(),
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Configuration Error"), "canvas=\n{text}");
        assert!(
            text.contains("The configuration file at /tmp/state.json contains invalid JSON."),
            "canvas=\n{text}"
        );
        assert!(text.contains("Unexpected token }"), "canvas=\n{text}");
        assert!(text.contains("Choose an option:"), "canvas=\n{text}");
        assert!(text.contains("Exit and fix manually"), "canvas=\n{text}");
        assert!(
            text.contains("Reset with default configuration"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn invalid_config_dialog_default_enter_calls_exit() {
        let exited = Arc::new(Mutex::new(0usize));
        let reset = Arc::new(Mutex::new(0usize));
        let exited_for_handler = Arc::clone(&exited);
        let reset_for_handler = Arc::clone(&reset);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    InvalidConfigDialog(
                        file_path: "/tmp/state.json".to_string(),
                        error_description: "Unexpected token".to_string(),
                        on_exit: move |_| {
                            *exited_for_handler.lock().expect("exited mutex") += 1;
                        },
                        on_reset: move |_| {
                            *reset_for_handler.lock().expect("reset mutex") += 1;
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
        assert_eq!(*reset.lock().expect("reset mutex"), 0);
    }

    #[test]
    fn invalid_config_dialog_down_enter_calls_reset_without_writing_or_exiting() {
        let exited = Arc::new(Mutex::new(0usize));
        let reset = Arc::new(Mutex::new(0usize));
        let exited_for_handler = Arc::clone(&exited);
        let reset_for_handler = Arc::clone(&reset);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    InvalidConfigDialog(
                        file_path: "/tmp/state.json".to_string(),
                        error_description: "Unexpected token".to_string(),
                        on_exit: move |_| {
                            *exited_for_handler.lock().expect("exited mutex") += 1;
                        },
                        on_reset: move |_| {
                            *reset_for_handler.lock().expect("reset mutex") += 1;
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
        assert_eq!(*reset.lock().expect("reset mutex"), 1);
    }

    #[test]
    fn invalid_config_safe_theme_matches_official_dark_constant() {
        assert_eq!(SAFE_ERROR_THEME_NAME, "dark");
    }
}
