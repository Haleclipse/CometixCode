//! Maps to: CC `components/agents/new-agent-creation/wizard-steps/ConfirmStepWrapper.tsx:1-112`.
//!
//! Telemetry is omitted by policy. The wrapper persists the file, delegates
//! runtime list refresh to its owner, and uses iocraft's raw-mode-safe external
//! editor handoff when requested.

use super::confirm_step::ConfirmStep;
use crate::components::agents::agent_file_utils::{
    AgentFileSource, get_new_agent_file_path, save_agent_to_file,
};
use crate::components::agents::new_agent_creation::types::{AgentWizardFinal, final_agent};
use crate::components::agents::tool_selector::AgentToolOption;
use crate::components::wizard::use_wizard;
use crate::tools::agent_tool::agent_memory::AgentMemoryScope;
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::utils::prompt_editor::{EditorResult, ExternalEditorRuntime};
use iocraft::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

fn source(value: &str) -> AgentFileSource {
    match value {
        "userSettings" => AgentFileSource::UserSettings,
        "policySettings" => AgentFileSource::PolicySettings,
        "localSettings" => AgentFileSource::LocalSettings,
        _ => AgentFileSource::ProjectSettings,
    }
}

fn memory(value: Option<&str>) -> Option<AgentMemoryScope> {
    match value {
        Some("user") => Some(AgentMemoryScope::User),
        Some("project") => Some(AgentMemoryScope::Project),
        Some("local") => Some(AgentMemoryScope::Local),
        _ => None,
    }
}

fn save(agent: &AgentWizardFinal) -> Result<(), String> {
    save_agent_to_file(
        source(&agent.source),
        &agent.agent_type,
        &agent.when_to_use,
        agent.tools.as_deref(),
        &agent.system_prompt,
        true,
        agent.color.as_deref(),
        agent.model.as_deref(),
        memory(agent.memory.as_deref()),
        None,
        &crate::bootstrap::state::get_original_cwd(),
        &crate::utils::env_utils::get_claude_config_home_dir(),
    )
    .map(|_| ())
}

#[derive(Default, Props)]
pub struct ConfirmStepWrapperProps<'a> {
    pub tools: Vec<AgentToolOption>,
    pub existing_agents: Vec<AgentDefinition>,
    pub on_complete: HandlerMut<'a, String>,
}

#[component]
pub fn ConfirmStepWrapper<'a>(
    props: &mut ConfirmStepWrapperProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let wizard = use_wizard(&mut hooks);
    let agent = final_agent(&wizard.wizard_data);
    let mut save_error = hooks.use_state(|| None::<String>);
    let mut pending = hooks.use_state(|| None::<bool>);
    let mut editor_result = hooks.use_state(|| None::<(String, EditorResult)>);
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let editor_channel =
        hooks.use_const(|| Arc::new(async_channel::unbounded::<(String, PathBuf)>()));
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok((agent_type, path)) = editor_receiver.recv().await {
            let result = match editor_runtime {
                Some(runtime) => runtime.edit_file(&path).await,
                None => EditorResult {
                    content: None,
                    error: Some("External editor is unavailable".to_string()),
                },
            };
            editor_result.set(Some((agent_type, result)));
        }
    });
    let completed_editor = { editor_result.read().clone() };
    if let Some((agent_type, _result)) = completed_editor {
        editor_result.set(None);
        (props.on_complete)(format!(
            "Created agent: {agent_type} and opened in editor. If you made edits, restart to load the latest version."
        ));
    }
    let action = { *pending.read() };
    if let Some(open_editor) = action {
        pending.set(None);
        if let Some(agent) = &agent {
            match save(agent) {
                Ok(()) => {
                    if open_editor {
                        match get_new_agent_file_path(
                            source(&agent.source),
                            &agent.agent_type,
                            &crate::bootstrap::state::get_original_cwd(),
                            &crate::utils::env_utils::get_claude_config_home_dir(),
                        ) {
                            Ok(path) => {
                                let _ = editor_channel.0.try_send((agent.agent_type.clone(), path));
                            }
                            Err(error) => save_error.set(Some(error)),
                        }
                    } else {
                        (props.on_complete)(format!("Created agent: {}", agent.agent_type));
                    }
                }
                Err(error) => save_error.set(Some(error)),
            }
        }
    }
    let mut save_pending = pending;
    let mut edit_pending = pending;
    element! {
        ConfirmStep(
            tools: props.tools.clone(), existing_agents: props.existing_agents.clone(), error: save_error.read().clone(),
            on_save: move |_| save_pending.set(Some(false)),
            on_save_and_edit: move |_| edit_pending.set(Some(true)),
        )
    }
}
