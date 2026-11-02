//! Maps to: CC `components/AutoModeOptInDialog.tsx`.
//!
//! Safety boundary: official `onChange(...)` logs analytics and writes user
//! settings (`skipAutoPermissionPrompt`, optionally `permissions.defaultMode =
//! 'auto'`) before invoking callbacks. Nyan preserves the dialog/options and
//! callback boundary only; no telemetry or settings writes are performed here.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Maps to: CC `components/AutoModeOptInDialog.tsx`
/// `AUTO_MODE_DESCRIPTION`.
pub const AUTO_MODE_DESCRIPTION: &str = "Auto mode lets Claude handle permission prompts automatically — Claude checks each tool call for risky actions and prompt injection before executing. Actions Claude identifies as safe are executed, while actions Claude identifies as risky are blocked and Claude may try a different approach. Ideal for long-running tasks. Sessions are slightly more expensive. Claude can make mistakes that allow harmful commands to run, it's recommended to only use in isolated environments. Shift+Tab to change mode.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoModeOptInChoice {
    Accept,
    AcceptDefault,
    Decline,
}

impl AutoModeOptInChoice {
    fn value(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::AcceptDefault => "accept-default",
            Self::Decline => "decline",
        }
    }
}

/// Maps to: CC `components/AutoModeOptInDialog.tsx` `Select` options.
pub fn auto_mode_opt_in_options(decline_exits: bool) -> Vec<SelectOptionData> {
    // The rebuilt CC source has the product gate inlined as
    // `"external" !== 'ant'`, so the default-mode option is present.
    vec![
        SelectOptionData {
            label: "Yes, and make it my default mode".to_string(),
            value: AutoModeOptInChoice::AcceptDefault.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Yes, enable auto mode".to_string(),
            value: AutoModeOptInChoice::Accept.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: if decline_exits {
                "No, exit"
            } else {
                "No, go back"
            }
            .to_string(),
            value: AutoModeOptInChoice::Decline.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

fn choice_from_value(value: &str) -> AutoModeOptInChoice {
    match value {
        "accept-default" => AutoModeOptInChoice::AcceptDefault,
        "accept" => AutoModeOptInChoice::Accept,
        _ => AutoModeOptInChoice::Decline,
    }
}

#[derive(Default, Props)]
pub struct AutoModeOptInDialogProps<'a> {
    pub decline_exits: bool,
    pub on_accept: HandlerMut<'a, AutoModeOptInChoice>,
    pub on_decline: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/AutoModeOptInDialog.tsx` `AutoModeOptInDialog`.
#[component]
pub fn AutoModeOptInDialog<'a>(
    props: &mut AutoModeOptInDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<AutoModeOptInChoice>::None);
    let options = auto_mode_opt_in_options(props.decline_exits);
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
            AutoModeOptInChoice::Accept | AutoModeOptInChoice::AcceptDefault => {
                (props.on_accept)(choice);
            }
            AutoModeOptInChoice::Decline => {
                (props.on_decline)(());
            }
        }
    }

    element! {
        Dialog(
            title: "Enable auto mode?".to_string(),
            color: Some(theme.warning),
            on_cancel: move |_| {
                pending_choice.set(Some(AutoModeOptInChoice::Decline));
            },
        ) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(content: AUTO_MODE_DESCRIPTION.to_string(), wrap: TextWrap::Wrap)
                Link(url: "https://code.claude.com/docs/en/security".to_string())
            }
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

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn auto_mode_options_match_official_copy_and_decline_label() {
        let options = auto_mode_opt_in_options(false);
        assert_eq!(options[0].label, "Yes, and make it my default mode");
        assert_eq!(options[0].value, "accept-default");
        assert_eq!(options[1].label, "Yes, enable auto mode");
        assert_eq!(options[1].value, "accept");
        assert_eq!(options[2].label, "No, go back");

        let exit_options = auto_mode_opt_in_options(true);
        assert_eq!(exit_options[2].label, "No, exit");
    }

    #[test]
    fn auto_mode_dialog_renders_official_description_link_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                AutoModeOptInDialog()
            }
        }
        .render(Some(110))
        .to_string();

        assert!(text.contains("Enable auto mode?"), "canvas=\n{text}");
        assert!(
            text.contains("Auto mode lets Claude handle permission prompts automatically"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("https://code.claude.com/docs/en/security"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Yes, and make it my default mode"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Yes, enable auto mode"), "canvas=\n{text}");
        assert!(text.contains("No, go back"), "canvas=\n{text}");
    }

    #[test]
    fn auto_mode_dialog_enter_accepts_default_without_settings_write() {
        let accepted = Arc::new(Mutex::new(Vec::<AutoModeOptInChoice>::new()));
        let declined = Arc::new(Mutex::new(0usize));
        let accepted_for_handler = Arc::clone(&accepted);
        let declined_for_handler = Arc::clone(&declined);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    AutoModeOptInDialog(
                        on_accept: move |choice: AutoModeOptInChoice| {
                            accepted_for_handler.lock().expect("accepted mutex").push(choice);
                        },
                        on_decline: move |_| {
                            *declined_for_handler.lock().expect("declined mutex") += 1;
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(110, 24),
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
            accepted.lock().expect("accepted mutex").as_slice(),
            &[AutoModeOptInChoice::AcceptDefault]
        );
        assert_eq!(*declined.lock().expect("declined mutex"), 0);
    }

    #[test]
    fn auto_mode_dialog_can_select_accept_and_decline() {
        let accepted = Arc::new(Mutex::new(Vec::<AutoModeOptInChoice>::new()));
        let accepted_for_handler = Arc::clone(&accepted);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    AutoModeOptInDialog(
                        on_accept: move |choice: AutoModeOptInChoice| {
                            accepted_for_handler.lock().expect("accepted mutex").push(choice);
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
                    .with_size(110, 24),
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
            accepted.lock().expect("accepted mutex").as_slice(),
            &[AutoModeOptInChoice::Accept]
        );
    }
}
