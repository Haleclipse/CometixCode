//! Maps to: CC `components/permissions/FilePermissionDialog/usePermissionHandler.ts`.
//!
//! The React hook mutates `ToolUseConfirm` callbacks and emits telemetry. This
//! Rust module keeps the official decision/update shaping as a pure outcome;
//! callers in the permission/tool execution flow decide how to send callbacks.

use super::permission_options::{PermissionOption, PermissionSessionScope};
use crate::types::permissions::{
    PermissionBehavior, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
};

pub use crate::tools::file_edit_tool::constants::{
    CLAUDE_FOLDER_PERMISSION_PATTERN, FILE_EDIT_TOOL_NAME, GLOBAL_CLAUDE_FOLDER_PERMISSION_PATTERN,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionHandlerOptions {
    pub has_feedback: bool,
    pub feedback: Option<String>,
    pub entered_feedback_mode: bool,
    pub scope: Option<PermissionSessionScope>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionHandlerEvent {
    Accept,
    Reject,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionHandlerOutcome {
    pub accepted: bool,
    pub rejected: bool,
    pub permission_updates: Vec<PermissionUpdate>,
    pub feedback: Option<String>,
    pub event: Option<PermissionHandlerEvent>,
}

/// Maps to: CC `usePermissionHandler.ts#handleAcceptSession` special
/// `.nyan/` session-scope update branch.
pub fn claude_folder_permission_update(scope: PermissionSessionScope) -> PermissionUpdate {
    let pattern = match scope {
        PermissionSessionScope::ClaudeFolder => CLAUDE_FOLDER_PERMISSION_PATTERN,
        PermissionSessionScope::GlobalClaudeFolder => GLOBAL_CLAUDE_FOLDER_PERMISSION_PATTERN,
    };
    PermissionUpdate::AddRules {
        destination: PermissionUpdateDestination::Session,
        behavior: PermissionBehavior::Allow,
        rules: vec![PermissionRuleValue::new(
            FILE_EDIT_TOOL_NAME,
            Some(pattern.to_string()),
        )],
    }
}

/// Maps to: CC `PERMISSION_HANDLERS` option dispatch.
pub fn handle_permission_option(
    option: &PermissionOption,
    generated_session_updates: Vec<PermissionUpdate>,
    options: PermissionHandlerOptions,
) -> PermissionHandlerOutcome {
    match option {
        PermissionOption::AcceptOnce => PermissionHandlerOutcome {
            accepted: true,
            feedback: options.feedback,
            event: Some(PermissionHandlerEvent::Accept),
            ..PermissionHandlerOutcome::default()
        },
        PermissionOption::AcceptSession { scope } => {
            let updates = scope
                .clone()
                .or(options.scope)
                .map(|scope| vec![claude_folder_permission_update(scope)])
                .unwrap_or(generated_session_updates);
            PermissionHandlerOutcome {
                accepted: true,
                permission_updates: updates,
                event: Some(PermissionHandlerEvent::Accept),
                ..PermissionHandlerOutcome::default()
            }
        }
        PermissionOption::Reject => PermissionHandlerOutcome {
            rejected: true,
            feedback: options.feedback,
            event: Some(PermissionHandlerEvent::Reject),
            ..PermissionHandlerOutcome::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_handler_accept_once_and_reject_forward_feedback() {
        let accept = handle_permission_option(
            &PermissionOption::AcceptOnce,
            vec![],
            PermissionHandlerOptions {
                feedback: Some("continue".to_string()),
                ..PermissionHandlerOptions::default()
            },
        );
        assert!(accept.accepted);
        assert_eq!(accept.feedback.as_deref(), Some("continue"));
        assert_eq!(accept.event, Some(PermissionHandlerEvent::Accept));

        let reject = handle_permission_option(
            &PermissionOption::Reject,
            vec![],
            PermissionHandlerOptions {
                feedback: Some("try differently".to_string()),
                ..PermissionHandlerOptions::default()
            },
        );
        assert!(reject.rejected);
        assert_eq!(reject.feedback.as_deref(), Some("try differently"));
        assert_eq!(reject.event, Some(PermissionHandlerEvent::Reject));
    }

    #[test]
    fn permission_handler_accept_session_prefers_claude_folder_scope() {
        let generated = vec![PermissionUpdate::AddDirectories {
            destination: PermissionUpdateDestination::Session,
            directories: vec!["/repo".to_string()],
        }];
        let outcome = handle_permission_option(
            &PermissionOption::AcceptSession {
                scope: Some(PermissionSessionScope::ClaudeFolder),
            },
            generated,
            PermissionHandlerOptions::default(),
        );
        assert!(outcome.accepted);
        assert_eq!(outcome.permission_updates.len(), 1);
        assert_eq!(
            outcome.permission_updates[0],
            PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::Session,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    FILE_EDIT_TOOL_NAME,
                    Some(CLAUDE_FOLDER_PERMISSION_PATTERN.to_string())
                )],
            }
        );
    }
}
