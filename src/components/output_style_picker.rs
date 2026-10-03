//! Maps to: CC `components/OutputStylePicker.tsx`.
//!
//! The picker loads the output styles itself, off the render frame (CC
//! :45-59), and owns its keys: its Select reports the chosen style through
//! `on_complete`, its Dialog's `confirm:no` (Esc / n) through `on_cancel`.

use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::dialog::Dialog;
use crate::constants::output_styles::{
    DEFAULT_OUTPUT_STYLE_NAME, OutputStyleConfig, built_in_output_styles_ordered,
};
use iocraft::prelude::*;

pub(crate) const DEFAULT_OUTPUT_STYLE_LABEL: &str = "Default";
pub(crate) const DEFAULT_OUTPUT_STYLE_DESCRIPTION: &str =
    "Claude completes coding tasks efficiently and provides concise responses";

/// Maps to: CC `components/OutputStylePicker.tsx`:13-27 `mapConfigsToOptions(...)`.
pub(crate) fn map_configs_to_options(
    styles: &[(String, Option<OutputStyleConfig>)],
) -> Vec<SelectOptionData> {
    styles
        .iter()
        .map(|(style, config)| SelectOptionData {
            label: config
                .as_ref()
                .map(|config| config.name.clone())
                .unwrap_or_else(|| DEFAULT_OUTPUT_STYLE_LABEL.to_string()),
            value: style.to_string(),
            description: Some(
                config
                    .as_ref()
                    .map(|config| config.description.clone())
                    .unwrap_or_else(|| DEFAULT_OUTPUT_STYLE_DESCRIPTION.to_string()),
            ),
            dim_description: true,
            disabled: false,
            input: None,
        })
        .collect()
}

pub(crate) fn output_style_options_for_cwd(cwd: &std::path::Path) -> Vec<SelectOptionData> {
    // Maps to CC `OutputStylePicker` `useEffect` loading
    // `getAllOutputStyles(getCwd()).then(mapConfigsToOptions)`.
    map_configs_to_options(&crate::constants::output_styles::get_all_output_styles_ordered(cwd))
}

pub(crate) fn output_style_options() -> Vec<SelectOptionData> {
    let cwd = std::env::current_dir().unwrap_or_default();
    output_style_options_for_cwd(&cwd)
}

#[derive(Default, Props)]
pub(crate) struct OutputStylePickerProps<'a> {
    /// CC `initialStyle`: the Select's `defaultValue`, the committed mark.
    pub initial_style: String,
    /// CC `onComplete(style)`.
    pub on_complete: HandlerMut<'a, String>,
    /// CC `onCancel`, the Dialog's `confirm:no`.
    pub on_cancel: HandlerMut<'a, ()>,
    pub is_standalone_command: bool,
}

