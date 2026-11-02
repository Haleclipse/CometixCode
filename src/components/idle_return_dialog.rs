//! Maps to: CC `components/IdleReturnDialog.tsx`.
//!
//! This component is callback-only like the official component boundary: REPL
//! owns telemetry, settings persistence for the `never` action, and any context
//! clearing/session behavior.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::format::format_tokens;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleReturnAction {
    Continue,
    Clear,
    Dismiss,
    Never,
}

impl IdleReturnAction {
    pub fn value(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Clear => "clear",
            Self::Dismiss => "dismiss",
            Self::Never => "never",
        }
    }
}

/// Maps to: CC `components/IdleReturnDialog.tsx` `formatIdleDuration`.
pub fn format_idle_duration(minutes: f64) -> String {
    if minutes < 1.0 {
        return "< 1m".to_string();
    }
    if minutes < 60.0 {
        return format!("{}m", minutes.floor() as u64);
    }
    let hours = (minutes / 60.0).floor() as u64;
    let remaining_minutes = (minutes % 60.0).floor() as u64;
    if remaining_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remaining_minutes}m")
    }
}

/// Maps to: CC `components/IdleReturnDialog.tsx` `Select` options.
pub fn idle_return_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            value: IdleReturnAction::Continue.value().to_string(),
            label: "Continue this conversation".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            value: IdleReturnAction::Clear.value().to_string(),
            label: "Send message as a new conversation".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            value: IdleReturnAction::Never.value().to_string(),
            label: "Don't ask me again".to_string(),
            ..SelectOptionData::default()
        },
    ]
}

fn action_from_value(value: &str) -> IdleReturnAction {
    match value {
        "clear" => IdleReturnAction::Clear,
        "never" => IdleReturnAction::Never,
        "dismiss" => IdleReturnAction::Dismiss,
        _ => IdleReturnAction::Continue,
    }
}

#[derive(Default, Props)]
pub struct IdleReturnDialogProps<'a> {
    pub idle_minutes: f64,
    pub total_input_tokens: u64,
    pub on_done: HandlerMut<'a, IdleReturnAction>,
}

/// Maps to: CC `components/IdleReturnDialog.tsx` `IdleReturnDialog`.
#[component]
pub fn IdleReturnDialog<'a>(
    props: &mut IdleReturnDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_action = hooks.use_state(|| Option::<IdleReturnAction>::None);
    let options = idle_return_options();
    let option_count = options.len().max(1);
    let formatted_idle = format_idle_duration(props.idle_minutes);
    let formatted_tokens = format_tokens(props.total_input_tokens);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_action = pending_action;
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
                        pending_action.set(Some(action_from_value(&option.value)));
                    }
                }
                _ => {}
            }
        }
    });

    let action = { *pending_action.read() };
    if let Some(action) = action {
        pending_action.set(None);
        (props.on_done)(action);
    }

    element! {
        Dialog(
            title: format!("You've been away {formatted_idle} and this conversation is {formatted_tokens} tokens."),
            on_cancel: move |_| {
                pending_action.set(Some(IdleReturnAction::Dismiss));
            },
        ) {
            View(flex_direction: FlexDirection::Column) {
                Text(
                    content: "If this is a new task, clearing context will save usage and be faster.".to_string(),
                    wrap: TextWrap::Wrap,
                )
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
    fn idle_return_format_idle_duration_matches_official_boundaries() {
        assert_eq!(format_idle_duration(0.5), "< 1m");
        assert_eq!(format_idle_duration(1.9), "1m");
        assert_eq!(format_idle_duration(59.9), "59m");
        assert_eq!(format_idle_duration(60.0), "1h");
        assert_eq!(format_idle_duration(125.5), "2h 5m");
    }

    #[test]
    fn idle_return_options_match_official_copy_and_order() {
        let options = idle_return_options();

        assert_eq!(options[0].value, "continue");
        assert_eq!(options[0].label, "Continue this conversation");
        assert_eq!(options[1].value, "clear");
        assert_eq!(options[1].label, "Send message as a new conversation");
        assert_eq!(options[2].value, "never");
        assert_eq!(options[2].label, "Don't ask me again");
    }

    #[test]
    fn idle_return_dialog_renders_official_title_body_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                IdleReturnDialog(idle_minutes: 125.0, total_input_tokens: 12_500u64)
            }
        }
        .render(Some(110))
        .to_string();

        assert!(
            text.contains("You've been away 2h 5m and this conversation is 12.5k tokens."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("If this is a new task, clearing context will save usage and be faster."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Continue this conversation"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Send message as a new conversation"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Don't ask me again"), "canvas=\n{text}");
    }

    #[test]
    fn idle_return_dialog_default_enter_returns_continue() {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let actions_for_handler = Arc::clone(&actions);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    IdleReturnDialog(
                        idle_minutes: 5.0,
                        total_input_tokens: 1_000u64,
                        on_done: move |action| {
                            actions_for_handler.lock().expect("actions mutex").push(action);
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
            actions.lock().expect("actions mutex").as_slice(),
            &[IdleReturnAction::Continue]
        );
    }

    #[test]
    fn idle_return_dialog_down_down_enter_returns_never() {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let actions_for_handler = Arc::clone(&actions);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    IdleReturnDialog(
                        idle_minutes: 5.0,
                        total_input_tokens: 1_000u64,
                        on_done: move |action| {
                            actions_for_handler.lock().expect("actions mutex").push(action);
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Down),
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
            actions.lock().expect("actions mutex").as_slice(),
            &[IdleReturnAction::Never]
        );
    }
}
