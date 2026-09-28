//! Maps to: CC `components/permissions/PermissionPrompt.tsx`.
//!
//! The shared option list of the simple permission dialogs (Fallback,
//! Skill). It owns its Select — so it registers the `select` overlay that
//! keeps the REPL's cancel handler off Esc — the per-option feedback inputs
//! (Tab to amend), the options' Confirmation keybindings, and the Esc path
//! (`attribution.escapeCount`, then `onCancel`). Analytics are not ported.

use crate::components::custom_select::select::SelectOptionLabel;
use crate::components::custom_select::{
    Select, SelectInputOptionData, SelectInputOptionMeta, SelectLayout, SelectOptionData,
    UseSelectInputOptions, UseSelectStateProps, use_select_input, use_select_state,
};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

/// Maps to: CC `FeedbackType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackType {
    Accept,
    Reject,
}

impl FeedbackType {
    /// CC `DEFAULT_PLACEHOLDERS` (:37-40).
    fn default_placeholder(self) -> &'static str {
        match self {
            Self::Accept => "tell Claude what to do next",
            Self::Reject => "tell Claude what to do differently",
        }
    }
}

/// Maps to: CC `PermissionPromptOption.feedbackConfig`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackConfig {
    pub feedback_type: FeedbackType,
    pub placeholder: Option<String>,
}

/// Maps to: CC `PermissionPromptOption<T>` (:14-22). `label` is the plain
/// text; `label_segments` carries a ReactNode label's styled spans.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionPromptOption {
    pub value: String,
    pub label: String,
    pub label_segments: Option<SelectOptionLabel>,
    pub feedback_config: Option<FeedbackConfig>,
    /// A Confirmation-context action that selects this option. Like CC's
    /// `useKeybindings(..., { context: 'Confirmation' })` (:206-217) it stays
    /// live while a feedback input is open, so its key is not typed there.
    pub keybinding: Option<&'static str>,
}

impl PermissionPromptOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn with_label_segments(mut self, segments: SelectOptionLabel) -> Self {
        self.label_segments = Some(segments);
        self
    }

    pub fn with_feedback(mut self, feedback_type: FeedbackType) -> Self {
        self.feedback_config = Some(FeedbackConfig {
            feedback_type,
            placeholder: None,
        });
        self
    }
}

/// CC `onSelect(value, feedback?)`.
pub type PermissionPromptSelection = (String, Option<String>);

#[derive(Default, Props)]
pub struct PermissionPromptProps {
    pub options: Vec<PermissionPromptOption>,
    pub on_select: Handler<PermissionPromptSelection>,
    pub on_cancel: Handler<()>,
    /// CC `question`, default "Do you want to proceed?".
    pub question: Option<String>,
}

