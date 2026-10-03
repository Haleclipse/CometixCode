//! Maps to: CC `components/ExportDialog.tsx`.
//!
//! The dialog owns filename/cursor state and writes before delivering onDone.
//! Clipboard dispatch belongs to the canonical ink/termio/osc owner; its raw
//! terminal response uses the existing iocraft control-sequence carrier.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectOptionData, UseSelectInputOptions, UseSelectStateProps,
    use_select_input, use_select_state,
};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::components::text_input::TextInput;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportDialogResult {
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportOption {
    Clipboard,
    File,
}

impl ExportOption {
    pub fn value(&self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::File => "file",
        }
    }
}

pub fn export_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Copy to clipboard".to_string(),
            value: ExportOption::Clipboard.value().to_string(),
            description: Some("Copy the conversation to your system clipboard".to_string()),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Save to file".to_string(),
            value: ExportOption::File.value().to_string(),
            description: Some(
                "Save the conversation to a file in the current directory".to_string(),
            ),
            ..SelectOptionData::default()
        },
    ]
}

pub fn export_option_from_value(value: &str) -> ExportOption {
    if value == "file" {
        ExportOption::File
    } else {
        ExportOption::Clipboard
    }
}

/// Maps to: CC `ExportDialog.tsx` `finalFilename` normalization.
pub fn export_final_filename(filename: &str) -> String {
    if filename.ends_with(".txt") {
        return filename.to_string();
    }
    if let Some(dot_index) = filename.rfind('.') {
        if dot_index + 1 < filename.len() && !filename[dot_index + 1..].contains('.') {
            return format!("{}.txt", &filename[..dot_index]);
        }
    }
    format!("{filename}.txt")
}

