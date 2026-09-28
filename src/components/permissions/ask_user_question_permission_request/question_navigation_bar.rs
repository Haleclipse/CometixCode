//! Maps to: CC
//! `components/permissions/AskUserQuestionPermissionRequest/QuestionNavigationBar.tsx`.

use super::use_multiple_choice_state::{AnswerValue, Question};
use crate::constants::figures::figures;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;
use unicode_width::UnicodeWidthStr;

#[derive(Default, Props)]
pub struct QuestionNavigationBarProps {
    pub questions: Vec<Question>,
    pub current_question_index: usize,
    pub answers: BTreeMap<String, AnswerValue>,
    pub hide_submit_tab: bool,
}

fn truncate_to_width(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut result = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + ch_width + 1 > width {
            break;
        }
        result.push(ch);
        used += ch_width;
    }
    result.push('…');
    result
}

/// Maps to: CC `tabDisplayTexts` calculation.
pub fn tab_display_texts(
    questions: &[Question],
    current_question_index: usize,
    columns: usize,
    hide_submit_tab: bool,
) -> Vec<String> {
    let left_arrow = "← ";
    let right_arrow = " →";
    let submit_text = if hide_submit_tab {
        "".to_string()
    } else {
        format!(" {} Submit ", figures().tick)
    };
    let checkbox_width = 2usize;
    let padding_per_tab = 2usize;
    let fixed_width = UnicodeWidthStr::width(left_arrow)
        + UnicodeWidthStr::width(right_arrow)
        + UnicodeWidthStr::width(submit_text.as_str());
    let available_for_tabs = columns.saturating_sub(fixed_width);

    if available_for_tabs == 0 {
        return questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                if index == current_question_index {
                    question.header.chars().take(3).collect()
                } else {
                    String::new()
                }
            })
            .collect();
    }

    let headers = questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            if question.header.is_empty() {
                format!("Q{}", index + 1)
            } else {
                question.header.clone()
            }
        })
        .collect::<Vec<_>>();
    let ideal_widths = headers
        .iter()
        .map(|header| checkbox_width + padding_per_tab + UnicodeWidthStr::width(header.as_str()))
        .collect::<Vec<_>>();
    let total_ideal_width = ideal_widths.iter().sum::<usize>();
    if total_ideal_width <= available_for_tabs {
        return headers;
    }

    let current_header = headers
        .get(current_question_index)
        .cloned()
        .unwrap_or_default();
    let current_ideal_width =
        checkbox_width + padding_per_tab + UnicodeWidthStr::width(current_header.as_str());
    let current_tab_width = current_ideal_width.min(available_for_tabs / 2);
    let remaining_width = available_for_tabs.saturating_sub(current_tab_width);
    let other_tab_count = questions.len().saturating_sub(1);
    let min_width_per_tab = checkbox_width + padding_per_tab + 2;
    let width_per_other_tab = min_width_per_tab.max(remaining_width / other_tab_count.max(1));

    headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            let width = if index == current_question_index {
                current_tab_width
            } else {
                width_per_other_tab
            };
            let max_text_width = width.saturating_sub(checkbox_width + padding_per_tab);
            truncate_to_width(header, max_text_width)
        })
        .collect()
}

/// Maps to: CC `QuestionNavigationBar`.
#[component]
pub fn QuestionNavigationBar(
    props: &QuestionNavigationBarProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let (columns, _) = hooks.use_terminal_size();
    let tab_texts = tab_display_texts(
        &props.questions,
        props.current_question_index,
        columns as usize,
        props.hide_submit_tab,
    );
    let hide_arrows = props.questions.len() == 1 && props.hide_submit_tab;

    element! {
        View(flex_direction: FlexDirection::Row, margin_bottom: 1u32) {
            #(if hide_arrows { None } else { Some(element! {
                Text(content: "← ".to_string(), color: if props.current_question_index == 0 { Some(theme.inactive) } else { None }, wrap: TextWrap::NoWrap)
            })})
            #(props.questions.iter().enumerate().map(|(index, question)| {
                let is_selected = index == props.current_question_index;
                // CC :106 `!!answers[q.question]`.
                let is_answered = super::use_multiple_choice_state::answer_for(&props.answers, &question.question).is_some();
                let checkbox = if is_answered { figures().checkbox_on } else { figures().checkbox_off };
                let display = tab_texts.get(index).cloned().unwrap_or_else(|| if question.header.is_empty() { format!("Q{}", index + 1) } else { question.header.clone() });
                element! {
                    Text(
                        content: format!(" {checkbox} {display} "),
                        background_color: if is_selected { Some(theme.permission) } else { None },
                        color: if is_selected { Some(theme.inverse_text) } else { None },
                        wrap: TextWrap::NoWrap,
                    )
                }
            }))
            #(if props.hide_submit_tab { None } else { Some(element! {
                Text(
                    content: format!(" {} Submit ", figures().tick),
                    background_color: if props.current_question_index == props.questions.len() { Some(theme.permission) } else { None },
                    color: if props.current_question_index == props.questions.len() { Some(theme.inverse_text) } else { None },
                    wrap: TextWrap::NoWrap,
                )
            })})
            #(if hide_arrows { None } else { Some(element! {
                Text(content: " →".to_string(), color: if props.current_question_index == props.questions.len() { Some(theme.inactive) } else { None }, wrap: TextWrap::NoWrap)
            })})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn question(header: &str) -> Question {
        Question {
            question: format!("{header}?"),
            header: header.to_string(),
            ..Question::default()
        }
    }

    #[test]
    fn tab_display_texts_prioritizes_current_tab_when_narrow() {
        let tabs = tab_display_texts(
            &[
                question("Authentication"),
                question("Database"),
                question("Deployment"),
            ],
            0,
            24,
            false,
        );
        assert_eq!(tabs.len(), 3);
        assert!(!tabs[0].is_empty());
        assert!(tabs[0].len() <= "Authentication".len());
    }

    #[test]
    fn navigation_bar_renders_checkmarks_and_submit_tab() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                QuestionNavigationBar(
                    questions: vec![question("Auth"), question("DB")],
                    current_question_index: 1usize,
                    answers: BTreeMap::from([("Auth?".to_string(), "OAuth".to_string())]),
                )
            }
        }
        .render(Some(80))
        .to_string();

        assert!(text.contains("Auth"), "canvas=\n{text}");
        assert!(text.contains("DB"), "canvas=\n{text}");
        assert!(text.contains("Submit"), "canvas=\n{text}");
    }
}
