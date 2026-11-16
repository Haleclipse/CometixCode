//! Maps to: CC `hooks/useGlobalKeybindings.tsx`.
//!
//! The live AppState actions, prompt/transcript screen transitions, transcript
//! expansion/exit actions, and terminal redraw action are registered here
//! rather than being split across PromptInput and REPL.

use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybindings};
use crate::state::app_state_store::ExpandedView;
use crate::state::store::AppStore;
use iocraft::prelude::*;

/// Maps to: CC `GlobalKeybindingHandlers` for currently owned global actions.
pub fn use_global_keybindings(
    hooks: &mut Hooks,
    runtime: Option<KeybindingRuntime>,
    app_store: AppStore,
    mut redraw_generation: State<u64>,
    mut screen: State<crate::screens::repl::Screen>,
    mut show_all_in_transcript: State<bool>,
) {
    let store_for_todos = app_store.clone();
    let store_for_preview = app_store;
    let handlers: KeybindingHandlers = vec![
        (
            "app:toggleTodos".to_string(),
            Box::new(move || {
                // @nyan: Ctrl+T only toggles Tasks <-> None (CC cycles through
                // Teammates when one is running; the agent panel replaces that
                // tree). A manual collapse suppresses auto-expand until the
                // next Ctrl+T expands again.
                store_for_todos.replace_with(|state| {
                    if state.expanded_view == ExpandedView::Tasks {
                        state.expanded_view = ExpandedView::None;
                        state.todo_auto_expand_suppressed = true;
                    } else {
                        state.expanded_view = ExpandedView::Tasks;
                        state.todo_auto_expand_suppressed = false;
                    }
                });
                true
            }),
        ),
        (
            "app:toggleTranscript".to_string(),
            Box::new(move || {
                screen.set(match screen.get() {
                    crate::screens::repl::Screen::Prompt => {
                        crate::screens::repl::Screen::Transcript
                    }
                    crate::screens::repl::Screen::Transcript => {
                        crate::screens::repl::Screen::Prompt
                    }
                });
                show_all_in_transcript.set(false);
                true
            }),
        ),
        (
            "app:toggleTeammatePreview".to_string(),
            Box::new(move || {
                store_for_preview.replace_with(|state| {
                    state.show_teammate_message_preview = !state.show_teammate_message_preview;
                });
                true
            }),
        ),
        (
            "app:redraw".to_string(),
            Box::new(move || {
                redraw_generation.set(redraw_generation.get().wrapping_add(1));
                true
            }),
        ),
    ];
    use_keybindings(
        hooks,
        runtime.clone(),
        handlers,
        ContextName::Global,
        || true,
    );

    let transcript_handlers: KeybindingHandlers = vec![
        (
            "transcript:toggleShowAll".to_string(),
            Box::new(move || {
                show_all_in_transcript.set(!show_all_in_transcript.get());
                true
            }),
        ),
        (
            "transcript:exit".to_string(),
            Box::new(move || {
                screen.set(crate::screens::repl::Screen::Prompt);
                show_all_in_transcript.set(false);
                true
            }),
        ),
    ];
    use_keybindings(
        hooks,
        runtime.clone(),
        transcript_handlers,
        ContextName::Transcript,
        move || screen.get() == crate::screens::repl::Screen::Transcript,
    );
}

/// Props of CC `GlobalKeybindingHandlers` (`useGlobalKeybindings.tsx:21-31`)
/// that this REPL owns: its `screen` / `showAllInTranscript` states, which
/// double as their setters, and the redraw generation `app:redraw` bumps.
#[derive(Default, Props)]
pub struct GlobalKeybindingHandlersProps {
    pub redraw_generation: Option<State<u64>>,
    pub screen: Option<State<crate::screens::repl::Screen>>,
    pub show_all_in_transcript: Option<State<bool>>,
}

