//! Maps to: CC `components/agents/AgentEditor.tsx:1-229`.
//!
//! Inline tools/model/color editing and persistence are ported. External files
//! use iocraft's raw-mode-safe child-process handoff; owner state refresh is
//! callback-driven.

use super::agent_file_utils::{get_actual_agent_file_path, update_agent_file};
use super::color_picker::ColorPicker;
use super::model_selector::ModelSelector;
use super::new_agent_creation::wizard_steps::choice::WizardChoice;
use super::tool_selector::{AgentToolOption, ToolSelector};
use super::utils::get_agent_source_display_name;
use crate::components::custom_select::SelectOptionData;
use crate::tools::agent_tool::agent_color_manager::{AgentColorName, parse_agent_color_name};
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::utils::prompt_editor::{EditorResult, ExternalEditorRuntime};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EditMode {
    #[default]
    Menu,
    Tools,
    Color,
    Model,
}

#[derive(Clone, Debug)]
enum SaveChange {
    Tools(Option<Vec<String>>),
    Color(Option<AgentColorName>),
    Model(Option<String>),
}

#[derive(Default, Props)]
pub struct AgentEditorProps<'a> {
    pub agent: Option<AgentDefinition>,
    pub tools: Vec<AgentToolOption>,
    pub on_saved: HandlerMut<'a, String>,
    pub on_back: HandlerMut<'a, ()>,
}

