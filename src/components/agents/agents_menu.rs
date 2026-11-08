//! Maps to: CC `components/agents/AgentsMenu.tsx:1-302`.

use super::agent_detail::AgentDetail;
use super::agent_file_utils::{delete_agent_from_file, get_actual_agent_file_path, is_editable};
use super::agent_navigation_footer::AgentNavigationFooter;
use super::agents_list::{AgentsList, AgentsListSource, ResolvedAgent, resolve_agent_overrides};
use super::new_agent_creation::CreateAgentWizard;
use super::new_agent_creation::wizard_steps::choice::WizardChoice;
use super::tool_selector::AgentToolOption;
use crate::components::custom_select::SelectOptionData;
use crate::components::design_system::dialog::Dialog;
use crate::tools::agent_tool::load_agents_dir::{
    AgentDefinition, FailedAgentFile, get_active_agents_from_list,
    get_agent_definitions_with_overrides,
};
use crate::utils::prompt_editor::{EditorResult, ExternalEditorRuntime};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
enum MenuMode {
    List,
    Create,
    Agent(AgentDefinition),
    View(AgentDefinition),
    Delete(AgentDefinition),
}

/// The file "Open in editor" hands to the external editor, or the message
/// to show instead. A missing file is reported here because the editor
/// runtime returns an empty result for it, indistinguishable from having no
/// editor configured.
fn editor_target(agent: &AgentDefinition) -> Result<PathBuf, String> {
    let path = get_actual_agent_file_path(agent)?;
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("File not found: {}", path.display()))
    }
}

/// The per-agent menu: View, then Open in editor and Delete for editable
/// sources, then Back. @nyan: editing happens only in the external editor —
/// the agent file is the single source of truth.
fn agent_menu_options(agent: &AgentDefinition) -> Vec<SelectOptionData> {
    let option = |label: &str, value: &str| SelectOptionData {
        label: label.to_string(),
        value: value.to_string(),
        ..Default::default()
    };
    let mut options = vec![option("View agent", "view")];
    if is_editable(agent) {
        options.push(option("Open in editor", "open"));
        options.push(option("Delete agent", "delete"));
    }
    options.push(option("Back", "back"));
    options
}

fn fresh_agent(all: &[AgentDefinition], requested: &AgentDefinition) -> AgentDefinition {
    all.iter()
        .find(|agent| agent.agent_type == requested.agent_type && agent.source == requested.source)
        .cloned()
        .unwrap_or_else(|| requested.clone())
}

#[derive(Default, Props)]
pub struct AgentsMenuProps<'a> {
    pub tools: Vec<AgentToolOption>,
    pub all_agents: Vec<AgentDefinition>,
    pub active_agents: Vec<AgentDefinition>,
    pub failed_files: Vec<FailedAgentFile>,
    pub on_exit: HandlerMut<'a, String>,
}