/// Maps to: CC `useGlobalKeybindings.tsx:40-264` `GlobalKeybindingHandlers`,
/// the null-rendering component REPL mounts inside `KeybindingSetup`
/// (REPL.tsx:5858, :6090), ahead of PromptInput.
#[component]
pub fn GlobalKeybindingHandlers(
    props: &GlobalKeybindingHandlersProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let app_store = crate::state::app_state::use_set_app_state(&mut hooks);
    use_global_keybindings(
        &mut hooks,
        runtime,
        app_store,
        props
            .redraw_generation
            .expect("GlobalKeybindingHandlers redraw_generation"),
        props.screen.expect("GlobalKeybindingHandlers screen"),
        props
            .show_all_in_transcript
            .expect("GlobalKeybindingHandlers show_all_in_transcript"),
    );
    element!(View(width: 0u32, height: 0u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    #[component]
    fn GlobalActionsHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = hooks
            .use_const(|| AppStore::new(crate::state::app_state_store::AppState::default(), None));
        let runtime = hooks.use_const(KeybindingRuntime::with_default_bindings);
        let redraw = hooks.use_state(|| 0u64);
        let screen = hooks.use_state(crate::screens::repl::Screen::default);
        let show_all = hooks.use_state(|| false);
        use_global_keybindings(
            &mut hooks,
            Some(runtime),
            store.clone(),
            redraw,
            screen,
            show_all,
        );
        let state = store.get();
        element! {
            Text(content: format!(
                "view={:?};preview={};redraw={};screen={:?};all={}",
                state.expanded_view,
                state.show_teammate_message_preview,
                redraw.get(),
                screen.get(),
                show_all.get(),
            ))
        }
    }

    /// Shows the Ctrl+T-relevant AppState fields; seeds a running teammate when
    /// `with_teammate` so the old Tasks -> Teammates step would be reachable.
    #[component]
    fn TodoToggleHarness(
        mut hooks: Hooks,
        props: &TodoToggleHarnessProps,
    ) -> impl Into<AnyElement<'static>> {
        let with_teammate = props.with_teammate;
        let store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            if with_teammate {
                let snapshot = crate::components::spinner::teammate_tree::TeammateTaskSnapshot {
                    id: "task-alpha".to_string(),
                    task_type:
                        crate::components::spinner::teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE
                            .to_string(),
                    status: crate::components::spinner::teammate_tree::RUNNING_TASK_STATUS
                        .to_string(),
                    agent_name: "alpha".to_string(),
                    ..Default::default()
                };
                std::sync::Arc::make_mut(&mut initial.tasks).insert(
                    snapshot.id.clone(),
                    std::sync::Arc::new(
                        crate::state::app_state_store::TaskState::InProcessTeammate(snapshot),
                    ),
                );
            }
            AppStore::new(initial, None)
        });
        let runtime = hooks.use_const(KeybindingRuntime::with_default_bindings);
        let redraw = hooks.use_state(|| 0u64);
        let screen = hooks.use_state(crate::screens::repl::Screen::default);
        let show_all = hooks.use_state(|| false);
        use_global_keybindings(
            &mut hooks,
            Some(runtime),
            store.clone(),
            redraw,
            screen,
            show_all,
        );
        // The probe subscribes through the provider so a store write alone
        // repaints; reading `store.get()` here would only show it on the next
        // unrelated render.
        let store = store.clone();
        element! {
            crate::state::app_state::AppStateProvider(
                prebuilt_store: Some(store),
                children: crate::state::app_state::ProviderChildren::new(|| {
                    element! { TodoTogglePanelProbe }.into_any()
                }),
            )
        }
    }

    #[component]
    fn TodoTogglePanelProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let view = crate::state::app_state::use_app_state(&mut hooks, |state| state.expanded_view);
        let suppressed = crate::state::app_state::use_app_state(&mut hooks, |state| {
            state.todo_auto_expand_suppressed
        });
        element! { Text(content: format!("view={view:?};suppressed={suppressed}")) }
    }

    #[derive(Default, Props)]
    struct TodoToggleHarnessProps {
        with_teammate: bool,
    }

    /// Frames seen after pressing Ctrl+T `presses` times, 100 ms apart.
    fn frames_after_ctrl_t(presses: usize, with_teammate: bool) -> Vec<String> {
        let mut ctrl_t = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('t'));
        ctrl_t.modifiers = KeyModifiers::CONTROL;
        let events = stream::unfold(
            std::iter::repeat_n(ctrl_t, presses),
            |mut events| async move {
                let event = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(60)).await;
                Some((TerminalEvent::Key(event), events))
            },
        );
        futures::executor::block_on(async move {
            let mut app = element!(TodoToggleHarness(with_teammate: with_teammate));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(60, 4),
            ));
            let mut frames = Vec::new();
            for _ in 0..40 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(150)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                frames.push(canvas.to_string());
            }
            frames
        })
    }

    #[test]
    fn ctrl_t_toggles_tasks_and_none_and_tracks_the_suppress_flag() {
        let frames = frames_after_ctrl_t(2, false);
        let joined = frames.join("\n---\n");
        let expanded = frames
            .iter()
            .position(|frame| frame.contains("view=Tasks;suppressed=false"))
            .unwrap_or_else(|| panic!("never expanded:\n{joined}"));
        let collapsed = frames
            .iter()
            .position(|frame| frame.contains("view=None;suppressed=true"))
            .unwrap_or_else(|| panic!("never collapsed with suppression:\n{joined}"));
        assert!(expanded < collapsed, "frames:\n{joined}");

        // A third press expands again and clears the flag.
        let frames = frames_after_ctrl_t(3, false);
        let last = frames.last().expect("a frame");
        assert!(
            last.contains("view=Tasks;suppressed=false"),
            "frames:\n{}",
            frames.join("\n---\n")
        );
    }

    #[test]
    fn ctrl_t_never_enters_teammates_even_with_a_running_teammate() {
        let frames = frames_after_ctrl_t(3, true);
        let joined = frames.join("\n---\n");
        assert!(
            !frames.iter().any(|frame| frame.contains("view=Teammates")),
            "frames:\n{joined}"
        );
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("view=None;suppressed=true")),
            "frames:\n{joined}"
        );
        assert!(
            frames
                .last()
                .is_some_and(|frame| frame.contains("view=Tasks;suppressed=false")),
            "frames:\n{joined}"
        );
    }

    #[test]
    fn global_actions_use_shared_runtime_and_app_state_owner() {
        let mut ctrl_t = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('t'));
        ctrl_t.modifiers = KeyModifiers::CONTROL;
        let mut ctrl_shift_o = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('o'));
        ctrl_shift_o.modifiers = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        let mut ctrl_l = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('l'));
        ctrl_l.modifiers = KeyModifiers::CONTROL;
        // mock_terminal_render_loop does not end when the event stream ends.
        let text = futures::executor::block_on(async move {
            let mut app = element!(GlobalActionsHarness);
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        TerminalEvent::Key(ctrl_t),
                        TerminalEvent::Key(ctrl_shift_o),
                        TerminalEvent::Key(ctrl_l),
                    ]))
                    .with_size(60, 4),
                ),
            );
            let mut last = String::new();
            for _ in 0..24 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("view=Tasks")
                    && last.contains("preview=true")
                    && last.contains("redraw=1")
                {
                    break;
                }
            }
            last
        });
        assert!(text.contains("view=Tasks"), "canvas=\n{text}");
        assert!(text.contains("preview=true"), "canvas=\n{text}");
        assert!(text.contains("redraw=1"), "canvas=\n{text}");
    }

    #[test]
    fn transcript_actions_toggle_expand_and_exit_through_runtime() {
        let mut ctrl_o = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('o'));
        ctrl_o.modifiers = KeyModifiers::CONTROL;
        let mut ctrl_e = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('e'));
        ctrl_e.modifiers = KeyModifiers::CONTROL;
        let events = stream::unfold(
            vec![
                TerminalEvent::Key(ctrl_o),
                TerminalEvent::Key(ctrl_e),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Esc)),
            ]
            .into_iter(),
            |mut events| async move {
                let event = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(20)).await;
                Some((event, events))
            },
        );
        let frames = futures::executor::block_on(async move {
            let mut app = element!(GlobalActionsHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(70, 4),
            ));
            let mut frames = Vec::new();
            for _ in 0..24 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                frames.push(canvas.to_string());
            }
            frames
        });
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("screen=Transcript;all=true")),
            "frames=\n{}",
            frames.join("\n---\n")
        );
        let last = frames.last().expect("final frame");
        assert!(last.contains("screen=Prompt;all=false"), "canvas=\n{last}");
    }
}
