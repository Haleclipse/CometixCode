//! Maps to: CC `components/DevChannelsDialog.tsx`.
//!
//! Safety boundary: the official component calls `gracefulShutdownSync(1)` for
//! the Exit option and `gracefulShutdownSync(0)` on Escape. Nyan preserves
//! the official warning, channel list formatting, option order, and accept
//! callback; exit/Escape intentionally do not terminate the process in this safe
//! UI slice.

use crate::bootstrap::state::ChannelEntry;
use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevChannelsChoice {
    Accept,
    Exit,
}

impl DevChannelsChoice {
    fn value(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Exit => "exit",
        }
    }
}

/// Maps to: CC `components/DevChannelsDialog.tsx` channel list formatter.
pub fn format_dev_channel_entry(entry: &ChannelEntry) -> String {
    match entry {
        ChannelEntry::Plugin {
            name, marketplace, ..
        } => format!("plugin:{name}@{marketplace}"),
        ChannelEntry::Server { name, .. } => format!("server:{name}"),
    }
}

/// Maps to: CC `components/DevChannelsDialog.tsx` `Select` options.
pub fn dev_channels_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "I am using this for local development".to_string(),
            value: DevChannelsChoice::Accept.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Exit".to_string(),
            value: DevChannelsChoice::Exit.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

fn choice_from_value(value: &str) -> DevChannelsChoice {
    match value {
        "accept" => DevChannelsChoice::Accept,
        _ => DevChannelsChoice::Exit,
    }
}

#[derive(Default, Props)]
pub struct DevChannelsDialogProps<'a> {
    pub channels: Vec<ChannelEntry>,
    pub on_accept: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/DevChannelsDialog.tsx` `DevChannelsDialog`.
#[component]
pub fn DevChannelsDialog<'a>(
    props: &mut DevChannelsDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<DevChannelsChoice>::None);
    let options = dev_channels_options();
    let option_count = options.len().max(1);
    let channel_list = props
        .channels
        .iter()
        .map(format_dev_channel_entry)
        .collect::<Vec<_>>()
        .join(", ");

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
            DevChannelsChoice::Accept => {
                (props.on_accept)(());
            }
            DevChannelsChoice::Exit => {
                // Official path: `gracefulShutdownSync(1)`. Safe Nyan path:
                // do not exit from this render-only dialog boundary.
            }
        }
    }

    element! {
        Dialog(
            title: "WARNING: Loading development channels".to_string(),
            color: Some(theme.error),
            on_cancel: move |_| {
                // Official path: `gracefulShutdownSync(0)`. Safe Nyan path:
                // no process exit.
                pending_choice.set(Some(DevChannelsChoice::Exit));
            },
        ) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(
                    content: "--dangerously-load-development-channels is for local channel\ndevelopment only. Do not use this option to run channels you have\ndownloaded off the internet.".to_string(),
                    wrap: TextWrap::Wrap,
                )
                Text(
                    content: "Please use --channels to run a list of approved channels.".to_string(),
                    wrap: TextWrap::Wrap,
                )
                Text(
                    content: format!("Channels: {channel_list}"),
                    color: theme.inactive,
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
    fn dev_channels_formats_channel_entries_like_official_dialog() {
        let plugin = ChannelEntry::plugin("mailbox", "anthropic", true);
        let server = ChannelEntry::server("planner", true);

        assert_eq!(
            format_dev_channel_entry(&plugin),
            "plugin:mailbox@anthropic"
        );
        assert_eq!(format_dev_channel_entry(&server), "server:planner");
    }

    #[test]
    fn dev_channels_options_match_official_copy_and_order() {
        let options = dev_channels_options();

        assert_eq!(options[0].label, "I am using this for local development");
        assert_eq!(options[0].value, "accept");
        assert_eq!(options[1].label, "Exit");
        assert_eq!(options[1].value, "exit");
    }

    #[test]
    fn dev_channels_dialog_renders_official_warning_channels_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                DevChannelsDialog(
                    channels: vec![
                        ChannelEntry::plugin("mailbox", "anthropic", true),
                        ChannelEntry::server("planner", true),
                    ],
                )
            }
        }
        .render(Some(110))
        .to_string();

        assert!(
            text.contains("WARNING: Loading development channels"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("--dangerously-load-development-channels is for local channel"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("downloaded off the internet"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Please use --channels to run a list of approved channels."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Channels: plugin:mailbox@anthropic, server:planner"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("I am using this for local development"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Exit"), "canvas=\n{text}");
    }

    #[test]
    fn dev_channels_dialog_default_enter_accepts() {
        let accepted = Arc::new(Mutex::new(0usize));
        let accepted_for_handler = Arc::clone(&accepted);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    DevChannelsDialog(
                        channels: vec![ChannelEntry::server("planner", true)],
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

        assert_eq!(*accepted.lock().expect("accepted mutex"), 1);
    }

    #[test]
    fn dev_channels_dialog_exit_option_does_not_accept_or_shutdown_in_safe_slice() {
        let accepted = Arc::new(Mutex::new(0usize));
        let accepted_for_handler = Arc::clone(&accepted);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    DevChannelsDialog(
                        channels: vec![ChannelEntry::server("planner", true)],
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

        assert_eq!(*accepted.lock().expect("accepted mutex"), 0);
    }
}
