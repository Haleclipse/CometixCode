//! Maps to: CC `components/CostThresholdDialog.tsx`.
//!
//! Official REPL usage persists `hasAcknowledgedCostThreshold` and logs
//! `tengu_cost_threshold_acknowledged` after `onDone`. This component mirrors
//! only the official dialog boundary: render the warning, costs link, and call
//! `on_done`. Persistence and analytics remain outside this safe UI slice.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use iocraft::prelude::*;

/// Maps to: CC `components/CostThresholdDialog.tsx` `Select` options.
pub fn cost_threshold_options() -> Vec<SelectOptionData> {
    vec![SelectOptionData {
        value: "ok".to_string(),
        label: "Got it, thanks!".to_string(),
        ..SelectOptionData::default()
    }]
}

#[derive(Default, Props)]
pub struct CostThresholdDialogProps<'a> {
    pub on_done: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/CostThresholdDialog.tsx` `CostThresholdDialog`.
#[component]
pub fn CostThresholdDialog<'a>(
    props: &mut CostThresholdDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut pending_done = hooks.use_state(|| false);
    let options = cost_threshold_options();

    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "select:accept",
        crate::keybindings::types::ContextName::Select,
        || true,
        move || {
            pending_done.set(true);
            true
        },
    );

    if pending_done.get() {
        pending_done.set(false);
        (props.on_done)(());
    }

    element! {
        Dialog(
            title: "You've spent $5 on the Anthropic API this session.".to_string(),
            on_cancel: move |_| {
                pending_done.set(true);
            },
        ) {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "Learn more about how to monitor your spending:".to_string())
                Link(url: format!("{}/cost.md", crate::constants::product::DOCS_URL))
            }
            Select(
                options: options,
                focused_index: 0usize,
                visible_option_count: 1usize,
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
    fn cost_threshold_options_match_official_copy() {
        let options = cost_threshold_options();

        assert_eq!(options.len(), 1);
        assert_eq!(options[0].value, "ok");
        assert_eq!(options[0].label, "Got it, thanks!");
    }

    #[test]
    fn cost_threshold_dialog_renders_official_title_body_link_and_option() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                CostThresholdDialog()
            }
        }
        .render(Some(90))
        .to_string();

        assert!(
            text.contains("You've spent $5 on the Anthropic API this session."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Learn more about how to monitor your spending:"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("https://github.com/blxzer77/nyan/blob/master/docs/nyan/cost.md"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Got it, thanks!"), "canvas=\n{text}");
    }

    #[test]
    fn cost_threshold_dialog_enter_calls_on_done_without_persistence_or_analytics_side_effects() {
        let done = Arc::new(Mutex::new(0usize));
        let done_for_handler = Arc::clone(&done);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        CostThresholdDialog(
                        on_done: move |_| {
                            *done_for_handler.lock().expect("done mutex") += 1;
                        },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(90, 20),
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

        assert_eq!(*done.lock().expect("done mutex"), 1);
    }
}
