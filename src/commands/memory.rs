//! `/memory` local command UI.
//! Maps to: CC `commands/memory/memory.tsx:1-102`.
//!
//! The official command creates the selected file and hands terminal ownership
//! to the configured external editor. The local iocraft fork performs the same
//! raw-mode/input suspension and full repaint around this explicit user action.
//! File/folder selection and toggle behavior remain in MemoryFileSelector.

use crate::components::design_system::dialog::Dialog;
use crate::components::memory::{
    ADD_CANDIDATE_PREFIX, CONSOLIDATE_MEMORY_VALUE, DELETE_CANDIDATE_PREFIX, MemoryFileSelector,
    get_relative_memory_path,
};
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
    // @nyan: manual consolidation results come back on their own channel; the
    // fork runs detached on the process runtime, not in the render loop.
    let consolidate_channel =
        hooks.use_const(|| Arc::new(async_channel::unbounded::<Result<String, String>>()));
    let consolidate_receiver = consolidate_channel.1.clone();
    let app_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    hooks.use_future(async move {
        while let Ok(result) = consolidate_receiver.recv().await {
            completed_selection.set(Some(result));
        }
    });
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
                } else { crate::utils::process_env::var("EDITOR").truthy().map(|value| format!("Using $EDITOR=\"{value}\".")) };
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
    if let Some(selection) = selection {
        pending_selection.set(None);
        if let Some(path) = selection.strip_prefix(ADD_CANDIDATE_PREFIX) {
            let result =
                crate::memdir::memdir::add_memory_to_index(&PathBuf::from(path)).map(|_| {
                    format!(
                        "Added memory candidate to MEMORY.md: {}",
                        get_relative_memory_path(&PathBuf::from(path))
                    )
                });
            completed_selection.set(Some(result));
        } else if let Some(path) = selection.strip_prefix(DELETE_CANDIDATE_PREFIX) {
            let result =
                crate::memdir::memdir::delete_memory_candidate(&PathBuf::from(path)).map(|_| {
                    format!(
                        "Deleted memory candidate: {}",
                        get_relative_memory_path(&PathBuf::from(path))
                    )
                });
            completed_selection.set(Some(result));
        } else if selection == CONSOLIDATE_MEMORY_VALUE {
            let sender = consolidate_channel.0.clone();
            match crate::utils::process_runtime::runtime_handle_for_detached_work() {
                Some(handle) => {
                    let store = app_store.clone();
                    handle.spawn(async move {
                        let result =
                            crate::services::auto_dream::run_manual_consolidation(store.clone())
                                .await;
                        // The panel closes right after "started", so the real
                        // outcome has to surface as a notification (success
                        // and every named failure alike).
                        if let Some(store) = store {
                            use crate::context::notifications::{
                                Notification, NotificationColor, NotificationPriority,
                                NotificationsWriter,
                            };
                            let (text, color) = match &result {
                                Ok(text) => (text.clone(), NotificationColor::Success),
                                Err(error) => (error.clone(), NotificationColor::Error),
                            };
                            NotificationsWriter::new(store).add_notification(
                                Notification::text(
                                    "memory-consolidate-result",
                                    text,
                                    NotificationPriority::High,
                                )
                                .with_color(color)
                                .with_timeout_ms(30_000),
                            );
                        }
                        let _ = sender.send(result).await;
                    });
                    completed_selection.set(Some(Ok(
                        "Memory consolidation started; progress shows in the background task list."
                            .to_string(),
                    )));
                }
                None => completed_selection.set(Some(Err(
                    "Memory consolidation cannot start: no process runtime is available."
                        .to_string(),
                ))),
            }
        } else {
            let _ = editor_channel.0.try_send(PathBuf::from(selection));
        }
    }
    let completed = { completed_selection.read().clone() };
    if let Some(result) = completed {
        completed_selection.set(None);
        match result {
            Ok(message) => (props.on_result)(message),
            Err(error) => (props.on_result)(format!("Memory action failed: {error}")),
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
