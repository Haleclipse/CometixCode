//! Maps to: CC `components/ApproveApiKey.tsx`.
//!
//! The dialog owns its keys: its Select (focused on "No") answers, the
//! Select's cancel (Esc) and the Dialog's confirm:no (n, and Esc when the
//! Select leaves it) answer "no". The Select sits in a carrier inside the
//! Dialog's content, so it sees Esc first, as CC's child-first listeners
//! give it. The answer records the truncated key in `customApiKeyResponses`
//! before `onDone(approved)`.
//!
//! Deliberate deviation: the first answer wins and is reported once. CC
//! would run `onChange` again for a second answer in the same input batch
//! (a pasted Enter+Esc), recording the key in both lists; the port's
//! answers arrive through a slot read on the next render, and a second
//! answer would land after the step moved on.

use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::dialog::Dialog;
use crate::utils::config::{CustomApiKeyResponses, GlobalConfig};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

/// Maps to: CC `components/ApproveApiKey.tsx:63-73` `options`.
pub(crate) fn approve_api_key_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Yes".to_string(),
            value: "yes".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No (recommended)".to_string(),
            value: "no".to_string(),
            ..SelectOptionData::default()
        },
    ]
}

/// Maps to: CC `components/ApproveApiKey.tsx:16-47` `onChange`'s config
/// updates: the key joins `approved` or `rejected`, the other list kept.
pub(crate) fn record_custom_api_key_response(
    config: &mut GlobalConfig,
    custom_api_key_truncated: &str,
    approved: bool,
) {
    let mut responses = config
        .custom_api_key_responses
        .clone()
        .unwrap_or_else(CustomApiKeyResponses::default);
    let list = if approved {
        &mut responses.approved
    } else {
        &mut responses.rejected
    };
    list.get_or_insert_with(Vec::new)
        .push(custom_api_key_truncated.to_string());
    config.custom_api_key_responses = Some(responses);
}

#[derive(Default, Props)]
pub(crate) struct ApproveApiKeyProps<'a> {
    /// CC `customApiKeyTruncated`.
    pub custom_api_key_truncated: String,
    /// CC `onDone(approved)`.
    pub on_done: HandlerMut<'a, bool>,
}

/// Maps to: CC `components/ApproveApiKey.tsx:12-79`.
#[component]
pub(crate) fn ApproveApiKey<'a>(
    props: &mut ApproveApiKeyProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    // The Dialog's and the Select's handlers cannot hold `on_done`; they
    // fill this slot, and the next render answers.
    let answer = hooks.use_state(|| None::<bool>);
    let mut reported = hooks.use_state(|| false);
    // CC :16-47 `onChange`.
    if let Some(approved) = answer.get()
        && !reported.get()
    {
        reported.set(true);
        let truncated = props.custom_api_key_truncated.clone();
        if let Err(error) = crate::utils::config::save_global_config(|config| {
            record_custom_api_key_response(config, &truncated, approved);
        }) {
            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        }
        (props.on_done)(approved);
    }

    let custom_api_key_truncated = props.custom_api_key_truncated.clone();
    // CC :49-78.
    element! {
        Dialog(
            title: "Detected a custom API key in your environment".to_string(),
            color: Some(theme.warning),
            on_cancel: move |_| answer_once(answer, false),
        ) {
            // CC :55-58: one wrapping Text, the name bold.
            MixedText(contents: vec![
                MixedTextContent::new("ANTHROPIC_API_KEY").weight(Weight::Bold),
                MixedTextContent::new(format!(": sk-ant-...{custom_api_key_truncated}")),
            ])
            Text(content: "Do you want to use this API key?".to_string())
            ApproveApiKeySelect(
                on_change: move |value: String| answer_once(answer, value == "yes"),
                on_cancel: move |_| answer_once(answer, false),
            )
        }
    }
}

/// Fills the answer slot unless an earlier answer already did.
fn answer_once(mut answer: State<Option<bool>>, approved: bool) {
    if answer.get().is_none() {
        answer.set(Some(approved));
    }
}

#[derive(Default, Props)]
struct ApproveApiKeySelectProps<'a> {
    /// CC `onChange(value)`.
    on_change: HandlerMut<'a, String>,
    /// CC `onCancel`.
    on_cancel: HandlerMut<'a, ()>,
}

