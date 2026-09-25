//! Maps to: CC `components/WorktreeExitDialog.tsx`.
//!
//! Safety boundary: the official component shells out to git, calls
//! `cleanupWorktree`/`keepWorktree`/`killTmuxSession`, changes process cwd,
//! clears the plans-directory cache, writes session worktree state, and logs
//! analytics. Cometix preserves the official render branches, option copy,
//! subtitle/result-message construction, and callbacks from an explicit
//! `WorktreeSession` snapshot. Runtime cleanup/tmux/chdir/session writes remain
//! deferred to the worktree runtime slice.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::components::spinner::Spinner;
use crate::utils::worktree::{
    CommandResultDisplay, WorktreeExitAction, WorktreeExitDone, WorktreeSession,
    worktree_exit_action_from_value, worktree_exit_options, worktree_exit_result_message,
    worktree_exit_subtitle,
};
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorktreeExitStatus {
    Loading,
    #[default]
    Asking,
    Keeping,
    Removing,
    Done,
}

#[derive(Default, Props)]
pub struct WorktreeExitDialogProps<'a> {
    pub on_done: HandlerMut<'a, WorktreeExitDone>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub session: Option<WorktreeSession>,
    pub status: WorktreeExitStatus,
    pub changes: Vec<String>,
    pub commit_count: usize,
}

fn option_data(options: &[crate::utils::worktree::WorktreeExitOption]) -> Vec<SelectOptionData> {
    options
        .iter()
        .map(|option| SelectOptionData {
            label: option.label.clone(),
            value: option.value.clone(),
            description: Some(option.description.clone()),
            ..SelectOptionData::default()
        })
        .collect()
}

