//! Maps to: CC
//! `components/permissions/AskUserQuestionPermissionRequest/SubmitQuestionsView.tsx`.
//!
//! The view owns its Select: Enter on "Submit answers" or "Cancel", and the
//! Select's cancel (Esc), report through `onFinalResponse`.

use super::question_navigation_bar::QuestionNavigationBar;
use super::use_multiple_choice_state::{AnswerValue, Question, answer_for};
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::divider::Divider;
use crate::components::permissions::permission_request_title::PermissionRequestTitle;
use crate::components::permissions::permission_rule_explanation::{
    PermissionRuleExplanation, PermissionRuleToolType,
};
use crate::constants::figures::figures;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_result::PermissionDecisionReason;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitQuestionsResponse {
    Submit,
    Cancel,
}

#[derive(Default, Props)]
pub struct SubmitQuestionsViewProps<'a> {
    pub questions: Vec<Question>,
    pub current_question_index: usize,
    pub answers: BTreeMap<String, AnswerValue>,
    /// CC `allQuestionsAnswered`.
    pub all_questions_answered: bool,
    /// Maps to: CC `SubmitQuestionsView.tsx:81-84` forwarding
    /// `permissionResult` (i.e. its `decisionReason`) to
    /// `PermissionRuleExplanation`.
    pub decision_reason: Option<PermissionDecisionReason>,
    pub permission_mode: PermissionMode,
    pub min_content_height: Option<u32>,
    /// CC `onFinalResponse(value)`.
    pub on_final_response: HandlerMut<'a, SubmitQuestionsResponse>,
}

fn submit_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Submit answers".to_string(),
            value: "submit".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Cancel".to_string(),
            value: "cancel".to_string(),
            ..SelectOptionData::default()
        },
    ]
}

/// Maps to: CC `SubmitQuestionsView` (:22-104).
#[component]
pub fn SubmitQuestionsView<'a>(
    props: &mut SubmitQuestionsViewProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let all_answered = props.all_questions_answered;
    let options = submit_options();
    // CC :87-98 `<Select options onChange onCancel>`: the default five-row
    // viewport, indexes shown.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            has_on_cancel: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    let accepted = events.take_accepted();
    let cancelled = events.take_cancelled();
    if let Some(value) = accepted {
        (props.on_final_response)(if value == "submit" {
            SubmitQuestionsResponse::Submit
        } else {
            SubmitQuestionsResponse::Cancel
        });
    }
    // An accept and a cancel in one poll report the accept only.
    else if cancelled {
        (props.on_final_response)(SubmitQuestionsResponse::Cancel);
    }
    let navigation = state.navigation.snapshot();

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            Divider(color: Some(theme.inactive))
            View(flex_direction: FlexDirection::Column, border_top: true, border_color: theme.inactive) {
                QuestionNavigationBar(
                    questions: props.questions.clone(),
                    current_question_index: props.current_question_index,
                    answers: props.answers.clone(),
                )
                PermissionRequestTitle(title: "Review your answers".to_string(), color: Some(theme.text))
                View(flex_direction: FlexDirection::Column, margin_top: 1u32, min_height: props.min_content_height.unwrap_or(0)) {
                    #(if all_answered { None } else { Some(element! {
                        View(margin_bottom: 1u32) {
                            Text(content: format!("{} You have not answered all questions", figures().warning), color: theme.warning, wrap: TextWrap::Wrap)
                        }
                    })})
                    #(if props.answers.is_empty() { None } else { Some(element! {
                        View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                            // CC :56-58: questions with a (non-empty) answer.
                            #(props.questions.iter().filter(|question| answer_for(&props.answers, &question.question).is_some()).map(|question| {
                                let answer = answer_for(&props.answers, &question.question).cloned().unwrap_or_default();
                                element! {
                                    View(flex_direction: FlexDirection::Column, margin_left: 1u32) {
                                        Text(content: format!("{} {}", figures().bullet, question.question), wrap: TextWrap::Wrap)
                                        View(margin_left: 2u32) {
                                            Text(content: format!("{} {answer}", figures().arrow_right), color: theme.success, wrap: TextWrap::Wrap)
                                        }
                                    }
                                }
                            }))
                        }
                    })})
                    PermissionRuleExplanation(
                        decision_reason: props.decision_reason.clone(),
                        tool_type: PermissionRuleToolType::Tool,
                        permission_mode: props.permission_mode,
                    )
                    Text(content: "Ready to submit your answers?".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    View(margin_top: 1u32) {
                        Select(
                            options: options,
                            focused_index: navigation.focused_index().unwrap_or(0),
                            visible_from_index: navigation.visible_from_index,
                            visible_option_count: navigation.visible_option_count,
                            selected_value: state.committed_value(),
                            layout: SelectLayout::Compact,
                            hide_indexes: false,
                        )
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn question(text: &str) -> Question {
        Question {
            question: text.to_string(),
            header: "Q".to_string(),
            ..Question::default()
        }
    }

    #[test]
    fn submit_questions_view_renders_review_warning_and_answers() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SubmitQuestionsView(
                    questions: vec![question("Proceed?"), question("Style?")],
                    current_question_index: 2usize,
                    answers: BTreeMap::from([("Proceed?".to_string(), "Yes".to_string())]),
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Review your answers"), "canvas=\n{text}");
        assert!(
            text.contains("You have not answered all questions"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Proceed?"), "canvas=\n{text}");
        assert!(text.contains("Yes"), "canvas=\n{text}");
        assert!(text.contains("Submit answers"), "canvas=\n{text}");
    }
}
