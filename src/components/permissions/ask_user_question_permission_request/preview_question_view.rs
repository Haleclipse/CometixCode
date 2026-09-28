//! Maps to: CC
//! `components/permissions/AskUserQuestionPermissionRequest/PreviewQuestionView.tsx`.
//!
//! Preview questions use a side-by-side option list and preview panel. The
//! view owns its keys, as in CC: option focus, the footer, the notes input
//! (n to open, Esc or Enter to leave), ctrl+g for the external editor while
//! in notes, and tab/←/→ question switching of its own.

use super::preview_box::PreviewBox;
use super::question_navigation_bar::QuestionNavigationBar;
use super::question_view::{
    AnswerLabel, QuestionAnswer, QuestionStateUpdateCall, external_editor_name,
};
use super::use_multiple_choice_state::{AnswerValue, Question, QuestionState, QuestionStateUpdate};
use crate::components::design_system::divider::Divider;
use crate::components::permissions::permission_request_title::PermissionRequestTitle;
use crate::components::text_input::TextInput;
use crate::constants::figures::figures;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

/// CC :295-296.
const LEFT_PANEL_WIDTH: usize = 30;
const GAP: usize = 4;
/// CC :300-308: lines in the content area that are not preview content.
const PREVIEW_OVERHEAD: usize = 11;

#[derive(Default, Props)]
pub struct PreviewQuestionViewProps {
    pub question: Question,
    pub questions: Vec<Question>,
    pub current_question_index: usize,
    pub answers: BTreeMap<String, AnswerValue>,
    pub question_states: BTreeMap<String, QuestionState>,
    pub hide_submit_tab: bool,
    /// CC reads the permission mode from AppState; see `QuestionView`.
    pub is_in_plan_mode: bool,
    pub min_content_height: Option<u32>,
    pub min_content_width: Option<usize>,
    pub on_update_question_state: Handler<QuestionStateUpdateCall>,
    pub on_answer: Handler<QuestionAnswer>,
    pub on_text_input_focus: Handler<bool>,
    pub on_cancel: Handler<()>,
    pub on_tab_prev: Handler<()>,
    pub on_tab_next: Handler<()>,
    pub on_respond_to_claude: Handler<()>,
    pub on_finish_plan_interview: Handler<()>,
}

