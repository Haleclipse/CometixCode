//! Maps to: CC `components/IdeAutoConnectDialog.tsx`.
//!
//! Safety boundary: the official auto-connect dialog writes global config
//! (`autoConnectIde`, `hasIdeAutoConnectDialogBeenShown`) and the disable
//! dialog may write `autoConnectIde: false`. Nyan preserves the visible UI,
//! option order/defaults, gate helpers (in `utils::ide`), and callbacks only;
//! config writes remain deferred to the IDE runtime/settings slice.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdeAutoConnectChoice {
    Yes,
    No,
}

impl IdeAutoConnectChoice {
    pub fn auto_connect(self) -> bool {
        matches!(self, Self::Yes)
    }

    fn value(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
        }
    }
}

fn choice_from_value(value: &str) -> IdeAutoConnectChoice {
    if value == "yes" {
        IdeAutoConnectChoice::Yes
    } else {
        IdeAutoConnectChoice::No
    }
}

/// Maps to: CC `IdeAutoConnectDialog.tsx` `options`.
pub fn ide_auto_connect_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Yes".to_string(),
            value: IdeAutoConnectChoice::Yes.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No".to_string(),
            value: IdeAutoConnectChoice::No.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

/// Maps to: CC `IdeAutoConnectDialog.tsx` disable-dialog `options`.
pub fn ide_disable_auto_connect_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "No".to_string(),
            value: IdeAutoConnectChoice::No.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Yes".to_string(),
            value: IdeAutoConnectChoice::Yes.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

#[derive(Default, Props)]
pub struct IdeAutoConnectDialogProps<'a> {
    /// Called with `Some(selected_auto_connect)` when the user selects an
    /// option. Called with `None` for the official cancel path, which invokes
    /// `onComplete()` without saving a preference.
    pub on_complete: HandlerMut<'a, Option<bool>>,
}

#[derive(Default, Props)]
pub struct IdeDisableAutoConnectDialogProps<'a> {
    /// Maps to official `onComplete(disableAutoConnect)`.
    pub on_complete: HandlerMut<'a, bool>,
}

fn handle_select_events(
    hooks: &mut Hooks,
    options: Vec<SelectOptionData>,
    focused_index: State<usize>,
    pending_choice: State<Option<IdeAutoConnectChoice>>,
) {
    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_choice = pending_choice;
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
}

/// Maps to: CC `components/IdeAutoConnectDialog.tsx` `IdeAutoConnectDialog`.
#[component]
pub fn IdeAutoConnectDialog<'a>(
    props: &mut IdeAutoConnectDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<IdeAutoConnectChoice>::None);
    let mut pending_cancel = hooks.use_state(|| false);
    let options = ide_auto_connect_options();
    let option_count = options.len().max(1);

    handle_select_events(&mut hooks, options.clone(), focused_index, pending_choice);

    let choice = { *pending_choice.read() };
    if let Some(choice) = choice {
        pending_choice.set(None);
        (props.on_complete)(Some(choice.auto_connect()));
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_complete)(None);
    }

    element! {
        Dialog(
            title: "Do you wish to enable auto-connect to IDE?".to_string(),
            color: Some(theme.ide),
            on_cancel: move |_| pending_cancel.set(true),
        ) {
            Select(
                options: options,
                focused_index: focused_index.get().min(option_count - 1),
                selected_value: Some("yes".to_string()),
                visible_option_count: option_count,
                layout: SelectLayout::CompactVertical,
                hide_indexes: true,
            )
            Text(
                content: "You can also configure this in /config or with the --ide flag".to_string(),
                dim: true,
                wrap: TextWrap::NoWrap,
            )
        }
    }
}

