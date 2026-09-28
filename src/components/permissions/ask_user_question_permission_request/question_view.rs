//! Maps to: CC
//! `components/permissions/AskUserQuestionPermissionRequest/QuestionView.tsx`.
//!
//! The view owns its keys, as in CC: its Select (or SelectMulti) answers and
//! cancels, ↓ from the last option focuses the footer, and the footer's own
//! handler takes ↑/↓, Enter and Esc while it has focus. Preview questions go
//! to `PreviewQuestionView`.
//!
//! The Select is keyed by the question, so it starts afresh per question;
//! the view's footer focus carries across questions, as CC's state does.

use super::preview_question_view::PreviewQuestionView;
use super::question_navigation_bar::QuestionNavigationBar;
use super::use_multiple_choice_state::{
    AnswerValue, Question, QuestionState, QuestionStateUpdate, question_has_preview,
};
use crate::components::custom_select::{
    Select, SelectInputOptionData, SelectInputOptionMeta, SelectLayout, SelectMulti,
    SelectOptionData, UseSelectInputOptions, UseSelectStateProps, use_select_input,
    use_select_state,
};
use crate::components::design_system::divider::Divider;
use crate::components::file_path_link::file_path_link_segment;
use crate::components::permissions::permission_request_title::PermissionRequestTitle;
use crate::components::prompt_input::input_paste::PastedContent;
use crate::constants::figures::figures;
use crate::utils::image_paste::ClipboardImage;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

pub(crate) const OTHER_VALUE: &str = "__other__";

/// CC `onAnswer`'s `label: string | string[]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerLabel {
    One(String),
    Many(Vec<String>),
}

/// CC `onAnswer(questionText, label, textInput?, shouldAdvance?)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionAnswer {
    pub question_text: String,
    pub label: AnswerLabel,
    pub text_input: Option<String>,
    pub should_advance: bool,
}

/// CC `onUpdateQuestionState(questionText, updates, isMultiSelect)`.
pub type QuestionStateUpdateCall = (String, QuestionStateUpdate, bool);

#[derive(Default, Props)]
pub struct QuestionViewProps {
    pub question: Question,
    pub questions: Vec<Question>,
    pub current_question_index: usize,
    pub answers: BTreeMap<String, AnswerValue>,
    pub question_states: BTreeMap<String, QuestionState>,
    pub hide_submit_tab: bool,
    pub plan_file_path: Option<String>,
    /// CC reads `useAppState(s => s.toolPermissionContext.mode) === 'plan'`;
    /// the port's dialog has the mode on its permission request.
    pub is_in_plan_mode: bool,
    pub pasted_contents: BTreeMap<usize, PastedContent>,
    pub min_content_height: Option<u32>,
    pub min_content_width: Option<usize>,
    pub on_update_question_state: Handler<QuestionStateUpdateCall>,
    pub on_answer: Handler<QuestionAnswer>,
    pub on_text_input_focus: Handler<bool>,
    pub on_cancel: Handler<()>,
    pub on_submit: Handler<()>,
    pub on_tab_prev: Handler<()>,
    pub on_tab_next: Handler<()>,
    pub on_respond_to_claude: Handler<()>,
    pub on_finish_plan_interview: Handler<()>,
    pub on_image_paste: Handler<ClipboardImage>,
    pub on_remove_image: Handler<usize>,
    /// Deterministic adapter seam for image-paste tests.
    pub clipboard_image_override: Option<ClipboardImage>,
}

/// Maps to: CC `textOptions` + `otherOption` (:163-208).
pub fn question_select_options(
    question: &Question,
    state: Option<&QuestionState>,
) -> Vec<SelectOptionData> {
    let mut options = question
        .options
        .iter()
        .map(|option| SelectOptionData {
            label: option.label.clone(),
            description: Some(option.description.clone())
                .filter(|description| !description.is_empty()),
            value: option.label.clone(),
            ..SelectOptionData::default()
        })
        .collect::<Vec<_>>();
    options.push(SelectOptionData {
        label: "Other".to_string(),
        value: OTHER_VALUE.to_string(),
        input: Some(SelectInputOptionData {
            placeholder: Some(if question.multi_select {
                "Type something".to_string()
            } else {
                "Type something.".to_string()
            }),
            // CC `initialValue`.
            value: state
                .map(|state| state.text_input_value.clone())
                .unwrap_or_default(),
            ..SelectInputOptionData::default()
        }),
        ..SelectOptionData::default()
    });
    options
}

