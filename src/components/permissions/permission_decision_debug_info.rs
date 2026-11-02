//! Maps to: CC `components/permissions/PermissionDecisionDebugInfo.tsx`.
//!
//! Supports the full `PermissionDecisionReason` union used by the permission
//! runtime, including compound subcommands, suggestions, and unreachable-rule
//! diagnostics. A legacy decision prop remains for older callers while they
//! transition to the official result type.

use crate::types::permissions::{
    PermissionBehavior, PermissionMode, PermissionPromptChoice, PermissionUpdate, PromptDecision,
};
use crate::utils::permissions::permission_mode::permission_mode_title;
use crate::utils::permissions::permission_result::{
    PermissionDecision as RuntimePermissionDecision, PermissionDecisionReason,
    PermissionResult as RuntimePermissionResult,
};
use crate::utils::permissions::permission_rule_parser::permission_rule_value_to_string;
use crate::utils::permissions::permission_update::extract_rules;
use crate::utils::permissions::permissions::permission_rule_source_display_string;
use crate::utils::permissions::shadowed_rule_detection::{
    DetectUnreachableRulesOptions, UnreachableRule, detect_unreachable_rules,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionDecisionDebugRows {
    pub behavior: String,
    pub choice: String,
    pub message: Option<String>,
    pub rules: Vec<String>,
    pub directories: Vec<String>,
    pub mode: Option<String>,
}

fn behavior_label(behavior: PermissionBehavior) -> &'static str {
    match behavior {
        PermissionBehavior::Allow => "allow",
        PermissionBehavior::Ask => "ask",
        PermissionBehavior::Deny => "deny",
    }
}

fn choice_label(choice: PermissionPromptChoice) -> &'static str {
    match choice {
        PermissionPromptChoice::AllowOnce => "allowOnce",
        PermissionPromptChoice::Deny => "deny",
        PermissionPromptChoice::AlwaysAllow => "alwaysAllow",
    }
}

/// Maps to: CC `extractDirectories(...)`.
pub fn extract_directories(updates: &[PermissionUpdate]) -> Vec<String> {
    updates
        .iter()
        .flat_map(|update| match update {
            PermissionUpdate::AddDirectories { directories, .. } => directories.clone(),
            _ => Vec::new(),
        })
        .collect()
}

/// Maps to: CC `extractMode(...)`.
pub fn extract_mode(updates: &[PermissionUpdate]) -> Option<PermissionMode> {
    updates.iter().rev().find_map(|update| match update {
        PermissionUpdate::SetMode { mode, .. } => Some(*mode),
        _ => None,
    })
}

