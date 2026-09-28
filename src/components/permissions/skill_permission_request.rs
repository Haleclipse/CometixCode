//! Maps to: CC
//! `components/permissions/SkillPermissionRequest/SkillPermissionRequest.tsx`.
//!
//! Permission dialog for Skill tool invocations. It mirrors the official
//! option model (`yes`, `yes-exact`, `yes-prefix`, `no`) through the shared
//! `PermissionPrompt`, which owns the keys (and so the `select` overlay),
//! Tab-to-amend and Esc, and returns permission-rule updates through the
//! Rust prompt response seam.

use super::permission_dialog::PermissionDialog;
use super::permission_prompt::{FeedbackType, PermissionPrompt, PermissionPromptOption};
use super::permission_rule_explanation::{PermissionRuleExplanation, PermissionRuleToolType};
use super::worker_badge::WorkerBadgeProps;
use crate::tools::skill_tool::constants::SKILL_TOOL_NAME;
use crate::types::permissions::{
    PermissionBehavior, PermissionMode, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest as PermissionRequestData, PermissionRuleValue, PermissionUpdate,
    PermissionUpdateDestination,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Maps to: CC `SkillOptionValue` (:21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillPermissionOptionValue {
    Yes,
    YesExact,
    YesPrefix,
    No,
}

impl SkillPermissionOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::YesExact => "yes-exact",
            Self::YesPrefix => "yes-prefix",
            Self::No => "no",
        }
    }

    fn from_value(value: &str) -> Option<Self> {
        [Self::Yes, Self::YesExact, Self::YesPrefix, Self::No]
            .into_iter()
            .find(|option| option.as_str() == value)
    }
}

#[derive(Default, Props)]
pub struct SkillPermissionRequestProps {
    pub request: Option<PermissionRequestData>,
    pub worker_badge: Option<WorkerBadgeProps>,
    /// CC `handleSelect`'s decisions; Esc is a `Deny` here too.
    pub on_select: Handler<PermissionPromptResponse>,
}

fn default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: SKILL_TOOL_NAME.to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: PermissionRuleValue::new(SKILL_TOOL_NAME, None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: PermissionMode::Default,
    }
}