/// L1 (inline Select state carrier, PORTING.md): CC writes this Select
/// inline in the Dialog (:60-76); the port's Select only renders, and its
/// state lives here in its place, so its keys are read before the Dialog's
/// confirm:no. An accept and a cancel in one poll report the accept first.
#[component]
fn ApproveApiKeySelect<'a>(
    props: &mut ApproveApiKeySelectProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let options = approve_api_key_options();
    // CC :60-76 `<Select defaultValue="no" defaultFocusValue="no" onChange
    // onCancel>`: the default five-row viewport.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: Some("no".to_string()),
            focus_value: Some("no".to_string()),
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
    // Both are taken every render, so neither is left to fire later.
    let accepted = events.take_accepted();
    let cancelled = events.take_cancelled();
    if let Some(value) = accepted {
        (props.on_change)(value);
    }
    if cancelled {
        (props.on_cancel)(());
    }

    let navigation = state.navigation.snapshot();
    // CC label is nested Text: only "recommended" is bold.
    let mut recommended = StyledSegment::new("recommended");
    recommended.styles.bold = Some(true);
    let option_labels = BTreeMap::from([(
        "no".to_string(),
        vec![
            StyledSegment::new("No ("),
            recommended,
            StyledSegment::new(")"),
        ],
    )]);

    element! {
        Select(
            is_disabled: false,
            hide_indexes: false,
            visible_option_count: navigation.visible_option_count,
            options: options,
            option_labels,
            focused_index: navigation.focused_index().unwrap_or(0),
            selected_value: state.committed_value(),
            visible_from_index: navigation.visible_from_index,
            layout: SelectLayout::Compact,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::keybinding_context::KeybindingRuntime;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    type Results = Arc<Mutex<Vec<bool>>>;

    #[derive(Default, Props)]
    struct ApproveHarnessProps {
        results: Option<Results>,
        /// Unbinds Esc in the Select context.
        unbind_select_escape: bool,
    }

    /// The dialog under the keybinding runtime and theme it mounts with, and
    /// a line counting key presses (so each key produces a frame) and
    /// listing the `onDone` answers.
    #[component]
    fn ApproveHarness(props: &ApproveHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let unbind_select_escape = props.unbind_select_escape;
        let runtime = hooks.use_const(|| {
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            if unbind_select_escape {
                bindings.push(crate::keybindings::types::ParsedBinding {
                    chord: crate::keybindings::parser::parse_chord("escape"),
                    action: None,
                    context: crate::keybindings::types::ContextName::Select,
                });
            }
            KeybindingRuntime::new(bindings)
        });
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks, runtime,
        );
        let results = props.results.clone().unwrap_or_default();
        let answered = results.clone();
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let answers = results.lock().unwrap().iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        ApproveApiKey(
                            custom_api_key_truncated: "abcd1234".to_string(),
                            on_done: move |approved: bool| answered.lock().unwrap().push(approved),
                        )
                        Text(content: format!("keys={} answers=[{answers}]", keys.get()))
                    }
                }
            }
        }
    }

    /// Sends one key per frame; see [`drive_batches`].
    fn drive(keys_in_order: Vec<KeyCode>) -> (Vec<bool>, String) {
        drive_batches(keys_in_order.into_iter().map(|code| vec![code]).collect())
    }

    /// Sends each batch in one go once the previous one has a frame, then
    /// an unbound F12 so the last answer has settled; returns the answers
    /// and the mount frame. Writes stay off: the answer's save is a dry run.
    fn drive_batches(batches: Vec<Vec<KeyCode>>) -> (Vec<bool>, String) {
        drive_with(batches, false)
    }

    fn drive_with(batches: Vec<Vec<KeyCode>>, unbind_select_escape: bool) -> (Vec<bool>, String) {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _no_write = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        let results: Results = Arc::new(Mutex::new(Vec::new()));
        let echo = results.clone();
        let mut batches = batches;
        batches.push(vec![KeyCode::F(12)]);
        let first = futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(ApproveHarness(results: Some(echo), unbind_select_escape));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
            let mut next_batch = 0;
            let mut sent = 0;
            let mut mount = String::new();
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                if !text.contains(&format!("keys={sent} ")) {
                    continue;
                }
                if next_batch == 0 {
                    mount = text.clone();
                }
                let Some(batch) = batches.get(next_batch) else {
                    break;
                };
                for code in batch {
                    keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code)))
                        .await
                        .unwrap();
                }
                sent += batch.len();
                next_batch += 1;
            }
            mount
        });
        let results = results.lock().unwrap().clone();
        (results, first)
    }

    #[test]
    fn approve_api_key_renders_official_dialog_copy_and_options() {
        let text = element!(ApproveHarness).render(Some(100)).to_string();

        assert!(
            text.contains("Detected a custom API key in your environment"),
            "canvas=\n{text}"
        );
        assert!(text.contains("ANTHROPIC_API_KEY"), "canvas=\n{text}");
        assert!(text.contains("sk-ant-...abcd1234"), "canvas=\n{text}");
        assert!(
            text.contains("Do you want to use this API key?"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Yes"), "canvas=\n{text}");
        assert!(text.contains("No (recommended)"), "canvas=\n{text}");
    }

    #[test]
    fn approve_api_key_select_preserves_source_child_bold_and_compact_layout() {
        let canvas = element!(ApproveHarness).render(Some(100));
        let text = canvas.to_string();
        let lines = text.lines().collect::<Vec<_>>();
        let row = lines
            .iter()
            .position(|line| line.contains("No (recommended)"))
            .unwrap();
        assert!(lines[row].contains("2. No (recommended)"), "{text}");
        assert!(
            lines[row - 1].contains("1. Yes"),
            "source compact has no expanded blank row: {text}"
        );
        for (needle, expected) in [("No (", Weight::Normal), ("recommended", Weight::Bold)] {
            let offset = lines[row].find(needle).unwrap();
            let column = unicode_width::UnicodeWidthStr::width(&lines[row][..offset]);
            assert_eq!(
                canvas.resolved_text_style(column, row).unwrap().weight,
                expected,
                "{needle}: {text}"
            );
        }
    }

    #[test]
    fn approve_api_key_opens_focused_on_no_and_enter_rejects() {
        // CC :61-62 `defaultValue="no" defaultFocusValue="no"`.
        let (answers, mount) = drive(vec![KeyCode::Enter]);
        let focused = mount.lines().find(|line| line.contains('❯')).unwrap_or_default();
        assert!(focused.contains("No (recommended)"), "canvas=\n{mount}");
        assert_eq!(answers, vec![false]);
    }

    #[test]
    fn approve_api_key_up_and_enter_approves() {
        let (answers, _) = drive(vec![KeyCode::Up, KeyCode::Enter]);
        assert_eq!(answers, vec![true]);
    }

    #[test]
    fn approve_api_key_escape_and_n_reject() {
        // CC :53 `<Dialog onCancel={() => onChange('no')}>` and :75: Esc is
        // also the Select's cancel; n is the Dialog's confirm:no.
        for key in [KeyCode::Esc, KeyCode::Char('n')] {
            let (answers, _) = drive(vec![key]);
            assert_eq!(answers, vec![false], "{key:?}");
        }
    }

    #[test]
    fn approve_api_key_select_sees_escape_before_the_dialog() {
        // CC: the Select is the Dialog's child, so its listener registers
        // first; with Esc unbound in Select, that unbound match stops Esc
        // (useKeybinding.ts:182-186) before the Dialog's confirm:no. n
        // still reaches the Dialog.
        let (answers, _) = drive_with(vec![vec![KeyCode::Esc]], true);
        assert_eq!(answers, Vec::<bool>::new());
        let (answers, _) = drive_with(vec![vec![KeyCode::Char('n')]], true);
        assert_eq!(answers, vec![false]);
    }

    #[test]
    fn approve_api_key_answers_once_for_keys_in_one_batch() {
        // Enter on "Yes" and Esc in one read: the accept is the answer, and
        // neither the Select's cancel nor the Dialog's confirm:no adds one.
        let (answers, _) = drive_batches(vec![vec![KeyCode::Up], vec![KeyCode::Enter, KeyCode::Esc]]);
        assert_eq!(answers, vec![true]);
        // Two rejections in one read, then another key once mounted still.
        let (answers, _) = drive_batches(vec![vec![KeyCode::Esc, KeyCode::Char('n')], vec![KeyCode::Enter]]);
        assert_eq!(answers, vec![false]);
    }

    #[test]
    fn approve_api_key_records_the_answer_beside_earlier_ones() {
        // CC :19-42: the key joins its list; the other list is kept.
        let mut config = GlobalConfig::default();
        record_custom_api_key_response(&mut config, "old", false);
        record_custom_api_key_response(&mut config, "abcd1234", true);
        let responses = config.custom_api_key_responses.unwrap();
        assert_eq!(responses.approved, Some(vec!["abcd1234".to_string()]));
        assert_eq!(responses.rejected, Some(vec!["old".to_string()]));
    }
}
