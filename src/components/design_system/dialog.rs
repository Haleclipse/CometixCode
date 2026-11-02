//! Maps to: CC `components/design-system/Dialog.tsx`.
//! Dialog shell built on Pane. When rendered inside FullscreenLayout's modal
//! context, Pane suppresses its own divider just like the official Ink tree.

use super::byline::Byline;
use super::keyboard_shortcut_hint::{KeyboardShortcutHint, KeyboardShortcutHintStyleContext};
use super::pane::Pane;
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::hooks::use_exit::{ExitKeyState, use_exit_on_ctrl_cd_with_keybindings};
use iocraft::prelude::*;

/// Maps to: CC `design-system/Dialog.tsx:22-23` `inputGuide` function prop.
/// L1 React callback/children carrier: the caller owns the ExitState decision
/// and returns the corresponding iocraft element instead of a React node.
pub type DialogInputGuide =
    std::sync::Arc<dyn Fn(ExitKeyState) -> AnyElement<'static> + Send + Sync>;

#[derive(Default, Props)]
pub struct DialogProps<'a> {
    pub title: String,
    /// Maps to official `title: React.ReactNode`. When present, these leaves
    /// replace the plain-string title while preserving the Dialog shell.
    pub title_children: Vec<AnyElement<'static>>,
    pub subtitle: Option<String>,
    pub color: Option<Color>,
    pub hide_input_guide: bool,
    pub hide_border: bool,
    /// Maps to official `isCancelActive`; defaults to true when omitted.
    pub is_cancel_active: Option<bool>,
    /// Rust-side text equivalent of official `inputGuide(exitState)`.
    pub input_guide: Option<String>,
    /// Maps to: CC `Dialog.tsx:88` `inputGuide(exitState)`. A supplied function
    /// decides its own pending-state priority, just as the source caller does.
    pub input_guide_renderer: Option<DialogInputGuide>,
    /// Rich iocraft equivalent of an official custom `inputGuide` tree. The
    /// pending Ctrl-C/D hint still takes precedence.
    pub input_guide_children: Vec<AnyElement<'static>>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn Dialog<'a>(props: &mut DialogProps<'a>, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut should_cancel = hooks.use_state(|| false);
    let is_cancel_active = props.is_cancel_active.unwrap_or(true);
    let exit_state = use_exit_on_ctrl_cd_with_keybindings(&mut hooks, is_cancel_active);

    // Maps to: CC `design-system/Dialog.tsx:55` —
    // `useKeybinding('confirm:no', ...)`. `escape` and `n` both resolve to
    // confirm:no via the Confirmation context table; Confirmation's escape
    // binding out-ranks Chat's chat:cancel (last-wins), and this deeper
    // component consumes the event before Repl's cancel handler bubbles.
    let keybinding_runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime,
        "confirm:no",
        crate::keybindings::types::ContextName::Confirmation,
        move || is_cancel_active,
        move || {
            let mut should_cancel = should_cancel;
            should_cancel.set(true);
            true
        },
    );

    if should_cancel.get() {
        should_cancel.set(false);
        (props.on_cancel)(());
    }

    // Official Dialog defaults both title and Pane to the `permission` theme
    // color rather than leaving an omitted color at terminal foreground.
    let color = props.color.or_else(|| {
        hooks
            .try_use_context::<crate::utils::theme::Theme>()
            .map(|theme| theme.permission)
            .or_else(|| Some(crate::utils::theme::current().permission))
    });
    let title = props.title.clone();
    let title_children = std::mem::take(&mut props.title_children);
    let subtitle = props.subtitle.clone();
    let hide_input_guide = props.hide_input_guide;
    let input_guide = props.input_guide.clone();
    let input_guide_children = std::mem::take(&mut props.input_guide_children);
    let exit_hint = exit_state.hint().map(str::to_string);
    let body = std::mem::take(&mut props.children);
    let input_guide_element: Option<AnyElement<'static>> = if hide_input_guide {
        None
    } else if let Some(render_input_guide) = props.input_guide_renderer.as_ref() {
        let input_guide = render_input_guide(exit_state);
        Some(
            element! {
                View(margin_top: 1u32) {
                    ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext {
                        dim: true,
                        italic: true,
                    })) {
                        #(input_guide)
                    }
                }
            }
            .into_any(),
        )
    } else if let Some(exit_hint) = exit_hint {
        // Retain the existing text/children carrier behavior for callers that
        // have not supplied the source-shaped inputGuide callback.
        Some(
            element! {
                View(margin_top: 1u32) {
                    Text(
                        content: exit_hint,
                        dim: true,
                        italic: true,
                        wrap: TextWrap::NoWrap,
                    )
                }
            }
            .into_any(),
        )
    } else if !input_guide_children.is_empty() {
        Some(
            element! {
                View(margin_top: 1u32) {
                    ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext {
                        dim: true,
                        italic: true,
                    })) {
                        #(input_guide_children)
                    }
                }
            }
            .into_any(),
        )
    } else if let Some(input_guide) = input_guide {
        Some(
            element! {
                View(margin_top: 1u32) {
                    Text(
                        content: input_guide,
                        dim: true,
                        italic: true,
                        wrap: TextWrap::NoWrap,
                    )
                }
            }
            .into_any(),
        )
    } else {
        Some(
            element! {
                View(margin_top: 1u32) {
                    ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext {
                        dim: true,
                        italic: true,
                    })) {
                        Byline {
                            KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
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
            .into_any(),
        )
    };

    let content = element! {
        Fragment {
            View(flex_direction: FlexDirection::Column) {
                #(if title_children.is_empty() {
                    vec![element! {
                        Text(content: title, color: color, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    }.into_any()]
                } else {
                    vec![element! {
                        View(flex_direction: FlexDirection::Row) {
                            #(title_children)
                        }
                    }.into_any()]
                })
                #(subtitle.map(|subtitle| element! {
                    Text(content: subtitle, dim: true, wrap: TextWrap::NoWrap)
                }))
            }
            View(margin_top: 1u32, flex_direction: FlexDirection::Column, gap: 1) {
                #(body)
            }
            #(input_guide_element)
        }
    }
    .into_any();

    let rendered: AnyElement<'static> = if props.hide_border {
        element! {
            View(flex_direction: FlexDirection::Column) {
                #(vec![content])
            }
        }
        .into_any()
    } else {
        element! {
            Pane(color: color) {
                #(vec![content])
            }
        }
        .into_any()
    };

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn canvas_text(canvas: &Canvas) -> String {
        canvas.to_string()
    }

    fn ctrl_key(ch: char) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(ch));
        event.modifiers = KeyModifiers::CONTROL;
        TerminalEvent::Key(event)
    }

    fn render_dialog_with_events(
        events: Vec<TerminalEvent>,
        is_cancel_active: Option<bool>,
        stop_when: impl Fn(&str) -> bool,
    ) -> String {
        let current_theme = *theme::current();

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        Dialog(
                        title: "Memory".to_string(),
                        is_cancel_active: is_cancel_active,
                        ) {
                            Text(content: "Body".to_string())
                        }
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(events)).with_size(80, 24),
            ));
            let mut last = String::new();
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(150)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if stop_when(&last) {
                    break;
                }
            }
            last
        })
    }

    #[test]
    fn dialog_renders_official_title_body_and_default_input_guide() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Dialog(title: "Memory".to_string(), color: Some(current_theme.remember)) {
                    Text(content: "Body".to_string())
                }
            }
        }
        .render(Some(80));
        let text = canvas_text(&canvas);

        assert!(text.contains("Memory"), "canvas=\n{text}");
        assert!(text.contains("Body"), "canvas=\n{text}");
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "canvas=\n{text}"
        );
    }

    fn run_dialog_cancel_key(code: KeyCode, is_cancel_active: Option<bool>) -> usize {
        let current_theme = *theme::current();
        let cancel_count = Arc::new(Mutex::new(0usize));
        let cancel_for_handler = Arc::clone(&cancel_count);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        Dialog(
                            title: "Memory".to_string(),
                            is_cancel_active: is_cancel_active,
                            on_cancel: move |_| {
                                *cancel_for_handler.lock().expect("cancel mutex") += 1;
                            },
                        ) {
                            Text(content: "Body".to_string())
                        }
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(
                        KeyEvent::new(KeyEventKind::Press, code),
                    )]))
                    .with_size(80, 24),
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

        *cancel_count.lock().expect("cancel mutex")
    }

    #[test]
    fn dialog_confirmation_no_keys_invoke_cancel_when_active() {
        assert_eq!(run_dialog_cancel_key(KeyCode::Esc, None), 1);
        assert_eq!(run_dialog_cancel_key(KeyCode::Char('n'), None), 1);
    }

    #[test]
    fn dialog_ctrl_c_first_press_shows_official_exit_pending_input_guide() {
        let text = render_dialog_with_events(vec![ctrl_key('c')], None, |text| {
            text.contains("Press Ctrl-C again to exit")
        });

        assert!(
            text.contains("Press Ctrl-C again to exit"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to confirm · Esc to cancel"),
            "pending exit guide should replace default guide; canvas=\n{text}"
        );
    }

    #[test]
    fn dialog_ctrl_c_is_ignored_when_cancel_inactive_so_embedded_inputs_keep_control() {
        let text = render_dialog_with_events(vec![ctrl_key('c')], Some(false), |text| {
            text.contains("Press Ctrl-C again to exit")
        });

        assert!(
            !text.contains("Press Ctrl-C again to exit"),
            "inactive Dialog should not consume ctrl-c; canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn dialog_confirmation_no_key_is_ignored_when_cancel_inactive() {
        assert_eq!(run_dialog_cancel_key(KeyCode::Char('n'), Some(false)), 0);
    }
}