/// Maps to: CC `components/WorktreeExitDialog.tsx` `WorktreeExitDialog`.
#[component]
pub fn WorktreeExitDialog<'a>(
    props: &mut WorktreeExitDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut reported_no_session = hooks.use_state(|| false);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_action = hooks.use_state(|| Option::<WorktreeExitAction>::None);
    let mut pending_cancel = hooks.use_state(|| false);

    let Some(session) = props.session.clone() else {
        if !reported_no_session.get() {
            reported_no_session.set(true);
            (props.on_done)(WorktreeExitDone {
                result: Some("No active worktree session found".to_string()),
                display: Some(CommandResultDisplay::System),
            });
        }
        return element! { View() }.into_any();
    };

    match props.status {
        WorktreeExitStatus::Loading | WorktreeExitStatus::Done => {
            return element! { View() }.into_any();
        }
        WorktreeExitStatus::Keeping => {
            return element! {
                View(flex_direction: FlexDirection::Row, margin_y: 1u32) {
                    Spinner
                    Text(content: "Keeping worktree…".to_string())
                }
            }
            .into_any();
        }
        WorktreeExitStatus::Removing => {
            return element! {
                View(flex_direction: FlexDirection::Row, margin_y: 1u32) {
                    Spinner
                    Text(content: "Removing worktree…".to_string())
                }
            }
            .into_any();
        }
        WorktreeExitStatus::Asking => {}
    }

    let changed_file_count = props.changes.len();
    let commit_count = props.commit_count;
    let subtitle = worktree_exit_subtitle(&session, changed_file_count, commit_count);
    let (worktree_options, default_value) =
        worktree_exit_options(&session, changed_file_count, commit_count);
    let options = option_data(&worktree_options);
    let option_count = options.len().max(1);
    let default_index = options
        .iter()
        .position(|option| option.value == default_value)
        .unwrap_or(0);
    if focused_index.get() >= options.len() && !options.is_empty() {
        focused_index.set(default_index);
    }

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
                        if let Some(action) = worktree_exit_action_from_value(&option.value) {
                            pending_action.set(Some(action));
                        }
                    }
                }
                _ => {}
            }
        }
    });

    let action = { pending_action.read().clone() };
    if let Some(action) = action {
        pending_action.set(None);
        let result =
            worktree_exit_result_message(&session, changed_file_count, commit_count, action);
        (props.on_done)(WorktreeExitDone {
            result: Some(result),
            display: None,
        });
    }

    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }

    element! {
        Dialog(
            title: "Exiting worktree session".to_string(),
            subtitle: Some(subtitle),
            on_cancel: move |_| {
                // Official behavior calls `onCancel` when provided; otherwise it
                // falls back to selecting `keep`. Cometix exposes the explicit
                // callback path in this safe UI boundary.
                pending_cancel.set(true);
            },
        ) {
            Select(
                options: options,
                focused_index: focused_index.get().min(option_count - 1),
                selected_value: Some(default_value),
                visible_option_count: option_count,
                layout: SelectLayout::Compact,
                hide_indexes: true,
            )
        }
    }
    .into_any()
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

    fn session(tmux: bool) -> WorktreeSession {
        WorktreeSession {
            original_cwd: "/repo".to_string(),
            worktree_path: "/repo-feature".to_string(),
            worktree_name: "feature".to_string(),
            worktree_branch: Some("cometix/feature".to_string()),
            original_branch: Some("main".to_string()),
            original_head_commit: Some("abc123".to_string()),
            session_id: "session-1".to_string(),
            tmux_session_name: tmux.then(|| "repo_cometix_feature".to_string()),
            hook_based: None,
            creation_duration_ms: None,
            used_sparse_paths: None,
        }
    }

    #[test]
    fn worktree_exit_dialog_renders_plain_options_and_subtitle() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WorktreeExitDialog(
                    session: Some(session(false)),
                    changes: vec!["M src/main.rs".to_string()],
                    commit_count: 0usize,
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("Exiting worktree session"), "canvas=\n{text}");
        assert!(
            text.contains(
                "You have 1 uncommitted file. These will be lost if you remove the worktree."
            ),
            "canvas=\n{text}"
        );
        assert!(text.contains("Keep worktree"), "canvas=\n{text}");
        assert!(text.contains("Stays at /repo-feature"), "canvas=\n{text}");
        assert!(text.contains("Remove worktree"), "canvas=\n{text}");
        assert!(
            text.contains("All changes and commits will be lost."),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn worktree_exit_dialog_renders_tmux_options_and_commit_subtitle() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WorktreeExitDialog(
                    session: Some(session(true)),
                    changes: Vec::<String>::new(),
                    commit_count: 2usize,
                )
            }
        }
        .render(Some(150))
        .to_string();

        assert!(
            text.contains("You have 2 commits on cometix/feature. The branch will be deleted if you remove the worktree."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Keep worktree and tmux session"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("tmux attach -t repo_cometix_feature"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Keep worktree, kill tmux session"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Remove worktree and tmux session"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn worktree_exit_dialog_status_rows_match_official_loading_copy() {
        let keeping = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WorktreeExitDialog(session: Some(session(false)), status: WorktreeExitStatus::Keeping)
            }
        }
        .render(Some(80))
        .to_string();
        let removing = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WorktreeExitDialog(session: Some(session(false)), status: WorktreeExitStatus::Removing)
            }
        }
        .render(Some(80))
        .to_string();

        assert!(keeping.contains("Keeping worktree…"), "canvas=\n{keeping}");
        assert!(
            removing.contains("Removing worktree…"),
            "canvas=\n{removing}"
        );
    }

    #[test]
    fn worktree_exit_dialog_no_session_calls_on_done_with_system_display() {
        let dones = Arc::new(Mutex::new(Vec::<WorktreeExitDone>::new()));
        let dones_for_handler = Arc::clone(&dones);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    WorktreeExitDialog(
                        session: Option::<WorktreeSession>::None,
                        on_done: move |done| {
                            dones_for_handler.lock().expect("dones mutex").push(done);
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(MockTerminalConfig::default().with_size(100, 20)),
            );
            for _ in 0..4 {
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
            dones.lock().expect("dones mutex").as_slice(),
            &[WorktreeExitDone {
                result: Some("No active worktree session found".to_string()),
                display: Some(CommandResultDisplay::System),
            }]
        );
    }

    #[test]
    fn worktree_exit_dialog_default_enter_keeps_worktree() {
        let dones = Arc::new(Mutex::new(Vec::<WorktreeExitDone>::new()));
        let dones_for_handler = Arc::clone(&dones);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    WorktreeExitDialog(
                        session: Some(session(false)),
                        changes: Vec::<String>::new(),
                        commit_count: 0usize,
                        on_done: move |done| {
                            dones_for_handler.lock().expect("dones mutex").push(done);
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
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
            dones.lock().expect("dones mutex").as_slice(),
            &[WorktreeExitDone {
                result: Some(
                    "Worktree kept. Your work is saved at /repo-feature on branch cometix/feature"
                        .to_string(),
                ),
                display: None,
            }]
        );
    }

    #[test]
    fn worktree_exit_dialog_down_enter_removes_worktree_without_runtime_side_effects() {
        let dones = Arc::new(Mutex::new(Vec::<WorktreeExitDone>::new()));
        let dones_for_handler = Arc::clone(&dones);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    WorktreeExitDialog(
                        session: Some(session(false)),
                        changes: vec!["M src/main.rs".to_string()],
                        commit_count: 2usize,
                        on_done: move |done| {
                            dones_for_handler.lock().expect("dones mutex").push(done);
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
            dones.lock().expect("dones mutex").as_slice(),
            &[WorktreeExitDone {
                result: Some(
                    "Worktree removed. 2 commits and uncommitted changes were discarded."
                        .to_string(),
                ),
                display: None,
            }]
        );
    }

    #[test]
    fn worktree_exit_dialog_escape_calls_on_cancel() {
        let cancelled = Arc::new(Mutex::new(0usize));
        let cancelled_for_handler = Arc::clone(&cancelled);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        WorktreeExitDialog(
                            session: Some(session(false)),
                            on_cancel: move |_| {
                                *cancelled_for_handler.lock().expect("cancelled mutex") += 1;
                            },
                        )
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

        assert_eq!(*cancelled.lock().expect("cancelled mutex"), 1);
    }
}
