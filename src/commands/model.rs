//! `/model` local command UI.
//! Maps to official `src/commands/model/model.tsx`:30-337.
//!
//! [`ModelPicker`] owns options, effort cycling, and keys. This wrapper only
//! applies the `/model` host delta: `initial` / `sessionModel` /
//! `showFastModeNotice` from AppState, plus official `handleSelect` /
//! `handleCancel` copy and `mainLoopModel` writes.

use crate::components::custom_select::SelectOptionData;
#[cfg(test)]
use crate::components::model_picker::displayed_effort;
use crate::components::model_picker::{
    MODEL_NO_PREFERENCE, ModelEffortLevel, ModelPicker, ModelPickerSelection, cycle_effort,
    model_picker_options,
};
use crate::state::app_state::use_app_state_maybe_outside_of_provider;
use crate::state::store::AppStore;
use crate::utils::extra_usage::is_billed_as_extra_usage;
use crate::utils::fast_mode::{
    clear_fast_mode_cooldown, is_fast_mode_available, is_fast_mode_enabled,
    is_fast_mode_supported_by_model,
};
use crate::utils::model::model::{
    get_default_main_loop_model_setting, get_main_loop_model, is_opus_1m_merge_enabled,
    parse_user_specified_model, render_default_model_setting, render_model_name,
};
use iocraft::prelude::*;

/// Maps to: CC `commands/model/index.ts:8-10` `get description()`.
pub fn description() -> String {
    if let Some(active_id) = crate::services::proxy_channel::get_active_channel_id() {
        let name = crate::services::proxy_channel::get_channel(&active_id)
            .map(|c| c.display_name())
            .unwrap_or(active_id.as_str());
        format!(
            "Set the AI model for Claude Code (currently {} · Proxy: {})",
            render_model_name(&get_main_loop_model()),
            name
        )
    } else {
        format!(
            "Set the AI model for Claude Code (currently {})",
            render_model_name(&get_main_loop_model())
        )
    }
}

/// Maps to: CC `commands/model/model.tsx:332-337` `renderModelLabel`.
pub fn render_model_label(model: Option<&str>) -> String {
    let rendered =
        render_default_model_setting(model.unwrap_or(&get_default_main_loop_model_setting()));
    match model {
        None => format!("{rendered} (default)"),
        Some(_) => rendered,
    }
}

fn selected_model_value(value: &str) -> Option<&str> {
    (value != MODEL_NO_PREFERENCE).then_some(value)
}

fn session_model_display(model: &str) -> String {
    let resolved = parse_user_specified_model(model);
    if model == resolved {
        resolved
    } else {
        format!("{model} ({resolved})")
    }
}

/// Maps to: CC `ModelPickerWrapper` `handleSelect` result string.
fn handle_select_message(
    model: Option<&str>,
    effort: Option<ModelEffortLevel>,
    is_fast_mode: bool,
) -> String {
    let mut message = format!("Set model to {}", render_model_label(model));
    if let Some(effort) = effort {
        message.push_str(&format!(" with {} effort", effort.label()));
    }

    let mut was_fast_mode_toggled_on = None;
    if is_fast_mode_enabled() {
        clear_fast_mode_cooldown();
        if !is_fast_mode_supported_by_model(model) && is_fast_mode {
            was_fast_mode_toggled_on = Some(false);
        } else if is_fast_mode_supported_by_model(model) && is_fast_mode_available() && is_fast_mode
        {
            message.push_str(" · Fast mode ON");
            was_fast_mode_toggled_on = Some(true);
        }
    }

    if is_billed_as_extra_usage(
        model,
        was_fast_mode_toggled_on == Some(true),
        is_opus_1m_merge_enabled(),
    ) {
        message.push_str(" · Billed as extra usage");
    }

    if was_fast_mode_toggled_on == Some(false) {
        message.push_str(" · Fast mode OFF");
    }

    message
}

fn apply_wrapper_model_state(store: &AppStore, model: Option<String>, is_fast_mode: bool) {
    if is_fast_mode_enabled() {
        clear_fast_mode_cooldown();
    }
    store.replace_with(|state| {
        state.main_loop_model = model.clone();
        state.main_loop_model_for_session = None;
        if is_fast_mode_enabled()
            && is_fast_mode
            && !is_fast_mode_supported_by_model(model.as_deref())
        {
            state.fast_mode = false;
        }
    });
}

fn handle_select_output(option: &SelectOptionData, effort: Option<ModelEffortLevel>) -> String {
    handle_select_message(selected_model_value(&option.value), effort, false)
}

fn handle_select_output_from_selection(
    selection: &ModelPickerSelection,
    is_fast_mode: bool,
) -> String {
    handle_select_message(
        selected_model_value(&selection.value),
        selection.effort,
        is_fast_mode,
    )
}