/// CC `getExternalEditor()` shown through `toIDEDisplayName`.
pub(crate) fn external_editor_name() -> Option<String> {
    crate::utils::prompt_editor::external_editor_command().map(|(program, args)| {
        let command = std::iter::once(program).chain(args).collect::<Vec<_>>().join(" ");
        crate::utils::ide::to_ide_display_name(Some(&command))
    })
}

/// Maps to: CC `QuestionView` (:66-398).
#[component]
pub fn QuestionView(props: &QuestionViewProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let is_in_plan_mode = props.is_in_plan_mode;
    let mut is_footer_focused = hooks.use_state(|| false);
    let mut footer_index = hooks.use_state(|| 0usize);
    let is_other_focused = hooks.use_state(|| false);
    let editor_name = external_editor_name();

    let question = props.question.clone();
    let question_text = question.question.clone();
    let multi_select = question.multi_select;

    // CC :97-104 `handleFocus`.
    let handle_focus = {
        let on_text_input_focus = props.on_text_input_focus.clone();
        move |value: String| {
            let is_other = value == OTHER_VALUE;
            let mut is_other_focused = is_other_focused;
            is_other_focused.set(is_other);
            on_text_input_focus(is_other);
        }
    };

    // CC :175-191 `handleOpenEditor`: the edited text goes into the Select
    // and the question state.
    let editor_runtime = hooks
        .try_use_context::<crate::utils::prompt_editor::ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let handle_open_editor = hooks.use_async_handler({
        let on_update_question_state = props.on_update_question_state.clone();
        let question_text = question_text.clone();
        move |(current, set_value): (String, Handler<String>)| {
            let on_update_question_state = on_update_question_state.clone();
            let question_text = question_text.clone();
            async move {
                let Some(runtime) = editor_runtime else {
                    return;
                };
                let result = runtime.edit_prompt(&current).await;
                if let Some(content) = result.content.filter(|content| *content != current) {
                    set_value(content.clone());
                    on_update_question_state((
                        question_text,
                        QuestionStateUpdate {
                            selected_value: None,
                            text_input_value: Some(content),
                        },
                        multi_select,
                    ));
                }
            }
        }
    });

    // CC :115-161 `handleKeyDown` on the view's Box: DOM dispatch, after the
    // input listeners, and only while the footer has focus. A preview
    // question renders PreviewQuestionView instead of that Box, so the
    // handler is off there even if the footer kept focus.
    let has_preview = question_has_preview(&question);
    let footer_keys = is_footer_focused.get() && !has_preview;
    hooks.use_propagated_terminal_events_for(
        if footer_keys {
            TerminalEventInterest::KEY
        } else {
            TerminalEventInterest::NONE
        },
        {
            let on_respond_to_claude = props.on_respond_to_claude.clone();
            let on_finish_plan_interview = props.on_finish_plan_interview.clone();
            let on_cancel = props.on_cancel.clone();
            move |event| {
                // Decided from this render, as CC's callback closes over
                // `isFooterFocused`.
                if !footer_keys {
                    return;
                }
                let TerminalEvent::Key(KeyEvent {
                    code,
                    kind,
                    modifiers,
                    ..
                }) = event.event()
                else {
                    return;
                };
                if *kind == KeyEventKind::Release {
                    return;
                }
                let ctrl = modifiers.contains(KeyModifiers::CONTROL);
                match code {
                    KeyCode::Up => {}
                    KeyCode::Char('p') if ctrl => {}
                    KeyCode::Down | KeyCode::Enter | KeyCode::Esc => {}
                    KeyCode::Char('n') if ctrl => {}
                    _ => return,
                }
                event.prevent_default();
                match code {
                    KeyCode::Up | KeyCode::Char('p') => {
                        if footer_index.get() == 0 {
                            is_footer_focused.set(false);
                        } else {
                            footer_index.set(0);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('n') => {
                        if is_in_plan_mode && footer_index.get() == 0 {
                            footer_index.set(1);
                        }
                    }
                    KeyCode::Enter => {
                        if footer_index.get() == 0 {
                            on_respond_to_claude(());
                        } else {
                            on_finish_plan_interview(());
                        }
                    }
                    KeyCode::Esc => on_cancel(()),
                    _ => {}
                }
            }
        },
    );

    let question_state = props.question_states.get(&question_text);
    let options = question_select_options(&question, question_state);

    // CC :210-237: previews are single-select only.
    if has_preview {
        return element! {
            PreviewQuestionView(
                question: question.clone(),
                questions: props.questions.clone(),
                current_question_index: props.current_question_index,
                answers: props.answers.clone(),
                question_states: props.question_states.clone(),
                hide_submit_tab: props.hide_submit_tab,
                is_in_plan_mode,
                min_content_height: props.min_content_height,
                min_content_width: props.min_content_width,
                on_update_question_state: props.on_update_question_state.clone(),
                on_answer: props.on_answer.clone(),
                on_text_input_focus: props.on_text_input_focus.clone(),
                on_cancel: props.on_cancel.clone(),
                on_tab_prev: props.on_tab_prev.clone(),
                on_tab_next: props.on_tab_next.clone(),
                on_respond_to_claude: props.on_respond_to_claude.clone(),
                on_finish_plan_interview: props.on_finish_plan_interview.clone(),
            )
        }
        .into_any();
    }

    // CC :106-108 `handleDownFromLastItem`.
    let handle_down_from_last_item = move |_: ()| {
        let mut is_footer_focused = is_footer_focused;
        is_footer_focused.set(true);
    };
    // CC :199-205 `otherOption.onChange`.
    let on_other_change = {
        let on_update_question_state = props.on_update_question_state.clone();
        let question_text = question_text.clone();
        move |(_, text): (String, String)| {
            on_update_question_state((
                question_text.clone(),
                QuestionStateUpdate {
                    selected_value: None,
                    text_input_value: Some(text),
                },
                multi_select,
            ));
        }
    };
    let footer_focused = is_footer_focused.get();
    let footer_row = |index: usize, label: String| {
        let focused = footer_focused && footer_index.get() == index;
        let color = focused.then_some(theme.suggestion);
        element! {
            View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
                Text(content: if focused { figures().pointer.to_string() } else { " ".to_string() }, color, wrap: TextWrap::NoWrap)
                Text(content: label, color, wrap: TextWrap::NoWrap)
            }
        }
    };
    let navigation_hint = if props.questions.len() == 1 {
        format!("{}/{} to navigate", figures().arrow_up, figures().arrow_down)
    } else {
        "Tab/Arrow keys to navigate".to_string()
    };
    let editor_hint = match (&editor_name, is_other_focused.get()) {
        (Some(name), true) => format!(" · ctrl+g to edit in {name}"),
        _ => String::new(),
    };

    let select = if multi_select {
        let default_value = question_state
            .map(|state| state.selected_value.clone())
            .unwrap_or_default();
        let on_update_question_state = props.on_update_question_state.clone();
        let on_answer = props.on_answer.clone();
        let question_states = props.question_states.clone();
        let change_question = question_text.clone();
        let on_submit = props.on_submit.clone();
        let on_cancel = props.on_cancel.clone();
        let submit_button_text = if props.current_question_index + 1 == props.questions.len() {
            "Submit"
        } else {
            "Next"
        };
        element! {
            SelectMulti(
                key: question_text.clone(),
                options: options.clone(),
                default_value,
                // CC :278-291: the selection is saved, then answered without
                // moving on; Other counts through its text.
                on_change: move |values: Vec<String>| {
                    on_update_question_state((
                        change_question.clone(),
                        QuestionStateUpdate {
                            selected_value: Some(values.clone()),
                            text_input_value: None,
                        },
                        true,
                    ));
                    let text_input = values
                        .iter()
                        .any(|value| value == OTHER_VALUE)
                        .then(|| {
                            question_states
                                .get(&change_question)
                                .map(|state| state.text_input_value.clone())
                        })
                        .flatten()
                        .filter(|text| !text.is_empty());
                    let final_values = values
                        .into_iter()
                        .filter(|value| value != OTHER_VALUE)
                        .chain(text_input)
                        .collect::<Vec<_>>();
                    on_answer(QuestionAnswer {
                        question_text: change_question.clone(),
                        label: AnswerLabel::Many(final_values),
                        text_input: None,
                        should_advance: false,
                    });
                },
                on_focus: handle_focus.clone(),
                on_cancel: move |_| on_cancel(()),
                submit_button_text: Some(submit_button_text.to_string()),
                on_submit: move |_| on_submit(()),
                on_down_from_last_item: handle_down_from_last_item,
                is_disabled: footer_focused,
                on_open_editor: handle_open_editor.clone(),
                on_image_paste: props.on_image_paste.clone(),
                pasted_contents: props.pasted_contents.clone(),
                on_remove_image: { let remove = props.on_remove_image.clone(); move |id| remove(id) },
                on_input_change: on_other_change,
                clipboard_image_override: props.clipboard_image_override.clone(),
            )
        }
        .into_any()
    } else {
        let on_update_question_state = props.on_update_question_state.clone();
        let on_answer = props.on_answer.clone();
        let question_states = props.question_states.clone();
        let change_question = question_text.clone();
        element! {
            QuestionViewSelect(
                key: question_text.clone(),
                options: options.clone(),
                default_value: question_state.and_then(|state| state.selected_value.first().cloned()),
                // CC :316-327.
                on_change: move |value: String| {
                    on_update_question_state((
                        change_question.clone(),
                        QuestionStateUpdate {
                            selected_value: Some(vec![value.clone()]),
                            text_input_value: None,
                        },
                        false,
                    ));
                    let text_input = (value == OTHER_VALUE)
                        .then(|| {
                            question_states
                                .get(&change_question)
                                .map(|state| state.text_input_value.clone())
                        })
                        .flatten();
                    on_answer(QuestionAnswer {
                        question_text: change_question.clone(),
                        label: AnswerLabel::One(value),
                        text_input,
                        should_advance: true,
                    });
                },
                on_focus: handle_focus.clone(),
                on_cancel: props.on_cancel.clone(),
                on_down_from_last_item: Handler::from(handle_down_from_last_item),
                is_disabled: footer_focused,
                on_open_editor: handle_open_editor.clone(),
                on_image_paste: props.on_image_paste.clone(),
                pasted_contents: props.pasted_contents.clone(),
                on_remove_image: props.on_remove_image.clone(),
                on_input_change: Handler::from(on_other_change),
                clipboard_image_override: props.clipboard_image_override.clone(),
            )
        }
        .into_any()
    };

    // CC :239-397.
    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 0u32) {
            // CC :247-254: the link is nested in the inactive Text, so it
            // takes that colour and wraps with the label.
            #(props.plan_file_path.as_deref().filter(|_| is_in_plan_mode).map(|path| {
                let mut label = StyledSegment::new("Planning: ");
                label.styles.color = Some(theme.inactive);
                let mut link = file_path_link_segment(path, None, None);
                link.styles.color = Some(theme.inactive);
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        Divider(color: Some(theme.inactive))
                        Text(segments: Some(vec![label, link]), wrap: TextWrap::Wrap)
                    }
                }
            }))
            View(margin_top: -1i32) {
                Divider(color: Some(theme.inactive))
            }
            View(flex_direction: FlexDirection::Column) {
                QuestionNavigationBar(
                    questions: props.questions.clone(),
                    current_question_index: props.current_question_index,
                    answers: props.answers.clone(),
                    hide_submit_tab: props.hide_submit_tab,
                )
                PermissionRequestTitle(title: question_text.clone(), color: Some(theme.text))
                View(flex_direction: FlexDirection::Column, min_height: props.min_content_height.unwrap_or(0)) {
                    View(margin_top: 1u32) {
                        #(select)
                    }
                    View(flex_direction: FlexDirection::Column) {
                        Divider(color: Some(theme.inactive))
                        #(footer_row(0, format!("{}. Chat about this", options.len() + 1)))
                        #(is_in_plan_mode.then(|| footer_row(1, format!("{}. Skip interview and plan immediately", options.len() + 2))))
                    }
                    View(margin_top: 1u32) {
                        // CC :379 `<Text color="inactive" dimColor>`.
                        Text(
                            content: format!("Enter to select · {navigation_hint}{editor_hint} · Esc to cancel"),
                            color: theme.inactive,
                            wrap: TextWrap::Wrap,
                        )
                    }
                }
            }
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct QuestionViewSelectProps {
    options: Vec<SelectOptionData>,
    default_value: Option<String>,
    on_change: Handler<String>,
    on_focus: Handler<String>,
    on_cancel: Handler<()>,
    on_down_from_last_item: Handler<()>,
    is_disabled: bool,
    on_open_editor: Handler<(String, Handler<String>)>,
    on_image_paste: Handler<ClipboardImage>,
    pasted_contents: BTreeMap<usize, PastedContent>,
    on_remove_image: Handler<usize>,
    /// The Other option's `onChange`.
    on_input_change: Handler<(String, String)>,
    clipboard_image_override: Option<ClipboardImage>,
}