pub fn export_filepath(cwd: impl Into<PathBuf>, filename: &str) -> String {
    // Node path.join concatenates an absolute second operand rather than
    // replacing cwd as PathBuf::join does, then normalizes dot segments.
    let cwd = cwd.into();
    let joined = if cwd.as_os_str().is_empty() {
        export_final_filename(filename)
    } else {
        format!("{}/{}", cwd.display(), export_final_filename(filename))
    };
    let mut normalized = PathBuf::new();
    for component in Path::new(&joined).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if normalized.file_name().is_some_and(|name| name != "..") {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized.to_string_lossy().into_owned()
}

pub fn export_clipboard_result() -> ExportDialogResult {
    ExportDialogResult {
        success: true,
        message: "Conversation copied to clipboard".to_string(),
    }
}

pub fn export_file_result(cwd: impl Into<PathBuf>, filename: &str) -> ExportDialogResult {
    ExportDialogResult {
        success: true,
        message: format!(
            "Conversation exported to: {}",
            export_filepath(cwd, filename)
        ),
    }
}

pub fn export_cancelled_result() -> ExportDialogResult {
    ExportDialogResult {
        success: false,
        message: "Export cancelled".to_string(),
    }
}

#[derive(Default, Props)]
pub struct ExportDialogProps {
    /// L1 source process.stdout carrier, retained by the owning REPL.
    pub command_stdout: Option<StdoutHandle>,
    pub content: String,
    pub default_filename: String,
    /// Existing getCwd transport from the REPL's current working directory.
    pub cwd: Option<String>,
    pub on_done: Handler<ExportDialogResult>,
}

/// Test-only imported writeFileSync seam; production always calls its owner.
#[cfg(test)]
#[derive(Clone)]
struct ExportWriteFixture(
    std::sync::Arc<dyn Fn(&Path, &str, bool) -> Result<(), String> + Send + Sync>,
);

/// Maps to: CC `components/ExportDialog.tsx#ExportDialog:25-173`.
#[component]
pub fn ExportDialog(
    props: &mut ExportDialogProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut selected_option = hooks.use_state(|| Option::<ExportOption>::None);
    let filename = hooks.use_state(|| props.default_filename.clone());
    let cursor_offset = hooks.use_state(|| props.default_filename.encode_utf16().count());
    let mut show_filename_input = hooks.use_state(|| false);
    let columns = hooks.use_terminal_size().0 as usize;
    // HandlerMut delivery is the established React/Ink -> iocraft pending-state
    // carrier; these values are not a second owner of source domain state.
    let mut pending_result = hooks.use_state(|| Option::<ExportDialogResult>::None);
    let mut should_cancel = hooks.use_state(|| false);
    let (local_stdout, _) = hooks.use_output();
    let stdout = props.command_stdout.clone().unwrap_or(local_stdout);
    let handle_select_clipboard = Handler::from({
        let content = props.content.clone();
        let on_done = props.on_done.clone();
        move |(): ()| {
            let content = content.clone();
            let stdout = stdout.clone();
            let on_done = on_done.clone();
            let copy = stdout.prepare_clipboard(&content);
            // Source async callback starts its promise immediately; detached
            // lifetime preserves native/tmux/raw work after cancelling the UI.
            crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("clipboard requires the initialized process runtime")
                .spawn(async move {
                    // CC :47-49 awaits setClipboard, writes its raw result, then onDone.
                    let raw = copy.await;
                    if !raw.is_empty() {
                        if let Err(error) = stdout.write_control_sequence_and_wait(raw).await {
                            crate::utils::debug::log_for_debugging(&error.to_string());
                            return;
                        }
                    }
                    // The source still invokes its captured onDone after unmount.
                    // The command's outer Promise owns first-settlement gating.
                    on_done(export_clipboard_result());
                });
        }
    });
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|value| value.clone());
    use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        ContextName::Settings,
        move || show_filename_input.get(),
        move || {
            should_cancel.set(true);
            true
        },
    );
    if should_cancel.get() {
        should_cancel.set(false);
        if show_filename_input.get() {
            // CC handleGoBack :39-42 preserves filename and cursorOffset.
            show_filename_input.set(false);
            selected_option.set(None);
        } else {
            pending_result.set(Some(export_cancelled_result()));
        }
    }
    let result = pending_result.read().clone();
    if let Some(result) = result {
        pending_result.set(None);
        (props.on_done)(result);
    }

    let content = props.content.clone();
    let cwd = props.cwd.clone();
    #[cfg(test)]
    let write_fixture = hooks
        .try_use_context::<ExportWriteFixture>()
        .map(|value| value.clone());
    let handle_filename_submit = move |_: String| {
        let cwd = cwd.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd())
                .to_string_lossy()
                .into_owned()
        });
        let filepath = export_filepath(&cwd, &filename.read());
        #[cfg(test)]
        let written = if let Some(fixture) = &write_fixture {
            (fixture.0)(Path::new(&filepath), &content, true)
        } else {
            crate::utils::slow_operations::write_file_sync_deprecated(
                Path::new(&filepath),
                &content,
                true,
            )
            .map_err(|error| error.to_string())
        };
        #[cfg(not(test))]
        let written = crate::utils::slow_operations::write_file_sync_deprecated(
            Path::new(&filepath),
            &content,
            true,
        )
        .map_err(|error| error.to_string());
        // Maps to: CC handleFilenameSubmit :56-77 (write first, then onDone).
        pending_result.set(Some(match written {
            Ok(()) => ExportDialogResult {
                success: true,
                message: format!("Conversation exported to: {filepath}"),
            },
            Err(error) => ExportDialogResult {
                success: false,
                message: format!("Failed to export conversation: {error}"),
            },
        }));
    };
    // Maps to: CC `ExportDialog.tsx#renderInputGuide:103-130`. The filename
    // branch precedes pending exit state; Dialog only delivers that state.
    let render_input_guide = move |exit_state: crate::hooks::use_exit::ExitKeyState| {
        if show_filename_input.get() {
            element! {
            Byline {
                KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "save".to_string())
                ConfigurableShortcutHint(action: "confirm:no".to_string(), context: "Confirmation".to_string(),
                    fallback: "Esc".to_string(), description: "go back".to_string())
            }
        }.into_any()
        } else if exit_state.pending {
            element! {
                Text(content: format!("Press {} again to exit", exit_state.key_name.unwrap_or("")))
            }
            .into_any()
        } else {
            element! {
            ConfigurableShortcutHint(action: "confirm:no".to_string(), context: "Confirmation".to_string(),
                fallback: "Esc".to_string(), description: "cancel".to_string())
        }.into_any()
        }
    };
    element! {
        Dialog(
            title: "Export Conversation".to_string(),
            subtitle: Some("Select export method:".to_string()),
            color: Some(theme.permission),
            is_cancel_active: Some(!show_filename_input.get()),
            input_guide_renderer: Some(std::sync::Arc::new(render_input_guide) as crate::components::design_system::dialog::DialogInputGuide),
            on_cancel: move |_| { should_cancel.set(true); },
        ) {
            #(if show_filename_input.get() {
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "Enter filename:")
                        View(flex_direction: FlexDirection::Row, column_gap: 1u32, margin_top: 1u32) {
                            Text(content: ">")
                            TextInput(
                                value: Some(filename), cursor_offset: Some(cursor_offset),
                                focus: true, show_cursor: true, columns: columns,
                                escape_event_passthrough: true,
                                on_submit: handle_filename_submit,
                            )
                        }
                    }
                }.into_any()
            } else {
                element! {
                    ExportDialogSelect(
                        on_change: move |value: String| {
                            if value == "clipboard" {
                                handle_select_clipboard(());
                            } else if value == "file" {
                                selected_option.set(Some(ExportOption::File));
                                show_filename_input.set(true);
                            }
                        },
                        on_cancel: move |_| { should_cancel.set(true); },
                    )
                }.into_any()
            })
        }
    }
}