/// Maps to: CC `PermissionPrompt` (:52-267).
#[component]
pub fn PermissionPrompt(
    props: &PermissionPromptProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    // CC's `useSetAppState()` requires the provider; this reads it optionally,
    // as the Select's overlay registration does, so a dialog rendered alone
    // (a static render, a unit test) still mounts and only skips the count.
    let app_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    // CC :60-69.
    let accept_feedback = hooks.use_state(String::new);
    let reject_feedback = hooks.use_state(String::new);
    let mut accept_input_mode = hooks.use_state(|| false);
    let mut reject_input_mode = hooks.use_state(|| false);
    let mut focused_value = hooks.use_state(|| None::<String>);

    let options = props.options.clone();
    let feedback_type_of = {
        let options = options.clone();
        move |value: &str| {
            options
                .iter()
                .find(|option| option.value == value)
                .and_then(|option| option.feedback_config.as_ref())
                .map(|config| config.feedback_type)
        }
    };
    let input_mode = |feedback_type: FeedbackType| match feedback_type {
        FeedbackType::Accept => accept_input_mode.get(),
        FeedbackType::Reject => reject_input_mode.get(),
    };
    let feedback_of = move |feedback_type: FeedbackType| match feedback_type {
        FeedbackType::Accept => accept_feedback.read().clone(),
        FeedbackType::Reject => reject_feedback.read().clone(),
    };

    // CC :71-78: the Tab hint while a feedback option is focused outside its
    // input mode.
    let focused_feedback_type = focused_value
        .read()
        .as_deref()
        .and_then(|value| feedback_type_of(value));
    let show_tab_hint = focused_feedback_type.is_some_and(|feedback_type| !input_mode(feedback_type));

    // CC :80-116: an option in input mode is an input option.
    let select_options = options
        .iter()
        .map(|option| {
            let input = option
                .feedback_config
                .as_ref()
                .filter(|config| input_mode(config.feedback_type))
                .map(|config| SelectInputOptionData {
                    placeholder: Some(
                        config
                            .placeholder
                            .clone()
                            .unwrap_or_else(|| config.feedback_type.default_placeholder().to_string()),
                    ),
                    value: feedback_of(config.feedback_type),
                    ..SelectInputOptionData::default()
                });
            SelectOptionData {
                label: option.label.clone(),
                value: option.value.clone(),
                input,
                ..SelectOptionData::default()
            }
        })
        .collect::<Vec<_>>();
    let option_labels = options
        .iter()
        .filter_map(|option| {
            option
                .label_segments
                .clone()
                .map(|segments| (option.value.clone(), segments))
        })
        .collect::<BTreeMap<_, _>>();

    // CC :236-261 `<Select options inlineDescriptions onChange onCancel
    // onFocus onInputModeToggle>`: the default five-row viewport.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: select_options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            has_on_cancel: true,
            has_on_input_mode_toggle: true,
            option_metas: select_options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    is_input: option.input.is_some(),
                    input_has_value: option
                        .input
                        .as_ref()
                        .is_some_and(|input| !input.value.trim().is_empty()),
                    // CC :106 `allowEmptySubmitToCancel: true`.
                    allow_empty_submit_to_cancel: option.input.is_some(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );

    // CC :154-204 `handleSelect`: a feedback option carries its trimmed
    // feedback, when there is any.
    let handle_select = {
        let on_select = props.on_select.clone();
        let feedback_type_of = feedback_type_of.clone();
        move |value: String| {
            let feedback = feedback_type_of(&value)
                .map(|feedback_type| feedback_of(feedback_type).trim().to_string())
                .filter(|feedback| !feedback.is_empty());
            on_select((value, feedback));
        }
    };
    // CC :219-231 `handleCancel`.
    let handle_cancel = {
        let on_cancel = props.on_cancel.clone();
        let app_store = app_store.clone();
        move || {
            if let Some(store) = app_store.as_ref() {
                store.set_state(|prev| {
                    let mut attribution = (*prev.attribution).clone();
                    attribution.escape_count += 1;
                    let mut next = (**prev).clone();
                    next.attribution = std::sync::Arc::new(attribution);
                    crate::state::store::UpdateDecision::Replace {
                        next: std::sync::Arc::new(next),
                        result: (),
                    }
                });
            }
            on_cancel(());
        }
    };

    // CC :206-217: options with a keybinding select themselves.
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let keybinding_handlers = options
        .iter()
        .filter_map(|option| option.keybinding.map(|action| (action, option.value.clone())))
        .map(|(action, value)| {
            let handle_select = handle_select.clone();
            (
                action.to_string(),
                Box::new(move || {
                    handle_select(value.clone());
                    true
                }) as crate::keybindings::use_keybinding::KeybindingHandler,
            )
        })
        .collect::<Vec<_>>();
    crate::keybindings::use_keybinding::use_keybindings(
        &mut hooks,
        runtime,
        keybinding_handlers,
        ContextName::Confirmation,
        || true,
    );

    // The input option's Enter and the Select's cancel, relayed to this
    // render (CC select.tsx:490-504: with `allowEmptySubmitToCancel` an input
    // submit always answers). A cancel seen by both the key hook and the
    // relay in one render is still one `handleCancel`.
    let mut input_submitted = hooks.use_state(|| None::<String>);
    let mut input_cancelled = hooks.use_state(|| false);

    // CC :241-259 `onFocus`: leaving a feedback option with nothing typed
    // collapses its input.
    if let Some(value) = state.navigation.take_focus_change() {
        let focused_type = feedback_type_of(&value);
        if focused_type != Some(FeedbackType::Accept)
            && accept_input_mode.get()
            && accept_feedback.read().trim().is_empty()
        {
            accept_input_mode.set(false);
        }
        if focused_type != Some(FeedbackType::Reject)
            && reject_input_mode.get()
            && reject_feedback.read().trim().is_empty()
        {
            reject_input_mode.set(false);
        }
        focused_value.set(Some(value));
    }
    // CC :118-152 `handleInputModeToggle`. Tab is read after the frame's
    // arrows (use_select_input's keybinding hooks drain first), so the value
    // toggled is the one just focused and the order against `onFocus` above
    // does not change the outcome; what a frame's keys come to is pinned by
    // `permission_prompt_reads_tab_after_the_frames_arrows`.
    if let Some(value) = events.take_input_mode_toggle() {
        match feedback_type_of(&value) {
            Some(FeedbackType::Accept) => accept_input_mode.set(!accept_input_mode.get()),
            Some(FeedbackType::Reject) => reject_input_mode.set(!reject_input_mode.get()),
            None => {}
        }
    }
    let submitted = input_submitted.read().clone();
    if submitted.is_some() {
        input_submitted.set(None);
    }
    let accepted = events.take_accepted().or(submitted);
    let cancelled = events.take_cancelled() || input_cancelled.get();
    if input_cancelled.get() {
        input_cancelled.set(false);
    }
    if let Some(value) = accepted {
        handle_select(value);
    } else if cancelled {
        handle_cancel();
    }

    let navigation = state.navigation.snapshot();
    let question = props
        .question
        .clone()
        .unwrap_or_else(|| "Do you want to proceed?".to_string());
    let hint = if show_tab_hint {
        "Esc to cancel · Tab to amend"
    } else {
        "Esc to cancel"
    };
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: question, wrap: TextWrap::Wrap)
            Select(
                options: select_options,
                option_labels,
                inline_descriptions: true,
                focused_index: navigation.focused_index().unwrap_or(0),
                visible_from_index: navigation.visible_from_index,
                visible_option_count: navigation.visible_option_count,
                selected_value: state.committed_value(),
                layout: SelectLayout::Compact,
                on_input_change: Handler::from(move |(key, text): (String, String)| {
                    let (mut accept_feedback, mut reject_feedback) = (accept_feedback, reject_feedback);
                    match feedback_type_of(&key) {
                        Some(FeedbackType::Accept) => accept_feedback.set(text),
                        Some(FeedbackType::Reject) => reject_feedback.set(text),
                        None => {}
                    }
                }),
                on_input_submit: Handler::from(move |(key, _text): (String, String)| {
                    let mut input_submitted = input_submitted;
                    input_submitted.set(Some(key));
                }),
                on_cancel: Handler::from(move |()| {
                    let mut input_cancelled = input_cancelled;
                    input_cancelled.set(true);
                }),
            )
            // CC :262-264 `<Text dimColor>`.
            View(margin_top: 1u32) {
                Text(content: hint.to_string(), color: theme.inactive)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    type Selections = Arc<Mutex<Vec<PermissionPromptSelection>>>;

    fn options() -> Vec<PermissionPromptOption> {
        vec![
            PermissionPromptOption::new("yes", "Yes").with_feedback(FeedbackType::Accept),
            PermissionPromptOption {
                keybinding: Some("confirm:yes"),
                ..PermissionPromptOption::new("always", "Yes, always")
            },
            PermissionPromptOption::new("no", "No").with_feedback(FeedbackType::Reject),
        ]
    }

    #[derive(Default, Props)]
    struct PromptHarnessProps {
        selections: Option<Selections>,
        cancels: Option<Arc<Mutex<usize>>>,
    }

    /// The prompt under the runtime, theme and AppState it mounts with, and a
    /// line with the key count, escape count and registered overlays.
    #[component]
    fn PromptHarness(props: &PromptHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            KeybindingRuntime::with_default_bindings(),
        );
        let selections = props.selections.clone().unwrap_or_default();
        let cancels = props.cancels.clone().unwrap_or_default();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || {
                            let selections = selections.clone();
                            let cancels = cancels.clone();
                            element! {
                                View(flex_direction: FlexDirection::Column) {
                                    PermissionPrompt(
                                        options: options(),
                                        on_select: Handler::from(move |selection| selections.lock().unwrap().push(selection)),
                                        on_cancel: Handler::from(move |()| *cancels.lock().unwrap() += 1),
                                    )
                                    Probe
                                }
                            }
                            .into_any()
                        }),
                    )
                }
            }
        }
    }

    #[component]
    fn Probe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let (escapes, overlays) = crate::state::app_state::use_app_state(&mut hooks, |state| {
            let mut overlays = state.active_overlays.iter().cloned().collect::<Vec<_>>();
            overlays.sort();
            (state.attribution.escape_count, overlays.join(","))
        });
        element! { Text(content: format!("keys={} escapes={escapes} overlays=[{overlays}]", keys.get())) }
    }

    struct Run {
        selections: Vec<PermissionPromptSelection>,
        cancels: usize,
        frames: Vec<String>,
    }

    /// One key per frame, then an unbound F12; returns the answers and the
    /// frame each key settled on (the last is the F12 one).
    fn drive(keys_in_order: Vec<KeyEvent>) -> Run {
        drive_groups(keys_in_order.into_iter().map(|key| vec![key]).collect())
    }

    /// `drive` with each group sent at once, so its keys are read in one
    /// frame; one frame per group, then the F12 one.
    fn drive_groups(groups: Vec<Vec<KeyEvent>>) -> Run {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let selections: Selections = Arc::default();
        let cancels = Arc::new(Mutex::new(0usize));
        let (recorded, counted) = (selections.clone(), cancels.clone());
        let frames = futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(PromptHarness(selections: Some(recorded), cancels: Some(counted)));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 20),
            ));
            let mut groups = groups;
            groups.push(vec![KeyEvent::new(KeyEventKind::Press, KeyCode::F(12))]);
            let mut groups = groups.into_iter();
            let mut sent = 0;
            let mut frames = Vec::new();
            let mut last = String::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(std::time::Duration::from_secs(10)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    panic!("no frame after key {sent}; last:\n{last}");
                };
                last = canvas.to_string();
                if !last.contains(&format!("keys={sent} ")) {
                    continue;
                }
                if sent > 0 {
                    frames.push(last.clone());
                }
                let Some(group) = groups.next() else {
                    break;
                };
                for key in group {
                    keys.send(TerminalEvent::Key(key)).await.unwrap();
                    sent += 1;
                }
            }
            frames
        });
        Run {
            selections: selections.lock().unwrap().clone(),
            cancels: *cancels.lock().unwrap(),
            frames,
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(KeyEventKind::Press, code)
    }

    fn typed(text: &str) -> Vec<KeyEvent> {
        text.chars().map(|c| press(KeyCode::Char(c))).collect()
    }

    #[test]
    fn permission_prompt_renders_the_question_indexed_options_and_tab_hint() {
        let run = drive(Vec::new());
        let text = run.frames.last().unwrap();
        assert!(text.contains("Do you want to proceed?"), "{text}");
        assert!(text.contains("❯ 1. Yes"), "{text}");
        assert!(text.contains("2. Yes, always"), "{text}");
        // CC :76-78: the focused "Yes" is amendable.
        assert!(text.contains("Esc to cancel · Tab to amend"), "{text}");
        // CC use-select-input.ts:101: the Select registers the overlay the
        // REPL's cancel handler defers to.
        assert!(text.contains("overlays=[select]"), "{text}");
    }

    #[test]
    fn permission_prompt_escape_counts_and_cancels_once() {
        // CC :219-231 `handleCancel`.
        let run = drive(vec![press(KeyCode::Esc)]);
        assert_eq!(run.cancels, 1);
        assert!(run.selections.is_empty());
        assert!(run.frames.last().unwrap().contains("escapes=1"), "{}", run.frames.last().unwrap());
    }

    #[test]
    fn permission_prompt_tab_amends_and_enter_carries_the_feedback() {
        // CC :118-152 and :154-204: Tab opens the accept input, the typed
        // text goes out trimmed with the answer.
        let mut keys = vec![press(KeyCode::Tab)];
        keys.extend(typed(" be careful "));
        keys.push(press(KeyCode::Enter));
        let run = drive(keys);
        assert_eq!(
            run.selections,
            vec![("yes".to_string(), Some("be careful".to_string()))]
        );
        // With the input open, the hint no longer offers Tab.
        let opened = &run.frames[0];
        assert!(!opened.contains("Tab to amend"), "{opened}");
    }

    #[test]
    fn permission_prompt_empty_amend_still_answers_and_leaving_collapses() {
        // CC :106 `allowEmptySubmitToCancel: true`: an empty input answers
        // without feedback.
        let run = drive(vec![press(KeyCode::Down), press(KeyCode::Down), press(KeyCode::Tab), press(KeyCode::Enter)]);
        assert_eq!(run.selections, vec![("no".to_string(), None)]);
        // CC :241-259: leaving an empty open input collapses it again.
        let run = drive(vec![press(KeyCode::Tab), press(KeyCode::Down), press(KeyCode::Up)]);
        let back = run.frames.last().unwrap();
        assert!(back.contains("Tab to amend"), "{back}");
    }

    /// What keys read in one frame come to. use_select_input's keybinding
    /// hooks (the arrows) drain before its raw hook (Tab), so Tab acts on the
    /// option the arrows land on, whatever order the keys came in; CC reads
    /// them key by key. The outcome matches CC when Tab starts on a feedback
    /// option or comes last, and differs when it starts elsewhere and the
    /// arrow lands on one: from "Yes, always", [Tab, Down] is a no-op Tab in
    /// CC, and here opens the reject input.
    #[test]
    fn permission_prompt_reads_tab_after_the_frames_arrows() {
        let settled = |groups: Vec<Vec<KeyEvent>>| {
            let run = drive_groups(groups);
            run.frames[run.frames.len() - 2].clone()
        };
        let (tab, down) = (press(KeyCode::Tab), press(KeyCode::Down));
        let from_yes = settled(vec![vec![tab.clone(), down.clone()]]);
        assert!(from_yes.contains("❯ 2. Yes, always"), "{from_yes}");
        assert!(!from_yes.contains("tell Claude"), "{from_yes}");
        for group in [vec![tab.clone(), down.clone()], vec![down.clone(), tab.clone()]] {
            let from_always = settled(vec![vec![down.clone()], group]);
            assert!(
                from_always.contains("❯ 3. No, tell Claude what to do differently"),
                "{from_always}"
            );
        }
    }

    #[test]
    fn permission_prompt_selects_by_digit_and_by_option_keybinding() {
        let run = drive(vec![press(KeyCode::Char('3'))]);
        assert_eq!(run.selections, vec![("no".to_string(), None)]);
        // CC :206-217: `confirm:yes` (y) selects the option that names it.
        let run = drive(vec![press(KeyCode::Char('y'))]);
        assert_eq!(run.selections, vec![("always".to_string(), None)]);
    }
}
