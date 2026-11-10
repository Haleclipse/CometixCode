//! Maps to: CC `tools/BashTool/bashCommandHelpers.ts`.
//!
//! Operator/pipe permission helpers. Parsing and redirection stripping delegate
//! to `utils/bash/ParsedCommand.ts`'s Rust owner, matching the CC boundary.

use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub struct CommandIdentityCheckers {
    pub is_normalized_cd_command: fn(&str) -> bool,
    pub is_normalized_git_command: fn(&str) -> bool,
}

impl Default for CommandIdentityCheckers {
    fn default() -> Self {
        Self {
            is_normalized_cd_command: super::bash_permissions::is_normalized_cd_command,
            is_normalized_git_command: super::bash_permissions::is_normalized_git_command,
        }
    }
}

/// Maps to CC local `segmentedCommandPermissionResult(...)`.
pub fn segmented_command_permission_result<F>(
    input_command: &str,
    segments: &[String],
    mut bash_tool_has_permission_fn: F,
    checkers: CommandIdentityCheckers,
) -> PermissionResult
where
    F: FnMut(&str) -> PermissionResult,
{
    let cd_commands = segments
        .iter()
        .filter(|segment| (checkers.is_normalized_cd_command)(segment.trim()))
        .count();
    if cd_commands > 1 {
        let reason = PermissionDecisionReason::Other {
            reason: "Multiple directory changes in one command require approval for clarity"
                .to_string(),
        };
        return PermissionResult::Ask {
            message: crate::utils::permissions::permissions::create_permission_request_message(
                crate::tools::bash_tool::tool_name::BASH_TOOL_NAME,
                Some(&reason),
            ),
            updated_input: None,
            decision_reason: Some(reason),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    let mut has_cd = false;
    let mut has_git = false;
    for segment in segments {
        for subcommand in crate::utils::bash::commands::split_command_deprecated(segment) {
            let trimmed = subcommand.trim();
            if (checkers.is_normalized_cd_command)(trimmed) {
                has_cd = true;
            }
            if (checkers.is_normalized_git_command)(trimmed) {
                has_git = true;
            }
        }
    }
    if has_cd && has_git {
        let reason = PermissionDecisionReason::Other {
            reason: "Compound commands with cd and git require approval to prevent bare repository attacks"
                .to_string(),
        };
        return PermissionResult::Ask {
            message: crate::utils::permissions::permissions::create_permission_request_message(
                crate::tools::bash_tool::tool_name::BASH_TOOL_NAME,
                Some(&reason),
            ),
            updated_input: None,
            decision_reason: Some(reason),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    let mut segment_results: BTreeMap<String, Box<PermissionResult>> = BTreeMap::new();
    for segment in segments {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let result = bash_tool_has_permission_fn(trimmed);
        segment_results.insert(trimmed.to_string(), Box::new(result));
    }

    if let Some((segment_command, segment_result)) = segment_results
        .iter()
        .find(|(_, result)| matches!(result.as_ref(), PermissionResult::Deny { .. }))
    {
        let message = match segment_result.as_ref() {
            PermissionResult::Deny { message, .. } => message.clone(),
            _ => format!("Permission denied for: {segment_command}"),
        };
        return PermissionResult::Deny {
            message,
            decision_reason: PermissionDecisionReason::SubcommandResults {
                reasons: segment_results,
            },
            tool_use_id: None,
        };
    }

    if !segment_results.is_empty()
        && segment_results
            .values()
            .all(|result| matches!(result.as_ref(), PermissionResult::Allow { .. }))
    {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": input_command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::SubcommandResults {
                reasons: segment_results,
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    let mut suggestions = Vec::new();
    for result in segment_results.values() {
        match result.as_ref() {
            PermissionResult::Ask { suggestions: s, .. }
            | PermissionResult::Passthrough { suggestions: s, .. } => {
                suggestions.extend(s.clone());
            }
            PermissionResult::Allow { .. } | PermissionResult::Deny { .. } => {}
        }
    }

    let reason = PermissionDecisionReason::SubcommandResults {
        reasons: segment_results,
    };
    PermissionResult::Ask {
        message: crate::utils::permissions::permissions::create_permission_request_message(
            crate::tools::bash_tool::tool_name::BASH_TOOL_NAME,
            Some(&reason),
        ),
        updated_input: None,
        decision_reason: Some(reason),
        suggestions,
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

/// Maps to CC local `buildSegmentWithoutRedirections(segmentCommand)`.
pub fn build_segment_without_redirections(segment_command: &str) -> String {
    if !segment_command.contains('>') {
        return segment_command.to_string();
    }
    crate::utils::bash::parsed_command::ParsedCommand::parse(segment_command)
        .map(|parsed| parsed.without_output_redirections())
        .unwrap_or_else(|| segment_command.to_string())
}

/// Maps to CC `checkCommandOperatorPermissions(...)`.
pub fn check_command_operator_permissions<F>(
    input_command: &str,
    bash_tool_has_permission_fn: F,
    checkers: CommandIdentityCheckers,
) -> PermissionResult
where
    F: FnMut(&str) -> PermissionResult,
{
    bash_tool_check_command_operator_permissions(
        input_command,
        bash_tool_has_permission_fn,
        checkers,
    )
}

/// Maps to CC local `bashToolCheckCommandOperatorPermissions(...)`.
pub fn bash_tool_check_command_operator_permissions<F>(
    input_command: &str,
    bash_tool_has_permission_fn: F,
    checkers: CommandIdentityCheckers,
) -> PermissionResult
where
    F: FnMut(&str) -> PermissionResult,
{
    let Some(parsed) = crate::utils::bash::parsed_command::ParsedCommand::parse(input_command)
    else {
        return PermissionResult::Passthrough {
            message: "Failed to parse command".to_string(),
            decision_reason: None,
            suggestions: Vec::new(),
            blocked_path: None,
            pending_classifier_check: None,
        };
    };
    let is_unsafe_compound = parsed.get_tree_sitter_analysis().is_some_and(|analysis| {
        analysis.compound_structure.has_subshell || analysis.compound_structure.has_command_group
    }) || (parsed.get_tree_sitter_analysis().is_none()
        && crate::utils::bash::commands::is_unsafe_compound_command_deprecated(input_command));
    if is_unsafe_compound {
        let safety_result = super::bash_security::bash_command_is_safe_deprecated(input_command);
        let reason = PermissionDecisionReason::Other {
            reason: match safety_result {
                PermissionResult::Ask { message, .. } if !message.is_empty() => message,
                _ => {
                    "This command uses shell operators that require approval for safety".to_string()
                }
            },
        };
        return PermissionResult::Ask {
            message: crate::utils::permissions::permissions::create_permission_request_message(
                crate::tools::bash_tool::tool_name::BASH_TOOL_NAME,
                Some(&reason),
            ),
            updated_input: None,
            decision_reason: Some(reason),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    let pipe_segments = parsed.get_pipe_segments();
    if pipe_segments.len() <= 1 {
        return PermissionResult::Passthrough {
            message: "No pipes found in command".to_string(),
            decision_reason: None,
            suggestions: Vec::new(),
            blocked_path: None,
            pending_classifier_check: None,
        };
    }

    let segments = pipe_segments
        .into_iter()
        .map(|segment| build_segment_without_redirections(&segment))
        .collect::<Vec<_>>();
    segmented_command_permission_result(
        input_command,
        &segments,
        bash_tool_has_permission_fn,
        checkers,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionMode, PermissionUpdate, PermissionUpdateDestination,
    };

    fn allow_result() -> PermissionResult {
        PermissionResult::Allow {
            updated_input: None,
            user_modified: None,
            decision_reason: None,
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        }
    }

    #[test]
    fn segmented_result_blocks_multiple_cd_and_cd_git_cross_segment() {
        let checkers = CommandIdentityCheckers::default();
        let result = segmented_command_permission_result(
            "cd a | cd b",
            &["cd a".to_string(), "cd b".to_string()],
            |_| allow_result(),
            checkers,
        );
        assert!(matches!(
            result,
            PermissionResult::Ask { ref message, decision_reason: Some(PermissionDecisionReason::Other { .. }), .. }
                if message == "Multiple directory changes in one command require approval for clarity"
        ));

        let result = segmented_command_permission_result(
            "cd repo | git status",
            &["cd repo".to_string(), "git status".to_string()],
            |_| allow_result(),
            checkers,
        );
        assert!(matches!(
            result,
            PermissionResult::Ask { ref message, decision_reason: Some(PermissionDecisionReason::Other { .. }), .. }
                if message == "Compound commands with cd and git require approval to prevent bare repository attacks"
        ));
    }

    #[test]
    fn segmented_result_shapes_allow_deny_and_ask_like_official() {
        let checkers = CommandIdentityCheckers::default();
        let allow = segmented_command_permission_result(
            "echo hi | grep hi",
            &["echo hi".to_string(), "grep hi".to_string()],
            |_| allow_result(),
            checkers,
        );
        assert!(matches!(
            allow,
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::SubcommandResults { .. }),
                ..
            }
        ));

        let deny = segmented_command_permission_result(
            "echo hi | rm x",
            &["echo hi".to_string(), "rm x".to_string()],
            |cmd| {
                if cmd == "rm x" {
                    PermissionResult::Deny {
                        message: "denied rm".to_string(),
                        decision_reason: PermissionDecisionReason::Other {
                            reason: "deny".to_string(),
                        },
                        tool_use_id: None,
                    }
                } else {
                    allow_result()
                }
            },
            checkers,
        );
        assert!(
            matches!(deny, PermissionResult::Deny { ref message, .. } if message == "denied rm")
        );

        let ask = segmented_command_permission_result(
            "echo hi | cargo test",
            &["echo hi".to_string(), "cargo test".to_string()],
            |cmd| {
                if cmd == "cargo test" {
                    PermissionResult::Ask {
                        message: "ask cargo".to_string(),
                        updated_input: None,
                        decision_reason: None,
                        suggestions: vec![PermissionUpdate::SetMode {
                            destination: PermissionUpdateDestination::Session,
                            mode: PermissionMode::AcceptEdits,
                        }],
                        blocked_path: None,
                        metadata: None,
                        is_bash_security_check_for_misparsing: false,
                        pending_classifier_check: None,
                        content_blocks: Vec::new(),
                    }
                } else {
                    allow_result()
                }
            },
            checkers,
        );
        assert!(matches!(
            ask,
            PermissionResult::Ask { ref message, ref suggestions, decision_reason: Some(PermissionDecisionReason::SubcommandResults { .. }), .. }
                if message.contains("The following part requires approval: cargo test") && suggestions.len() == 1
        ));
    }

    #[test]
    fn operator_permission_strips_redirections_and_handles_pipe_segments() {
        assert_eq!(
            build_segment_without_redirections("grep foo file.txt > out.txt"),
            "grep foo file.txt"
        );
        let result = check_command_operator_permissions(
            "echo hi | grep hi > out.txt",
            |cmd| {
                if cmd == "grep hi" {
                    PermissionResult::Ask {
                        message: "ask grep".to_string(),
                        updated_input: None,
                        decision_reason: None,
                        suggestions: Vec::new(),
                        blocked_path: None,
                        metadata: None,
                        is_bash_security_check_for_misparsing: false,
                        pending_classifier_check: None,
                        content_blocks: Vec::new(),
                    }
                } else {
                    allow_result()
                }
            },
            CommandIdentityCheckers::default(),
        );
        assert!(matches!(
            result,
            PermissionResult::Ask { ref message, .. }
                if message.contains("grep hi") && !message.contains("out.txt")
        ));
    }

    #[test]
    fn no_pipe_passthrough_and_compound_group_asks() {
        assert!(matches!(
            check_command_operator_permissions(
                "echo hi",
                |_| allow_result(),
                CommandIdentityCheckers::default()
            ),
            PermissionResult::Passthrough { ref message, .. } if message == "No pipes found in command"
        ));
        assert!(matches!(
            check_command_operator_permissions(
                "(echo hi)",
                |_| allow_result(),
                CommandIdentityCheckers::default()
            ),
            PermissionResult::Ask { ref message, .. }
                if message == "This command uses shell operators that require approval for safety"
        ));
    }
}