/// L1 (inline Select state carrier, PORTING.md): CC writes this Select
/// inline in QuestionView (:308-337), keyed by the question and absent for
/// preview and multi-select questions; the port's Select only renders, and
/// its state lives here in its place. An accept and a cancel in one poll
/// report the accept first.
#[component]
fn QuestionViewSelect(
    props: &QuestionViewSelectProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let options = props.options.clone();
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: props.default_value.clone(),
            focus_value: None,
        },
    );
    // The Other text as the Select holds it, for Enter on a pre-filled
    // input and for CC's empty-submit rule.
    let other_text = hooks.use_state(|| {
        options
            .iter()
            .find_map(|option| option.input.as_ref().map(|input| input.value.clone()))
            .unwrap_or_default()
    });
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: props.is_disabled,
            has_on_cancel: true,
            has_on_down_from_last_item: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    is_input: option.input.is_some(),
                    input_has_value: option.input.is_some() && !other_text.read().trim().is_empty(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    // CC select.tsx:490-503: submitting an input option answers when it has
    // text or images, and cancels otherwise.
    let mut input_submitted = hooks.use_state(|| None::<String>);
    let has_images = props
        .pasted_contents
        .values()
        .any(|content| matches!(content, PastedContent::Image { .. }));
    let accepted = events.take_accepted().or_else(|| {
        let submitted = input_submitted.read().clone();
        if submitted.is_some() {
            input_submitted.set(None);
        }
        submitted
    });
    let cancelled = events.take_cancelled();
    let down_from_last = events.take_down_from_last_item();
    let focused = state.navigation.take_focus_change();
    if let Some(value) = focused {
        (props.on_focus)(value);
    }
    if let Some(value) = accepted {
        let is_input = options
            .iter()
            .any(|option| option.value == value && option.input.is_some());
        if !is_input || !other_text.read().trim().is_empty() || has_images {
            (props.on_change)(value);
        } else {
            (props.on_cancel)(());
        }
    } else if cancelled {
        (props.on_cancel)(());
    }
    if down_from_last {
        (props.on_down_from_last_item)(());
    }
    let navigation = state.navigation.snapshot();
    let on_input_change = props.on_input_change.clone();
    element! {
        Select(
            is_disabled: props.is_disabled,
            hide_indexes: false,
            visible_option_count: navigation.visible_option_count,
            options,
            focused_index: navigation.focused_index().unwrap_or(0),
            selected_value: state.committed_value(),
            visible_from_index: navigation.visible_from_index,
            layout: SelectLayout::CompactVertical,
            pasted_contents: props.pasted_contents.clone(),
            on_remove_image: props.on_remove_image.clone(),
            on_open_editor: props.on_open_editor.clone(),
            on_image_paste: props.on_image_paste.clone(),
            clipboard_image_override: props.clipboard_image_override.clone(),
            on_input_change: Handler::from(move |(key, text): (String, String)| {
                let mut other_text = other_text;
                other_text.set(text.clone());
                on_input_change((key, text));
            }),
            on_input_submit: Handler::from(move |(key, _text): (String, String)| {
                let mut input_submitted = input_submitted;
                input_submitted.set(Some(key));
            }),
            on_cancel: props.on_cancel.clone(),
        )
    }
}
