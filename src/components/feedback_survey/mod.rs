//! Maps to: CC `components/FeedbackSurvey/FeedbackSurvey.tsx`,
//! `FeedbackSurveyView.tsx`, and `TranscriptSharePrompt.tsx`.
//!
//! Telemetry, GrowthBook, OTel, and transcript upload/network code are
//! intentionally not implemented in this UI boundary. State transitions and
//! submission side effects are represented by pure helpers and caller callbacks.

pub mod feedback_survey;
pub mod feedback_survey_view;
pub mod transcript_share_prompt;
pub mod use_debounced_digit_input;
pub mod utils;

use crate::constants::figures::BLACK_CIRCLE;
use iocraft::prelude::*;
pub use utils::{FeedbackSurveyResponse, FeedbackSurveyType, TranscriptShareResponse};

pub const DEFAULT_MESSAGE: &str = "How is Claude doing this session? (optional)";
pub const TRANSCRIPT_LEARN_MORE_URL: &str =
    "https://code.claude.com/docs/en/data-usage#session-quality-surveys";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FeedbackSurveyState {
    #[default]
    Closed,
    Open,
    Thanks,
    TranscriptPrompt,
    Submitting,
    Submitted,
}

/// Maps to: CC `FeedbackSurveyView.tsx#isValidResponseInput`.
pub fn is_valid_response_input(input: &str) -> bool {
    matches!(input, "0" | "1" | "2" | "3")
}

/// Maps to: CC `TranscriptSharePrompt.tsx#isValidResponseInput`.
pub fn is_valid_transcript_response_input(input: &str) -> bool {
    matches!(input, "1" | "2" | "3")
}

/// Maps to: CC `FeedbackSurveyView.tsx#inputToResponse`.
pub fn feedback_response_from_input(input: &str) -> Option<FeedbackSurveyResponse> {
    match input {
        "0" => Some(FeedbackSurveyResponse::Dismissed),
        "1" => Some(FeedbackSurveyResponse::Bad),
        "2" => Some(FeedbackSurveyResponse::Fine),
        "3" => Some(FeedbackSurveyResponse::Good),
        _ => None,
    }
}

/// Maps to: CC `TranscriptSharePrompt.tsx#inputToResponse`.
pub fn transcript_response_from_input(input: &str) -> Option<TranscriptShareResponse> {
    match input {
        "1" => Some(TranscriptShareResponse::Yes),
        "2" => Some(TranscriptShareResponse::No),
        "3" => Some(TranscriptShareResponse::DontAskAgain),
        _ => None,
    }
}

/// Maps to: CC `FeedbackSurvey.tsx` open-state visibility guard.
pub fn feedback_survey_should_render_open(input_value: &str) -> bool {
    input_value.is_empty() || is_valid_response_input(input_value)
}

/// Maps to: CC `FeedbackSurvey.tsx` transcript-prompt visibility guard.
pub fn feedback_survey_should_render_transcript_prompt(input_value: &str) -> bool {
    input_value.is_empty() || is_valid_transcript_response_input(input_value)
}

#[derive(Default, Props)]
pub struct FeedbackSurveyViewProps<'a> {
    pub input_value: String,
    pub message: Option<String>,
    pub on_select: HandlerMut<'a, FeedbackSurveyResponse>,
}

/// Maps to: CC `FeedbackSurveyView.tsx#FeedbackSurveyView`.
#[component]
pub fn FeedbackSurveyView<'a>(
    props: &mut FeedbackSurveyViewProps<'a>,
) -> impl Into<AnyElement<'static>> {
    let _ = &props.on_select;
    let message = props
        .message
        .clone()
        .unwrap_or_else(|| DEFAULT_MESSAGE.to_string());

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: "● ".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                Text(content: message, weight: Weight::Bold, wrap: TextWrap::Wrap)
            }
            View(flex_direction: FlexDirection::Row, margin_left: 2u32) {
                View(width: 10u32, flex_direction: FlexDirection::Row) {
                    Text(content: "1".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                    Text(content: ": Bad".to_string(), wrap: TextWrap::NoWrap)
                }
                View(width: 10u32, flex_direction: FlexDirection::Row) {
                    Text(content: "2".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                    Text(content: ": Fine".to_string(), wrap: TextWrap::NoWrap)
                }
                View(width: 10u32, flex_direction: FlexDirection::Row) {
                    Text(content: "3".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                    Text(content: ": Good".to_string(), wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "0".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                    Text(content: ": Dismiss".to_string(), wrap: TextWrap::NoWrap)
                }
            }
        }
    }
}

#[derive(Default, Props)]
pub struct TranscriptSharePromptProps<'a> {
    pub input_value: String,
    pub on_select: HandlerMut<'a, TranscriptShareResponse>,
}