#[component]
pub fn AgentsMenu<'a>(
    props: &mut AgentsMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let initial = props.all_agents.clone();
    let mut all_agents = hooks.use_state(move || initial);
    let initial_failed = props.failed_files.clone();
    let mut failed_files = hooks.use_state(move || initial_failed);
    let mut mode = hooks.use_state(|| MenuMode::List);
    let mut changes = hooks.use_state(Vec::<String>::new);
    let mut pending_menu = hooks.use_state(|| None::<String>);
    let mut pending_delete = hooks.use_state(|| None::<AgentDefinition>);
    let mut pending_created = hooks.use_state(|| None::<String>);
    let mut pending_exit = hooks.use_state(|| None::<String>);
    let mut menu_error = hooks.use_state(|| None::<String>);
    let mut editor_result = hooks.use_state(|| None::<(AgentDefinition, EditorResult)>);
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let editor_channel =
        hooks.use_const(|| Arc::new(async_channel::unbounded::<(AgentDefinition, PathBuf)>()));
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok((agent, path)) = editor_receiver.recv().await {
            let result = match editor_runtime {
                Some(runtime) => runtime.edit_file(&path).await,
                None => EditorResult {
                    content: None,
                    error: Some("External editor is unavailable".to_string()),
                },
            };
            editor_result.set(Some((agent, result)));
        }
    });

    let exit = { pending_exit.read().clone() };
    if let Some(message) = exit {
        pending_exit.set(None);
        (props.on_exit)(message);
    }

    let menu_action = { pending_menu.read().clone() };
    if let Some(action) = menu_action {
        pending_menu.set(None);
        let current = mode.read().clone();
        if let MenuMode::Agent(agent) = current {
            menu_error.set(None);
            match action.as_str() {
                "view" => mode.set(MenuMode::View(agent)),
                "open" => match editor_target(&agent) {
                    Ok(path) => {
                        let _ = editor_channel.0.try_send((agent, path));
                    }
                    Err(message) => menu_error.set(Some(message)),
                },
                "delete" => mode.set(MenuMode::Delete(agent)),
                "back" => mode.set(MenuMode::List),
                _ => {}
            }
        }
    }
    let delete = { pending_delete.read().clone() };
    if let Some(agent) = delete {
        pending_delete.set(None);
        match delete_agent_from_file(&agent) {
            Ok(()) => {
                let next = all_agents
                    .read()
                    .iter()
                    .filter(|candidate| {
                        !(candidate.agent_type == agent.agent_type
                            && candidate.source == agent.source)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                all_agents.set(next);
                let mut next_changes = changes.read().clone();
                next_changes.push(format!("Deleted agent: {}", agent.agent_type));
                changes.set(next_changes);
                mode.set(MenuMode::List);
            }
            Err(error) => {
                let mut next_changes = changes.read().clone();
                next_changes.push(format!("Failed to delete {}: {error}", agent.agent_type));
                changes.set(next_changes);
            }
        }
    }
    let edited = { editor_result.read().clone() };
    if let Some((agent, result)) = edited {
        editor_result.set(None);
        if let Some(message) = result.error {
            menu_error.set(Some(message));
        } else if result.content.is_some() {
            pending_created.set(Some(format!("Opened {} in editor", agent.agent_type)));
        } else {
            // The file was checked before handoff, so an empty result means
            // no editor could be resolved.
            menu_error.set(Some("No editor found — set $VISUAL or $EDITOR".to_string()));
        }
    }
    let created = { pending_created.read().clone() };
    if let Some(message) = created {
        pending_created.set(None);
        let mut next_changes = changes.read().clone();
        next_changes.push(message);
        changes.set(next_changes);
        let loaded =
            get_agent_definitions_with_overrides(&crate::bootstrap::state::get_original_cwd());
        all_agents.set(loaded.all_agents);
        failed_files.set(loaded.failed_files);
        mode.set(MenuMode::List);
    }

    let snapshot = all_agents.read().clone();
    let active = if props.active_agents.is_empty() {
        get_active_agents_from_list(&snapshot)
    } else {
        props.active_agents.clone()
    };
    let tool_names = props
        .tools
        .iter()
        .filter(|tool| tool.available_to_custom_agent)
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let theme = hooks.use_context::<Theme>();
    let current_mode = { mode.read().clone() };
    match current_mode {
        MenuMode::List => {
            let resolved: Vec<ResolvedAgent> = resolve_agent_overrides(&snapshot, &active);
            let changes_snapshot = changes.read().clone();
            let exit_changes = changes_snapshot.clone();
            let mut exit = pending_exit;
            let mut open_agent = mode;
            let mut create = mode;
            return element! { Fragment {
                AgentsList(
                    source: Some(AgentsListSource::All), agents: resolved, changes: changes_snapshot,
                    failed_files: failed_files.read().clone(),
                    on_back: move |_| {
                        let message = if exit_changes.is_empty() { "Agents dialog dismissed".to_string() } else { format!("Agent changes:\n{}", exit_changes.join("\n")) };
                        exit.set(Some(message));
                    },
                    on_select: move |agent| open_agent.set(MenuMode::Agent(agent)),
                    on_create_new: move |_| create.set(MenuMode::Create),
                )
                AgentNavigationFooter()
            }}.into_any();
        }
        MenuMode::Create => {
            let mut complete = pending_created;
            let mut cancel = mode;
            element! { CreateAgentWizard(
                tools: props.tools.clone(), existing_agents: active,
                on_complete: move |message| complete.set(Some(message)),
                on_cancel: move |_| cancel.set(MenuMode::List),
            ) }
            .into_any()
        }
        MenuMode::Agent(requested) => {
            let agent = fresh_agent(&snapshot, &requested);
            let options = agent_menu_options(&agent);
            let error = menu_error.read().clone();
            let last_change = changes.read().last().cloned();
            let mut select = pending_menu;
            let mut back = mode;
            element! { Fragment {
                Dialog(title: agent.agent_type.clone(), hide_input_guide: true, on_cancel: move |_| back.set(MenuMode::List)) {
                    View(flex_direction: FlexDirection::Column) {
                        WizardChoice(options: options, on_select: move |value| select.set(Some(value)), on_cancel: move |_| back.set(MenuMode::List))
                        #(last_change.map(|message| element! { View(margin_top: 1u32) { Text(content: message, dim: true) } }))
                        #(error.map(|message| element! { View(margin_top: 1u32) { Text(content: message, color: theme.error) } }))
                    }
                }
                AgentNavigationFooter()
            }}.into_any()
        }
        MenuMode::View(requested) => {
            let agent = fresh_agent(&snapshot, &requested);
            let for_back = agent.clone();
            let for_cancel = agent.clone();
            let mut back = mode;
            let mut cancel = mode;
            element! { Fragment {
                Dialog(title: agent.agent_type.clone(), hide_input_guide: true, on_cancel: move |_| cancel.set(MenuMode::Agent(for_cancel.clone()))) {
                    AgentDetail(agent: Some(agent), available_tool_names: tool_names, on_back: move |_| back.set(MenuMode::Agent(for_back.clone())))
                }
                AgentNavigationFooter(instructions: Some("Press Enter or Esc to go back".to_string()))
            }}.into_any()
        }
        MenuMode::Delete(requested) => {
            let agent = fresh_agent(&snapshot, &requested);
            let cancel_agent = agent.clone();
            let no_agent_for_select = agent.clone();
            let no_agent_for_cancel = agent.clone();
            let mut cancel = mode;
            let mut select_cancel = mode;
            let mut delete = pending_delete;
            element! { Fragment {
                Dialog(title: "Delete agent".to_string(), color: Some(theme.error), on_cancel: move |_| cancel.set(MenuMode::Agent(cancel_agent.clone()))) {
                    View(flex_direction: FlexDirection::Column) {
                        MixedText(contents: vec![MixedTextContent::new("Are you sure you want to delete the agent "), MixedTextContent::new(agent.agent_type.clone()).weight(Weight::Bold), MixedTextContent::new("?")])
                        View(margin_top: 1u32) { Text(content: format!("Source: {}", agent.source.official_name()), dim: true) }
                        View(margin_top: 1u32) {
                            WizardChoice(
                                options: vec![
                                    SelectOptionData { label: "Yes, delete".to_string(), value: "yes".to_string(), ..Default::default() },
                                    SelectOptionData { label: "No, cancel".to_string(), value: "no".to_string(), ..Default::default() },
                                ],
                                on_select: move |value| if value == "yes" { delete.set(Some(agent.clone())); } else { select_cancel.set(MenuMode::Agent(no_agent_for_select.clone())); },
                                on_cancel: move |_| select_cancel.set(MenuMode::Agent(no_agent_for_cancel.clone())),
                            )
                        }
                    }
                }
                AgentNavigationFooter(instructions: Some("Press ↑↓ to navigate, Enter to select, Esc to cancel".to_string()))
            }}.into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    fn custom_agent() -> AgentDefinition {
        let mut agent = AgentDefinition::new(
            "reviewer",
            "Use this agent for code review",
            AgentDefinitionSource::ProjectSettings,
        );
        agent.system_prompt =
            Some("You are a careful code reviewer with a complete workflow.".to_string());
        agent
    }

    #[test]
    fn only_user_and_project_agents_can_be_opened_or_deleted() {
        let values = |source| {
            agent_menu_options(&AgentDefinition::new("a", "d", source))
                .into_iter()
                .map(|option| option.value)
                .collect::<Vec<_>>()
        };
        for source in [
            AgentDefinitionSource::UserSettings,
            AgentDefinitionSource::ProjectSettings,
        ] {
            assert_eq!(values(source), ["view", "open", "delete", "back"]);
        }
        for source in [
            AgentDefinitionSource::BuiltIn,
            AgentDefinitionSource::Plugin,
            AgentDefinitionSource::FlagSettings,
        ] {
            assert_eq!(values(source), ["view", "back"], "{source:?}");
        }
    }

    #[test]
    fn open_in_editor_reports_a_missing_agent_file() {
        let path = std::env::temp_dir().join(format!("nyan-missing-{}.md", uuid::Uuid::new_v4()));
        let mut agent = custom_agent();
        agent.file_path = Some(path.clone());
        assert_eq!(
            editor_target(&agent).unwrap_err(),
            format!("File not found: {}", path.display())
        );
        std::fs::write(&path, "x").unwrap();
        assert_eq!(editor_target(&agent).unwrap(), path);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn initial_mode_renders_resolved_list_and_footer() {
        let agent = custom_agent();
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                // Fragment is layout-transparent, so the list and the footer
                // would become siblings in the default (row) flex direction.
                View(flex_direction: FlexDirection::Column) {
                    AgentsMenu(all_agents: vec![agent.clone()], active_agents: vec![agent])
                }
            }
        }
        .render(Some(110))
        .to_string();
        assert!(text.contains("Agents"));
        assert!(text.contains("Create new agent"));
        assert!(text.contains("reviewer"));
        assert!(text.contains("Press ↑↓ to navigate"), "canvas=\n{text}");
    }

    #[test]
    fn list_action_opens_editable_agent_menu_without_mutation() {
        let events = stream::iter(vec![KeyCode::Down, KeyCode::Enter])
            .then(|code| async move {
                futures_timer::Delay::new(Duration::from_millis(45)).await;
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
            })
            .chain(stream::pending());
        futures::executor::block_on(async move {
            let agent = custom_agent();
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        AgentsMenu(all_agents: vec![agent.clone()], active_agents: vec![agent])
                    }
                }
            };
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 28),
            ));
            let mut opened = false;
            for _ in 0..30 {
                if let Some(frame) = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await
                {
                    let text = frame.to_string();
                    if text.contains("View agent")
                        && text.contains("Open in editor")
                        && text.contains("Delete agent")
                    {
                        opened = true;
                        break;
                    }
                }
            }
            assert!(opened);
        });
    }
}