#[component]
pub fn AgentEditor<'a>(
    props: &mut AgentEditorProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(agent) = props.agent.clone() else {
        return element! { Fragment }.into_any();
    };
    let mut mode = hooks.use_state(EditMode::default);
    let mut error = hooks.use_state(|| None::<String>);
    let mut pending_menu = hooks.use_state(|| None::<String>);
    let mut pending_save = hooks.use_state(|| None::<SaveChange>);
    let mut editor_result = hooks.use_state(|| None::<EditorResult>);
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let editor_channel = hooks.use_const(|| Arc::new(async_channel::unbounded::<PathBuf>()));
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(path) = editor_receiver.recv().await {
            let result = match editor_runtime {
                Some(runtime) => runtime.edit_file(&path).await,
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
        if let Some(message) = result.error {
            error.set(Some(message));
        } else if result.content.is_some() {
            error.set(None);
            (props.on_saved)(format!(
                "Opened {} in editor. If you made edits, restart to load the latest version.",
                agent.agent_type
            ));
        }
    }
    let menu_action = { pending_menu.read().clone() };
    if let Some(action) = menu_action {
        pending_menu.set(None);
        error.set(None);
        match action.as_str() {
            "open" => match get_actual_agent_file_path(
                &agent,
                &crate::bootstrap::state::get_original_cwd(),
                &crate::utils::env_utils::get_claude_config_home_dir(),
            ) {
                Ok(path) => {
                    let _ = editor_channel.0.try_send(path);
                }
                Err(message) => error.set(Some(message)),
            },
            "tools" => mode.set(EditMode::Tools),
            "model" => mode.set(EditMode::Model),
            "color" => mode.set(EditMode::Color),
            _ => {}
        }
    }
    let save = { pending_save.read().clone() };
    if let Some(change) = save {
        pending_save.set(None);
        let (tools, color, model) = match change {
            SaveChange::Tools(tools) => (tools, agent.color.clone(), agent.model.clone()),
            SaveChange::Color(color) => (
                agent.tools.clone(),
                color.map(|value| value.official_name().to_string()),
                agent.model.clone(),
            ),
            SaveChange::Model(model) => (agent.tools.clone(), agent.color.clone(), model),
        };
        let result = update_agent_file(
            &agent,
            &agent.when_to_use,
            tools.as_deref(),
            agent.system_prompt.as_deref().unwrap_or_default(),
            color.as_deref(),
            model.as_deref(),
            agent.memory,
            agent.effort.as_ref(),
            &crate::bootstrap::state::get_original_cwd(),
            &crate::utils::env_utils::get_claude_config_home_dir(),
        );
        mode.set(EditMode::Menu);
        match result {
            Ok(_) => {
                error.set(None);
                (props.on_saved)(format!("Updated agent: {}", agent.agent_type));
            }
            Err(message) => error.set(Some(message)),
        }
    }
    let theme = hooks.use_context::<Theme>();
    // Declared before the match: iocraft resolves hooks by call index, so
    // creating this only in the `Menu` arm shifts every later hook as soon as
    // the mode changes, and the next render panics with "Unexpected hook
    // type!". The state is only consulted inside that arm.
    let mut back = hooks.use_state(|| false);
    match mode.get() {
        EditMode::Menu => {
            let mut select = pending_menu;
            if back.get() {
                back.set(false);
                (props.on_back)(());
            }
            return element! { View(flex_direction: FlexDirection::Column) {
                Text(content: format!("Source: {}", get_agent_source_display_name(Some(agent.source))), dim: true)
                View(margin_top: 1u32) {
                    WizardChoice(
                        options: vec![
                            SelectOptionData { label: "Open in editor".to_string(), value: "open".to_string(), ..Default::default() },
                            SelectOptionData { label: "Edit tools".to_string(), value: "tools".to_string(), ..Default::default() },
                            SelectOptionData { label: "Edit model".to_string(), value: "model".to_string(), ..Default::default() },
                            SelectOptionData { label: "Edit color".to_string(), value: "color".to_string(), ..Default::default() },
                        ],
                        on_select: move |value| select.set(Some(value)),
                        on_cancel: move |_| back.set(true),
                    )
                }
                #(error.read().clone().map(|message| element! { View(margin_top: 1u32) { Text(content: message, color: theme.error) } }))
            }}.into_any();
        }
        EditMode::Tools => {
            let mut save = pending_save;
            let mut cancel = mode;
            element! { ToolSelector(
                tools: props.tools.clone(), initial_tools: agent.tools.clone(),
                on_complete: move |tools| save.set(Some(SaveChange::Tools(tools))),
                on_cancel: move |_| cancel.set(EditMode::Menu),
            ) }
            .into_any()
        }
        EditMode::Color => {
            let mut save = pending_save;
            element! { ColorPicker(
                agent_name: agent.agent_type.clone(), current_color: agent.color.as_deref().and_then(parse_agent_color_name),
                on_confirm: move |color| save.set(Some(SaveChange::Color(color))),
            ) }.into_any()
        }
        EditMode::Model => {
            let mut save = pending_save;
            let mut cancel = mode;
            element! { ModelSelector(
                initial_model: agent.model.clone(),
                on_complete: move |model| save.set(Some(SaveChange::Model(model))),
                on_cancel: move |_| cancel.set(EditMode::Menu),
            ) }
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    fn agent() -> AgentDefinition {
        let mut agent = AgentDefinition::new(
            "reviewer",
            "Use for code review",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.system_prompt =
            Some("You are a careful reviewer with a comprehensive workflow.".to_string());
        agent
    }

    #[test]
    fn menu_preserves_source_and_four_official_actions() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentEditor(agent: Some(agent()))
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains("Source: Project"));
        for label in ["Open in editor", "Edit tools", "Edit model", "Edit color"] {
            assert!(text.contains(label));
        }
    }

    #[test]
    fn open_action_requires_an_injected_terminal_handoff_without_spawning_in_test() {
        let events = stream::iter(vec![KeyCode::Enter])
            .then(|code| async move {
                futures_timer::Delay::new(Duration::from_millis(40)).await;
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
            })
            .chain(stream::pending());
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        AgentEditor(agent: Some(agent()))
                    }
                }
            };
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            let mut shown = false;
            for _ in 0..20 {
                if let Some(frame) = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await
                {
                    if frame.to_string().contains("External editor is unavailable") {
                        shown = true;
                        break;
                    }
                }
            }
            assert!(shown);
        });
    }
}