/// Maps to: CC `components/OutputStylePicker.tsx:36-95`.
#[component]
pub(crate) fn OutputStylePicker<'a>(
    props: &mut OutputStylePickerProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    // CC :42-43 `styleOptions` / `isLoading`: `None` until the load settles.
    let mut style_options = hooks.use_state(|| None::<Vec<SelectOptionData>>);
    // CC :45-59: load every style, custom ones included, on mount; on error
    // fall back to the built-ins. The markdown directories are read on a
    // thread, never in a render frame; a panicking load drops the sender.
    hooks.use_future(async move {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let _ = std::thread::Builder::new()
            .name("output-style-picker-load".to_string())
            .spawn(move || {
                let _ = sender.send(output_style_options());
            });
        let options = receiver
            .await
            .unwrap_or_else(|_| map_configs_to_options(&built_in_output_styles_ordered()));
        style_options.set(Some(options));
    });
    let is_loading = style_options.read().is_none();
    let options = style_options.read().clone().unwrap_or_default();

    // CC :85-90 `<Select options onChange visibleOptionCount={10}
    // defaultValue={initialStyle}>`, mounted once loaded — until then it
    // takes no keys. `defaultValue` marks the committed style; focus starts
    // on the first option, as CC's Select passes no focus value. The hooks
    // live here rather than in an inline-Select carrier (PORTING.md): the
    // Select only ever goes from absent to shown, once per picker, while
    // loading `is_disabled` unregisters its keys, and the empty-to-loaded
    // options reset leaves the state a fresh mount would have.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(10),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: Some(props.initial_style.clone()),
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: is_loading,
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
    // CC :61-67 `handleStyleSelect`.
    if let Some(style) = events.take_accepted() {
        (props.on_complete)(style);
    }
    // The Dialog's handler cannot hold this component's `HandlerMut`; it
    // sets a flag the next render hands to `onCancel`.
    let mut pending_cancel = hooks.use_state(|| false);
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }

    let navigation = state.navigation.snapshot();
    let focused_index = navigation.focused_index().unwrap_or(0);
    let hide_input_guide = !props.is_standalone_command;
    let hide_border = !props.is_standalone_command;

    // CC :69-94.
    element! {
        Dialog(
            title: "Preferred output style".to_string(),
            on_cancel: move |_| pending_cancel.set(true),
            hide_input_guide: hide_input_guide,
            hide_border: hide_border,
        ) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                View(margin_top: 1u32) {
                    Text(content: "This changes how Claude Code communicates with you".to_string(), color: theme.inactive)
                }
                #(if is_loading {
                    element! { Text(content: "Loading output styles…".to_string(), color: theme.inactive) }.into_any()
                } else {
                    element! {
                        Select(
                            is_disabled: false,
                            hide_indexes: false,
                            visible_option_count: navigation.visible_option_count,
                            options: options,
                            focused_index: focused_index,
                            selected_value: state.committed_value(),
                            visible_from_index: navigation.visible_from_index,
                            layout: SelectLayout::Compact,
                        )
                    }.into_any()
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Props)]
    struct PickerHarnessProps {
        initial_style: String,
        is_standalone_command: bool,
        results: Option<Arc<Mutex<Vec<String>>>>,
    }

    /// The picker under the keybinding runtime and theme it mounts with, and
    /// a line counting key presses (so each key produces a frame) and
    /// listing the callbacks it made.
    #[component]
    fn PickerHarness(props: &PickerHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let results = props.results.clone().unwrap_or_default();
        let completed = results.clone();
        let cancelled = results.clone();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        OutputStylePicker(
                            initial_style: props.initial_style.clone(),
                            is_standalone_command: props.is_standalone_command,
                            on_complete: move |style: String| {
                                completed.lock().unwrap().push(format!("complete {style}"));
                            },
                            on_cancel: move |_| cancelled.lock().unwrap().push("cancel".to_string()),
                        )
                        ResultsEcho(results: Some(results))
                    }
                }
            }
        }
    }

    #[derive(Default, Props)]
    struct ResultsEchoProps {
        results: Option<Arc<Mutex<Vec<String>>>>,
    }

    #[component]
    fn ResultsEcho(props: &ResultsEchoProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let results = props.results.clone().unwrap_or_default();
        let results = results.lock().unwrap().join(",");
        element! {
            Text(content: format!("keys={} results=[{results}]", keys.get()))
        }
    }

    /// Returns the first frame, the first frame with the styles loaded, and
    /// the frame after each key (sent one per frame once loaded).
    fn drive(initial_style: &str, keys_in_order: Vec<KeyCode>) -> (String, String, Vec<String>) {
        let initial_style = initial_style.to_string();
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(PickerHarness(
                initial_style: initial_style,
                results: Some(Arc::new(Mutex::new(Vec::new()))),
            ));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 40),
            ));
            let mut first = None;
            let mut loaded = None;
            let mut after_keys = Vec::new();
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                first.get_or_insert_with(|| text.clone());
                if loaded.is_none() {
                    if !text.contains("Explanatory") {
                        continue;
                    }
                    loaded = Some(text.clone());
                } else if text.contains(&format!("keys={} ", after_keys.len() + 1)) {
                    after_keys.push(text.clone());
                } else {
                    continue;
                }
                let Some(code) = keys_in_order.get(after_keys.len()) else {
                    break;
                };
                keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code)))
                    .await
                    .unwrap();
            }
            (first.unwrap(), loaded.unwrap(), after_keys)
        })
    }

    #[test]
    fn map_configs_to_options_uses_official_default_fallback_copy() {
        let options = map_configs_to_options(&[
            (DEFAULT_OUTPUT_STYLE_NAME.to_string(), None),
            (
                "Custom".to_string(),
                Some(OutputStyleConfig {
                    name: "Custom".to_string(),
                    description: "Custom description".to_string(),
                    prompt: "Prompt".to_string(),
                    source: "projectSettings".to_string(),
                    keep_coding_instructions: None,
                    force_for_plugin: None,
                }),
            ),
        ]);

        assert_eq!(options[0].label, DEFAULT_OUTPUT_STYLE_LABEL);
        assert_eq!(options[0].value, DEFAULT_OUTPUT_STYLE_NAME);
        assert_eq!(
            options[0].description.as_deref(),
            Some(DEFAULT_OUTPUT_STYLE_DESCRIPTION)
        );
        assert_eq!(options[1].label, "Custom");
        assert_eq!(
            options[1].description.as_deref(),
            Some("Custom description")
        );
    }

    #[test]
    fn output_style_picker_renders_official_dialog_copy_and_options() {
        // CC :82-91: "Loading output styles…" until the load settles, then
        // the Select.
        let (first, text, _) = drive(DEFAULT_OUTPUT_STYLE_NAME, Vec::new());
        assert!(first.contains("Loading output styles…"), "canvas=\n{first}");
        assert!(!text.contains("Loading output styles…"), "canvas=\n{text}");

        assert!(text.contains("Preferred output style"), "canvas=\n{text}");
        assert!(
            text.contains("This changes how Claude Code communicates with you"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Default"), "canvas=\n{text}");
        assert!(
            text.contains(DEFAULT_OUTPUT_STYLE_DESCRIPTION),
            "canvas=\n{text}"
        );
        assert!(text.contains("Explanatory"), "canvas=\n{text}");
        assert!(
            text.contains("Claude explains its implementation choices"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Learning"), "canvas=\n{text}");
    }

    #[test]
    fn output_style_options_for_cwd_include_custom_markdown_styles() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-output-style-picker-{}",
            uuid::Uuid::new_v4()
        ));
        let cwd = root.join("repo");
        let config_home = root.join("config");
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        std::fs::create_dir_all(cwd.join(".claude/output-styles")).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::write(
            cwd.join(".claude/output-styles/mentor.md"),
            "---\nname: Mentor\ndescription: Project mentor\n---\nPrompt",
        )
        .unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let options = output_style_options_for_cwd(&cwd);
        let _ = std::fs::remove_dir_all(root);

        let mentor = options
            .iter()
            .find(|option| option.value == "Mentor")
            .unwrap();
        assert_eq!(mentor.label, "Mentor");
        assert_eq!(mentor.description.as_deref(), Some("Project mentor"));
    }

    #[test]
    fn output_style_picker_renders_loading_and_standalone_input_guide() {
        let text = element!(PickerHarness(is_standalone_command: true))
            .render(Some(120))
            .to_string();

        assert!(text.contains("Loading output styles…"), "canvas=\n{text}");
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn output_style_picker_marks_the_initial_style_and_focuses_the_first() {
        // CC :89 `defaultValue={initialStyle}` marks the committed style;
        // Select passes no focus value, so focus starts on the first option
        // and Enter completes with it.
        let (_, loaded, after) = drive("Learning", vec![KeyCode::Enter]);
        let learning_row = loaded
            .lines()
            .find(|line| line.contains("Learning"))
            .unwrap_or_default();
        assert!(learning_row.contains("✔"), "canvas=\n{loaded}");
        assert!(
            after[0].contains(&format!("results=[complete {DEFAULT_OUTPUT_STYLE_NAME}]")),
            "{}",
            after[0]
        );
    }

    #[test]
    fn output_style_picker_completes_with_the_focused_style() {
        // CC :61-67 `onComplete(style)`.
        let (_, _, after) = drive(DEFAULT_OUTPUT_STYLE_NAME, vec![KeyCode::Down, KeyCode::Enter]);
        assert!(after[1].contains("results=[complete Explanatory]"), "{}", after[1]);
    }

    #[test]
    fn output_style_picker_escape_cancels_while_loading() {
        // CC :69-75: the Dialog, and its confirm:no, is mounted before the
        // styles load. Esc goes out on the "Loading…" frame; whether or not
        // the load lands first, the Dialog cancels.
        let results = Arc::new(Mutex::new(Vec::new()));
        let echo = results.clone();
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(PickerHarness(results: Some(echo)));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 40),
            ));
            let mut sent = 0;
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                if sent == 0 {
                    assert!(text.contains("Loading output styles…"), "{text}");
                    keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Esc)))
                        .await
                        .unwrap();
                    sent = 1;
                } else if text.contains("keys=1 ") && sent == 1 {
                    // A key nothing binds: its frame shows the settled cancel.
                    keys.send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::F(12),
                    )))
                    .await
                    .unwrap();
                    sent = 2;
                } else if text.contains("keys=2 ") {
                    break;
                }
            }
        });
        assert_eq!(*results.lock().unwrap(), vec!["cancel".to_string()]);
    }

    #[test]
    fn output_style_picker_escape_and_n_cancel_through_the_dialog() {
        // CC :72 `<Dialog onCancel={onCancel}>`: confirm:no is Esc and n.
        for key in [KeyCode::Esc, KeyCode::Char('n')] {
            let (_, _, after) = drive(DEFAULT_OUTPUT_STYLE_NAME, vec![key, KeyCode::Char('x')]);
            assert!(after[1].contains("results=[cancel]"), "{key:?}: {}", after[1]);
        }
    }
}