fn handle_cancel_output_for(model: Option<&str>) -> String {
    format!("Kept model as {}", render_model_label(model))
}

/// Maps to: CC `ModelPickerWrapper` `handleCancel` when AppState has no
/// `mainLoopModel`.
pub fn handle_cancel_output() -> String {
    handle_cancel_output_for(None)
}

pub fn inline_output_for_args(args: &str) -> Option<String> {
    let args = args.trim();
    if matches!(args, "help" | "-h" | "--help") {
        return Some(
            "Run /model to open the model selection menu, or /model [modelName] to set the model."
                .to_string(),
        );
    }
    if matches!(
        args,
        "list"
            | "show"
            | "display"
            | "current"
            | "view"
            | "get"
            | "check"
            | "describe"
            | "print"
            | "version"
            | "about"
            | "status"
            | "?"
    ) {
        return Some(format!("Current model: {}", render_model_label(None)));
    }
    if args.is_empty() {
        return None;
    }

    let model = if args == "default" { None } else { Some(args) };
    Some(handle_select_message(model, None, false))
}

pub fn env_flag_truthy(value: Option<&str>) -> bool {
    value
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[derive(Default, Props)]
pub struct ModelPickerWrapperProps<'a> {
    pub on_close: HandlerMut<'a, ()>,
    pub on_select: HandlerMut<'a, String>,
}