/// Maps to: CC `TranscriptSharePrompt.tsx#TranscriptSharePrompt`.
#[component]
pub fn TranscriptSharePrompt<'a>(
    props: &mut TranscriptSharePromptProps<'a>,
) -> impl Into<AnyElement<'static>> {
    let _ = &props.on_select;
    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: format!("{BLACK_CIRCLE} "), color: Color::Cyan, wrap: TextWrap::NoWrap)
                Text(
                    content: "Can Anthropic look at your session transcript to help us improve nyan?".to_string(),
                    weight: Weight::Bold,
                    wrap: TextWrap::Wrap,
                )
            }
            View(margin_left: 2u32) {
                Text(
                    content: format!("Learn more:{TRANSCRIPT_LEARN_MORE_URL}"),
                    dim: true,
                    wrap: TextWrap::NoWrap,
                )
            }
            View(flex_direction: FlexDirection::Row, margin_left: 2u32) {
                View(width: 10u32, flex_direction: FlexDirection::Row) {
                    Text(content: "1".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                    Text(content: ": Yes".to_string(), wrap: TextWrap::NoWrap)
                }
                View(width: 10u32, flex_direction: FlexDirection::Row) {
                    Text(content: "2".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                    Text(content: ": No".to_string(), wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "3".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                    Text(content: ": Don't ask again".to_string(), wrap: TextWrap::NoWrap)
                }
            }
        }
    }
}

#[derive(Default, Props)]
pub struct FeedbackSurveyProps<'a> {
    pub state: FeedbackSurveyState,
    pub last_response: Option<FeedbackSurveyResponse>,
    pub input_value: String,
    pub message: Option<String>,
    pub has_transcript_select_handler: bool,
    pub on_request_feedback_available: bool,
    pub on_select: HandlerMut<'a, FeedbackSurveyResponse>,
    pub on_transcript_select: HandlerMut<'a, TranscriptShareResponse>,
}

/// Maps to: CC `FeedbackSurvey.tsx#FeedbackSurvey`.
#[component]
pub fn FeedbackSurvey<'a>(props: &mut FeedbackSurveyProps<'a>) -> impl Into<AnyElement<'static>> {
    match props.state {
        FeedbackSurveyState::Closed => element! { View(width: 0u32, height: 0u32) }.into_any(),
        FeedbackSurveyState::Thanks => element! {
            FeedbackSurveyThanks(
                last_response: props.last_response,
                on_request_feedback_available: props.on_request_feedback_available,
            )
        }
        .into_any(),
        FeedbackSurveyState::Submitted => element! {
            View(margin_top: 1u32) {
                Text(content: "✓ Thanks for sharing your transcript!".to_string(), color: Color::Green)
            }
        }
        .into_any(),
        FeedbackSurveyState::Submitting => element! {
            View(margin_top: 1u32) {
                Text(content: "Sharing transcript…".to_string(), dim: true)
            }
        }
        .into_any(),
        FeedbackSurveyState::TranscriptPrompt => {
            if !props.has_transcript_select_handler
                || !feedback_survey_should_render_transcript_prompt(&props.input_value)
            {
                element! { View(width: 0u32, height: 0u32) }.into_any()
            } else {
                element! {
                    TranscriptSharePrompt(input_value: props.input_value.clone())
                }
                .into_any()
            }
        }
        FeedbackSurveyState::Open => {
            if !feedback_survey_should_render_open(&props.input_value) {
                element! { View(width: 0u32, height: 0u32) }.into_any()
            } else {
                element! {
                    FeedbackSurveyView(
                        input_value: props.input_value.clone(),
                        message: props.message.clone(),
                    )
                }
                .into_any()
            }
        }
    }
}

#[derive(Default, Props)]
pub struct FeedbackSurveyThanksProps {
    pub last_response: Option<FeedbackSurveyResponse>,
    pub on_request_feedback_available: bool,
}

