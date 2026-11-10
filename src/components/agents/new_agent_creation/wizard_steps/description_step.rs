//! Maps to: CC `components/agents/new-agent-creation/wizard-steps/DescriptionStep.tsx:1-94`.
//! External editing uses iocraft's raw-mode-safe child-process handoff.

use crate::components::agents::new_agent_creation::types::{string, update};
use crate::components::text_input::TextInput;
use crate::components::wizard::{WizardDialogLayout, use_wizard};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::prompt_editor::{EditorResult, ExternalEditorRuntime};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use serde_json::json;
use std::sync::Arc;

#[component]
pub fn DescriptionStep(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let wizard = use_wizard(&mut hooks);
    let initial = string(&wizard.wizard_data, "whenToUse").unwrap_or_default();
    let initial_len = initial.len();
    let mut value = hooks.use_state(move || initial);
    let mut cursor = hooks.use_state(move || initial_len);
    let mut error = hooks.use_state(|| None::<String>);
    let mut submit = hooks.use_state(|| None::<String>);
    let mut editor_result = hooks.use_state(|| None::<EditorResult>);
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let editor_channel = hooks.use_const(|| Arc::new(async_channel::unbounded::<String>()));
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(content) = editor_receiver.recv().await {
            let result = match editor_runtime {
                Some(runtime) => runtime.edit_prompt(&content).await,
                None => EditorResult {
                    content: None,
                    error: Some("External editor is unavailable".to_string()),
                },
            };
            editor_result.set(Some(result));
        }
    });
    let completed_editor = { editor_result.read().clone() };
    if let Some(result) = completed_editor {
        editor_result.set(None);
        if let Some(content) = result.content {
            cursor.set(content.len());
            value.set(content);
        }
        if let Some(message) = result.error {
            error.set(Some(message));
        }
    }
    let submitted = { submit.read().clone() };
    if let Some(submitted) = submitted {
        submit.set(None);
        let trimmed = submitted.trim().to_string();
        if trimmed.is_empty() {
            error.set(Some("Description is required".to_string()));
        } else {
            error.set(None);
            wizard.update_wizard_data(update("whenToUse", json!(trimmed)));
            wizard.go_next();
        }
    }
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|value| value.clone());
    let back = wizard.clone();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Settings,
        || true,
        move || {
            back.go_back();
            true
        },
    );
    let mut value_for_editor = value;
    let editor_sender = editor_channel.0.clone();
    use_keybinding(
        &mut hooks,
        runtime,
        "chat:externalEditor",
        ContextName::Chat,
        || true,
        move || {
            let _ = editor_sender.try_send(value_for_editor.read().clone());
            true
        },
    );
    let theme = hooks.use_context::<Theme>();
    let mut submit_handler = submit;
    element! {
        WizardDialogLayout(
            subtitle: Some("Description (tell nyan when to use this agent)".to_string()),
            footer_text: Some("Type to enter text · Enter to continue · Ctrl+G to open in editor · Esc to go back".to_string()),
        ) {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "When should nyan use this agent?".to_string())
                View(margin_top: 1u32) {
                    TextInput(
                        value: value, cursor_offset: cursor, focus: true, show_cursor: true, columns: 80usize,
                        placeholder: Some("e.g., use this agent after you're done writing code...".to_string()),
                        on_submit: move |text| submit_handler.set(Some(text)),
                    )
                }
                #(error.read().clone().map(|message| element! { View(margin_top: 1u32) { Text(content: message, color: theme.error) } }))
            }
        }
    }
}
