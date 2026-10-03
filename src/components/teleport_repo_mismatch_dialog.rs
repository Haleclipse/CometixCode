//! Maps to: CC `components/TeleportRepoMismatchDialog.tsx`.
//!
//! Safety boundary: the official component validates selected paths with
//! `validateRepoAtPath(...)` and removes invalid paths from the persisted GitHub
//! repo mapping via `removePathFromRepo(...)`. Cometix preserves the dialog,
//! option construction, empty-state copy, invalid-path message helper, and
//! `on_select_path`/`on_cancel` callbacks only. Filesystem/git validation and
//! config mutation belong to the future `utils/githubRepoPathMapping` runtime
//! slice.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::file::get_display_path;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeleportRepoMismatchChoice {
    Path(String),
    Cancel,
}

impl TeleportRepoMismatchChoice {
    fn value(&self) -> String {
        match self {
            Self::Path(path) => path.clone(),
            Self::Cancel => "cancel".to_string(),
        }
    }
}

/// Maps to: CC `components/TeleportRepoMismatchDialog.tsx` invalid path error
/// message after `removePathFromRepo(...)`.
pub fn teleport_repo_mismatch_error_message(path: &str) -> String {
    format!(
        "{} no longer contains the correct repository. Select another path.",
        get_display_path(path)
    )
}

/// Maps to: CC `components/TeleportRepoMismatchDialog.tsx` `options`.
pub fn teleport_repo_mismatch_options(paths: &[String]) -> Vec<SelectOptionData> {
    paths
        .iter()
        .map(|path| SelectOptionData {
            label: format!("Use {}", get_display_path(path)),
            value: TeleportRepoMismatchChoice::Path(path.clone()).value(),
            ..SelectOptionData::default()
        })
        .chain(std::iter::once(SelectOptionData {
            label: "Cancel".to_string(),
            value: TeleportRepoMismatchChoice::Cancel.value(),
            ..SelectOptionData::default()
        }))
        .collect()
}

fn choice_from_value(value: &str) -> TeleportRepoMismatchChoice {
    if value == "cancel" {
        TeleportRepoMismatchChoice::Cancel
    } else {
        TeleportRepoMismatchChoice::Path(value.to_string())
    }
}

