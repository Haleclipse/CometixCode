//! Maps to: CC
//! `components/permissions/AskUserQuestionPermissionRequest/use-multiple-choice-state.ts`.
//!
//! The official file owns the reducer shape used by the AskUserQuestion
//! permission UI. Rust keeps the same state vocabulary and pure transitions;
//! `use_multiple_choice_state` holds the state in one `State`, and the CC
//! callbacks (`next_question`, `set_answer`, …) dispatch into it.

use iocraft::prelude::*;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub type AnswerValue = String;

/// Maps to: CC `QuestionOption` from
/// `tools/AskUserQuestionTool/AskUserQuestionTool.tsx`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    pub preview: Option<String>,
}

/// Maps to: CC `Question` from
/// `tools/AskUserQuestionTool/AskUserQuestionTool.tsx`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

/// Maps to: CC `QuestionState`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionState {
    pub selected_value: Vec<String>,
    pub text_input_value: String,
}

/// Maps to: CC reducer `State`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultipleChoiceState {
    pub current_question_index: usize,
    pub answers: BTreeMap<String, AnswerValue>,
    pub question_states: BTreeMap<String, QuestionState>,
    pub is_in_text_input: bool,
}

/// Maps to: CC reducer `Action` (:17-32).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultipleChoiceAction {
    NextQuestion,
    PrevQuestion,
    UpdateQuestionState {
        question_text: String,
        updates: QuestionStateUpdate,
        is_multi_select: bool,
    },
    SetAnswer {
        question_text: String,
        answer: String,
        should_advance: bool,
    },
    SetTextInputMode {
        is_in_input: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionStateUpdate {
    pub selected_value: Option<Vec<String>>,
    pub text_input_value: Option<String>,
}

/// Maps to: CC reducer in `use-multiple-choice-state.ts:34-96`. Unbounded, as
/// in CC: the callers bound tab navigation (`handleTabNext`), and answering
/// the last question moves to the review view.
pub fn reduce_multiple_choice_state(
    state: &MultipleChoiceState,
    action: MultipleChoiceAction,
) -> MultipleChoiceState {
    match action {
        MultipleChoiceAction::NextQuestion => MultipleChoiceState {
            current_question_index: state.current_question_index + 1,
            is_in_text_input: false,
            ..state.clone()
        },
        MultipleChoiceAction::PrevQuestion => MultipleChoiceState {
            current_question_index: state.current_question_index.saturating_sub(1),
            is_in_text_input: false,
            ..state.clone()
        },
        MultipleChoiceAction::UpdateQuestionState {
            question_text,
            updates,
            is_multi_select: _,
        } => {
            let existing = state.question_states.get(&question_text).cloned();
            let new_state = QuestionState {
                // CC falls back to `[]` for multi-select and `undefined`
                // for single; both are an empty list here.
                selected_value: updates
                    .selected_value
                    .or_else(|| existing.as_ref().map(|state| state.selected_value.clone()))
                    .unwrap_or_default(),
                text_input_value: updates
                    .text_input_value
                    .or_else(|| {
                        existing
                            .as_ref()
                            .map(|state| state.text_input_value.clone())
                    })
                    .unwrap_or_default(),
            };
            let mut question_states = state.question_states.clone();
            question_states.insert(question_text, new_state);
            MultipleChoiceState {
                question_states,
                ..state.clone()
            }
        }
        MultipleChoiceAction::SetAnswer {
            question_text,
            answer,
            should_advance,
        } => {
            let mut answers = state.answers.clone();
            answers.insert(question_text, answer);
            // CC :79-87: only advancing leaves text-input mode.
            if should_advance {
                MultipleChoiceState {
                    answers,
                    current_question_index: state.current_question_index + 1,
                    is_in_text_input: false,
                    ..state.clone()
                }
            } else {
                MultipleChoiceState {
                    answers,
                    ..state.clone()
                }
            }
        }
        MultipleChoiceAction::SetTextInputMode { is_in_input } => MultipleChoiceState {
            is_in_text_input: is_in_input,
            ..state.clone()
        },
    }
}

/// Maps to: CC `useMultipleChoiceState` (:125-179): the reducer state. The
/// callbacks below are CC's, dispatching into it.
pub fn use_multiple_choice_state(hooks: &mut Hooks) -> State<MultipleChoiceState> {
    hooks.use_state(MultipleChoiceState::default)
}

fn dispatch(mut state: State<MultipleChoiceState>, action: MultipleChoiceAction) {
    let next = reduce_multiple_choice_state(&state.read(), action);
    state.set(next);
}

/// CC `nextQuestion`.
pub fn next_question(state: State<MultipleChoiceState>) {
    dispatch(state, MultipleChoiceAction::NextQuestion);
}

/// CC `prevQuestion`.
pub fn prev_question(state: State<MultipleChoiceState>) {
    dispatch(state, MultipleChoiceAction::PrevQuestion);
}

/// CC `updateQuestionState`.
pub fn update_question_state(
    state: State<MultipleChoiceState>,
    question_text: &str,
    updates: QuestionStateUpdate,
    is_multi_select: bool,
) {
    dispatch(
        state,
        MultipleChoiceAction::UpdateQuestionState {
            question_text: question_text.to_string(),
            updates,
            is_multi_select,
        },
    );
}

/// CC `setAnswer`.
pub fn set_answer(
    state: State<MultipleChoiceState>,
    question_text: &str,
    answer: String,
    should_advance: bool,
) {
    dispatch(
        state,
        MultipleChoiceAction::SetAnswer {
            question_text: question_text.to_string(),
            answer,
            should_advance,
        },
    );
}

/// CC `setTextInputMode`.
pub fn set_text_input_mode(state: State<MultipleChoiceState>, is_in_input: bool) {
    dispatch(state, MultipleChoiceAction::SetTextInputMode { is_in_input });
}

/// Maps to: CC `AskUserQuestionTool.inputSchema.safeParse(...).data.questions`.
pub fn questions_from_input(input: &Value) -> Vec<Question> {
    input
        .get("questions")
        .and_then(Value::as_array)
        .map(|questions| questions.iter().filter_map(question_from_value).collect())
        .unwrap_or_default()
}

fn question_from_value(value: &Value) -> Option<Question> {
    let object = value.as_object()?;
    let question = object.get("question")?.as_str()?.to_string();
    let header = object
        .get("header")
        .and_then(Value::as_str)
        .unwrap_or("Question")
        .to_string();
    let options = object
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(question_option_from_value)
                .collect()
        })
        .unwrap_or_default();
    let multi_select = object
        .get("multiSelect")
        .or_else(|| object.get("multi_select"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(Question {
        question,
        header,
        options,
        multi_select,
    })
}

fn question_option_from_value(value: &Value) -> Option<QuestionOption> {
    let object = value.as_object()?;
    Some(QuestionOption {
        label: object.get("label")?.as_str()?.to_string(),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        // CC tests `opt.preview` for truthiness everywhere (QuestionView.tsx:
        // 213, PreviewQuestionView.tsx:292, submitAnswers :395), so an empty
        // preview is none.
        preview: object
            .get("preview")
            .and_then(Value::as_str)
            .filter(|preview| !preview.is_empty())
            .map(str::to_string),
    })
}

/// Maps to: CC `QuestionView` preview branch guard.
pub fn question_has_preview(question: &Question) -> bool {
    !question.multi_select
        && question
            .options
            .iter()
            .any(|option| option.preview.is_some())
}

pub fn hide_submit_tab(questions: &[Question]) -> bool {
    questions.len() == 1
        && !questions
            .first()
            .is_some_and(|question| question.multi_select)
}

/// CC `!!answers[q.question]` / `if (answer)`: an empty answer (a
/// multi-select toggled back to nothing) is no answer.
pub fn answer_for<'a>(
    answers: &'a BTreeMap<String, AnswerValue>,
    question_text: &str,
) -> Option<&'a AnswerValue> {
    answers
        .get(question_text)
        .filter(|answer| !answer.is_empty())
}

