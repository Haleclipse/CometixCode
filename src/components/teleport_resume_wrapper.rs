//! Maps to: CC `components/TeleportResumeWrapper.tsx`.
//!
//! The official wrapper composes `useTeleportResume`, analytics, keybindings,
//! and `ResumeTask`. Cometix keeps analytics and external teleport execution
//! outside this render boundary; callers provide the same state snapshot.

use crate::components::resume_task::{CodeSession, ResumeTask, ResumeTaskState};
use crate::components::spinner::Spinner;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TeleportSource {
    #[default]
    CliArg,
    LocalCommand,
}

impl TeleportSource {
    /// Maps to: CC `hooks/useTeleportResume.tsx#TeleportSource`.
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::CliArg => "cliArg",
            Self::LocalCommand => "localCommand",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeleportResumeError {
    pub message: String,
    pub formatted_message: Option<String>,
    pub is_operation_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeleportResumeWrapperState {
    Selecting { resume_task_state: ResumeTaskState },
    Resuming { selected_session: CodeSession },
    Error { error: TeleportResumeError },
}

impl Default for TeleportResumeWrapperState {
    fn default() -> Self {
        Self::Selecting {
            resume_task_state: ResumeTaskState::default(),
        }
    }
}

#[derive(Default, Props)]
pub struct TeleportResumeWrapperProps {
    pub state: TeleportResumeWrapperState,
    pub is_embedded: bool,
    pub source: TeleportSource,
    /// Maps to official optional `onError`: when true, wrapper suppresses the
    /// inline error UI because caller handles the error externally.
    pub has_external_error_handler: bool,
}

/// Maps to: CC `TeleportResumeWrapper.tsx` loading branch.
pub fn teleport_resume_loading_lines(session_title: &str) -> (String, String) {
    (
        "Resuming session…".to_string(),
        format!("Loading \"{session_title}\"…"),
    )
}

/// Maps to: CC `components/TeleportResumeWrapper.tsx#TeleportResumeWrapper`.
#[component]
pub fn TeleportResumeWrapper(props: &TeleportResumeWrapperProps) -> impl Into<AnyElement<'static>> {
    let _ = props.source.as_official_str();
    match &props.state {
        TeleportResumeWrapperState::Resuming { selected_session } => {
            let (title, detail) = teleport_resume_loading_lines(&selected_session.title);
            element! {
                View(flex_direction: FlexDirection::Column, padding: 1u32) {
                    View(flex_direction: FlexDirection::Row) {
                        Spinner
                        Text(content: title, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    }
                    Text(content: detail, dim: true, wrap: TextWrap::NoWrap)
                }
            }
            .into_any()
        }
        TeleportResumeWrapperState::Error { error } => {
            if props.has_external_error_handler {
                return element! { View(width: 0u32, height: 0u32) }.into_any();
            }
            element! {
                View(flex_direction: FlexDirection::Column, padding: 1u32) {
                    Text(content: "Failed to resume session".to_string(), weight: Weight::Bold, color: Color::Red, wrap: TextWrap::NoWrap)
                    Text(content: error.message.clone(), dim: true, wrap: TextWrap::Wrap)
                    View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                        Text(content: "Press ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                        Text(content: "Esc".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: " to cancel".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    }
                }
            }
            .into_any()
        }
        TeleportResumeWrapperState::Selecting { resume_task_state } => element! {
            ResumeTask(state: resume_task_state.clone())
        }
        .into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::resume_task::{CodeSession, ResumeTaskLoadErrorType};
    use crate::utils::theme;

    fn render(state: TeleportResumeWrapperState, has_external_error_handler: bool) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TeleportResumeWrapper(
                    state: state,
                    source: TeleportSource::LocalCommand,
                    has_external_error_handler: has_external_error_handler,
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn teleport_source_preserves_official_strings() {
        assert_eq!(TeleportSource::CliArg.as_official_str(), "cliArg");
        assert_eq!(
            TeleportSource::LocalCommand.as_official_str(),
            "localCommand"
        );
    }

    #[test]
    fn teleport_resume_loading_lines_match_official_copy() {
        assert_eq!(
            teleport_resume_loading_lines("Fix MCP"),
            (
                "Resuming session…".to_string(),
                "Loading \"Fix MCP\"…".to_string()
            )
        );
    }

    #[test]
    fn teleport_resume_wrapper_renders_resuming_error_and_selecting_branches() {
        let resuming = render(
            TeleportResumeWrapperState::Resuming {
                selected_session: CodeSession {
                    id: "s1".to_string(),
                    title: "Fix MCP".to_string(),
                    updated_at: String::new(),
                    repo: None,
                },
            },
            false,
        );
        assert!(
            resuming.contains("Resuming session…"),
            "canvas=\n{resuming}"
        );
        assert!(
            resuming.contains("Loading \"Fix MCP\"…"),
            "canvas=\n{resuming}"
        );

        let error = render(
            TeleportResumeWrapperState::Error {
                error: TeleportResumeError {
                    message: "branch checkout failed".to_string(),
                    formatted_message: None,
                    is_operation_error: true,
                },
            },
            false,
        );
        assert!(
            error.contains("Failed to resume session"),
            "canvas=\n{error}"
        );
        assert!(error.contains("branch checkout failed"), "canvas=\n{error}");
        assert!(error.contains("Press Esc to cancel"), "canvas=\n{error}");

        let suppressed = render(
            TeleportResumeWrapperState::Error {
                error: TeleportResumeError {
                    message: "handled elsewhere".to_string(),
                    formatted_message: None,
                    is_operation_error: false,
                },
            },
            true,
        );
        assert_eq!(suppressed, "");

        let selecting = render(
            TeleportResumeWrapperState::Selecting {
                resume_task_state: ResumeTaskState::LoadError {
                    error_type: ResumeTaskLoadErrorType::Network,
                    esc_key: "Esc".to_string(),
                },
            },
            false,
        );
        assert!(
            selecting.contains("Error loading Claude Code sessions"),
            "canvas=\n{selecting}"
        );
    }
}
