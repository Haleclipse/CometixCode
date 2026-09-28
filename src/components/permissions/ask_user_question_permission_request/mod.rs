//! Maps to: CC
//! `components/permissions/AskUserQuestionPermissionRequest/AskUserQuestionPermissionRequest.tsx`.
//!
//! The dialog keeps CC's split: this component owns the answers (the
//! multiple-choice state), the per-question pasted images and the
//! question-switching keys, and hands its handlers to the views; each view
//! owns its own keys (`QuestionView`, `PreviewQuestionView`,
//! `SubmitQuestionsView`). The handlers hold this render's snapshot, as
//! CC's closures do. Analytics remain outside the component.
//!
//! Esc and the other rejections are a `Deny` on `on_select`, as elsewhere
//! in the port's permission dialogs; `app:interrupt` belongs to
//! `PermissionRequest` (CC `PermissionRequest.tsx:206-214`).

pub mod preview_box;
pub mod preview_question_view;
pub mod question_navigation_bar;
pub mod question_view;
pub mod submit_questions_view;
pub mod use_multiple_choice_state;

use question_view::{AnswerLabel, OTHER_VALUE, QuestionAnswer, QuestionStateUpdateCall, QuestionView};
use submit_questions_view::{SubmitQuestionsResponse, SubmitQuestionsView};
use use_multiple_choice_state::{
    AnswerValue, Question, QuestionState, all_questions_answered, answer_for, build_updated_input_with_answers,
    hide_submit_tab, next_question, prev_question, question_has_preview, questions_from_input,
    set_answer, set_text_input_mode, update_question_state, use_multiple_choice_state,
};

use crate::components::permissions::worker_badge::WorkerBadgeProps;
use crate::components::prompt_input::input_paste::PastedContent;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::types::permissions::{
    PermissionContentBlock, PermissionMode, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest as PermissionRequestData, PermissionRuleValue,
};
use iocraft::prelude::*;
use std::collections::BTreeMap;

const MIN_CONTENT_HEIGHT: usize = 12;
const MIN_CONTENT_WIDTH: usize = 40;
const CONTENT_CHROME_OVERHEAD: usize = 15;

/// CC `pastedContentsByQuestion`: questions in the order they first got an
/// image (a JS object's key order), each question's images by paste id (a
/// JS object's integer keys).
type PastedByQuestion = indexmap::IndexMap<String, BTreeMap<usize, PastedContent>>;

#[derive(Default, Props)]
pub struct AskUserQuestionPermissionRequestProps {
    pub request: Option<PermissionRequestData>,
    pub worker_badge: Option<WorkerBadgeProps>,
    pub on_select: Handler<PermissionPromptResponse>,
    /// Deterministic adapter seam for permission image-paste tests.
    pub clipboard_image_override: Option<crate::utils::image_paste::ClipboardImage>,
}

fn default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "AskUserQuestion".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: PermissionRuleValue::new("AskUserQuestion", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: PermissionMode::Default,
    }
}

/// Maps to: CC `globalContentHeight/globalContentWidth` calculation.
pub fn ask_user_question_content_dimensions(
    questions: &[Question],
    terminal_rows: usize,
) -> (usize, usize) {
    let mut max_height = 0usize;
    let mut max_width = MIN_CONTENT_WIDTH;
    let footer_help_lines = 7usize;
    let max_allowed_height =
        MIN_CONTENT_HEIGHT.max(terminal_rows.saturating_sub(CONTENT_CHROME_OVERHEAD));
    let preview_overhead = 11usize;

    for question in questions {
        if question_has_preview(question) {
            let max_preview_content_lines =
                1usize.max(max_allowed_height.saturating_sub(preview_overhead));
            let mut max_preview_box_height = 0usize;
            for option in &question.options {
                if let Some(preview) = &option.preview {
                    let preview_lines = preview.lines().collect::<Vec<_>>();
                    let is_truncated = preview_lines.len() > max_preview_content_lines;
                    let displayed_lines = if is_truncated {
                        max_preview_content_lines
                    } else {
                        preview_lines.len().max(1)
                    };
                    max_preview_box_height = max_preview_box_height
                        .max(displayed_lines + if is_truncated { 1 } else { 0 } + 2);
                    for line in preview_lines {
                        max_width = max_width.max(unicode_width::UnicodeWidthStr::width(line));
                    }
                }
            }
            let right_panel_height = max_preview_box_height + 2;
            let left_panel_height = question.options.len() + 2;
            max_height =
                max_height.max(right_panel_height.max(left_panel_height) + footer_help_lines);
        } else {
            max_height = max_height.max(question.options.len() + 3 + footer_help_lines);
        }
    }

    (
        max_height.max(MIN_CONTENT_HEIGHT).min(max_allowed_height),
        max_width.max(MIN_CONTENT_WIDTH),
    )
}