/// Maps to: CC `AskUserQuestionPermissionRequest.tsx:249-251`.
pub fn all_questions_answered(
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
) -> bool {
    questions.iter().all(|question| {
        !question.question.is_empty() && answer_for(answers, &question.question).is_some()
    })
}

/// Maps to: CC `submitAnswers(...)` `updatedInput` construction, including
/// selected-option previews and user notes in `annotations`.
pub fn build_updated_input_with_answers(
    original_input: &Value,
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
    question_states: &BTreeMap<String, QuestionState>,
) -> Value {
    let mut object = original_input.as_object().cloned().unwrap_or_default();
    object.insert(
        "answers".to_string(),
        Value::Object(
            answers
                .iter()
                .map(|(question, answer)| (question.clone(), Value::String(answer.clone())))
                .collect::<Map<_, _>>(),
        ),
    );

    let annotations = build_annotations(questions, answers, question_states);
    if !annotations.is_empty() {
        object.insert("annotations".to_string(), Value::Object(annotations));
    }

    Value::Object(object)
}

fn build_annotations(
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
    question_states: &BTreeMap<String, QuestionState>,
) -> Map<String, Value> {
    // CC `AskUserQuestionPermissionRequest.tsx:384-401`: every question's
    // notes count, answered or not; a preview only through its answer.
    let mut annotations = Map::new();
    for question in questions {
        let answer = answers.get(&question.question);
        let notes = question_states
            .get(&question.question)
            .map(|state| state.text_input_value.trim())
            .filter(|notes| !notes.is_empty());
        let preview = answer.and_then(|answer| {
            question
                .options
                .iter()
                .find(|option| option.label == *answer)
                .and_then(|option| option.preview.as_ref())
        });
        if preview.is_none() && notes.is_none() {
            continue;
        }
        let mut annotation = Map::new();
        if let Some(preview) = preview {
            annotation.insert("preview".to_string(), Value::String(preview.clone()));
        }
        if let Some(notes) = notes {
            annotation.insert("notes".to_string(), Value::String(notes.to_string()));
        }
        annotations.insert(question.question.clone(), Value::Object(annotation));
    }
    annotations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reducer_updates_answer_and_advances_like_official_state() {
        let state = MultipleChoiceState::default();
        let state = reduce_multiple_choice_state(
            &state,
            MultipleChoiceAction::SetAnswer {
                question_text: "Proceed?".to_string(),
                answer: "Yes".to_string(),
                should_advance: true,
            },
        );
        assert_eq!(state.answers.get("Proceed?"), Some(&"Yes".to_string()));
        assert_eq!(state.current_question_index, 1);
    }

    #[test]
    fn reducer_keeps_text_input_mode_unless_the_answer_advances() {
        // CC :70-88: a multi-select toggle (shouldAdvance false) leaves the
        // user typing in Other.
        let typing = MultipleChoiceState {
            is_in_text_input: true,
            ..MultipleChoiceState::default()
        };
        let kept = reduce_multiple_choice_state(
            &typing,
            MultipleChoiceAction::SetAnswer {
                question_text: "Features?".to_string(),
                answer: "Cache".to_string(),
                should_advance: false,
            },
        );
        assert!(kept.is_in_text_input);
        assert_eq!(kept.current_question_index, 0);
        let advanced = reduce_multiple_choice_state(
            &typing,
            MultipleChoiceAction::SetAnswer {
                question_text: "Features?".to_string(),
                answer: "Cache".to_string(),
                should_advance: true,
            },
        );
        assert!(!advanced.is_in_text_input);
        assert_eq!(advanced.current_question_index, 1);
    }

    #[test]
    fn annotations_keep_notes_of_unanswered_questions() {
        // CC :387-401 walks every question; a question's notes count even
        // without an answer.
        let questions = vec![Question {
            question: "Why?".to_string(),
            ..Question::default()
        }];
        let states = BTreeMap::from([(
            "Why?".to_string(),
            QuestionState {
                selected_value: Vec::new(),
                text_input_value: " because ".to_string(),
            },
        )]);
        let input = build_updated_input_with_answers(
            &serde_json::json!({}),
            &questions,
            &BTreeMap::new(),
            &states,
        );
        assert_eq!(input["annotations"]["Why?"]["notes"], "because");
    }

    #[test]
    fn updated_input_includes_answers_preview_annotations_and_notes() {
        let input = serde_json::json!({
            "questions": [{
                "question": "Pick UI?",
                "header": "UI",
                "options": [
                    {"label": "A", "description": "First", "preview": "```ts\nA\n```"},
                    {"label": "B", "description": "Second"}
                ]
            }]
        });
        let questions = questions_from_input(&input);
        let answers = BTreeMap::from([("Pick UI?".to_string(), "A".to_string())]);
        let states = BTreeMap::from([(
            "Pick UI?".to_string(),
            QuestionState {
                selected_value: vec!["A".to_string()],
                text_input_value: "looks clearer".to_string(),
            },
        )]);

        let updated = build_updated_input_with_answers(&input, &questions, &answers, &states);

        assert_eq!(updated["answers"]["Pick UI?"], "A");
        assert_eq!(
            updated["annotations"]["Pick UI?"]["preview"],
            "```ts\nA\n```"
        );
        assert_eq!(updated["annotations"]["Pick UI?"]["notes"], "looks clearer");
    }

    #[test]
    fn an_empty_answer_is_no_answer() {
        // CC :250 `!!answers[q.question]`: a multi-select toggled back to
        // nothing leaves "" and the question unanswered.
        let questions = vec![Question {
            question: "Features?".to_string(),
            ..Question::default()
        }];
        let answers = BTreeMap::from([("Features?".to_string(), String::new())]);
        assert!(!all_questions_answered(&questions, &answers));
        assert_eq!(answer_for(&answers, "Features?"), None);
    }

    #[test]
    fn an_empty_preview_is_no_preview() {
        let questions = questions_from_input(&serde_json::json!({
            "questions": [{
                "question": "Layout?",
                "header": "Layout",
                "options": [{"label": "List", "description": "Rows", "preview": ""}]
            }]
        }));
        assert_eq!(questions[0].options[0].preview, None);
        assert!(!question_has_preview(&questions[0]));
    }

    #[test]
    fn parses_multi_select_camel_case_shape() {
        let questions = questions_from_input(&serde_json::json!({
            "questions": [{
                "question": "Features?",
                "header": "Features",
                "multiSelect": true,
                "options": [
                    {"label": "A", "description": "First"},
                    {"label": "B", "description": "Second"}
                ]
            }]
        }));

        assert_eq!(questions.len(), 1);
        assert!(questions[0].multi_select);
    }
}
