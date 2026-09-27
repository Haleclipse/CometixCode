//! Maps to: CC `components/ChannelDowngradeDialog.tsx`.
//!
//! The dialog owns its keys: its Select reports the choice through
//! `on_choice`, and its Dialog's `confirm:no` (Esc / n) reports `Cancel`.

use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::dialog::Dialog;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelDowngradeChoice {
    Downgrade,
    Stay,
    Cancel,
}

impl ChannelDowngradeChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downgrade => "downgrade",
            Self::Stay => "stay",
            Self::Cancel => "cancel",
        }
    }

    pub fn from_value(value: &str) -> Option<Self> {
        match value {
            "downgrade" => Some(Self::Downgrade),
            "stay" => Some(Self::Stay),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[derive(Default, Props)]
pub struct ChannelDowngradeDialogProps<'a> {
    pub current_version: String,
    /// CC `onChoice`.
    pub on_choice: HandlerMut<'a, ChannelDowngradeChoice>,
}

/// Maps to: CC `ChannelDowngradeDialog.tsx:42-54` `<Select options>`.
pub fn channel_downgrade_options(current_version: &str) -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Allow possible downgrade to stable version".to_string(),
            value: ChannelDowngradeChoice::Downgrade.as_str().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: format!("Stay on current version ({current_version}) until stable catches up"),
            value: ChannelDowngradeChoice::Stay.as_str().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

/// Maps to: CC `ChannelDowngradeDialog.tsx:17-57`.
#[component]
pub fn ChannelDowngradeDialog<'a>(
    props: &mut ChannelDowngradeDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    let options = channel_downgrade_options(&props.current_version);
    // CC :42-54 `<Select options onChange={handleSelect}>`: the default
    // five-row viewport, no default value, no cancel of its own.
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
    // CC :21-23 `handleSelect`.
    if let Some(choice) = events
        .take_accepted()
        .as_deref()
        .and_then(ChannelDowngradeChoice::from_value)
    {
        (props.on_choice)(choice);
    }
    // CC :25-27 `handleCancel`. The Dialog's handler cannot hold this
    // component's `HandlerMut`; it sets a flag the next render hands on.
    let mut pending_cancel = hooks.use_state(|| false);
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_choice)(ChannelDowngradeChoice::Cancel);
    }

    let navigation = state.navigation.snapshot();
    let focused_index = navigation.focused_index().unwrap_or(0);

    // CC :29-56.
    element! {
        Dialog(
            title: "Switch to Stable Channel".to_string(),
            on_cancel: move |_| pending_cancel.set(true),
            color: Some(theme.permission),
            hide_border: true,
            hide_input_guide: true,
        ) {
            Text(
                content: format!(
                    "The stable channel may have an older version than what you're currently running ({}).",
                    props.current_version
                ),
                wrap: TextWrap::Wrap,
            )
            Text(content: "How would you like to handle this?".to_string(), color: theme.inactive)
            Select(
                is_disabled: false,
                hide_indexes: false,
                visible_option_count: navigation.visible_option_count,
                options: options,
                focused_index: focused_index,
                visible_from_index: navigation.visible_from_index,
                layout: SelectLayout::Compact,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    #[test]
    fn channel_downgrade_choices_match_official_values() {
        assert_eq!(ChannelDowngradeChoice::Downgrade.as_str(), "downgrade");
        assert_eq!(ChannelDowngradeChoice::Stay.as_str(), "stay");
        assert_eq!(ChannelDowngradeChoice::Cancel.as_str(), "cancel");
        assert_eq!(
            ChannelDowngradeChoice::from_value("downgrade"),
            Some(ChannelDowngradeChoice::Downgrade)
        );
        assert_eq!(ChannelDowngradeChoice::from_value("other"), None);
    }

    #[test]
    fn channel_downgrade_options_match_official_labels() {
        let options = channel_downgrade_options("1.2.3");
        assert_eq!(options.len(), 2);
        assert_eq!(
            options[0].label,
            "Allow possible downgrade to stable version"
        );
        assert_eq!(options[0].value, "downgrade");
        assert_eq!(
            options[1].label,
            "Stay on current version (1.2.3) until stable catches up"
        );
        assert_eq!(options[1].value, "stay");
    }

    #[derive(Default, Props)]
    struct DialogHarnessProps {
        choices: Option<Arc<Mutex<Vec<&'static str>>>>,
    }

    /// The dialog under the keybinding runtime and theme it mounts with, and
    /// a line counting key presses (so each key produces a frame) and
    /// listing the choices it reported.
    #[component]
    fn DialogHarness(props: &DialogHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let choices = props.choices.clone().unwrap_or_default();
        let reported = choices.clone();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        ChannelDowngradeDialog(
                            current_version: "1.2.3".to_string(),
                            on_choice: move |choice: ChannelDowngradeChoice| {
                                reported.lock().unwrap().push(choice.as_str());
                            },
                        )
                        ChoicesEcho(choices: Some(choices))
                    }
                }
            }
        }
    }

    #[component]
    fn ChoicesEcho(props: &DialogHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let choices = props.choices.clone().unwrap_or_default();
        let choices = choices.lock().unwrap().join(",");
        element! { Text(content: format!("keys={} choices=[{choices}]", keys.get())) }
    }

    /// Sends one key per frame and returns the frame after the last one.
    fn drive(keys_in_order: Vec<KeyCode>) -> String {
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(DialogHarness(choices: Some(Arc::new(Mutex::new(Vec::new())))));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 30),
            ));
            let mut sent = 0;
            let mut last = String::new();
            while let Some(canvas) = render_loop.next().await {
                last = canvas.to_string();
                if !last.contains(&format!("keys={sent} ")) {
                    continue;
                }
                let Some(code) = keys_in_order.get(sent) else {
                    break;
                };
                keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code)))
                    .await
                    .unwrap();
                sent += 1;
            }
            last
        })
    }

    #[test]
    fn channel_downgrade_dialog_reports_the_chosen_option() {
        // CC :21-23, :53 `onChange={handleSelect}`.
        let last = drive(vec![KeyCode::Down, KeyCode::Enter, KeyCode::Char('x')]);
        assert!(last.contains("choices=[stay]"), "{last}");
    }

    #[test]
    fn channel_downgrade_dialog_escape_and_n_report_cancel() {
        // CC :25-27, :32 `<Dialog onCancel={handleCancel}>`: Esc and n.
        for key in [KeyCode::Esc, KeyCode::Char('n')] {
            let last = drive(vec![key, KeyCode::Char('x')]);
            assert!(last.contains("choices=[cancel]"), "{key:?}: {last}");
        }
    }

    #[test]
    fn channel_downgrade_dialog_renders_official_body_without_input_guide() {
        let text = element!(DialogHarness).render(Some(120)).to_string();

        assert!(text.contains("Switch to Stable Channel"), "canvas=\n{text}");
        assert!(
            text.contains("The stable channel may have an older version than what you're currently running (1.2.3)."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("How would you like to handle this?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Allow possible downgrade to stable version"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Stay on current version (1.2.3) until stable catches up"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to confirm"),
            "official ChannelDowngradeDialog hideInputGuide omits a separate footer; canvas=\n{text}"
        );
        // Dialog.tsx:76 `gap={1}` separates each child of the dialog body.
        let lines: Vec<&str> = text.lines().collect();
        let body = lines
            .iter()
            .position(|line| line.contains("The stable channel may have"))
            .unwrap();
        assert!(lines[body + 1].trim().is_empty(), "canvas=\n{text}");
        assert!(lines[body + 2].contains("How would you like"), "canvas=\n{text}");
    }
}
