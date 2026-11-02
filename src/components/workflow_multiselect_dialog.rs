//! Maps to: CC `components/WorkflowMultiselectDialog.tsx`.
//!
//! Pure UI boundary for GitHub workflow selection. The parent install command
//! owns GitHub/API/file side effects; this component preserves the official
//! workflow options, selection requirement, help link, input guide, and
//! submit/cancel behavior.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::{SelectMulti, SelectOptionData};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub const WORKFLOW_EXAMPLES_URL: &str =
    "https://github.com/anthropics/claude-code-action/blob/main/examples/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Workflow {
    Claude,
    ClaudeReview,
}

impl Workflow {
    pub fn value(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::ClaudeReview => "claude-review",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Claude => "@nyan - Tag @claude in issues and PR comments",
            Self::ClaudeReview => "nyan Review - Automated code review on new PRs",
        }
    }
}

pub fn workflow_from_value(value: &str) -> Option<Workflow> {
    match value {
        "claude" => Some(Workflow::Claude),
        "claude-review" => Some(Workflow::ClaudeReview),
        _ => None,
    }
}

pub fn workflow_options() -> Vec<SelectOptionData> {
    vec![Workflow::Claude, Workflow::ClaudeReview]
        .into_iter()
        .map(|workflow| SelectOptionData {
            label: workflow.label().to_string(),
            value: workflow.value().to_string(),
            ..SelectOptionData::default()
        })
        .collect()
}

pub fn workflow_values(workflows: &[Workflow]) -> Vec<String> {
    workflows
        .iter()
        .map(|workflow| workflow.value().to_string())
        .collect()
}

pub fn workflows_from_values(values: &[String]) -> Vec<Workflow> {
    values
        .iter()
        .filter_map(|value| workflow_from_value(value))
        .collect()
}

#[derive(Default, Props)]
pub struct WorkflowMultiselectDialogProps<'a> {
    pub default_selections: Vec<Workflow>,
    pub on_submit: HandlerMut<'a, Vec<Workflow>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkflowAction {
    Submit(Vec<String>),
    ShowError,
}

/// Maps to: CC `components/WorkflowMultiselectDialog.tsx`.
#[component]
pub fn WorkflowMultiselectDialog<'a>(
    props: &mut WorkflowMultiselectDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut show_error = hooks.use_state(|| false);
    let mut pending_action = hooks.use_state(|| Option::<WorkflowAction>::None);

    let pending = {
        let action = pending_action.read();
        action.clone()
    };
    if let Some(action) = pending {
        pending_action.set(None);
        match action {
            WorkflowAction::Submit(values) => {
                let workflows = workflows_from_values(&values);
                if workflows.is_empty() {
                    show_error.set(true);
                } else {
                    show_error.set(false);
                    (props.on_submit)(workflows);
                }
            }
            WorkflowAction::ShowError => show_error.set(true),
        }
    }

    let mut submit_pending = pending_action;
    let mut cancel_pending = pending_action;
    let mut dialog_cancel_pending = pending_action;

    element! {
        Dialog(
            title: "Select GitHub workflows to install".to_string(),
            subtitle: Some("We'll create a workflow file in your repository for each one you select.".to_string()),
            color: Some(theme.permission),
            hide_input_guide: true,
            on_cancel: move |_| dialog_cancel_pending.set(Some(WorkflowAction::ShowError)),
        ) {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "More workflow examples (issue triage, CI fixes, etc.) at:".to_string(), dim: true, wrap: TextWrap::Wrap)
                Link(url: WORKFLOW_EXAMPLES_URL.to_string())
            }
            SelectMulti(
                options: workflow_options(),
                default_value: workflow_values(&props.default_selections),
                hide_indexes: true,
                handle_escape: Some(false),
                on_submit: move |selected_values: Vec<String>| submit_pending.set(Some(WorkflowAction::Submit(selected_values))),
                on_cancel: move |_| cancel_pending.set(Some(WorkflowAction::ShowError)),
            )
            #(if show_error.get() {
                Some(element! {
                    Text(content: "You must select at least one workflow to continue".to_string(), color: theme.error, wrap: TextWrap::Wrap)
                })
            } else { None })
            View(margin_top: 1u32) {
                Byline {
                    KeyboardShortcutHint(shortcut: "↑↓".to_string(), action: "navigate".to_string())
                    KeyboardShortcutHint(shortcut: "Space".to_string(), action: "toggle".to_string())
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
    fn workflow_helpers_match_official_options() {
        let options = workflow_options();
        assert_eq!(options[0].value, "claude");
        assert_eq!(options[1].value, "claude-review");
        assert_eq!(workflow_from_value("claude"), Some(Workflow::Claude));
        assert_eq!(workflow_from_value("missing"), None);
        assert_eq!(
            workflow_values(&[Workflow::ClaudeReview]),
            vec!["claude-review".to_string()]
        );
        assert_eq!(
            workflows_from_values(&["claude".to_string(), "missing".to_string()]),
            vec![Workflow::Claude]
        );
    }

    #[test]
    fn workflow_multiselect_dialog_renders_options_link_and_hints() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WorkflowMultiselectDialog(default_selections: vec![Workflow::Claude])
            }
        }
        .render(Some(140))
        .to_string();

        assert!(
            text.contains("Select GitHub workflows to install"),
            "canvas=\n{text}"
        );
        assert!(text.contains("@nyan - Tag @claude"), "canvas=\n{text}");
        assert!(text.contains("nyan Review"), "canvas=\n{text}");
        assert!(text.contains(WORKFLOW_EXAMPLES_URL), "canvas=\n{text}");
        assert!(text.contains("Space to toggle"), "canvas=\n{text}");
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");
    }

    #[test]
    fn workflow_multiselect_submit_emits_selected_workflows() {
        let submissions = Arc::new(Mutex::new(Vec::<Vec<Workflow>>::new()));
        let submissions_for_handler = Arc::clone(&submissions);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    WorkflowMultiselectDialog(
                        default_selections: vec![Workflow::Claude],
                        on_submit: move |workflows| submissions_for_handler.lock().expect("submissions mutex").push(workflows),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(140, 24),
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
            submissions.lock().expect("submissions mutex").as_slice(),
            &[vec![Workflow::Claude]]
        );
    }

    #[test]
    fn workflow_multiselect_rejects_empty_selection_with_error() {
        let text = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    WorkflowMultiselectDialog(default_selections: vec![Workflow::Claude])
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Char(' ')),
                        key(KeyCode::Enter),
                    ]))
                    .with_size(140, 24),
                ),
            );
            let mut latest = String::new();
            for _ in 0..10 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if let Some(canvas) = next {
                    latest = canvas.to_string();
                } else {
                    break;
                }
            }
            latest
        });

        assert!(
            text.contains("You must select at least one workflow to continue"),
            "canvas=\n{text}"
        );
    }
}