/// Maps to: CC `parseInput(...)` inside `SkillPermissionRequest`.
pub fn skill_name_from_permission_input(input: &serde_json::Value) -> String {
    input
        .get("skill")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn original_cwd_for_label() -> String {
    crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string()
}

fn skill_prefix(skill: &str) -> &str {
    match skill.find(' ') {
        Some(space_index) if space_index > 0 => &skill[..space_index],
        _ => skill,
    }
}

/// Maps to: CC `options` (SkillPermissionRequest.tsx:64-113): yes/no
/// amendable, the always-allow labels' skill, prefix and directory bold.
fn skill_prompt_options(
    skill: &str,
    original_cwd: &str,
    show_always_allow_options: bool,
) -> Vec<PermissionPromptOption> {
    let bold = |text: &str| {
        let mut segment = StyledSegment::new(text);
        segment.styles.bold = Some(true);
        segment
    };
    let mut options = vec![
        PermissionPromptOption::new(SkillPermissionOptionValue::Yes.as_str(), "Yes")
            .with_feedback(FeedbackType::Accept),
    ];

    if show_always_allow_options {
        options.push(
            PermissionPromptOption::new(
                SkillPermissionOptionValue::YesExact.as_str(),
                format!("Yes, and don't ask again for {skill} in {original_cwd}"),
            )
            .with_label_segments(vec![
                StyledSegment::new("Yes, and don't ask again for "),
                bold(skill),
                StyledSegment::new(" in "),
                bold(original_cwd),
            ]),
        );

        if let Some(space_index) = skill.find(' ').filter(|space_index| *space_index > 0) {
            let command_prefix = format!("{}:*", &skill[..space_index]);
            options.push(
                PermissionPromptOption::new(
                    SkillPermissionOptionValue::YesPrefix.as_str(),
                    format!("Yes, and don't ask again for {command_prefix} commands in {original_cwd}"),
                )
                .with_label_segments(vec![
                    StyledSegment::new("Yes, and don't ask again for "),
                    bold(&command_prefix),
                    StyledSegment::new(" commands in "),
                    bold(original_cwd),
                ]),
            );
        }
    }

    options.push(
        PermissionPromptOption::new(SkillPermissionOptionValue::No.as_str(), "No")
            .with_feedback(FeedbackType::Reject),
    );
    options
}

/// Maps to: CC `handleSelect(...)` update payloads in
/// `SkillPermissionRequest`.
pub fn skill_permission_option_to_prompt_response(
    value: SkillPermissionOptionValue,
    skill: &str,
) -> PermissionPromptResponse {
    match value {
        SkillPermissionOptionValue::Yes => {
            PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce)
        }
        SkillPermissionOptionValue::YesExact => {
            PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce)
                .with_permission_updates(vec![PermissionUpdate::AddRules {
                    destination: PermissionUpdateDestination::LocalSettings,
                    behavior: PermissionBehavior::Allow,
                    rules: vec![PermissionRuleValue::new(
                        SKILL_TOOL_NAME,
                        Some(skill.to_string()),
                    )],
                }])
        }
        SkillPermissionOptionValue::YesPrefix => {
            PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce)
                .with_permission_updates(vec![PermissionUpdate::AddRules {
                    destination: PermissionUpdateDestination::LocalSettings,
                    behavior: PermissionBehavior::Allow,
                    rules: vec![PermissionRuleValue::new(
                        SKILL_TOOL_NAME,
                        Some(format!("{}:*", skill_prefix(skill))),
                    )],
                }])
        }
        SkillPermissionOptionValue::No => {
            PermissionPromptResponse::new(PermissionPromptChoice::Deny)
        }
    }
}

