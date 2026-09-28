//! Maps to: CC `components/hooks/PromptDialog.tsx`.

use super::{use_select_bindings, visible_from_index};
use crate::components::custom_select::{Select, SelectOptionData};
use crate::components::permissions::permission_dialog::PermissionDialog;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::types::hooks::PromptRequest;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct PromptDialogProps<'a> {
    pub title: String,
    pub tool_input_summary: Option<String>,
    pub request: PromptRequest,
    pub on_respond: HandlerMut<'a, String>,
    pub on_abort: HandlerMut<'a, ()>,
}

/// Maps to: CC `PromptDialog`.
#[component]
pub fn PromptDialog<'a>(
    props: &mut PromptDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_selection = hooks.use_state(|| Option::<usize>::None);
    let mut pending_abort = hooks.use_state(|| false);
    let option_count = props.request.options.len();

    if focused_index.get() >= option_count {
        focused_index.set(option_count.saturating_sub(1));
    }
    let selected_index = { *pending_selection.read() };
    if let Some(index) = selected_index {
        pending_selection.set(None);
        if let Some(option) = props.request.options.get(index) {
            (props.on_respond)(option.key.clone());
        }
    }
    if pending_abort.get() {
        pending_abort.set(false);
        (props.on_abort)(());
    }

    use_select_bindings(&mut hooks, option_count, focused_index, pending_selection);
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    use_keybinding(
        &mut hooks,
        runtime,
        "app:interrupt",
        ContextName::Global,
        || true,
        {
            let mut pending_abort = pending_abort;
            move || {
                pending_abort.set(true);
                true
            }
        },
    );

    let options = props
        .request
        .options
        .iter()
        .map(|option| SelectOptionData {
            label: option.label.clone(),
            value: option.key.clone(),
            description: option.description.clone(),
            ..SelectOptionData::default()
        })
        .collect::<Vec<_>>();

    element! {
        PermissionDialog(
            title: props.title.clone(),
            subtitle: Some(props.request.message.clone()),
            title_right: props.tool_input_summary.clone(),
            title_right_dim: true,
        ) {
            View(
                flex_direction: FlexDirection::Column,
                padding_top: 1u32,
                padding_bottom: 1u32,
            ) {
                Select(
                    options: options,
                    focused_index: focused_index.get(),
                    visible_option_count: 5usize,
                    visible_from_index: visible_from_index(focused_index.get(), option_count),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::hooks::PromptRequestOption;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn prompt_dialog_renders_permission_shell_summary_and_options() {
        let request = PromptRequest {
            prompt: "prompt-1".to_string(),
            message: "Choose an environment".to_string(),
            options: vec![PromptRequestOption {
                key: "staging".to_string(),
                label: "Staging".to_string(),
                description: Some("Deploy for review".to_string()),
            }],
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PromptDialog(
                    title: "Deployment hook".to_string(),
                    tool_input_summary: Some("Bash(deploy)".to_string()),
                    request: request,
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Deployment hook"), "canvas=\n{text}");
        assert!(text.contains("Choose an environment"), "canvas=\n{text}");
        assert!(text.contains("Bash(deploy)"), "canvas=\n{text}");
        assert!(text.contains("Staging"), "canvas=\n{text}");
        assert!(text.contains("Deploy for review"), "canvas=\n{text}");
    }

    #[test]
    fn prompt_dialog_app_interrupt_aborts_through_keybinding_runtime() {
        let abort_count = Arc::new(Mutex::new(0usize));
        let abort_for_handler = Arc::clone(&abort_count);
        // Cometix binds Ctrl+C to app:exit; F6 stands in for a key a user
        // binds to app:interrupt.
        let event_stream = stream::once(async {
            futures_timer::Delay::new(Duration::from_millis(30)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::F(6)))
        });
        let mut bindings = crate::keybindings::default_bindings::default_bindings();
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("f6"),
            action: Some("app:interrupt".to_string()),
            context: crate::keybindings::types::ContextName::Global,
        });

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    KeybindingRuntime::new(bindings)
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        PromptDialog(
                        title: "Hook prompt".to_string(),
                        request: PromptRequest {
                            prompt: "prompt-1".to_string(),
                            message: "Choose".to_string(),
                            options: vec![PromptRequestOption {
                                key: "yes".to_string(),
                                label: "Yes".to_string(),
                                description: None,
                            }],
                        },
                            on_abort: move |_| *abort_for_handler.lock().expect("abort mutex") += 1,
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(event_stream).with_size(90, 24),
            ));
            for _ in 0..12 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(*abort_count.lock().expect("abort mutex"), 1);
    }
}
