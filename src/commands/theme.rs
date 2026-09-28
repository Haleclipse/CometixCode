//! Maps to: CC `commands/theme/theme.tsx`.
//!
//! `ThemePickerCommand` is a `Pane` around the ThemePicker, which owns its keys
//! and the preview. Selecting sets the theme through the root ThemeProvider
//! (`setTheme`); the REPL's local-command callbacks carry CC's `onDone`.

use crate::components::design_system::pane::Pane;
use crate::components::design_system::theme_provider::use_theme;
use crate::components::theme_picker::ThemePicker;
use crate::utils::theme::{Theme, ThemeSetting};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct ThemePickerCommandProps<'a> {
    /// CC `onDone('Theme picker dismissed', { display: 'system' })`.
    pub on_close: HandlerMut<'a, ()>,
    /// CC `onDone(\`Theme set to ${setting}\`)`.
    pub on_select: HandlerMut<'a, String>,
}

/// Maps to: CC `theme.tsx:15-32` `ThemePickerCommand`.
#[component]
pub fn ThemePickerCommand<'a>(
    props: &mut ThemePickerCommandProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let palette = *hooks.use_context::<Theme>();
    let (_, theme_control) = use_theme(&hooks);
    // The picker's callbacks run inside its own render; the results reach
    // this component's `onDone` props on the next one.
    let mut selected = hooks.use_state(|| None::<ThemeSetting>);
    let mut cancelled = hooks.use_state(|| false);
    if let Some(setting) = selected.get() {
        selected.set(None);
        (props.on_select)(format!("Theme set to {}", setting.setting_value()));
    }
    if cancelled.get() {
        cancelled.set(false);
        (props.on_close)(());
    }

    element! {
        Pane(color: Some(palette.permission)) {
            ThemePicker(
                on_theme_select: move |setting: ThemeSetting| {
                    theme_control.set_theme_setting(setting);
                    selected.set(Some(setting));
                },
                on_cancel: move |_| cancelled.set(true),
                skip_exit_handling: true,
            )
        }
    }
}

/// CC `onCancel`'s message (`theme.tsx:26-28`).
pub fn handle_cancel_output() -> String {
    "Theme picker dismissed".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::design_system::theme_provider::{ThemeProvider, ThemeSaveHandler};
    use crate::utils::theme::ThemeName;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Props)]
    struct CommandHarnessProps {
        results: Option<Arc<Mutex<Vec<String>>>>,
    }

    /// The command under the providers the REPL mounts it in, with a line
    /// showing the provider's setting, the `onDone` results and a key count
    /// that makes every key produce a frame.
    #[component]
    fn CommandHarness(props: &CommandHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let results = props.results.clone().unwrap_or_default();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ThemeProvider(
                    initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                    on_theme_save: Some(Arc::new(|_| {}) as ThemeSaveHandler),
                ) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || {
                            let selected = results.clone();
                            let closed = results.clone();
                            let echo = results.clone();
                            element! {
                                View(flex_direction: FlexDirection::Column) {
                                    ThemePickerCommand(
                                        on_select: move |result: String| selected.lock().unwrap().push(result),
                                        on_close: move |_| closed.lock().unwrap().push(handle_cancel_output()),
                                    )
                                    ResultsEcho(results: Some(echo))
                                }
                            }
                            .into_any()
                        }),
                    )
                }
            }
        }
    }

    #[derive(Default, Props)]
    struct ResultsEchoProps {
        results: Option<Arc<Mutex<Vec<String>>>>,
    }

    #[component]
    fn ResultsEcho(props: &ResultsEchoProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let (_, value) = use_theme(&hooks);
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let results = props.results.clone().unwrap_or_default();
        let results = results.lock().unwrap().join(",");
        element! {
            Text(content: format!(
                "keys={} setting={} results=[{results}]",
                keys.get(),
                value.theme_setting().setting_value(),
            ))
        }
    }

    #[test]
    fn theme_command_ctrl_c_arms_the_hint_and_does_not_exit() {
        // CC ThemePicker.tsx:85-88 with `skipExitHandling`: in /theme nothing
        // else owns Ctrl+C (the prompt is unmounted, REPL's cancel is idle),
        // so the picker's hook takes it — the hint shows, and the second press
        // runs the no-op exit. The third key's frame proves the app runs on.
        let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
        ctrl_c.modifiers = KeyModifiers::CONTROL;
        let frames = futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(CommandHarness(results: Some(Arc::new(Mutex::new(Vec::new())))));
            // The mock ends on an unconsumed Ctrl+C (production ignores one:
            // CC `exitOnCtrlC: false`), so the picker must take both presses.
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 40),
            ));
            let keys_in_order = [
                TerminalEvent::Key(ctrl_c.clone()),
                TerminalEvent::Key(ctrl_c),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
            ];
            let mut frames = Vec::new();
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                if !text.contains(&format!("keys={} ", frames.len())) {
                    continue;
                }
                frames.push(text);
                let Some(event) = keys_in_order.get(frames.len() - 1) else {
                    break;
                };
                keys.send(event.clone()).await.unwrap();
            }
            frames
        });
        assert_eq!(frames.len(), 4, "the app must keep running: {frames:#?}");
        assert!(frames[1].contains("Press Ctrl-C again to exit"), "{}", frames[1]);
    }

    /// Sends one key per frame and returns the frame after the last one.
    fn run(keys_in_order: Vec<KeyCode>) -> String {
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(CommandHarness(results: Some(Arc::new(Mutex::new(Vec::new())))));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 40),
            ));
            let mut sent = 0;
            let mut last = String::new();
            while let Some(canvas) = render_loop.next().await {
                last = canvas.to_string();
                assert!(last.contains("keys="), "the echo stopped rendering:\n{last}");
                if !last.contains(&format!("keys={sent} ")) {
                    continue;
                }
                let Some(code) = keys_in_order.get(sent) else {
                    break;
                };
                keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code)))
                    .await
                    .unwrap();
                sent += 1;
            }
            last
        })
    }

    #[test]
    fn theme_command_sets_the_theme_and_reports_it() {
        // CC theme.tsx:21-24: `setTheme(setting)` then `onDone(\`Theme set to
        // ${setting}\`)`.
        let last = run(vec![KeyCode::Down, KeyCode::Enter]);
        assert!(
            last.contains("setting=light results=[Theme set to light]"),
            "{last}"
        );
    }

    #[test]
    fn theme_command_escape_reports_the_dismissal() {
        // CC theme.tsx:25-27.
        let last = run(vec![KeyCode::Esc]);
        assert!(
            last.contains("setting=dark results=[Theme picker dismissed]"),
            "{last}"
        );
    }
}