/// Maps to: CC `FeedbackSurvey.tsx#FeedbackSurveyThanks`.
#[component]
pub fn FeedbackSurveyThanks(props: &FeedbackSurveyThanksProps) -> impl Into<AnyElement<'static>> {
    let show_follow_up = props.on_request_feedback_available
        && props.last_response == Some(FeedbackSurveyResponse::Good);
    let feedback_command = "/feedback";
    let detail = if show_follow_up {
        element! {
            View(flex_direction: FlexDirection::Row) {
                Text(content: "(Optional) Press [".to_string(), dim: true, wrap: TextWrap::NoWrap)
                Text(content: "1".to_string(), color: Color::Cyan, wrap: TextWrap::NoWrap)
                Text(content: "] to tell us what went well · /feedback".to_string(), dim: true, wrap: TextWrap::NoWrap)
            }
        }
        .into_any()
    } else if props.last_response == Some(FeedbackSurveyResponse::Bad) {
        element! { Text(content: "Use /issue to report model behavior issues.".to_string(), dim: true, wrap: TextWrap::NoWrap) }
            .into_any()
    } else {
        element! { Text(content: format!("Use {feedback_command} to share detailed feedback anytime."), dim: true, wrap: TextWrap::NoWrap) }
            .into_any()
    };

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            Text(content: "Thanks for the feedback!".to_string(), color: Color::Green)
            #(vec![detail])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::feedback_survey::use_debounced_digit_input::{
        DEFAULT_DEBOUNCE_MS, debounced_digit_candidate,
    };

    #[test]
    fn feedback_survey_input_maps_match_official() {
        assert_eq!(
            feedback_response_from_input("0"),
            Some(FeedbackSurveyResponse::Dismissed)
        );
        assert_eq!(
            feedback_response_from_input("1"),
            Some(FeedbackSurveyResponse::Bad)
        );
        assert_eq!(
            feedback_response_from_input("2"),
            Some(FeedbackSurveyResponse::Fine)
        );
        assert_eq!(
            feedback_response_from_input("3"),
            Some(FeedbackSurveyResponse::Good)
        );
        assert_eq!(
            transcript_response_from_input("1"),
            Some(TranscriptShareResponse::Yes)
        );
        assert_eq!(
            transcript_response_from_input("2"),
            Some(TranscriptShareResponse::No)
        );
        assert_eq!(
            transcript_response_from_input("3"),
            Some(TranscriptShareResponse::DontAskAgain)
        );
        assert!(feedback_survey_should_render_open(""));
        assert!(feedback_survey_should_render_open("3"));
        assert!(!feedback_survey_should_render_open("s3cmd"));
        assert!(feedback_survey_should_render_transcript_prompt("2"));
        assert!(!feedback_survey_should_render_transcript_prompt("0"));
    }

    #[test]
    fn feedback_survey_debounced_digit_helper_accepts_full_width_digits() {
        let candidate = debounced_digit_candidate("", "３", true, false, false, None, |digit| {
            feedback_response_from_input(digit)
        })
        .expect("candidate");
        assert_eq!(candidate.trimmed_input, "");
        assert_eq!(candidate.digit, FeedbackSurveyResponse::Good);
        assert_eq!(candidate.debounce_ms, DEFAULT_DEBOUNCE_MS);
    }

    #[test]
    fn feedback_survey_view_renders_official_prompt_and_options() {
        let text = element! { FeedbackSurveyView }
            .render(Some(100))
            .to_string();
        assert!(
            text.contains("How is Claude doing this session?"),
            "canvas=\n{text}"
        );
        assert!(text.contains("1: Bad"), "canvas=\n{text}");
        assert!(text.contains("2: Fine"), "canvas=\n{text}");
        assert!(text.contains("3: Good"), "canvas=\n{text}");
        assert!(text.contains("0: Dismiss"), "canvas=\n{text}");
    }

    #[test]
    fn feedback_survey_state_renderer_matches_official_branches() {
        let submitted = element! { FeedbackSurvey(state: FeedbackSurveyState::Submitted) }
            .render(Some(100))
            .to_string();
        assert!(
            submitted.contains("✓ Thanks for sharing your transcript!"),
            "canvas=\n{submitted}"
        );

        let submitting = element! { FeedbackSurvey(state: FeedbackSurveyState::Submitting) }
            .render(Some(100))
            .to_string();
        assert!(
            submitting.contains("Sharing transcript…"),
            "canvas=\n{submitting}"
        );

        let thanks = element! {
            FeedbackSurvey(
                state: FeedbackSurveyState::Thanks,
                last_response: Some(FeedbackSurveyResponse::Good),
                on_request_feedback_available: true,
            )
        }
        .render(Some(100))
        .to_string();
        assert!(
            thanks.contains("Thanks for the feedback!"),
            "canvas=\n{thanks}"
        );
        assert!(
            thanks.contains("to tell us what went well"),
            "canvas=\n{thanks}"
        );
    }

    #[test]
    fn transcript_share_prompt_renders_official_copy_and_options() {
        let text = element! { TranscriptSharePrompt }
            .render(Some(120))
            .to_string();
        assert!(
            text.contains("Can Anthropic look at your session transcript"),
            "canvas=\n{text}"
        );
        assert!(text.contains(TRANSCRIPT_LEARN_MORE_URL), "canvas=\n{text}");
        assert!(text.contains("1: Yes"), "canvas=\n{text}");
        assert!(text.contains("2: No"), "canvas=\n{text}");
        assert!(text.contains("3: Don't ask again"), "canvas=\n{text}");
    }
}