#[component]
pub fn ModelPickerWrapper<'a>(
    props: &mut ModelPickerWrapperProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let main_loop_model =
        use_app_state_maybe_outside_of_provider(&mut hooks, |state| state.main_loop_model.clone())
            .flatten();
    let main_loop_model_for_session =
        use_app_state_maybe_outside_of_provider(&mut hooks, |state| {
            state.main_loop_model_for_session.clone()
        })
        .flatten();
    let is_fast_mode = use_app_state_maybe_outside_of_provider(&mut hooks, |state| state.fast_mode)
        .unwrap_or(false);
    let session_model_label = main_loop_model_for_session
        .as_deref()
        .map(session_model_display);
    let show_fast_mode_notice = is_fast_mode_enabled()
        && is_fast_mode
        && is_fast_mode_supported_by_model(main_loop_model.as_deref())
        && is_fast_mode_available();
    let store = hooks
        .try_use_context::<AppStore>()
        .map(|store| store.clone());
    let mut pending_output = hooks.use_state(|| Option::<String>::None);

    let selected = { pending_output.read().clone() };
    if let Some(output) = selected {
        pending_output.set(None);
        (props.on_select)(output);
    }

    let cancel_model = main_loop_model.clone();
    element! {
        ModelPicker(
            initial: main_loop_model,
            header_text: None,
            session_model_label: session_model_label,
            is_standalone_command: true,
            skip_settings_write: false,
            show_fast_mode_notice: show_fast_mode_notice,
            show_fast_mode_available_hint: false,
            fast_mode_is_on: is_fast_mode,
            exit_pending: false,
            exit_key_name: None,
            on_select: move |selection: ModelPickerSelection| {
                let model = selected_model_value(&selection.value).map(str::to_string);
                if let Some(store) = store.as_ref() {
                    apply_wrapper_model_state(store, model, is_fast_mode);
                }
                pending_output.set(Some(handle_select_output_from_selection(
                    &selection,
                    is_fast_mode,
                )));
            },
            on_cancel: move |_| {
                pending_output.set(Some(handle_cancel_output_for(cancel_model.as_deref())));
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn canvas_lines(canvas: &Canvas) -> Vec<String> {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn find_text_cell(canvas: &Canvas, needle: &str) -> Option<(usize, usize)> {
        canvas_lines(canvas)
            .iter()
            .enumerate()
            .find_map(|(row, line)| line.find(needle).map(|column| (column, row)))
    }

    fn render_model_canvas_after(events: Vec<TerminalEvent>) -> (Canvas, Vec<String>, usize) {
        let selected_outputs = Arc::new(Mutex::new(Vec::<String>::new()));
        let close_count = Arc::new(Mutex::new(0usize));
        let selected_for_handler = Arc::clone(&selected_outputs);
        let close_for_handler = Arc::clone(&close_count);
        let current_theme = *theme::current();
        let keybinding_runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();

        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(keybinding_runtime)) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        ModelPickerWrapper(
                        on_close: move |_| {
                            *close_for_handler.lock().expect("close mutex") += 1;
                        },
                        on_select: move |output: String| {
                            selected_for_handler.lock().expect("select mutex").push(output);
                        },
                    )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(events)).with_size(110, 32),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        });

        let canvas = canvases
            .last()
            .cloned()
            .unwrap_or_else(|| Canvas::new(0, 0));
        let selected = selected_outputs.lock().expect("selected outputs").clone();
        let close_count = *close_count.lock().expect("close count");
        (canvas, selected, close_count)
    }

    fn render_model_text_after(events: Vec<TerminalEvent>) -> (String, Vec<String>, usize) {
        let (canvas, selected, close_count) = render_model_canvas_after(events);
        let text = canvas_lines(&canvas).join("\n");
        (text, selected, close_count)
    }

    #[test]
    fn model_picker_options_use_official_visible_copy_and_select_output() {
        let options = model_picker_options(120);

        assert_eq!(options[0].label, "Default (recommended)");
        assert_eq!(options[0].value, MODEL_NO_PREFERENCE);
        assert_eq!(
            options[0].description.as_deref(),
            Some("Use the default model (currently Sonnet 4.6)")
        );
        assert!(options.iter().any(|option| {
            option.label == "Sonnet"
                && option
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("Sonnet 4.6"))
        }));

        let output = handle_select_output(&options[1], None);
        assert_eq!(
            output,
            format!(
                "Set model to {}",
                render_model_label(Some(&options[1].value))
            )
        );
        assert_eq!(
            handle_cancel_output(),
            format!("Kept model as {}", render_model_label(None))
        );
    }

    #[test]
    fn model_picker_wrapper_pointer_uses_suggestion_color_without_background() {
        let current_theme = *theme::current();
        let (canvas, _, _) = render_model_canvas_after(Vec::new());
        let text = canvas_lines(&canvas).join("\n");
        let (column, row) = find_text_cell(&canvas, "❯").expect("pointer cell");
        let pointer = canvas.cell(column, row).expect("pointer cell");
        let style = pointer.text_style().expect("pointer style");

        assert_eq!(pointer.text(), Some("❯"), "canvas=\n{text}");
        assert_eq!(
            style.color,
            Some(current_theme.suggestion),
            "canvas=\n{text}"
        );
        assert_eq!(pointer.background_color, None, "canvas=\n{text}");
        assert_eq!(canvas.cursor_declaration(), None, "canvas=\n{text}");
    }

    #[test]
    fn model_picker_wrapper_renders_official_modelpicker_shape() {
        let (text, selected, close_count) = render_model_text_after(Vec::new());

        assert!(text.contains("Select model"), "canvas=\n{text}");
        assert!(
            text.contains("Switch between Claude models."),
            "canvas=\n{text}"
        );
        assert!(text.contains("specify with --model."), "canvas=\n{text}");
        assert!(text.contains("Default (recommended)"), "canvas=\n{text}");
        assert!(text.contains("Sonnet 4.6 · Best"), "canvas=\n{text}");
        assert!(text.contains("● High effort (default)"), "canvas=\n{text}");
        assert!(
            !text.contains("Use /fast to turn on Fast mode"),
            "no fast-mode notice should render when ModelPickerWrapper does not enable showFastModeNotice; canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to exit"),
            "canvas=\n{text}"
        );
        assert!(selected.is_empty());
        assert_eq!(close_count, 0);
    }

    #[test]
    fn model_picker_wrapper_selects_official_set_model_message() {
        let options = model_picker_options(120);
        let (text, selected, close_count) =
            render_model_text_after(vec![key(KeyCode::Down), key(KeyCode::Enter)]);

        assert_eq!(selected, vec![handle_select_output(&options[1], None)]);
        assert_eq!(close_count, 0);
        assert!(text.contains("Select model"), "canvas=\n{text}");
    }

    #[test]
    fn model_command_panel_effort_cycle_matches_modelpicker_keys() {
        let options = model_picker_options(120);
        assert_eq!(
            cycle_effort(ModelEffortLevel::High, KeyCode::Right, "opus"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            cycle_effort(ModelEffortLevel::Xhigh, KeyCode::Right, "opus"),
            ModelEffortLevel::Max
        );
        assert_eq!(
            displayed_effort(ModelEffortLevel::Max, "sonnet"),
            ModelEffortLevel::Max
        );

        let (_, selected, _) = render_model_text_after(vec![
            key(KeyCode::Down),
            key(KeyCode::Right),
            key(KeyCode::Enter),
        ]);
        // Official QOe denies xhigh on 4.6, so High → Max.
        assert_eq!(
            selected,
            vec![handle_select_output(
                &options[1],
                Some(ModelEffortLevel::Max)
            )]
        );
    }

    #[test]
    fn model_picker_wrapper_esc_uses_official_cancel_message() {
        let (_, selected, close_count) = render_model_text_after(vec![key(KeyCode::Esc)]);

        assert_eq!(selected, vec![handle_cancel_output()]);
        assert_eq!(close_count, 0);
    }
}