/// Maps to: CC `components/IdeAutoConnectDialog.tsx`
/// `IdeDisableAutoConnectDialog`.
#[component]
pub fn IdeDisableAutoConnectDialog<'a>(
    props: &mut IdeDisableAutoConnectDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<IdeAutoConnectChoice>::None);
    let mut pending_cancel = hooks.use_state(|| false);
    let options = ide_disable_auto_connect_options();
    let option_count = options.len().max(1);

    handle_select_events(&mut hooks, options.clone(), focused_index, pending_choice);

    let choice = { *pending_choice.read() };
    if let Some(choice) = choice {
        pending_choice.set(None);
        (props.on_complete)(choice.auto_connect());
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_complete)(false);
    }

    element! {
        Dialog(
            title: "Do you wish to disable auto-connect to IDE?".to_string(),
            subtitle: Some("You can also configure this in /config".to_string()),
            color: Some(theme.ide),
            on_cancel: move |_| pending_cancel.set(true),
        ) {
            Select(
                options: options,
                focused_index: focused_index.get().min(option_count - 1),
                selected_value: Some("no".to_string()),
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
    fn ide_auto_connect_dialog_renders_official_copy_and_default_yes() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                IdeAutoConnectDialog()
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Do you wish to enable auto-connect to IDE?"),
            "canvas=\n{text}"
        );
        // CC design-system/ListItem.tsx:179 uses installed figures.tick: U+2714,
        // confirmed with the actual Node dependency; the old U+2713 was not its output.
        assert!(text.contains("Yes ✔"), "canvas=\n{text}");
        assert!(text.contains("No"), "canvas=\n{text}");
        assert!(
            text.contains("You can also configure this in /config or with the --ide flag"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn ide_disable_auto_connect_dialog_renders_official_copy_and_default_no() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                IdeDisableAutoConnectDialog()
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Do you wish to disable auto-connect to IDE?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("You can also configure this in /config"),
            "canvas=\n{text}"
        );
        // CC design-system/ListItem.tsx:179 uses installed figures.tick: U+2714,
        // confirmed with the actual Node dependency; the old U+2713 was not its output.
        assert!(text.contains("No ✔"), "canvas=\n{text}");
        assert!(text.contains("Yes"), "canvas=\n{text}");
    }

    #[test]
    fn ide_auto_connect_dialog_enter_reports_yes_and_down_enter_reports_no() {
        for (events, expected) in [
            (vec![key(KeyCode::Enter)], true),
            (vec![key(KeyCode::Down), key(KeyCode::Enter)], false),
        ] {
            let completions = Arc::new(Mutex::new(Vec::<Option<bool>>::new()));
            let completions_for_handler = Arc::clone(&completions);

            futures::executor::block_on(async move {
                let mut app = element! {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        IdeAutoConnectDialog(on_complete: move |auto_connect| {
                            completions_for_handler.lock().expect("completions mutex").push(auto_connect);
                        })
                    }
                };
                let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(events)).with_size(120, 24),
                ));
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
                &[Some(expected)]
            );
        }
    }

    #[test]
    fn ide_auto_connect_dialog_escape_reports_no_preference_saved() {
        let completions = Arc::new(Mutex::new(Vec::<Option<bool>>::new()));
        let completions_for_handler = Arc::clone(&completions);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        IdeAutoConnectDialog(on_complete: move |auto_connect| {
                            completions_for_handler.lock().expect("completions mutex").push(auto_connect);
                        })
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Esc)]))
                        .with_size(120, 24),
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

    #[test]
    fn ide_disable_auto_connect_dialog_escape_reports_false_and_yes_reports_true() {
        for (events, expected) in [
            (vec![key(KeyCode::Esc)], false),
            (vec![key(KeyCode::Down), key(KeyCode::Enter)], true),
        ] {
            let completions = Arc::new(Mutex::new(Vec::<bool>::new()));
            let completions_for_handler = Arc::clone(&completions);

            futures::executor::block_on(async move {
                let mut app = element! {
                    ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                        ContextProvider(value: Context::owned(*theme::current())) {
                            IdeDisableAutoConnectDialog(on_complete: move |disable| {
                                completions_for_handler.lock().expect("completions mutex").push(disable);
                            })
                        }
                    }
                };
                let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(events)).with_size(120, 24),
                ));
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
                &[expected]
            );
        }
    }
}