/// Maps to CC `handleRespondToClaude` / `handleFinishPlanInterview` question
/// summary, without analytics.
fn question_feedback_summary(
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
) -> String {
    questions
        .iter()
        .map(|question| match answer_for(answers, &question.question) {
            Some(answer) => format!("- \"{}\"\n  Answer: {answer}", question.question),
            None => format!("- \"{}\"\n  (No answer provided)", question.question),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn respond_to_claude_feedback(
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
) -> String {
    format!(
        "The user wants to clarify these questions.\n    This means they may have additional information, context or questions for you.\n    Take their response into account and then reformulate the questions if appropriate.\n    Start by asking them what they would like to clarify.\n\n    Questions asked:\n{}",
        question_feedback_summary(questions, answers)
    )
}

fn finish_plan_interview_feedback(
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
) -> String {
    format!(
        "The user has indicated they have provided enough answers for the plan interview.\nStop asking clarifying questions and proceed to finish the plan with the information you have.\n\nQuestions asked and answers provided:\n{}",
        question_feedback_summary(questions, answers)
    )
}

/// Maps to: CC `allImageAttachments` (:220-222) through
/// `convertImagesToBlocks` (:586-604): every question's images in
/// `pastedContentsByQuestion` order, each through
/// `maybeResizeAndDownsampleImageBlock`. CC awaits this before answering, and
/// a failed resize rejects the answer (`.catch(logError)`); the callers do
/// the same.
fn convert_images_to_blocks(
    pasted_by_question: &PastedByQuestion,
) -> Result<Vec<PermissionContentBlock>, crate::utils::image_resizer::ImageResizeError> {
    pasted_by_question
        .values()
        .flat_map(BTreeMap::values)
        .filter_map(|content| match content {
            PastedContent::Image {
                media_type,
                data: Some(data),
                ..
            } => Some((media_type.as_deref().unwrap_or("image/png"), data)),
            _ => None,
        })
        .map(|(media_type, data)| {
            let resized = crate::utils::image_resizer::maybe_resize_and_downsample_image_base64(
                data,
                Some(media_type),
            )?;
            Ok(PermissionContentBlock::image_base64(
                resized.media_type.clone(),
                crate::utils::image_resizer::resize_result_base64(&resized),
            ))
        })
        .collect()
}

/// CC's `.catch(logError)` on a rejected answer.
fn log_image_error(error: crate::utils::image_resizer::ImageResizeError) {
    crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
}

fn question_has_images(pasted_by_question: &PastedByQuestion, question: &str) -> bool {
    pasted_by_question.get(question).is_some_and(|contents| {
        contents
            .values()
            .any(|content| matches!(content, PastedContent::Image { .. }))
    })
}

/// Maps to: CC `components/permissions/AskUserQuestionPermissionRequest/AskUserQuestionPermissionRequest.tsx:187-210`.
fn cache_and_store_permission_image(id: usize, image: &crate::utils::image_paste::ClipboardImage) {
    let stored = crate::utils::image_store::PastedImageContent {
        id: id as u64,
        media_type: Some(image.media_type.clone()),
        data: Some(image.base64.clone()),
    };
    crate::utils::image_store::cache_image_path(&stored);
    std::thread::spawn(move || {
        let _ = crate::utils::image_store::store_image(&stored);
    });
}

/// Maps to: CC `submitAnswers` (:370-428): the updated input with answers
/// and annotations, allowed with explicit empty permission updates.
fn submit_response(
    request_input: &serde_json::Value,
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
    question_states: &BTreeMap<String, QuestionState>,
    content_blocks: Vec<PermissionContentBlock>,
) -> PermissionPromptResponse {
    PermissionPromptResponse::allow_once_with_input(build_updated_input_with_answers(
        request_input,
        questions,
        answers,
        question_states,
    ))
    // Maps to: CC `AskUserQuestionPermissionRequest.tsx:412-416` explicit empty permission updates.
    .with_permission_updates(Vec::new())
    .with_content_blocks(content_blocks)
}

/// Whether the dialog still shows the question a handler was rendered for.
///
/// Deliberate deviation: CC moves on from an answer (`setAnswer(…, true)`,
/// the multi-select Submit's `nextQuestion`) every time one is given, so an
/// answer repeated in one read (two Enters) skips the next question, or
/// steps past the review. The port moves on only from the question the
/// answer was given on; the repeat still records the same answer.
fn still_on_question(
    state: State<use_multiple_choice_state::MultipleChoiceState>,
    rendered_index: usize,
) -> bool {
    state.read().current_question_index == rendered_index
}

/// Maps to: CC `handleQuestionAnswer`'s answer (:437-459).
fn answer_text(label: &AnswerLabel, text_input: Option<&str>, has_images: bool) -> String {
    match label {
        AnswerLabel::Many(labels) => labels.join(", "),
        AnswerLabel::One(label) => match text_input.filter(|text| !text.is_empty()) {
            Some(text) if has_images => format!("{text} (Image attached)"),
            Some(text) => text.to_string(),
            None if label == OTHER_VALUE && has_images => "(Image attached)".to_string(),
            None => label.clone(),
        },
    }
}

/// Maps to: CC `AskUserQuestionPermissionRequestBody` (:74-584).
#[component]
pub fn AskUserQuestionPermissionRequest(
    props: &AskUserQuestionPermissionRequestProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let request = props.request.clone().unwrap_or_else(default_request);
    let questions = questions_from_input(&request.input);
    let (_, terminal_rows) = hooks.use_terminal_size();
    let (global_content_height, global_content_width) =
        ask_user_question_content_dimensions(&questions, terminal_rows as usize);

    // CC :182-222.
    let pasted_contents_by_question = hooks.use_state(PastedByQuestion::new);
    let next_paste_id = hooks.use_state(|| 0usize);
    let pasted_snapshot = pasted_contents_by_question.read().clone();

    // CC :224-228.
    let is_in_plan_mode = request.mode == PermissionMode::Plan;
    let plan_file_path = is_in_plan_mode
        .then(|| crate::utils::plans::get_plan_file_path(None).display().to_string());

    // CC :230-254.
    let state = use_multiple_choice_state(&mut hooks);
    let snapshot = state.read().clone();
    let current_question_index = snapshot.current_question_index;
    let answers = snapshot.answers.clone();
    let question_states = snapshot.question_states.clone();
    let current_question = questions.get(current_question_index).cloned();
    let is_in_submit_view = current_question_index == questions.len();
    let all_answered = all_questions_answered(&questions, &answers);
    let hide_submit = hide_submit_tab(&questions);

    // CC :256-278 `handleCancel`.
    let handle_cancel = {
        let on_select = props.on_select.clone();
        Handler::from(move |()| {
            on_select(PermissionPromptResponse::new(PermissionPromptChoice::Deny));
        })
    };
    // CC :280-324 `handleRespondToClaude`.
    let handle_respond_to_claude = {
        let on_select = props.on_select.clone();
        let questions = questions.clone();
        let answers = answers.clone();
        let pasted = pasted_snapshot.clone();
        Handler::from(move |()| match convert_images_to_blocks(&pasted) {
            Ok(blocks) => on_select(
                PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                    .with_feedback(respond_to_claude_feedback(&questions, &answers))
                    .with_content_blocks(blocks),
            ),
            Err(error) => log_image_error(error),
        })
    };
    // CC :326-368 `handleFinishPlanInterview`.
    let handle_finish_plan_interview = {
        let on_select = props.on_select.clone();
        let questions = questions.clone();
        let answers = answers.clone();
        let pasted = pasted_snapshot.clone();
        Handler::from(move |()| match convert_images_to_blocks(&pasted) {
            Ok(blocks) => on_select(
                PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                    .with_feedback(finish_plan_interview_feedback(&questions, &answers))
                    .with_content_blocks(blocks),
            ),
            Err(error) => log_image_error(error),
        })
    };
    // CC :370-428 `submitAnswers`.
    let submit_answers = {
        let on_select = props.on_select.clone();
        let request_input = request.input.clone();
        let questions = questions.clone();
        let question_states = question_states.clone();
        let pasted = pasted_snapshot.clone();
        move |answers_to_submit: &BTreeMap<String, AnswerValue>| match convert_images_to_blocks(
            &pasted,
        ) {
            Ok(blocks) => on_select(submit_response(
                &request_input,
                &questions,
                answers_to_submit,
                &question_states,
                blocks,
            )),
            Err(error) => log_image_error(error),
        }
    };
    // CC :430-481 `handleQuestionAnswer`.
    let handle_question_answer = {
        let submit_answers = submit_answers.clone();
        let answers = answers.clone();
        let question_count = questions.len();
        let pasted = pasted_snapshot.clone();
        Handler::from(move |call: QuestionAnswer| {
            let has_images = question_has_images(&pasted, &call.question_text);
            let answer = answer_text(&call.label, call.text_input.as_deref(), has_images);
            let is_multi_select = matches!(call.label, AnswerLabel::Many(_));
            // CC :461-470: a single single-select question submits at once.
            if !is_multi_select && question_count == 1 && call.should_advance {
                let mut updated = answers.clone();
                updated.insert(call.question_text.clone(), answer);
                submit_answers(&updated);
                return;
            }
            let should_advance =
                call.should_advance && still_on_question(state, current_question_index);
            set_answer(state, &call.question_text, answer, should_advance);
        })
    };
    // CC :483-492 `handleFinalResponse`.
    let handle_final_response = {
        let handle_cancel = handle_cancel.clone();
        let answers = answers.clone();
        move |value: SubmitQuestionsResponse| match value {
            SubmitQuestionsResponse::Cancel => handle_cancel(()),
            SubmitQuestionsResponse::Submit => submit_answers(&answers),
        }
    };

    // CC :494-510. Deliberate deviation: CC's callbacks test this render's
    // index, so two tab keys read together can both pass and step past the
    // review, which then renders nothing; the port tests the index as it
    // stands, so each key steps once and stops at the bound.
    let max_index = if hide_submit {
        questions.len().max(1) - 1
    } else {
        questions.len()
    };
    let handle_tab_prev = Handler::from(move |()| {
        if state.read().current_question_index > 0 {
            prev_question(state);
        }
    });
    let handle_tab_next = Handler::from(move |()| {
        if state.read().current_question_index < max_index {
            next_question(state);
        }
    });

    // CC :512-523: question navigation, off while typing in a question.
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let tabs_active = !(snapshot.is_in_text_input && !is_in_submit_view);
    crate::keybindings::use_keybinding::use_keybindings(
        &mut hooks,
        runtime.clone(),
        vec![
            ("tabs:previous".to_string(), {
                let handle_tab_prev = handle_tab_prev.clone();
                Box::new(move || {
                    handle_tab_prev(());
                    true
                })
            }),
            ("tabs:next".to_string(), {
                let handle_tab_next = handle_tab_next.clone();
                Box::new(move || {
                    handle_tab_next(());
                    true
                })
            }),
        ],
        ContextName::Tabs,
        move || tabs_active,
    );

    if let Some(question) = current_question {
        let question_text = question.question.clone();
        let pasted_contents = pasted_snapshot.get(&question_text).cloned().unwrap_or_default();
        // CC :187-210 `onImagePaste`, for this question.
        let on_image_paste = {
            let question_text = question_text.clone();
            Handler::from(move |image: crate::utils::image_paste::ClipboardImage| {
                let (mut next_paste_id, mut pasted_contents_by_question) =
                    (next_paste_id, pasted_contents_by_question);
                let id = next_paste_id.get();
                next_paste_id.set(id + 1);
                cache_and_store_permission_image(id, &image);
                let mut all = pasted_contents_by_question.read().clone();
                all.entry(question_text.clone()).or_default().insert(
                    id,
                    PastedContent::Image {
                        id,
                        media_type: Some(image.media_type),
                        data: Some(image.base64),
                        filename: Some("Pasted image".to_string()),
                        dimensions: image.dimensions,
                        source_path: None,
                    },
                );
                pasted_contents_by_question.set(all);
            })
        };
        // CC :212-218 `onRemoveImage`.
        let on_remove_image = {
            let question_text = question_text.clone();
            Handler::from(move |id: usize| {
                let mut pasted_contents_by_question = pasted_contents_by_question;
                let mut all = pasted_contents_by_question.read().clone();
                if let Some(contents) = all.get_mut(&question_text) {
                    contents.remove(&id);
                }
                pasted_contents_by_question.set(all);
            })
        };
        // CC :525-563.
        return element! {
            QuestionView(
                question,
                questions: questions.clone(),
                current_question_index,
                answers: answers.clone(),
                question_states: question_states.clone(),
                hide_submit_tab: hide_submit,
                plan_file_path,
                is_in_plan_mode,
                min_content_height: Some(global_content_height as u32),
                min_content_width: Some(global_content_width),
                on_update_question_state: Handler::from(move |(question_text, updates, is_multi_select): QuestionStateUpdateCall| {
                    update_question_state(state, &question_text, updates, is_multi_select);
                }),
                on_answer: handle_question_answer,
                on_text_input_focus: Handler::from(move |is_in_input: bool| set_text_input_mode(state, is_in_input)),
                on_cancel: handle_cancel,
                on_submit: Handler::from(move |()| {
                    if still_on_question(state, current_question_index) {
                        next_question(state);
                    }
                }),
                on_tab_prev: handle_tab_prev,
                on_tab_next: handle_tab_next,
                on_respond_to_claude: handle_respond_to_claude,
                on_finish_plan_interview: handle_finish_plan_interview,
                on_image_paste,
                pasted_contents,
                on_remove_image,
                clipboard_image_override: props.clipboard_image_override.clone(),
            )
        }
        .into_any();
    }

    if is_in_submit_view {
        // CC :566-580.
        return element! {
            SubmitQuestionsView(
                questions: questions.clone(),
                current_question_index,
                answers: answers.clone(),
                all_questions_answered: all_answered,
                decision_reason: request.decision_reason.clone(),
                permission_mode: request.mode,
                min_content_height: Some(global_content_height as u32),
                on_final_response: handle_final_response,
            )
        }
        .into_any();
    }

    // CC :582-583: not reached.
    element! { View }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(KeyEventKind::Press, code)
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, code);
        event.modifiers = modifiers;
        event
    }

    fn chars(text: &str) -> Vec<KeyEvent> {
        text.chars().map(|c| key(KeyCode::Char(c))).collect()
    }

    fn ask_request(input: serde_json::Value) -> PermissionRequestData {
        PermissionRequestData {
            id: "req".to_string(),
            tool_use_id: "toolu_ask".to_string(),
            input,
            ..default_request()
        }
    }

    fn single_question_input() -> serde_json::Value {
        serde_json::json!({
            "questions": [{
                "question": "Proceed?",
                "header": "Proceed",
                "options": [
                    {"label": "Yes", "description": "Continue"},
                    {"label": "No", "description": "Stop"}
                ]
            }]
        })
    }

    fn multi_question_input() -> serde_json::Value {
        serde_json::json!({
            "questions": [
                {
                    "question": "Library?",
                    "header": "Library",
                    "options": [
                        {"label": "Serde", "description": "Use serde"},
                        {"label": "Manual", "description": "Write parser"}
                    ]
                },
                {
                    "question": "Features?",
                    "header": "Features",
                    "multiSelect": true,
                    "options": [
                        {"label": "Cache", "description": "Enable cache"},
                        {"label": "Logs", "description": "Enable logs"}
                    ]
                }
            ]
        })
    }

    fn preview_question_input() -> serde_json::Value {
        serde_json::json!({
            "questions": [{
                "question": "Which layout?",
                "header": "Layout",
                "options": [
                    {"label": "List", "description": "Rows", "preview": "line 1\nline 2"},
                    {"label": "Grid", "description": "Cards", "preview": "card"}
                ]
            }]
        })
    }

    type Responses = Arc<Mutex<Vec<PermissionPromptResponse>>>;

    #[derive(Default, Props)]
    struct AskHarnessProps {
        input: serde_json::Value,
        bindings: Option<Vec<crate::keybindings::types::ParsedBinding>>,
        clipboard_image_override: Option<crate::utils::image_paste::ClipboardImage>,
        responses: Option<Responses>,
    }

    /// The dialog under the keybinding runtime, theme and AppState it mounts
    /// with (the Other input's TextInput reads AppState), and a line
    /// counting key presses (so each read produces a frame).
    #[component]
    fn AskHarness(props: &AskHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let bindings = props.bindings.clone();
        let runtime = hooks.use_const(move || {
            KeybindingRuntime::new(
                bindings.unwrap_or_else(crate::keybindings::default_bindings::default_bindings),
            )
        });
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks, runtime,
        );
        let responses = props.responses.clone().unwrap_or_default();
        let input = props.input.clone();
        let clipboard_image_override = props.clipboard_image_override.clone();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || {
                            let responses = responses.clone();
                            element! {
                                View(flex_direction: FlexDirection::Column) {
                                    AskUserQuestionPermissionRequest(
                                        request: Some(ask_request(input.clone())),
                                        clipboard_image_override: clipboard_image_override.clone(),
                                        on_select: move |response| responses.lock().unwrap().push(response),
                                    )
                                    KeyEcho
                                    OverlayProbe
                                }
                            }
                            .into_any()
                        }),
                    )
                }
            }
        }
    }

    /// Lists the registered overlays, which gate the REPL's cancel handler.
    #[component]
    fn OverlayProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let overlays = crate::state::app_state::use_app_state(&mut hooks, |state| {
            let mut overlays = state.active_overlays.iter().cloned().collect::<Vec<_>>();
            overlays.sort();
            overlays.join(",")
        });
        element! { Text(content: format!("overlays=[{overlays}]")) }
    }

    #[component]
    fn KeyEcho(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        element! { Text(content: format!("keys={} pressed", keys.get())) }
    }

    struct Run {
        responses: Vec<PermissionPromptResponse>,
        last: String,
    }

    /// Sends each batch in one go once the previous one has a frame (and,
    /// when given, the frame shows `wait_for`), then an unbound F12 to
    /// settle. Panics when a frame takes over ten seconds.
    fn drive(harness: AskHarnessProps, batches: Vec<(Vec<KeyEvent>, &'static str)>) -> Run {
        // The notes and Other inputs' TextInput raises notifications (its
        // double-Esc hint), whose timers run on the process runtime.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let responses: Responses = Arc::default();
        let harness = AskHarnessProps {
            responses: Some(responses.clone()),
            ..harness
        };
        let last = futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(AskHarness(
                input: harness.input,
                bindings: harness.bindings,
                clipboard_image_override: harness.clipboard_image_override,
                responses: harness.responses,
            ));
            // Ctrl+C is a dialog key here, not iocraft's default exit.
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events)
                    .with_size(110, 40)
                    .with_ignore_ctrl_c(true),
            ));
            let mut batches = batches;
            batches.push((vec![key(KeyCode::F(12))], ""));
            let mut next_batch = 0;
            let mut sent = 0;
            let mut wait_for = "";
            let mut last = String::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(std::time::Duration::from_secs(10)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    panic!("no frame after batch {next_batch}; last:\n{last}");
                };
                last = canvas.to_string();
                if !last.contains(&format!("keys={sent} ")) || !last.contains(wait_for) {
                    continue;
                }
                let Some((batch, marker)) = batches.get(next_batch) else {
                    break;
                };
                for event in batch {
                    keys.send(TerminalEvent::Key(event.clone())).await.unwrap();
                }
                sent += batch.len();
                wait_for = marker;
                next_batch += 1;
            }
            last
        });
        Run {
            responses: responses.lock().unwrap().clone(),
            last,
        }
    }

    fn harness(input: serde_json::Value) -> AskHarnessProps {
        AskHarnessProps {
            input,
            ..AskHarnessProps::default()
        }
    }

    fn steps(keys: Vec<KeyEvent>) -> Vec<(Vec<KeyEvent>, &'static str)> {
        keys.into_iter().map(|key| (vec![key], "")).collect()
    }

    #[test]
    fn content_dimensions_follow_official_minimums() {
        let questions = questions_from_input(&preview_question_input());
        let (height, width) = ask_user_question_content_dimensions(&questions, 40);
        assert!(height >= MIN_CONTENT_HEIGHT);
        assert!(width >= MIN_CONTENT_WIDTH);
    }

    #[test]
    fn ask_user_question_permission_request_renders_single_question() {
        let run = drive(harness(single_question_input()), Vec::new());
        let text = &run.last;
        assert!(text.contains("Proceed?"), "canvas=\n{text}");
        assert!(text.contains("1. Yes"), "canvas=\n{text}");
        // CC select-input-option.tsx:341-345: an unfocused, empty Other
        // shows its placeholder.
        assert!(text.contains("3. Type something."), "canvas=\n{text}");
        assert!(text.contains("4. Chat about this"), "canvas=\n{text}");
    }

    #[test]
    fn ask_user_question_permission_request_renders_preview_question() {
        let run = drive(harness(preview_question_input()), Vec::new());
        let text = &run.last;
        assert!(text.contains("Which layout?"), "canvas=\n{text}");
        assert!(text.contains("List"), "canvas=\n{text}");
        assert!(text.contains("┌"), "canvas=\n{text}");
        assert!(!text.contains("Type something"), "preview questions have no Other; canvas=\n{text}");
        assert!(text.contains("press n to add notes"), "canvas=\n{text}");
    }

    #[test]
    fn preview_question_notes_submit_with_annotations() {
        // n opens the notes, Esc leaves them, Enter answers the focused
        // option; the notes ride along as an annotation.
        let mut batches = steps(vec![key(KeyCode::Char('n'))]);
        batches.extend(steps(chars("ok")));
        batches.extend(steps(vec![key(KeyCode::Esc), key(KeyCode::Enter)]));
        let run = drive(harness(preview_question_input()), batches);
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        let input = run.responses[0].updated_input.as_ref().expect("updated input");
        assert_eq!(input["answers"]["Which layout?"], "List");
        assert_eq!(input["annotations"]["Which layout?"]["notes"], "ok");
        assert_eq!(input["annotations"]["Which layout?"]["preview"], "line 1\nline 2");
    }

    #[test]
    fn preview_question_moves_with_arrows_and_digits_and_reaches_the_footer() {
        // CC PreviewQuestionView.tsx:240-271: ↓ moves, a digit jumps, ↓ from
        // the last option focuses the footer; Enter there asks to clarify.
        let run = drive(
            harness(preview_question_input()),
            steps(vec![key(KeyCode::Char('2')), key(KeyCode::Down), key(KeyCode::Enter)]),
        );
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        assert_eq!(run.responses[0].choice, PermissionPromptChoice::Deny);
        assert!(
            run.responses[0]
                .feedback
                .as_deref()
                .is_some_and(|feedback| feedback.starts_with("The user wants to clarify these questions.")),
            "{:?}",
            run.responses[0].feedback
        );
    }

    #[test]
    fn single_select_enter_submits_updated_input() {
        let run = drive(harness(single_question_input()), steps(vec![key(KeyCode::Enter)]));
        assert_eq!(run.responses.len(), 1);
        assert_eq!(run.responses[0].choice, PermissionPromptChoice::AllowOnce);
        assert_eq!(
            run.responses[0].updated_input.as_ref().unwrap()["answers"]["Proceed?"],
            "Yes"
        );
    }

    #[test]
    fn single_select_other_text_submits_updated_input() {
        let mut batches = steps(vec![key(KeyCode::Down), key(KeyCode::Down)]);
        batches.extend(steps(chars("custom")));
        batches.extend(steps(vec![key(KeyCode::Enter)]));
        let run = drive(harness(single_question_input()), batches);
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        let input = run.responses[0].updated_input.as_ref().expect("updated input");
        assert_eq!(input["answers"]["Proceed?"], "custom");
        assert_eq!(input["annotations"]["Proceed?"]["notes"], "custom");
    }

    #[test]
    fn empty_other_submit_cancels() {
        // CC select.tsx:490-503: an empty input option with no images
        // cancels, which rejects the question.
        let run = drive(
            harness(single_question_input()),
            steps(vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)]),
        );
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        assert_eq!(run.responses[0].choice, PermissionPromptChoice::Deny);
        assert!(run.responses[0].feedback.is_none());
    }

    #[test]
    fn other_image_paste_honors_null_unbind_and_live_remap_before_model_transport() {
        let mut bindings = crate::keybindings::default_bindings::default_bindings();
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("ctrl+v"),
            action: None,
            context: ContextName::Chat,
        });
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("f4"),
            action: Some("chat:imagePaste".to_string()),
            context: ContextName::Chat,
        });
        let run = drive(
            AskHarnessProps {
                input: single_question_input(),
                bindings: Some(bindings),
                clipboard_image_override: Some(crate::utils::image_paste::ClipboardImage {
                    base64: "AAAA".to_string(),
                    media_type: "image/png".to_string(),
                    dimensions: None,
                }),
                ..AskHarnessProps::default()
            },
            vec![
                (vec![key(KeyCode::Down)], ""),
                (vec![key(KeyCode::Down)], ""),
                (vec![modified_key(KeyCode::Char('v'), KeyModifiers::CONTROL)], ""),
                (vec![key(KeyCode::F(4))], "[Image #0]"),
                (vec![key(KeyCode::Enter)], ""),
            ],
        );
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        assert_eq!(run.responses[0].choice, PermissionPromptChoice::AllowOnce);
        assert!(run.responses[0].permission_updates_explicit);
        assert_eq!(
            run.responses[0].updated_input.as_ref().unwrap()["answers"]["Proceed?"],
            "(Image attached)"
        );
        assert!(matches!(
            run.responses[0].content_blocks.as_slice(),
            [PermissionContentBlock::Image { source }]
                if source.media_type == "image/png" && source.data == "AAAA"
        ));
    }

    #[test]
    fn multi_question_review_submits_all_answers() {
        let run = drive(
            harness(multi_question_input()),
            steps(vec![
                key(KeyCode::Enter),     // Library = Serde, advance
                key(KeyCode::Char(' ')), // Toggle Cache
                key(KeyCode::Down),      // Logs
                key(KeyCode::Down),      // Other
                key(KeyCode::Down),      // Submit row
                key(KeyCode::Enter),     // Next: the review
                key(KeyCode::Enter),     // Submit answers
            ]),
        );
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        let input = run.responses[0].updated_input.as_ref().expect("updated input");
        assert_eq!(input["answers"]["Library?"], "Serde");
        assert_eq!(input["answers"]["Features?"], "Cache");
    }

    #[test]
    fn tab_keys_switch_questions_up_to_the_review() {
        // CC :494-523: tab/→ move on up to the review, ←/shift+tab back.
        let run = drive(harness(multi_question_input()), steps(vec![key(KeyCode::Tab)]));
        assert!(run.last.contains("Features?"), "{}", run.last);
        let run = drive(
            harness(multi_question_input()),
            steps(vec![key(KeyCode::Right), key(KeyCode::Right), key(KeyCode::Right)]),
        );
        assert!(run.last.contains("Review your answers"), "{}", run.last);
        let run = drive(
            harness(multi_question_input()),
            steps(vec![key(KeyCode::Tab), key(KeyCode::Left)]),
        );
        assert!(run.last.contains("Library?") && !run.last.contains("Features?\n"), "{}", run.last);
    }

    #[test]
    fn typing_in_a_multi_select_other_keeps_the_arrows_in_the_question() {
        // CC use-multiple-choice-state.ts:70-88: the toggle that typing
        // makes does not leave text-input mode, so ← stays in the question.
        let mut batches = steps(vec![key(KeyCode::Tab), key(KeyCode::Down), key(KeyCode::Down)]);
        batches.extend(steps(chars("x")));
        batches.extend(steps(vec![key(KeyCode::Left)]));
        let run = drive(harness(multi_question_input()), batches);
        // The title is the question; the tab bar shows headers only.
        assert!(run.last.contains("Features?"), "{}", run.last);
        assert!(!run.last.contains("Library?"), "{}", run.last);
    }

    #[test]
    fn each_list_registers_the_overlay_that_keeps_the_cancel_handler_off() {
        // CC use-select-input.ts:101 `useRegisterOverlay('select', …)` and
        // use-multi-select-state.ts:215 `useRegisterOverlay('multi-select')`:
        // with one registered, the REPL's cancel handler leaves Esc and
        // app:interrupt to the question.
        let run = drive(harness(multi_question_input()), Vec::new());
        assert!(run.last.contains("overlays=[select]"), "{}", run.last);
        let run = drive(harness(multi_question_input()), steps(vec![key(KeyCode::Tab)]));
        assert!(run.last.contains("overlays=[multi-select]"), "{}", run.last);
    }

    #[test]
    fn a_multi_select_toggled_back_to_nothing_is_unanswered() {
        // CC :250 `!!answers[q.question]`: toggling Cache on and off leaves
        // "", so the review still warns.
        let input = serde_json::json!({
            "questions": [{
                "question": "Features?",
                "header": "Features",
                "multiSelect": true,
                "options": [
                    {"label": "Cache", "description": "Enable cache"},
                    {"label": "Logs", "description": "Enable logs"}
                ]
            }]
        });
        let run = drive(
            harness(input),
            steps(vec![
                key(KeyCode::Char(' ')),
                key(KeyCode::Char(' ')),
                key(KeyCode::Down),
                key(KeyCode::Down),
                key(KeyCode::Down),
                key(KeyCode::Enter),
            ]),
        );
        assert!(run.last.contains("Review your answers"), "{}", run.last);
        assert!(run.last.contains("You have not answered all questions"), "{}", run.last);
    }

    #[test]
    fn keys_read_together_never_step_past_the_review() {
        // Two tab keys on the last question stop at the review (CC's stale
        // index would step past it and render nothing).
        let run = drive(
            harness(multi_question_input()),
            vec![
                (vec![key(KeyCode::Tab)], ""),
                (vec![key(KeyCode::Tab), key(KeyCode::Tab)], ""),
            ],
        );
        assert!(run.last.contains("Review your answers"), "{}", run.last);
        // An answer given twice in one read moves on once: the second Enter
        // on a preview question records the same answer without skipping
        // the next question.
        let input = serde_json::json!({
            "questions": [
                {
                    "question": "Which layout?",
                    "header": "Layout",
                    "options": [
                        {"label": "List", "description": "Rows", "preview": "line 1"},
                        {"label": "Grid", "description": "Cards", "preview": "card"}
                    ]
                },
                {
                    "question": "Library?",
                    "header": "Library",
                    "options": [
                        {"label": "Serde", "description": "Use serde"},
                        {"label": "Manual", "description": "Write parser"}
                    ]
                }
            ]
        });
        let run = drive(harness(input), vec![(vec![key(KeyCode::Enter), key(KeyCode::Enter)], "")]);
        assert!(run.last.contains("Library?"), "{}", run.last);
        assert!(!run.last.contains("Review your answers"), "{}", run.last);
    }

    #[test]
    fn keys_read_with_the_notes_esc_stay_in_the_notes() {
        // CC PreviewQuestionView.tsx:193-238: the view's callback closes over
        // `isInNotesInput`, so a digit read together with the Esc that leaves
        // the notes is still a notes key (the notes input, mounted until the
        // next render, takes it) and does not move the option focus.
        let mut batches = steps(vec![key(KeyCode::Char('n'))]);
        batches.extend(steps(chars("ok")));
        batches.push((vec![key(KeyCode::Esc), key(KeyCode::Char('2'))], ""));
        batches.push((vec![key(KeyCode::Enter)], ""));
        let run = drive(harness(preview_question_input()), batches);
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        let input = run.responses[0].updated_input.as_ref().expect("updated input");
        assert_eq!(input["answers"]["Which layout?"], "List");
    }

    #[test]
    fn image_blocks_follow_question_order_and_are_resized() {
        use image::ImageEncoder;
        // CC :220-222: questions in the order they first got an image, then
        // by paste id — not by id across questions.
        let image = |id: usize, data: &str| PastedContent::Image {
            id,
            media_type: Some("image/png".to_string()),
            data: Some(data.to_string()),
            filename: None,
            dimensions: None,
            source_path: None,
        };
        let mut pasted = PastedByQuestion::new();
        pasted.insert("Second?".to_string(), BTreeMap::from([(5, image(5, "BBBB"))]));
        pasted.insert("First?".to_string(), BTreeMap::from([(3, image(3, "AAAA"))]));
        let blocks = convert_images_to_blocks(&pasted).expect("blocks");
        let data = blocks
            .iter()
            .map(|block| match block {
                PermissionContentBlock::Image { source } => source.data.clone(),
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(data, vec!["BBBB".to_string(), "AAAA".to_string()]);

        // CC :601 `maybeResizeAndDownsampleImageBlock`: an oversized image
        // is scaled down before it goes out.
        let rgba = image::RgbaImage::from_pixel(2500, 500, image::Rgba([16, 32, 48, 255]));
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(rgba.as_raw(), 2500, 500, image::ExtendedColorType::Rgba8)
            .unwrap();
        let large = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png);
        let mut pasted = PastedByQuestion::new();
        pasted.insert("Q?".to_string(), BTreeMap::from([(0, image(0, &large))]));
        let blocks = convert_images_to_blocks(&pasted).expect("blocks");
        let PermissionContentBlock::Image { source } = &blocks[0] else {
            panic!("{:?}", blocks[0]);
        };
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &source.data).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(image::GenericImageView::dimensions(&decoded), (2000, 400));
    }

    #[test]
    fn escape_rejects_once_and_ctrl_c_answers_nothing() {
        let run = drive(harness(single_question_input()), steps(vec![key(KeyCode::Esc)]));
        assert_eq!(run.responses.len(), 1);
        assert_eq!(run.responses[0].choice, PermissionPromptChoice::Deny);
        // The dialog takes no interrupt of its own: CC's is PermissionRequest's
        // app:interrupt (:206-214), and Cometix binds Ctrl+C to app:exit.
        let run = drive(
            harness(single_question_input()),
            steps(vec![modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL)]),
        );
        assert!(run.responses.is_empty());
    }

    #[test]
    fn footer_takes_focus_from_the_last_option_and_asks_to_clarify() {
        // CC QuestionView.tsx:106-145: ↓ from Other focuses "Chat about
        // this"; ↑ gives it back; Enter there sends the clarify feedback.
        let run = drive(
            harness(single_question_input()),
            steps(vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down)]),
        );
        assert!(run.last.contains("❯ 4. Chat about this"), "{}", run.last);
        let run = drive(
            harness(single_question_input()),
            steps(vec![
                key(KeyCode::Down),
                key(KeyCode::Down),
                key(KeyCode::Down),
                key(KeyCode::Up),
                key(KeyCode::Down),
                key(KeyCode::Enter),
            ]),
        );
        assert_eq!(run.responses.len(), 1, "{}", run.last);
        assert_eq!(run.responses[0].choice, PermissionPromptChoice::Deny);
        let feedback = run.responses[0].feedback.as_deref().unwrap_or_default();
        assert!(feedback.contains("- \"Proceed?\"\n  (No answer provided)"), "{feedback}");
    }

    #[test]
    fn review_cancel_and_escape_reject() {
        for key_code in [KeyCode::Esc, KeyCode::Down] {
            let mut keys = vec![key(KeyCode::Tab), key(KeyCode::Tab), key(key_code)];
            if key_code == KeyCode::Down {
                keys.push(key(KeyCode::Enter));
            }
            let run = drive(harness(multi_question_input()), steps(keys));
            assert_eq!(run.responses.len(), 1, "{key_code:?}: {}", run.last);
            assert_eq!(run.responses[0].choice, PermissionPromptChoice::Deny);
        }
    }

    #[test]
    fn footer_feedback_preserves_official_question_summary() {
        let questions = vec![Question {
            question: "Proceed?".to_string(),
            ..Default::default()
        }];
        let answers = BTreeMap::from([("Proceed?".to_string(), "Yes".to_string())]);
        let clarify = respond_to_claude_feedback(&questions, &answers);
        assert!(clarify.starts_with("The user wants to clarify these questions."));
        assert!(clarify.contains("- \"Proceed?\"\n  Answer: Yes"));
        let finish = finish_plan_interview_feedback(&questions, &answers);
        assert!(finish.starts_with("The user has indicated they have provided enough answers"));
        assert!(finish.contains("Questions asked and answers provided:"));
    }
}
