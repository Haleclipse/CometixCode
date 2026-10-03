//! `/memory` local command UI.
//! Maps to: CC `commands/memory/memory.tsx:1-102`.
//!
//! The official command creates the selected file and hands terminal ownership
//! to the configured external editor. The local iocraft fork performs the same
//! raw-mode/input suspension and full repaint around this explicit user action.
//! File/folder selection and toggle behavior remain in MemoryFileSelector.

use crate::components::design_system::dialog::Dialog;
use crate::components::memory::{MemoryFileSelector, get_relative_memory_path};
use crate::utils::process_env::JsTruthy;
use crate::utils::prompt_editor::{EditorResult, ExternalEditorRuntime};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default, Props)]
pub struct MemoryCommandPanelProps<'a> {
    pub on_close: HandlerMut<'a, ()>,
    pub on_result: HandlerMut<'a, String>,
}

/// Maps to the official private `MemoryCommand` component.
#[component]
pub fn MemoryCommandPanel<'a>(
    props: &mut MemoryCommandPanelProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut pending_close = hooks.use_state(|| false);
    let mut pending_selection = hooks.use_state(|| Option::<String>::None);
    let mut completed_selection = hooks.use_state(|| Option::<Result<String, String>>::None);
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let editor_channel = hooks.use_const(|| Arc::new(async_channel::unbounded::<PathBuf>()));
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(memory_path) = editor_receiver.recv().await {
            let result = async {
                let config_home = crate::utils::env_utils::get_claude_config_home_dir();
                if memory_path.starts_with(&config_home) {
                    std::fs::create_dir_all(&config_home).map_err(|error| error.to_string())?;
                }
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&memory_path)
                {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
                let editor_result = match editor_runtime {
                    Some(runtime) => runtime.edit_file(&memory_path).await,
                    None => EditorResult {
                        content: None,
                        error: Some("External editor is unavailable".to_string()),
                    },
                };
                // CC ignores EditorResult.error here and still reports which
                // file was selected; only mkdir/create failures reject.
                let _ = editor_result;
                let editor_info = if let Some(value) =
                    crate::utils::process_env::var("VISUAL").truthy()
                {
                    Some(format!("Using $VISUAL=\"{value}\"."))
                } else if let Some(value) = crate::utils::process_env::var("EDITOR").truthy() {
                    Some(format!("Using $EDITOR=\"{value}\"."))
                } else {
                    None
                };
                let hint = editor_info.map_or_else(
                    || "> To use a different editor, set the $EDITOR or $VISUAL environment variable.".to_string(),
                    |info| format!("> {info} To change editor, set $EDITOR or $VISUAL environment variable."),
                );
                Ok(format!(
                    "Opened memory file at {}\n\n{hint}",
                    get_relative_memory_path(&memory_path)
                ))
            }
            .await;
            completed_selection.set(Some(result));
        }
    });

    if pending_close.get() {
        pending_close.set(false);
        (props.on_close)(());
    }
    let selection = { pending_selection.read().clone() };
    if let Some(memory_path) = selection {
        pending_selection.set(None);
        let _ = editor_channel.0.try_send(PathBuf::from(memory_path));
    }
    let completed = { completed_selection.read().clone() };
    if let Some(result) = completed {
        completed_selection.set(None);
        match result {
            Ok(message) => (props.on_result)(message),
            Err(error) => (props.on_result)(format!("Error opening memory file: {error}")),
        }
    }

    let mut pending_close_for_dialog = pending_close;
    let mut pending_close_for_selector = pending_close;
    let mut pending_selection_for_selector = pending_selection;
    element! {
        Dialog(
            title: "Memory".to_string(),
            color: Some(theme.remember),
            on_cancel: move |_| pending_close_for_dialog.set(true),
        ) {
            View(flex_direction: FlexDirection::Column) {
                MemoryFileSelector(
                    on_select: move |path| pending_selection_for_selector.set(Some(path)),
                    on_cancel: move |_| pending_close_for_selector.set(true),
                )
                View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                    Text(content: "Learn more: ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    Link(url: "https://code.claude.com/docs/en/memory".to_string())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::SettingsJson;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use crate::utils::theme;

    #[test]
    fn memory_command_matches_official_dialog_selector_and_docs_boundaries() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::memdir::paths::seed_settings(
            SettingsJson {
                auto_memory_enabled: Some(false),
                ..SettingsJson::default()
            },
            None,
        );
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                MemoryCommandPanel
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Memory"), "canvas=\n{text}");
        assert!(text.contains("Auto-memory: off"), "canvas=\n{text}");
        assert!(text.contains("User memory"), "canvas=\n{text}");
        assert!(
            text.contains("Learn more: https://code.claude.com/docs/en/memory"),
            "canvas=\n{text}"
        );
    }
}