/// Maps to: CC `SkillPermissionRequest` render path.
#[component]
pub fn SkillPermissionRequest(
    props: &SkillPermissionRequestProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let request = props.request.clone().unwrap_or_else(default_request);
    let skill = skill_name_from_permission_input(&request.input);
    let original_cwd = original_cwd_for_label();
    // CC :65 `shouldShowAlwaysAllowOptions()`.
    let options = skill_prompt_options(
        &skill,
        &original_cwd,
        crate::utils::permissions::permissions_loader::should_show_always_allow_options(),
    );
    // CC :123-215 `handleSelect`: "yes" and "no" carry the amend feedback.
    let handle_select = {
        let on_select = props.on_select.clone();
        let skill = skill.clone();
        Handler::from(move |(value, feedback): (String, Option<String>)| {
            let Some(value) = SkillPermissionOptionValue::from_value(&value) else {
                return;
            };
            let response = skill_permission_option_to_prompt_response(value, &skill);
            let response = match (value, feedback) {
                (
                    SkillPermissionOptionValue::Yes | SkillPermissionOptionValue::No,
                    Some(feedback),
                ) => response.with_feedback(feedback),
                _ => response,
            };
            on_select(response);
        })
    };
    // CC :217-230 `handleCancel`: the rejection without feedback.
    let handle_cancel = {
        let on_select = props.on_select.clone();
        Handler::from(move |()| {
            on_select(PermissionPromptResponse::new(PermissionPromptChoice::Deny));
        })
    };
    // Maps to: CC `SkillPermissionRequest.tsx:46-52`
    // ```
    // const commandObj =
    //   toolUseConfirm.permissionResult.behavior === 'ask' &&
    //   toolUseConfirm.permissionResult.metadata &&
    //   'command' in toolUseConfirm.permissionResult.metadata
    //     ? toolUseConfirm.permissionResult.metadata.command
    //     : undefined
    // ```
    // and `:236` `<Text dimColor>{commandObj?.description}</Text>`.
    //
    // This is the SKILL's own description, produced by `SkillTool.ts:576`. It is
    // NOT `toolUseConfirm.description` (`SkillTool.ts:342` →
    // `Execute skill: ${skill}`, the tool describing itself) and not the
    // decision's `message`; the port rendered `description` until #133 gave the
    // request its `metadata` carrier.
    //
    // The `behavior === 'ask'` guard is structural in this port: `metadata` only
    // ever reaches `PermissionRequest` from `PermissionResult::Ask`
    // (`permissions.rs#process_tool_permission_result`).
    let command_description = match request.metadata.as_ref() {
        Some(crate::utils::permissions::permission_result::PermissionMetadata::Command {
            command,
        }) => command.description.clone().unwrap_or_default(),
        None => String::new(),
    };

    element! {
        PermissionDialog(
            title: format!("Use skill \"{skill}\"?"),
            worker_badge: props.worker_badge.clone(),
        ) {
            Text(
                content: "Claude may use instructions, code, or files from this Skill.".to_string(),
                wrap: TextWrap::Wrap,
            )
            // CC `:235-237` renders this Box unconditionally; `commandObj?.description`
            // simply resolves to `undefined` and the dim Text is empty, so the
            // `paddingY={1}` block still occupies its rows.
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                // CC :236 `<Text dimColor>`.
                Text(content: command_description.clone(), color: theme.inactive, wrap: TextWrap::Wrap)
            }
            View(flex_direction: FlexDirection::Column) {
                PermissionRuleExplanation(
                    decision_reason: request.decision_reason.clone(),
                    tool_type: PermissionRuleToolType::Tool,
                    permission_mode: request.mode,
                )
                // CC :244-249.
                PermissionPrompt(
                    options,
                    on_select: handle_select,
                    on_cancel: handle_cancel,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn option_values(options: &[PermissionPromptOption]) -> Vec<&str> {
        options.iter().map(|option| option.value.as_str()).collect()
    }

    #[test]
    fn skill_permission_options_match_official_exact_and_prefix_rules() {
        let options = skill_prompt_options("deploy staging", "/repo", true);
        assert_eq!(
            option_values(&options),
            vec!["yes", "yes-exact", "yes-prefix", "no"]
        );
        assert_eq!(
            options[1].label,
            "Yes, and don't ask again for deploy staging in /repo"
        );
        assert_eq!(
            options[2].label,
            "Yes, and don't ask again for deploy:* commands in /repo"
        );
        // CC :70-71, :108-109: only yes and no are amendable.
        assert_eq!(
            options
                .iter()
                .map(|option| option.feedback_config.as_ref().map(|config| config.feedback_type))
                .collect::<Vec<_>>(),
            vec![Some(FeedbackType::Accept), None, None, Some(FeedbackType::Reject)]
        );
        // CC :90-91: no prefix option without an argument.
        assert_eq!(
            option_values(&skill_prompt_options("deploy", "/repo", true)),
            vec!["yes", "yes-exact", "no"]
        );
    }

    #[test]
    fn skill_permission_options_hide_always_allow_when_managed_only() {
        let options = skill_prompt_options("deploy staging", "/repo", false);
        assert_eq!(option_values(&options), vec!["yes", "no"]);
    }

    #[test]
    fn skill_permission_response_emits_official_local_settings_rule_updates() {
        let exact = skill_permission_option_to_prompt_response(
            SkillPermissionOptionValue::YesExact,
            "deploy staging",
        );
        assert_eq!(exact.choice, PermissionPromptChoice::AllowOnce);
        assert_eq!(
            exact.permission_updates,
            vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    SKILL_TOOL_NAME,
                    Some("deploy staging".to_string())
                )],
            }]
        );

        let prefix = skill_permission_option_to_prompt_response(
            SkillPermissionOptionValue::YesPrefix,
            "deploy staging",
        );
        assert_eq!(
            prefix.permission_updates,
            vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    SKILL_TOOL_NAME,
                    Some("deploy:*".to_string())
                )],
            }]
        );
    }

    fn skill_request(
        metadata: Option<crate::utils::permissions::permission_result::PermissionMetadata>,
        description: &str,
    ) -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: SKILL_TOOL_NAME.to_string(),
            mcp_info: None,
            decision_reason: None,
            description: description.to_string(),
            message: String::new(),
            input_summary: "deploy staging".to_string(),
            input: serde_json::json!({ "skill": "deploy staging" }),
            call_input: None,
            rule: PermissionRuleValue::new(SKILL_TOOL_NAME, Some("deploy staging".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn command_metadata(
        description: &str,
    ) -> crate::utils::permissions::permission_result::PermissionMetadata {
        crate::utils::permissions::permission_result::PermissionMetadata::Command {
            command: crate::utils::permissions::permission_result::PermissionCommandMetadata {
                name: "deploy".to_string(),
                description: Some(description.to_string()),
                extra: std::collections::BTreeMap::new(),
            },
        }
    }

    #[test]
    fn skill_permission_request_renders_official_title_and_warning_copy() {
        // Re-derived 2026-08-26 (#133): the dim line under the warning is
        // `commandObj?.description` (`SkillPermissionRequest.tsx:236`), sourced
        // from `permissionResult.metadata.command` (`:47-52`) — not
        // `toolUseConfirm.description`, which for this tool is
        // `Execute skill: ${skill}` (`SkillTool.ts:342`). The old fixture put
        // the skill's description on `description` and asserted it rendered,
        // which is the port's shape, not CC's.
        let request = skill_request(Some(command_metadata("Deploys the app")), "");

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SkillPermissionRequest(
                    request: Some(request),                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Use skill \"deploy staging\"?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Claude may use instructions, code, or files from this Skill."),
            "canvas=\n{text}"
        );
        assert!(text.contains("Deploys the app"), "canvas=\n{text}");
        assert!(text.contains("deploy:*"), "canvas=\n{text}");
    }

    /// Maps to: CC `SkillPermissionRequest.tsx:46-52` and `:236`
    /// `<Text dimColor>{commandObj?.description}</Text>`, whose source is
    /// `toolUseConfirm.permissionResult.metadata.command` — produced by
    /// `SkillTool.ts:576`.
    ///
    /// `toolUseConfirm.description` is a different string entirely
    /// (`SkillTool.ts:342` `Execute skill: ${skill}`, computed at
    /// `useCanUseTool.tsx:138-143`); CC never renders it in this dialog.
    #[test]
    fn skill_permission_request_matches_official_metadata_command_description() {
        let render = |request: PermissionRequestData| {
            element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    SkillPermissionRequest(
                        request: Some(request),                    )
                }
            }
            .render(Some(120))
            .to_string()
        };

        // The tool's own `description()` is present but must NOT be rendered;
        // the skill's description from `metadata.command` must be.
        let with_metadata = render(skill_request(
            Some(command_metadata("Deploys the app to staging")),
            "Execute skill: deploy staging",
        ));
        assert!(
            with_metadata.contains("Deploys the app to staging"),
            "canvas=\n{with_metadata}"
        );
        assert!(
            !with_metadata.contains("Execute skill: deploy staging"),
            "the tool's own description is not this dialog's dim line; canvas=\n{with_metadata}"
        );

        // No metadata: CC's `commandObj?.description` is `undefined`, so the
        // dim line is empty — and `description` still must not stand in.
        let without_metadata = render(skill_request(None, "Execute skill: deploy staging"));
        assert!(
            !without_metadata.contains("Execute skill: deploy staging"),
            "canvas=\n{without_metadata}"
        );

        // The tool's own `description(input)` stays reachable and unchanged
        // (`SkillTool.ts:342`).
        assert_eq!(
            crate::tool::ToolCall::description(
                &crate::tools::skill_tool::SkillTool,
                &serde_json::json!({ "skill": "deploy staging" }),
            ),
            "Execute skill: deploy staging",
        );
    }
}