#[derive(Default, Props)]
pub struct TeleportRepoMismatchDialogProps<'a> {
    pub target_repo: String,
    pub initial_paths: Vec<String>,
    pub on_select_path: HandlerMut<'a, String>,
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/TeleportRepoMismatchDialog.tsx`
/// `TeleportRepoMismatchDialog`.
#[component]
pub fn TeleportRepoMismatchDialog<'a>(
    props: &mut TeleportRepoMismatchDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<TeleportRepoMismatchChoice>::None);
    let available_paths = props.initial_paths.clone();
    let options = teleport_repo_mismatch_options(&available_paths);
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

    let choice = { pending_choice.read().clone() };
    if let Some(choice) = choice {
        pending_choice.set(None);
        match choice {
            TeleportRepoMismatchChoice::Path(path) => {
                // Official path: async validateRepoAtPath; if valid, onSelectPath.
                // Safe Cometix path: caller/runtime owns validation, this dialog
                // only returns the selected path.
                (props.on_select_path)(path);
            }
            TeleportRepoMismatchChoice::Cancel => {
                (props.on_cancel)(());
            }
        }
    }

    element! {
        Dialog(
            title: "Teleport to Repo".to_string(),
            color: Some(theme.background),
            on_cancel: move |_| {
                pending_choice.set(Some(TeleportRepoMismatchChoice::Cancel));
            },
        ) {
            #(if available_paths.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                        Text(
                            content: format!("Run claude --teleport from a checkout of {}", props.target_repo),
                            color: theme.inactive,
                            wrap: TextWrap::Wrap,
                        )
                    }
                }.into_any())
            } else {
                Some(element! {
                    Fragment {
                        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                            Text(content: format!("Open Claude Code in {}:", props.target_repo), wrap: TextWrap::Wrap)
                        }
                        Select(
                            options: options,
                            focused_index: focused_index.get().min(option_count - 1),
                            visible_option_count: option_count,
                            layout: SelectLayout::CompactVertical,
                            hide_indexes: true,
                        )
                    }
                }.into_any())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn teleport_repo_options_match_official_use_labels_and_cancel_order() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _home_guard = EnvVarGuard::set(HOME_VAR, "/home/alice");
        let paths = vec![
            "/home/alice/src/repo".to_string(),
            "/work/other/repo".to_string(),
        ];

        let options = teleport_repo_mismatch_options(&paths);

        assert_eq!(options[0].label, "Use ~/src/repo");
        assert_eq!(options[0].value, "/home/alice/src/repo");
        assert_eq!(options[1].label, "Use /work/other/repo");
        assert_eq!(options[1].value, "/work/other/repo");
        assert_eq!(options[2].label, "Cancel");
        assert_eq!(options[2].value, "cancel");
    }

    #[test]
    fn teleport_repo_invalid_path_message_matches_official_copy() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _home_guard = EnvVarGuard::set(HOME_VAR, "/home/alice");

        assert_eq!(
            teleport_repo_mismatch_error_message("/home/alice/src/repo"),
            "~/src/repo no longer contains the correct repository. Select another path."
        );
    }

    #[test]
    fn teleport_repo_dialog_renders_available_paths_and_cancel() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _home_guard = EnvVarGuard::set(HOME_VAR, "/home/alice");
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TeleportRepoMismatchDialog(
                    target_repo: "anthropic/claude-code".to_string(),
                    initial_paths: vec![
                        "/home/alice/src/claude-code".to_string(),
                        "/work/claude-code".to_string(),
                    ],
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Teleport to Repo"), "canvas=\n{text}");
        assert!(
            text.contains("Open Claude Code in anthropic/claude-code:"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Use ~/src/claude-code"), "canvas=\n{text}");
        assert!(text.contains("Use /work/claude-code"), "canvas=\n{text}");
        assert!(text.contains("Cancel"), "canvas=\n{text}");
    }

    #[test]
    fn teleport_repo_dialog_renders_no_paths_guidance() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TeleportRepoMismatchDialog(
                    target_repo: "anthropic/claude-code".to_string(),
                    initial_paths: Vec::<String>::new(),
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Teleport to Repo"), "canvas=\n{text}");
        assert!(
            text.contains("Run claude --teleport from a checkout of anthropic/claude-code"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Open Claude Code in"), "canvas=\n{text}");
    }

    #[test]
    fn teleport_repo_dialog_default_enter_selects_first_path() {
        let selected = Arc::new(Mutex::new(Vec::<String>::new()));
        let cancelled = Arc::new(Mutex::new(0usize));
        let selected_for_handler = Arc::clone(&selected);
        let cancelled_for_handler = Arc::clone(&cancelled);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    TeleportRepoMismatchDialog(
                        target_repo: "anthropic/claude-code".to_string(),
                        initial_paths: vec!["/tmp/repo-a".to_string(), "/tmp/repo-b".to_string()],
                        on_select_path: move |path| {
                            selected_for_handler.lock().expect("selected mutex").push(path);
                        },
                        on_cancel: move |_| {
                            *cancelled_for_handler.lock().expect("cancelled mutex") += 1;
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(100, 24),
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
            selected.lock().expect("selected mutex").as_slice(),
            &["/tmp/repo-a".to_string()]
        );
        assert_eq!(*cancelled.lock().expect("cancelled mutex"), 0);
    }

    #[test]
    fn teleport_repo_dialog_cancel_option_calls_on_cancel() {
        let selected = Arc::new(Mutex::new(Vec::<String>::new()));
        let cancelled = Arc::new(Mutex::new(0usize));
        let selected_for_handler = Arc::clone(&selected);
        let cancelled_for_handler = Arc::clone(&cancelled);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    TeleportRepoMismatchDialog(
                        target_repo: "anthropic/claude-code".to_string(),
                        initial_paths: vec!["/tmp/repo-a".to_string()],
                        on_select_path: move |path| {
                            selected_for_handler.lock().expect("selected mutex").push(path);
                        },
                        on_cancel: move |_| {
                            *cancelled_for_handler.lock().expect("cancelled mutex") += 1;
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
                    .with_size(100, 24),
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

        assert!(selected.lock().expect("selected mutex").is_empty());
        assert_eq!(*cancelled.lock().expect("cancelled mutex"), 1);
    }
}
