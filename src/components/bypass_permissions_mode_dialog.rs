//! Maps to: CC `components/BypassPermissionsModeDialog.tsx`.
//!
//! Safety boundary: the official dialog logs analytics, writes
//! `skipDangerousModePermissionPrompt` to user settings on accept, and calls
//! `gracefulShutdownSync(...)` on decline/Escape. Nyan preserves the dialog,
//! option, and `onAccept` boundary only. Decline/Escape intentionally perform no
//! process exit or settings write in this safe UI slice.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BypassPermissionsModeChoice {
    Accept,
    Decline,
}

impl BypassPermissionsModeChoice {
    fn value(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
        }
    }
}

/// Maps to: CC `components/BypassPermissionsModeDialog.tsx` `Select` options.
pub fn bypass_permissions_mode_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "No, exit".to_string(),
            value: BypassPermissionsModeChoice::Decline.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Yes, I accept".to_string(),
            value: BypassPermissionsModeChoice::Accept.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

fn choice_from_value(value: &str) -> BypassPermissionsModeChoice {
    match value {
        "accept" => BypassPermissionsModeChoice::Accept,
        _ => BypassPermissionsModeChoice::Decline,
    }
}

#[derive(Default, Props)]
pub struct BypassPermissionsModeDialogProps<'a> {
    pub on_accept: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/BypassPermissionsModeDialog.tsx`
/// `BypassPermissionsModeDialog`.
#[component]
pub fn BypassPermissionsModeDialog<'a>(
    props: &mut BypassPermissionsModeDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<BypassPermissionsModeChoice>::None);
    let options = bypass_permissions_mode_options();
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
            BypassPermissionsModeChoice::Accept => {
                (props.on_accept)(());
            }
            BypassPermissionsModeChoice::Decline => {
                // Official path: `gracefulShutdownSync(1)` from select or
                // `gracefulShutdownSync(0)` from Escape. Safe Nyan path:
                // no process exit from render-only dialog slice.
            }
        }
    }

    element! {
        Dialog(
            title: "WARNING: nyan running in Bypass Permissions mode".to_string(),
            color: Some(theme.error),
            on_cancel: move |_| {
                pending_choice.set(Some(BypassPermissionsModeChoice::Decline));
            },
        ) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(
                    content: "In Bypass Permissions mode, nyan will not ask for your approval\nbefore running potentially dangerous commands.\nThis mode should only be used in a sandboxed container/VM that has\nrestricted internet access and can easily be restored if damaged.".to_string(),
                    wrap: TextWrap::Wrap,
                )
                Text(
                    content: "By proceeding, you accept all responsibility for actions taken while\nrunning in Bypass Permissions mode.".to_string(),
                    wrap: TextWrap::Wrap,
                )
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
    fn bypass_permissions_options_match_official_copy_and_order() {
        let options = bypass_permissions_mode_options();

        assert_eq!(options[0].label, "No, exit");
        assert_eq!(options[0].value, "decline");
        assert_eq!(options[1].label, "Yes, I accept");
        assert_eq!(options[1].value, "accept");
    }

    #[test]
    fn bypass_permissions_dialog_renders_official_warning_copy_link_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                BypassPermissionsModeDialog()
            }
        }
        .render(Some(110))
        .to_string();

        assert!(
            text.contains("WARNING: nyan running in Bypass Permissions mode"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("nyan will not ask for your approval"),
            "canvas=\n{text}"
        );
        assert!(text.contains("sandboxed container/VM"), "canvas=\n{text}");
        assert!(
            text.contains("accept all responsibility"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("https://code.claude.com/docs/en/security"),
            "canvas=\n{text}"
        );
        assert!(text.contains("No, exit"), "canvas=\n{text}");
        assert!(text.contains("Yes, I accept"), "canvas=\n{text}");
    }

    #[test]
    fn bypass_permissions_dialog_down_enter_accepts_without_settings_or_shutdown_side_effects() {
        let accepted = Arc::new(Mutex::new(0usize));
        let accepted_for_handler = Arc::clone(&accepted);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    BypassPermissionsModeDialog(
                        on_accept: move |_| {
                            *accepted_for_handler.lock().expect("accepted mutex") += 1;
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

        assert_eq!(*accepted.lock().expect("accepted mutex"), 1);
    }

    #[test]
    fn bypass_permissions_dialog_default_enter_declines_without_accepting() {
        let accepted = Arc::new(Mutex::new(0usize));
        let accepted_for_handler = Arc::clone(&accepted);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    BypassPermissionsModeDialog(
                        on_accept: move |_| {
                            *accepted_for_handler.lock().expect("accepted mutex") += 1;
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

        assert_eq!(*accepted.lock().expect("accepted mutex"), 0);
    }
}