/// Maps to: CC `PreviewQuestionView` (:54-465).
#[component]
pub fn PreviewQuestionView(
    props: &PreviewQuestionViewProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let (columns, _) = hooks.use_terminal_size();
    let is_in_plan_mode = props.is_in_plan_mode;
    let mut is_footer_focused = hooks.use_state(|| false);
    let mut footer_index = hooks.use_state(|| 0usize);
    let mut is_in_notes_input = hooks.use_state(|| false);
    let mut cursor_offset = hooks.use_state(|| 0usize);
    // The notes input's value. CC's is the question state itself (:386
    // `value={notesValue}`); the port's TextInput holds its own State, so
    // it follows the question state whenever that changes elsewhere (the
    // external editor), with the cursor at the end.
    let mut notes_draft = hooks.use_state(String::new);
    let editor_name = external_editor_name();

    let question_text = props.question.question.clone();
    let question_state = props.question_states.get(&question_text);
    // CC :84-85: only real options, no Other.
    let all_options = props.question.options.clone();
    let option_count = all_options.len();
    let selected_value = question_state.and_then(|state| state.selected_value.first().cloned());
    let notes_value = question_state
        .map(|state| state.text_input_value.clone())
        .unwrap_or_default();
    if is_in_notes_input.get() && *notes_draft.read() != notes_value {
        cursor_offset.set(notes_value.len());
        notes_draft.set(notes_value.clone());
    }

    // CC :88-99: focus starts on the first option, and follows the selected
    // option when the question changes.
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut prev_question_text = hooks.use_state(|| question_text.clone());
    if *prev_question_text.read() != question_text {
        prev_question_text.set(question_text.clone());
        focused_index.set(
            selected_value
                .as_ref()
                .and_then(|selected| all_options.iter().position(|option| &option.label == selected))
                .unwrap_or(0),
        );
    }
    let focused = focused_index.get().min(option_count.saturating_sub(1));

    // CC :105-120 `handleSelectOption`.
    let handle_select_option = {
        let all_options = all_options.clone();
        let question_text = question_text.clone();
        let on_update_question_state = props.on_update_question_state.clone();
        let on_answer = props.on_answer.clone();
        move |index: usize| {
            let Some(option) = all_options.get(index) else {
                return;
            };
            let mut focused_index = focused_index;
            focused_index.set(index);
            on_update_question_state((
                question_text.clone(),
                QuestionStateUpdate {
                    selected_value: Some(vec![option.label.clone()]),
                    text_input_value: None,
                },
                false,
            ));
            on_answer(QuestionAnswer {
                question_text: question_text.clone(),
                label: AnswerLabel::One(option.label.clone()),
                text_input: None,
                should_advance: true,
            });
        }
    };

    // CC :175-181 `handleNotesExit`: re-answer with the plain label; the
    // notes reach the submit through the question state.
    let handle_notes_exit = {
        let question_text = question_text.clone();
        let selected_value = selected_value.clone();
        let on_answer = props.on_answer.clone();
        let on_text_input_focus = props.on_text_input_focus.clone();
        move || {
            let mut is_in_notes_input = is_in_notes_input;
            is_in_notes_input.set(false);
            on_text_input_focus(false);
            if let Some(selected) = &selected_value {
                on_answer(QuestionAnswer {
                    question_text: question_text.clone(),
                    label: AnswerLabel::One(selected.clone()),
                    text_input: None,
                    should_advance: true,
                });
            }
        }
    };

    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // CC :143-158: ctrl+g opens the external editor for the notes.
    let editor_runtime = hooks
        .try_use_context::<crate::utils::prompt_editor::ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let open_editor = hooks.use_async_handler({
        let question_text = question_text.clone();
        let on_update_question_state = props.on_update_question_state.clone();
        move |current: String| {
            let question_text = question_text.clone();
            let on_update_question_state = on_update_question_state.clone();
            async move {
                let Some(runtime) = editor_runtime else {
                    return;
                };
                let result = runtime.edit_prompt(&current).await;
                if let Some(content) = result.content.filter(|content| *content != current) {
                    on_update_question_state((
                        question_text,
                        QuestionStateUpdate {
                            selected_value: None,
                            text_input_value: Some(content),
                        },
                        false,
                    ));
                }
            }
        }
    });
    let editor_active = is_in_notes_input.get() && editor_name.is_some();
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "chat:externalEditor",
        ContextName::Chat,
        move || editor_active,
        {
            let notes_value = notes_value.clone();
            move || {
                open_editor(notes_value.clone());
                true
            }
        },
    );
    // CC :160-171: this view's own question switching, ahead of the
    // parent's, so it holds whatever the listener order.
    let tabs_active = !is_in_notes_input.get() && !is_footer_focused.get();
    crate::keybindings::use_keybinding::use_keybindings(
        &mut hooks,
        runtime,
        vec![
            ("tabs:previous".to_string(), {
                let on_tab_prev = props.on_tab_prev.clone();
                Box::new(move || {
                    on_tab_prev(());
                    true
                })
            }),
            ("tabs:next".to_string(), {
                let on_tab_next = props.on_tab_next.clone();
                Box::new(move || {
                    on_tab_next(());
                    true
                })
            }),
        ],
        ContextName::Tabs,
        move || tabs_active,
    );

    // CC :193-290 `handleKeyDown` on the view's Box, always active: DOM
    // dispatch, after the input listeners (the notes TextInput included).
    // Which mode a key belongs to is decided from this render, as CC's
    // callback closes over `isFooterFocused`/`isInNotesInput`: after the
    // notes TextInput's Enter leaves the notes, an Esc read with it still
    // leaves the notes rather than cancelling the question.
    let footer_at_render = is_footer_focused.get();
    let notes_at_render = is_in_notes_input.get();
    hooks.use_propagated_terminal_events({
        let on_respond_to_claude = props.on_respond_to_claude.clone();
        let on_finish_plan_interview = props.on_finish_plan_interview.clone();
        let on_cancel = props.on_cancel.clone();
        let on_text_input_focus = props.on_text_input_focus.clone();
        let handle_select_option = handle_select_option.clone();
        let handle_notes_exit = handle_notes_exit.clone();
        let notes_value = notes_value.clone();
        move |event| {
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
            let up = *code == KeyCode::Up || (ctrl && *code == KeyCode::Char('p'));
            let down = *code == KeyCode::Down || (ctrl && *code == KeyCode::Char('n'));
            if footer_at_render {
                if up {
                    event.prevent_default();
                    if footer_index.get() == 0 {
                        is_footer_focused.set(false);
                    } else {
                        footer_index.set(0);
                    }
                } else if down {
                    event.prevent_default();
                    if is_in_plan_mode && footer_index.get() == 0 {
                        footer_index.set(1);
                    }
                } else if *code == KeyCode::Enter {
                    event.prevent_default();
                    if footer_index.get() == 0 {
                        on_respond_to_claude(());
                    } else {
                        on_finish_plan_interview(());
                    }
                } else if *code == KeyCode::Esc {
                    event.prevent_default();
                    on_cancel(());
                }
                return;
            }
            if notes_at_render {
                // CC :231-238: Esc leaves the notes.
                if *code == KeyCode::Esc {
                    event.prevent_default();
                    handle_notes_exit();
                }
                return;
            }
            let current = focused_index.get().min(option_count.saturating_sub(1));
            if up {
                event.prevent_default();
                if current > 0 {
                    focused_index.set(current - 1);
                }
            } else if down {
                event.prevent_default();
                if current + 1 == option_count {
                    // CC :248-250: from the last option, to the footer.
                    is_footer_focused.set(true);
                } else if current + 1 < option_count {
                    focused_index.set(current + 1);
                }
            } else if *code == KeyCode::Enter {
                event.prevent_default();
                handle_select_option(current);
            } else if *code == KeyCode::Char('n')
                && !ctrl
                && !modifiers.contains(KeyModifiers::ALT)
            {
                event.prevent_default();
                notes_draft.set(notes_value.clone());
                is_in_notes_input.set(true);
                on_text_input_focus(true);
            } else if *code == KeyCode::Esc {
                event.prevent_default();
                on_cancel(());
            } else if let KeyCode::Char(c @ '1'..='9') = code {
                event.prevent_default();
                let index = (*c as usize) - ('1' as usize);
                if index < option_count {
                    focused_index.set(index);
                }
            }
        }
    });

    let focused_option = all_options.get(focused);
    let preview_content = focused_option
        .and_then(|option| option.preview.clone())
        .unwrap_or_else(|| "No preview available".to_string());
    let preview_max_width = usize::from(columns).saturating_sub(LEFT_PANEL_WIDTH + GAP);
    let preview_max_lines = props
        .min_content_height
        .map(|height| (height as usize).saturating_sub(PREVIEW_OVERHEAD).max(1));
    let footer_focused = is_footer_focused.get();
    let footer_row = |index: usize, label: &str| {
        let focused = footer_focused && footer_index.get() == index;
        let color = focused.then_some(theme.suggestion);
        element! {
            View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
                Text(content: if focused { figures().pointer.to_string() } else { " ".to_string() }, color, wrap: TextWrap::NoWrap)
                Text(content: label.to_string(), color, wrap: TextWrap::NoWrap)
            }
        }
    };
    let notes = if is_in_notes_input.get() {
        let on_update_question_state = props.on_update_question_state.clone();
        let change_question = question_text.clone();
        let submit = handle_notes_exit.clone();
        let exit = handle_notes_exit.clone();
        element! {
            TextInput(
                value: Some(notes_draft),
                cursor_offset: Some(cursor_offset),
                placeholder: Some("Add notes on this design…".to_string()),
                on_change: Handler::from(move |value: String| {
                    on_update_question_state((
                        change_question.clone(),
                        QuestionStateUpdate {
                            selected_value: None,
                            text_input_value: Some(value),
                        },
                        false,
                    ));
                }),
                on_submit: move |_| submit(),
                on_exit: move |_| exit(),
                // The view's own handler takes Esc (CC :231-238).
                escape_event_passthrough: true,
                focus: Some(true),
                show_cursor: true,
                columns: 60usize,
            )
        }
        .into_any()
    } else {
        // CC :404-406 `<Text dimColor italic>`.
        element! {
            Text(
                content: if notes_value.is_empty() { "press n to add notes".to_string() } else { notes_value.clone() },
                color: theme.inactive,
                italic: true,
                wrap: TextWrap::Wrap,
            )
        }
        .into_any()
    };
    let tab_hint = if props.questions.len() > 1 {
        " · Tab to switch questions"
    } else {
        ""
    };
    let editor_hint = match (&editor_name, is_in_notes_input.get()) {
        (Some(name), true) => format!(" · ctrl+g to edit in {name}"),
        _ => String::new(),
    };

    // CC :320-464.
    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            Divider(color: Some(theme.inactive))
            View(flex_direction: FlexDirection::Column) {
                QuestionNavigationBar(
                    questions: props.questions.clone(),
                    current_question_index: props.current_question_index,
                    answers: props.answers.clone(),
                    hide_submit_tab: props.hide_submit_tab,
                )
                PermissionRequestTitle(title: question_text.clone(), color: Some(theme.text))
                View(flex_direction: FlexDirection::Column, min_height: props.min_content_height.unwrap_or(0)) {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Row, column_gap: GAP as u32) {
                        View(flex_direction: FlexDirection::Column, width: LEFT_PANEL_WIDTH as u32) {
                            #(all_options.iter().enumerate().map(|(index, option)| {
                                let is_focused = index == focused;
                                let is_selected = selected_value.as_ref() == Some(&option.label);
                                element! {
                                    View(key: option.label.clone(), flex_direction: FlexDirection::Row) {
                                        Text(content: if is_focused { figures().pointer.to_string() } else { " ".to_string() }, color: is_focused.then_some(theme.suggestion), wrap: TextWrap::NoWrap)
                                        Text(content: format!(" {}.", index + 1), color: theme.inactive, wrap: TextWrap::NoWrap)
                                        Text(
                                            content: format!(" {}", option.label),
                                            color: if is_selected { Some(theme.success) } else if is_focused { Some(theme.suggestion) } else { None },
                                            weight: if is_focused { Weight::Bold } else { Weight::Normal },
                                            wrap: TextWrap::Wrap,
                                        )
                                        #(is_selected.then(|| element! { Text(content: format!(" {}", figures().tick), color: theme.success, wrap: TextWrap::NoWrap) }))
                                    }
                                }
                            }))
                        }
                        View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                            PreviewBox(
                                content: preview_content,
                                max_lines: preview_max_lines,
                                min_width: props.min_content_width,
                                max_width: Some(preview_max_width),
                            )
                            View(margin_top: 1u32, flex_direction: FlexDirection::Row, column_gap: 1u32) {
                                Text(content: "Notes:".to_string(), color: theme.suggestion, wrap: TextWrap::NoWrap)
                                #(notes)
                            }
                        }
                    }
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Divider(color: Some(theme.inactive))
                        #(footer_row(0, "Chat about this"))
                        #(is_in_plan_mode.then(|| footer_row(1, "Skip interview and plan immediately")))
                    }
                    View(margin_top: 1u32) {
                        // CC :451 `<Text color="inactive" dimColor>`.
                        Text(
                            content: format!(
                                "Enter to select · {}/{} to navigate · n to add notes{tab_hint}{editor_hint} · Esc to cancel",
                                figures().arrow_up,
                                figures().arrow_down,
                            ),
                            color: theme.inactive,
                            wrap: TextWrap::Wrap,
                        )
                    }
                }
            }
        }
    }
}