/// Maps to: CC `SuggestionDisplay(...)` data shaping.
pub fn permission_decision_debug_rows(
    permission_result: &PromptDecision,
) -> PermissionDecisionDebugRows {
    let rules = extract_rules(&permission_result.updates)
        .iter()
        .map(permission_rule_value_to_string)
        .collect::<Vec<_>>();
    let directories = extract_directories(&permission_result.updates);
    let mode = extract_mode(&permission_result.updates).map(permission_mode_title);

    PermissionDecisionDebugRows {
        behavior: behavior_label(permission_result.behavior).to_string(),
        choice: choice_label(permission_result.choice).to_string(),
        message: (!permission_result.transcript.trim().is_empty())
            .then(|| permission_result.transcript.clone()),
        rules,
        directories,
        mode: mode.map(str::to_string),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionReasonLine {
    pub text: String,
    pub indent: usize,
    pub success: Option<bool>,
}

/// Maps to CC `decisionReasonDisplayString` with Rust's typed sandbox enum.
pub fn decision_reason_display(reason: &PermissionDecisionReason) -> String {
    match reason {
        PermissionDecisionReason::Rule { rule } => format!(
            "{} rule from {}",
            permission_rule_value_to_string(&rule.rule_value),
            permission_rule_source_display_string(rule.source),
        ),
        PermissionDecisionReason::Mode { mode } => {
            format!("{} mode", permission_mode_title(*mode))
        }
        PermissionDecisionReason::PermissionPromptTool {
            permission_prompt_tool_name,
            ..
        } => format!("{permission_prompt_tool_name} permission prompt tool"),
        PermissionDecisionReason::Hook {
            hook_name, reason, ..
        } => reason
            .as_ref()
            .map(|reason| format!("{hook_name} hook: {reason}"))
            .unwrap_or_else(|| format!("{hook_name} hook")),
        PermissionDecisionReason::AsyncAgent { reason }
        | PermissionDecisionReason::WorkingDir { reason }
        | PermissionDecisionReason::SafetyCheck { reason, .. }
        | PermissionDecisionReason::Other { reason } => reason.clone(),
        PermissionDecisionReason::SandboxOverride { .. } => {
            "Requires permission to bypass sandbox".to_string()
        }
        PermissionDecisionReason::Classifier { classifier, reason } => {
            format!("{classifier} classifier: {reason}")
        }
        PermissionDecisionReason::SubcommandResults { .. } => String::new(),
    }
}

fn result_parts(
    result: &RuntimePermissionResult,
) -> (
    &'static str,
    Option<&PermissionDecisionReason>,
    &[PermissionUpdate],
) {
    match result {
        RuntimePermissionResult::Allow {
            decision_reason, ..
        } => ("allow", decision_reason.as_ref(), &[]),
        RuntimePermissionResult::Ask {
            decision_reason,
            suggestions,
            ..
        }
        | RuntimePermissionResult::Passthrough {
            decision_reason,
            suggestions,
            ..
        } => (result.behavior(), decision_reason.as_ref(), suggestions),
        RuntimePermissionResult::Deny {
            decision_reason, ..
        } => ("deny", Some(decision_reason), &[]),
    }
}

/// Maps to CC `PermissionDecisionInfoItem`, including compound subcommands.
pub fn decision_reason_lines(reason: &PermissionDecisionReason) -> Vec<DecisionReasonLine> {
    let PermissionDecisionReason::SubcommandResults { reasons } = reason else {
        return vec![DecisionReasonLine {
            text: decision_reason_display(reason),
            indent: 0,
            success: None,
        }];
    };
    let mut lines = Vec::new();
    for (subcommand, result) in reasons {
        let (behavior, child_reason, suggestions) = result_parts(result);
        lines.push(DecisionReasonLine {
            text: subcommand.clone(),
            indent: 0,
            success: Some(behavior == "allow"),
        });
        if let Some(child_reason) = child_reason
            && !matches!(
                child_reason,
                PermissionDecisionReason::SubcommandResults { .. }
            )
        {
            lines.push(DecisionReasonLine {
                text: format!("⎿  {}", decision_reason_display(child_reason)),
                indent: 2,
                success: None,
            });
        }
        let rules = extract_rules(suggestions)
            .iter()
            .map(permission_rule_value_to_string)
            .collect::<Vec<_>>();
        if behavior == "ask" && !rules.is_empty() {
            lines.push(DecisionReasonLine {
                text: format!("⎿  Suggested rules: {}", rules.join(", ")),
                indent: 2,
                success: None,
            });
        }
    }
    lines
}

fn runtime_decision_parts(
    result: &RuntimePermissionDecision,
) -> (
    &'static str,
    Option<&str>,
    Option<&PermissionDecisionReason>,
    &[PermissionUpdate],
) {
    match result {
        RuntimePermissionDecision::Allow {
            decision_reason, ..
        } => ("allow", None, decision_reason.as_ref(), &[]),
        RuntimePermissionDecision::Ask {
            message,
            decision_reason,
            suggestions,
            ..
        } => ("ask", Some(message), decision_reason.as_ref(), suggestions),
        RuntimePermissionDecision::Deny {
            message,
            decision_reason,
            ..
        } => ("deny", Some(message), Some(decision_reason), &[]),
    }
}

fn matching_unreachable_rules(
    context: Option<&crate::tool::ToolPermissionContext>,
    suggestions: &[PermissionUpdate],
    tool_name: Option<&str>,
    sandbox_auto_allow_enabled: bool,
) -> Vec<UnreachableRule> {
    let Some(context) = context else {
        return Vec::new();
    };
    let all = detect_unreachable_rules(
        context,
        DetectUnreachableRulesOptions {
            sandbox_auto_allow_enabled,
        },
    );
    let suggested = extract_rules(suggestions);
    if !suggested.is_empty() {
        return all
            .into_iter()
            .filter(|unreachable| {
                suggested.iter().any(|rule| {
                    rule.tool_name == unreachable.rule.rule_value.tool_name
                        && rule.rule_content == unreachable.rule.rule_value.rule_content
                })
            })
            .collect();
    }
    tool_name.map_or(all.clone(), |tool_name| {
        all.into_iter()
            .filter(|unreachable| unreachable.rule.rule_value.tool_name == tool_name)
            .collect()
    })
}

#[derive(Default, Props)]
pub struct PermissionDecisionDebugInfoProps {
    /// Official runtime decision shape.
    pub runtime_permission_result: Option<RuntimePermissionDecision>,
    /// Compatibility seam for older UI-only decisions.
    pub permission_result: Option<PromptDecision>,
    pub tool_name: Option<String>,
    pub permission_context: Option<crate::tool::ToolPermissionContext>,
    pub sandbox_auto_allow_enabled: bool,
}

/// Maps to: CC `PermissionDecisionDebugInfo(...)` render path.
#[component]
pub fn PermissionDecisionDebugInfo(
    props: &PermissionDecisionDebugInfoProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    if let Some(runtime_result) = props.runtime_permission_result.as_ref() {
        let (behavior, message, reason, suggestions) = runtime_decision_parts(runtime_result);
        let behavior = behavior.to_string();
        let message = message.map(str::to_string);
        let reason_lines = reason.map(decision_reason_lines).unwrap_or_else(|| {
            vec![DecisionReasonLine {
                text: "undefined".to_string(),
                indent: 0,
                success: None,
            }]
        });
        let rules = extract_rules(suggestions)
            .iter()
            .map(permission_rule_value_to_string)
            .collect::<Vec<_>>();
        let directories = extract_directories(suggestions);
        let mode = extract_mode(suggestions).map(permission_mode_title);
        let no_displayable_suggestions =
            rules.is_empty() && directories.is_empty() && mode.is_none();
        let suggestion_label = if suggestions.is_empty() {
            "Suggestions "
        } else {
            "Suggestion "
        };
        let unreachable = matching_unreachable_rules(
            props.permission_context.as_ref(),
            suggestions,
            props.tool_name.as_deref(),
            props.sandbox_auto_allow_enabled,
        );
        let width = 10u32;
        return element! {
            View(flex_direction: FlexDirection::Column) {
                View(flex_direction: FlexDirection::Row) {
                    View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                        Text(content: "Behavior ".to_string(), color: theme.inactive)
                    }
                    Text(content: behavior)
                }
                #(message.map(|message| element! {
                    View(flex_direction: FlexDirection::Row) {
                        View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                            Text(content: "Message ".to_string(), color: theme.inactive)
                        }
                        Text(content: message, wrap: TextWrap::Wrap)
                    }
                }))
                View(flex_direction: FlexDirection::Row) {
                    View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                        Text(content: "Reason ".to_string(), color: theme.inactive)
                    }
                    View(flex_direction: FlexDirection::Column) {
                        #(reason_lines.into_iter().map(|line| {
                            let prefix = match line.success {
                                Some(true) => "✓ ",
                                Some(false) => "✗ ",
                                None => "",
                            };
                            element! {
                                View(margin_left: line.indent as u32) {
                                    Text(
                                        content: format!("{prefix}{}", line.text),
                                        color: match line.success { Some(true) => Some(theme.success), Some(false) => Some(theme.error), None => None },
                                        wrap: TextWrap::Wrap,
                                    )
                                }
                            }
                        }))
                    }
                }
                #(if no_displayable_suggestions {
                    Some(element! {
                        View(flex_direction: FlexDirection::Row) {
                            View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                                Text(content: suggestion_label.to_string(), color: theme.inactive)
                            }
                            Text(content: "None".to_string())
                        }
                    })
                } else {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column) {
                            View(flex_direction: FlexDirection::Row) {
                                View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                                    Text(content: "Suggestions ".to_string(), color: theme.inactive)
                                }
                                Text(content: " ".to_string())
                            }
                            #((!rules.is_empty()).then(|| element! {
                                View(flex_direction: FlexDirection::Row) {
                                    View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                                        Text(content: " Rules ".to_string(), color: theme.inactive)
                                    }
                                    View(flex_direction: FlexDirection::Column) {
                                        #(rules.into_iter().map(|rule| element! { Text(content: format!("• {rule}"), wrap: TextWrap::Wrap) }))
                                    }
                                }
                            }))
                            #((!directories.is_empty()).then(|| element! {
                                View(flex_direction: FlexDirection::Row) {
                                    View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                                        Text(content: " Directories ".to_string(), color: theme.inactive)
                                    }
                                    View(flex_direction: FlexDirection::Column) {
                                        #(directories.into_iter().map(|directory| element! { Text(content: format!("• {directory}"), wrap: TextWrap::Wrap) }))
                                    }
                                }
                            }))
                            #(mode.map(|mode| element! {
                                View(flex_direction: FlexDirection::Row) {
                                    View(justify_content: JustifyContent::FLEX_END, min_width: width) {
                                        Text(content: " Mode ".to_string(), color: theme.inactive)
                                    }
                                    Text(content: mode.to_string())
                                }
                            }))
                        }
                    })
                })
                #((!unreachable.is_empty()).then(|| element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: format!("⚠ Unreachable Rules ({})", unreachable.len()), color: theme.warning)
                        #(unreachable.into_iter().map(|item| element! {
                            View(flex_direction: FlexDirection::Column, margin_left: 2u32) {
                                Text(content: permission_rule_value_to_string(&item.rule.rule_value), color: theme.warning)
                                Text(content: format!("  {}", item.reason), color: theme.inactive, wrap: TextWrap::Wrap)
                                Text(content: format!("  Fix: {}", item.fix), color: theme.inactive, wrap: TextWrap::Wrap)
                            }
                        }))
                    }
                }))
            }
        }
        .into_any();
    }

    let Some(permission_result) = props.permission_result.as_ref() else {
        return element! { View() {} }.into_any();
    };
    let rows = permission_decision_debug_rows(permission_result);
    let width = 10u32;
    let suggestion_empty =
        rows.rules.is_empty() && rows.directories.is_empty() && rows.mode.is_none();
    let _tool_name = props.tool_name.as_deref();

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Row) {
                View(justify_content: JustifyContent::FLEX_END, width: width) {
                    Text(content: "Behavior ".to_string(), color: theme.inactive)
                }
                Text(content: rows.behavior.clone())
            }
            #(rows.message.as_ref().map(|message| element! {
                View(flex_direction: FlexDirection::Row) {
                    View(justify_content: JustifyContent::FLEX_END, width: width) {
                        Text(content: "Message ".to_string(), color: theme.inactive)
                    }
                    Text(content: message.clone(), wrap: TextWrap::Wrap)
                }
            }))
            View(flex_direction: FlexDirection::Row) {
                View(justify_content: JustifyContent::FLEX_END, width: width) {
                    Text(content: "Choice ".to_string(), color: theme.inactive)
                }
                Text(content: rows.choice.clone())
            }
            #(if suggestion_empty {
                Some(element! {
                    View(flex_direction: FlexDirection::Row) {
                        View(justify_content: JustifyContent::FLEX_END, width: width) {
                            Text(content: "Suggestions ".to_string(), color: theme.inactive)
                        }
                        Text(content: "None".to_string())
                    }
                })
            } else {
                None
            })
            #((!rows.rules.is_empty()).then(|| element! {
                View(flex_direction: FlexDirection::Row) {
                    View(justify_content: JustifyContent::FLEX_END, width: width) {
                        Text(content: " Rules ".to_string(), color: theme.inactive)
                    }
                    View(flex_direction: FlexDirection::Column) {
                        #(rows.rules.iter().map(|rule| element! {
                            Text(content: format!("• {rule}"), wrap: TextWrap::Wrap)
                        }))
                    }
                }
            }))
            #((!rows.directories.is_empty()).then(|| element! {
                View(flex_direction: FlexDirection::Row) {
                    View(justify_content: JustifyContent::FLEX_END, width: width) {
                        Text(content: " Directories ".to_string(), color: theme.inactive)
                    }
                    View(flex_direction: FlexDirection::Column) {
                        #(rows.directories.iter().map(|directory| element! {
                            Text(content: format!("• {directory}"), wrap: TextWrap::Wrap)
                        }))
                    }
                }
            }))
            #(rows.mode.as_ref().map(|mode| element! {
                View(flex_direction: FlexDirection::Row) {
                    View(justify_content: JustifyContent::FLEX_END, width: width) {
                        Text(content: " Mode ".to_string(), color: theme.inactive)
                    }
                    Text(content: mode.clone())
                }
            }))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionPromptChoice, PermissionRuleValue, PermissionUpdateDestination,
    };
    use crate::utils::permissions::permission_result::{
        PermissionDecision as RuntimeDecision, PermissionDecisionReason,
        PermissionResult as RuntimeResult,
    };
    use crate::utils::theme;
    use std::collections::BTreeMap;

    fn decision_with_suggestions() -> PromptDecision {
        PromptDecision {
            behavior: PermissionBehavior::Ask,
            choice: PermissionPromptChoice::AlwaysAllow,
            updates: vec![
                PermissionUpdate::AddRules {
                    destination: PermissionUpdateDestination::LocalSettings,
                    behavior: PermissionBehavior::Allow,
                    rules: vec![PermissionRuleValue::new(
                        "Bash",
                        Some("cargo test:*".to_string()),
                    )],
                },
                PermissionUpdate::AddDirectories {
                    destination: PermissionUpdateDestination::Session,
                    directories: vec!["/repo".to_string()],
                },
                PermissionUpdate::SetMode {
                    destination: PermissionUpdateDestination::Session,
                    mode: PermissionMode::AcceptEdits,
                },
            ],
            transcript: "needs confirmation".to_string(),
            updated_input: None,
        }
    }

    #[test]
    fn permission_debug_rows_extract_suggestions_like_official_helpers() {
        let rows = permission_decision_debug_rows(&decision_with_suggestions());
        assert_eq!(rows.behavior, "ask");
        assert_eq!(rows.choice, "alwaysAllow");
        assert_eq!(rows.message.as_deref(), Some("needs confirmation"));
        assert_eq!(rows.rules, vec!["Bash(cargo test:*)"]);
        assert_eq!(rows.directories, vec!["/repo"]);
        assert_eq!(rows.mode.as_deref(), Some("Accept edits"));
    }

    #[test]
    fn permission_debug_info_renders_behavior_message_and_suggestions() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionDecisionDebugInfo(permission_result: Some(decision_with_suggestions()))
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Behavior"), "canvas=\n{text}");
        assert!(text.contains("ask"), "canvas=\n{text}");
        assert!(text.contains("needs confirmation"), "canvas=\n{text}");
        assert!(text.contains("Bash(cargo test:*)"), "canvas=\n{text}");
        assert!(text.contains("/repo"), "canvas=\n{text}");
        assert!(text.contains("Accept edits"), "canvas=\n{text}");
    }

    #[test]
    fn runtime_permission_debug_renders_compound_reasons_and_suggestions() {
        let suggestions = vec![PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::Session,
            behavior: PermissionBehavior::Allow,
            rules: vec![PermissionRuleValue::new(
                "Bash",
                Some("cargo test:*".to_string()),
            )],
        }];
        let reasons = BTreeMap::from([
            (
                "cargo check".to_string(),
                Box::new(RuntimeResult::Allow {
                    updated_input: None,
                    user_modified: None,
                    decision_reason: Some(PermissionDecisionReason::Mode {
                        mode: PermissionMode::AcceptEdits,
                    }),
                    tool_use_id: None,
                    accept_feedback: None,
                    content_blocks: Vec::new(),
                }),
            ),
            (
                "cargo test".to_string(),
                Box::new(RuntimeResult::Ask {
                    message: "confirm".to_string(),
                    updated_input: None,
                    decision_reason: Some(PermissionDecisionReason::Other {
                        reason: "requires approval".to_string(),
                    }),
                    suggestions: suggestions.clone(),
                    blocked_path: None,
                    metadata: None,
                    is_bash_security_check_for_misparsing: false,
                    pending_classifier_check: None,
                    content_blocks: Vec::new(),
                }),
            ),
        ]);
        let result = RuntimeDecision::Ask {
            message: "compound command".to_string(),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::SubcommandResults { reasons }),
            suggestions,
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionDecisionDebugInfo(runtime_permission_result: Some(result))
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("Behavior ask"), "canvas=\n{text}");
        assert!(text.contains("Message compound command"), "canvas=\n{text}");
        assert!(text.contains("✓ cargo check"), "canvas=\n{text}");
        assert!(text.contains("✗ cargo test"), "canvas=\n{text}");
        assert!(
            text.contains("Suggested rules: Bash(cargo test:*)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Rules"), "canvas=\n{text}");
    }
}