#[derive(Default, Props)]
struct ExportDialogSelectProps<'a> {
    on_change: HandlerMut<'a, String>,
    on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC ExportDialog.tsx:147-152 Select mount boundary.
/// L1 (inline Select state carrier, PORTING.md): Rust Select exposes canonical
/// hooks separately; keep them in this conditionally mounted child so returning
/// from TextInput recreates Select state exactly as the source does. No
/// navigation policy lives here.
#[component]
fn ExportDialogSelect<'a>(
    props: &mut ExportDialogSelectProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let options = export_options();
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
    if let Some(value) = events.take_accepted() {
        (props.on_change)(value);
    }
    if events.take_cancelled() {
        (props.on_cancel)(());
    }
    element! {
        Select(options: options, focused_index: state.navigation.snapshot().focused_index().unwrap_or(0),
            selected_value: state.committed_value(), visible_from_index: state.navigation.snapshot().visible_from_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{CLIPBOARD_OFF_SYSTEM, EnvVarGuard, in_child_process};
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn ctrl(character: char) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(character));
        event.modifiers = KeyModifiers::CONTROL;
        TerminalEvent::Key(event)
    }

    // Imported write effect is the only fixture replacement. Navigation,
    // focus, keybinding, TextInput, Dialog, output queue and viewport are real.
    // A caller that reaches "Copy to clipboard" runs in a child with SSH and
    // no tmux (`CLIPBOARD_OFF_SYSTEM`), or iocraft writes the real clipboard.
    fn run_dialog(
        events: Vec<TerminalEvent>,
        width: u16,
        filename: &str,
        write_error: Option<&str>,
        runtime: KeybindingRuntime,
    ) -> (Vec<String>, Vec<serde_json::Value>) {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let trace = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let writes = trace.clone();
        let write_error = write_error.map(str::to_owned);
        let fixture = ExportWriteFixture(Arc::new(move |path, content, flush| {
            writes.lock().unwrap().push(
                serde_json::json!(["write", path, content, {"encoding":"utf-8", "flush":flush}]),
            );
            match &write_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }));
        let done = trace.clone();
        let frames = futures::executor::block_on(async {
            let (sender, receiver) = async_channel::unbounded();
            let mut app = element! {
                ContextProvider(value: Context::owned(crate::state::store::AppStore::new(crate::state::app_state_store::AppState::default(), None))) {
                    ContextProvider(value: Context::owned(runtime)) {
                        ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                            ContextProvider(value: Context::owned(fixture)) {
                                FocusScope(handle_keys: false) {
                                    ExportDialog(content: "hello\n你好".to_string(), default_filename: filename.to_string(),
                                        cwd: Some("/oracle/cwd".to_string()),
                                        on_done: move |result: ExportDialogResult| {
                                            done.lock().unwrap().push(serde_json::json!(["done", {"success":result.success,"message":result.message}]));
                                        })
                                }
                            }
                        }
                    }
                }
            };
            let mut app = element! {
                ContextProvider(value: Context::owned(iocraft::Clipboard::new(std::sync::Arc::new(crate::utils::exec_file_no_throw::ExecFileClipboardBackend)))) {
                    #(app)
                }
            };
            let mut output = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(receiver)
                        .with_size(width, 20)
                        .with_ignore_ctrl_c(true),
                ),
            );
            let mut frames = Vec::new();
            let collect = async {
                while let Some(frame) = output.next().await {
                    frames.push(frame.to_string());
                }
            };
            let drive = async move {
                sender.send(TerminalEvent::FocusGained).await.unwrap();
                for event in events {
                    futures_timer::Delay::new(Duration::from_millis(35)).await;
                    sender.send(event).await.unwrap();
                }
                futures_timer::Delay::new(Duration::from_millis(120)).await;
            };
            crate::utils::race(collect, drive).await;
            frames
        });
        let events = trace.lock().unwrap().clone();
        (frames, events)
    }

    #[test]
    fn export_filename_and_path_match_official_bun_oracle() {
        // CC ExportDialog.tsx:63-67; actual full body oracle in export-0913.
        for (filename, expected) in [
            ("conversation.txt", "/oracle/cwd/conversation.txt"),
            ("conversation.md", "/oracle/cwd/conversation.txt"),
            ("conversation", "/oracle/cwd/conversation.txt"),
            ("name.", "/oracle/cwd/name..txt"),
            (".hidden", "/oracle/cwd/.txt"),
            ("a.b/file", "/oracle/cwd/a.txt"),
            ("../saved", "/oracle/cwd/..txt"),
            ("/absolute.md", "/oracle/cwd/absolute.txt"),
            ("", "/oracle/cwd/.txt"),
            ("a..md", "/oracle/cwd/a..txt"),
            ("😀.TXT", "/oracle/cwd/😀.txt"),
        ] {
            assert_eq!(export_filepath("/oracle/cwd", filename), expected);
        }
    }

    #[test]
    fn export_file_effect_order_matches_official_submit_and_n_input() {
        // CC :132-136 Settings context keeps 'n' in filename; :62-76 write then callback.
        let (frames, trace) = run_dialog(
            vec![
                key(KeyCode::Char('2')),
                key(KeyCode::Char('n')),
                key(KeyCode::Enter),
            ],
            80,
            "conversation.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        assert!(frames.iter().any(|frame| frame.contains("Enter filename:")));
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("Enter to save · Esc to go back"))
        );
        assert_eq!(
            trace,
            vec![
                serde_json::json!(["write","/oracle/cwd/conversation.txt","hello\n你好",{"encoding":"utf-8","flush":true}]),
                serde_json::json!(["done",{"success":true,"message":"Conversation exported to: /oracle/cwd/conversation.txt"}]),
            ]
        );
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("conversation.mdn")),
            "frames={frames:?}"
        );
    }

    #[test]
    fn export_file_failure_matches_official_error_result() {
        // CC :71-76 — a caught Error produces failure without success callback.
        let error = "EACCES: permission denied, open '/oracle/cwd/conversation.txt'";
        let (_, trace) = run_dialog(
            vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Enter)],
            80,
            "conversation.md",
            Some(error),
            KeybindingRuntime::with_default_bindings(),
        );
        assert_eq!(trace.len(), 2);
        assert_eq!(
            trace[1],
            serde_json::json!(["done",{"success":false,"message":format!("Failed to export conversation: {error}")}])
        );
    }

    #[test]
    fn export_back_matches_official_filename_cursor_and_select_remount() {
        if !in_child_process(
            module_path!(),
            "export_back_matches_official_filename_cursor_and_select_remount",
            CLIPBOARD_OFF_SYSTEM,
        ) {
            return;
        }
        // CC :39-42/147-167 — back retains parent filename/cursor, remounts Select.
        let (frames, trace) = run_dialog(
            vec![
                key(KeyCode::Char('2')),
                key(KeyCode::Left),
                key(KeyCode::Char('n')),
                key(KeyCode::Esc),
                key(KeyCode::Char('2')),
                key(KeyCode::Char('x')),
                key(KeyCode::Enter),
            ],
            80,
            "name.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        assert!(
            frames.iter().any(|frame| frame.contains("name.mnxd")),
            "frames={frames:?}"
        );
        assert_eq!(trace[0][1], "/oracle/cwd/name.txt");
        let (_, trace) = run_dialog(
            vec![
                key(KeyCode::Char('2')),
                key(KeyCode::Esc),
                key(KeyCode::Enter),
            ],
            80,
            "name.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        assert_eq!(
            trace,
            vec![
                serde_json::json!(["done",{"success":true,"message":"Conversation copied to clipboard"}])
            ]
        );
    }

    #[test]
    fn export_cancel_and_wrap_match_official_select_runtime() {
        // Select uses wrap navigation and numeric selection; Dialog also accepts n.
        for cancel in [KeyCode::Esc, KeyCode::Char('n')] {
            let (_, trace) = run_dialog(
                vec![key(cancel)],
                45,
                "name.md",
                None,
                KeybindingRuntime::with_default_bindings(),
            );
            assert_eq!(
                trace,
                vec![serde_json::json!(["done",{"success":false,"message":"Export cancelled"}])]
            );
        }
        let (frames, trace) = run_dialog(
            vec![
                key(KeyCode::Up),
                key(KeyCode::Enter),
                key(KeyCode::Esc),
                key(KeyCode::Esc),
            ],
            45,
            "name.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        assert!(frames.iter().any(|frame| frame.contains("Enter filename:")));
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0][1]["message"], "Export cancelled");
    }

    #[test]
    fn export_filename_ctrl_c_and_narrow_viewport_match_official_input() {
        // CC :145 isCancelActive false lets TextInput own ctrl+c/d; :164 columns.
        let (frames, trace) = run_dialog(
            vec![
                key(KeyCode::Char('2')),
                ctrl('c'),
                key(KeyCode::Char('n')),
                key(KeyCode::Enter),
            ],
            37,
            "long-unicode-😀-filename.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        assert!(
            frames
                .iter()
                .all(|frame| !frame.contains("Press Ctrl-C again to exit")),
            "frames={frames:?}"
        );
        assert_eq!(trace[0][1], "/oracle/cwd/n.txt");
        assert_eq!(trace[1][1]["success"], true);
    }
    #[test]
    fn export_rebound_cancel_and_hint_match_official_contexts() {
        // CC :108-112 uses Confirmation for the displayed hint, while
        // :149-153 deliberately resolves filename cancellation in Settings.
        let mut bindings = crate::keybindings::default_bindings::default_bindings();
        for context in [ContextName::Settings, ContextName::Confirmation] {
            bindings.push(crate::keybindings::types::ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("ctrl+b"),
                action: Some("confirm:no".to_string()),
                context,
            });
        }
        let (frames, trace) = run_dialog(
            vec![key(KeyCode::Char('2')), ctrl('b'), ctrl('b')],
            80,
            "name.md",
            None,
            KeybindingRuntime::new(bindings),
        );
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("ctrl+b to go back")),
            "frames={frames:?}"
        );
        assert_eq!(
            trace,
            vec![serde_json::json!(["done",{"success":false,"message":"Export cancelled"}])]
        );
    }

    #[test]
    fn export_tab_and_exit_pending_match_official_select_and_dialog() {
        if !in_child_process(
            module_path!(),
            "export_tab_and_exit_pending_match_official_select_and_dialog",
            CLIPBOARD_OFF_SYSTEM,
        ) {
            return;
        }
        // CC Select has no onInputModeToggle here: Tab must not advance.
        let (_, trace) = run_dialog(
            vec![key(KeyCode::Tab), key(KeyCode::Enter)],
            80,
            "name.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        assert_eq!(
            trace,
            vec![
                serde_json::json!(["done",{"success":true,"message":"Conversation copied to clipboard"}])
            ]
        );
        // CC renderInputGuide :124-126 uses the Dialog pending exit message.
        let (frames, trace) = run_dialog(
            vec![ctrl('c')],
            80,
            "name.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("Press Ctrl-C again to exit")),
            "frames={frames:?}"
        );
        assert!(trace.is_empty());
    }

    #[test]
    fn export_input_guide_matches_official_filename_priority_during_pending_exit() {
        // CC ExportDialog.tsx:103-120: entering the filename screen within
        // the same 800ms pending-exit window must show save/go-back guidance.
        // Returning to options preserves that pending state, rather than
        // clearing it as a workaround for the guide's branch priority.
        let (frames, trace) = run_dialog(
            vec![
                ctrl('c'),
                key(KeyCode::Down),
                key(KeyCode::Enter),
                key(KeyCode::Esc),
            ],
            80,
            "name.md",
            None,
            KeybindingRuntime::with_default_bindings(),
        );
        let filename_frames = frames
            .iter()
            .filter(|frame| frame.contains("Enter filename:"))
            .collect::<Vec<_>>();
        assert!(!filename_frames.is_empty(), "frames={frames:?}");
        for frame in filename_frames {
            assert!(frame.contains("Enter to save"), "frame={frame:?}");
            assert!(frame.contains("Esc to go back"), "frame={frame:?}");
            assert!(
                !frame.contains("Press Ctrl-C again to exit"),
                "frame={frame:?}"
            );
        }
        let last = frames.last().expect("dialog rendered");
        assert!(!last.contains("Enter filename:"), "frames={frames:?}");
        assert!(
            last.contains("Press Ctrl-C again to exit"),
            "frames={frames:?}"
        );
        assert!(trace.is_empty(), "trace={trace:?}");
    }

    #[derive(Default, Props)]
    struct UnmountAfterExportProps {
        done: Arc<Mutex<Vec<ExportDialogResult>>>,
    }
    #[component]
    fn UnmountAfterExport(
        props: &UnmountAfterExportProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut mounted = hooks.use_state(|| true);
        let (command_stdout, _) = hooks.use_output();
        let done = props.done.clone();
        if !mounted.get() {
            return element! { Text(content: "export-parent-after-done") }.into_any();
        }
        element! {
            ExportDialog(command_stdout: Some(command_stdout), content: "hello\n你好".to_string(), default_filename: "fixture.md".to_string(),
                on_done: move |result: ExportDialogResult| { done.lock().unwrap().push(result); let mut mounted = mounted; mounted.set(false); })
        }.into_any()
    }
    /// CC :47-49,84-90: Esc settles the parent first; native/tmux work and raw
    /// still complete after child removal, including the captured late callback.
    /// MockTerminal discards raw bytes; real PTY capture verifies that transport.
    #[cfg(unix)]
    #[test]
    fn export_clipboard_matches_official_parent_unmount_after_feedback() {
        use std::os::unix::fs::PermissionsExt;
        // iocraft reads the session's terminal from the OS environment.
        if !in_child_process(
            module_path!(),
            "export_clipboard_matches_official_parent_unmount_after_feedback",
            &[
                ("SSH_CONNECTION", Some("fixture")),
                ("TMUX", Some("fixture")),
            ],
        ) {
            return;
        }
        // The actual tmux subprocess blocks until after Esc unmounts the panel.
        let directory =
            std::env::temp_dir().join(format!("cometix-export-clipboard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let tool = directory.join("tmux");
        std::fs::write(&tool, "#!/bin/sh\n/bin/cat > \"$CLIPBOARD_FIXTURE_DIR/input\"\n/bin/sleep 0.25\n: > \"$CLIPBOARD_FIXTURE_DIR/finished\"\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _path = EnvVarGuard::set("PATH", &directory);
        let _fixture = EnvVarGuard::set("CLIPBOARD_FIXTURE_DIR", &directory);
        crate::utils::process_runtime::initialize_test_process_runtime();
        let done = Arc::new(Mutex::new(Vec::new()));
        let done_for_app = done.clone();
        let frames = futures::executor::block_on(async {
            let (sender, receiver) = async_channel::unbounded();
            let mut app = element! {
                ContextProvider(value: Context::owned(crate::state::store::AppStore::new(crate::state::app_state_store::AppState::default(), None))) {
                    ContextProvider(value: Context::owned(KeybindingRuntime::with_default_bindings())) {
                        ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                            FocusScope(handle_keys: false) { UnmountAfterExport(done: done_for_app) }
                        }
                    }
                }
            };
            let mut app = element! {
                ContextProvider(value: Context::owned(iocraft::Clipboard::new(std::sync::Arc::new(crate::utils::exec_file_no_throw::ExecFileClipboardBackend)))) {
                    #(app)
                }
            };
            let mut output = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(receiver).with_size(80, 20),
            ));
            let mut frames = Vec::new();
            let collect = async {
                while let Some(frame) = output.next().await {
                    frames.push(frame.to_string());
                }
            };
            let drive = async move {
                sender.send(TerminalEvent::FocusGained).await.unwrap();
                futures_timer::Delay::new(Duration::from_millis(35)).await;
                sender.send(key(KeyCode::Enter)).await.unwrap();
                futures_timer::Delay::new(Duration::from_millis(60)).await;
                sender.send(key(KeyCode::Esc)).await.unwrap();
                futures_timer::Delay::new(Duration::from_millis(450)).await;
            };
            crate::utils::race(collect, drive).await;
            frames
        });
        // This is the component's arbitrary owned callback, before the outer
        // local-jsx Promise gate: source calls late success even after unmount.
        assert_eq!(
            *done.lock().unwrap(),
            vec![export_cancelled_result(), export_clipboard_result()]
        );
        assert!(directory.join("finished").exists());
        assert_eq!(
            std::fs::read_to_string(directory.join("input")).unwrap(),
            "hello\n你好"
        );
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("Export Conversation"))
        );
        let final_frame = frames.last().unwrap();
        assert!(
            final_frame.contains("export-parent-after-done"),
            "{frames:?}"
        );
        assert!(!final_frame.contains("Export Conversation"));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
