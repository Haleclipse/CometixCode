//! Maps to: CC `components/agents/AgentsList.tsx:1-342`.

use super::utils::get_agent_source_display_name;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::divider::Divider;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::tools::agent_tool::load_agents_dir::{
    AgentDefinition, AgentDefinitionSource, FailedAgentFile,
};
use crate::utils::model::agent::effective_subagent_profile;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentDisplaySource {
    User,
    Project,
    Local,
    Managed,
    Plugin,
    Flag,
    BuiltIn,
}

impl AgentDisplaySource {
    fn from_definition(source: AgentDefinitionSource) -> Self {
        match source {
            AgentDefinitionSource::UserSettings => Self::User,
            AgentDefinitionSource::ProjectSettings => Self::Project,
            AgentDefinitionSource::PolicySettings => Self::Managed,
            AgentDefinitionSource::Plugin => Self::Plugin,
            AgentDefinitionSource::FlagSettings => Self::Flag,
            AgentDefinitionSource::BuiltIn => Self::BuiltIn,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::User => "User agents",
            Self::Project => "Project agents",
            Self::Local => "Local agents",
            Self::Managed => "Managed agents",
            Self::Plugin => "Plugin agents",
            Self::Flag => "CLI arg agents",
            Self::BuiltIn => "Built-in agents",
        }
    }

    fn override_label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "project, gitignored",
            Self::Managed => "managed",
            Self::Plugin => "plugin",
            Self::Flag => "cli flag",
            Self::BuiltIn => "built-in",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAgent {
    pub agent: AgentDefinition,
    pub display_source: AgentDisplaySource,
    pub overridden_by: Option<AgentDisplaySource>,
}

impl ResolvedAgent {
    pub fn from_agent(agent: AgentDefinition) -> Self {
        let display_source = AgentDisplaySource::from_definition(agent.source);
        Self {
            agent,
            display_source,
            overridden_by: None,
        }
    }
}

/// Maps to CC `tools/AgentTool/agentDisplay.ts#resolveAgentOverrides`, kept
/// adjacent to the list projection until the broader agent-display module lands.
pub fn resolve_agent_overrides(
    all_agents: &[AgentDefinition],
    active_agents: &[AgentDefinition],
) -> Vec<ResolvedAgent> {
    use std::collections::{HashMap, HashSet};
    let active = active_agents
        .iter()
        .map(|agent| (agent.agent_type.as_str(), agent.source))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    all_agents
        .iter()
        .filter_map(|agent| {
            if !seen.insert((agent.agent_type.clone(), agent.source)) {
                return None;
            }
            let display_source = AgentDisplaySource::from_definition(agent.source);
            let overridden_by = active
                .get(agent.agent_type.as_str())
                .copied()
                .filter(|source| *source != agent.source)
                .map(AgentDisplaySource::from_definition);
            Some(ResolvedAgent {
                agent: agent.clone(),
                display_source,
                overridden_by,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentsListSource {
    All,
    Source(AgentDisplaySource),
}

fn source_title(source: AgentsListSource) -> &'static str {
    match source {
        AgentsListSource::All => get_agent_source_display_name(None),
        AgentsListSource::Source(AgentDisplaySource::BuiltIn) => "Built-in agents",
        AgentsListSource::Source(AgentDisplaySource::Plugin) => "Plugin agents",
        AgentsListSource::Source(AgentDisplaySource::User) => "User",
        AgentsListSource::Source(AgentDisplaySource::Project) => "Project",
        AgentsListSource::Source(AgentDisplaySource::Local) => "Project, gitignored",
        AgentsListSource::Source(AgentDisplaySource::Managed) => "Managed",
        AgentsListSource::Source(AgentDisplaySource::Flag) => "Cli flag",
    }
}

const GROUP_ORDER: [AgentDisplaySource; 7] = [
    AgentDisplaySource::User,
    AgentDisplaySource::Project,
    AgentDisplaySource::Local,
    AgentDisplaySource::Managed,
    AgentDisplaySource::Plugin,
    AgentDisplaySource::Flag,
    AgentDisplaySource::BuiltIn,
];

fn selectable_agents(source: AgentsListSource, sorted: &[ResolvedAgent]) -> Vec<ResolvedAgent> {
    let non_builtin = sorted
        .iter()
        .filter(|agent| agent.display_source != AgentDisplaySource::BuiltIn);
    match source {
        AgentsListSource::All => GROUP_ORDER
            .into_iter()
            .filter(|source| *source != AgentDisplaySource::BuiltIn)
            .flat_map(|source| {
                non_builtin
                    .clone()
                    .filter(move |agent| agent.display_source == source)
                    .cloned()
            })
            .collect(),
        AgentsListSource::Source(_) => non_builtin.cloned().collect(),
    }
}

#[derive(Clone, Copy, Debug)]
enum ListAction {
    Previous,
    Next,
    Accept,
    Back,
}

#[derive(Default, Props)]
pub struct AgentsListProps<'a> {
    pub source: Option<AgentsListSource>,
    pub agents: Vec<ResolvedAgent>,
    pub on_back: HandlerMut<'a, ()>,
    pub on_select: HandlerMut<'a, AgentDefinition>,
    pub on_create_new: HandlerMut<'a, ()>,
    pub changes: Vec<String>,
    /// @nyan: agents that failed to load (e.g. an unknown `model:`), shown
    /// above the list.
    pub failed_files: Vec<FailedAgentFile>,
}

#[component]
pub fn AgentsList<'a>(
    props: &mut AgentsListProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let source = props.source.unwrap_or(AgentsListSource::All);
    let mut sorted = props.agents.clone();
    sorted.sort_by(|left, right| {
        left.agent
            .agent_type
            .to_ascii_lowercase()
            .cmp(&right.agent.agent_type.to_ascii_lowercase())
    });
    let selectable = selectable_agents(source, &sorted);
    let has_create = !props.on_create_new.is_default();
    let mut selected_key = hooks.use_state(|| None::<(String, AgentDisplaySource)>);
    let mut create_selected = hooks.use_state(move || has_create);
    let mut pending_action = hooks.use_state(|| None::<ListAction>);

    if !has_create
        && selected_key.read().is_none()
        && let Some(first) = selectable.first()
    {
        selected_key.set(Some((first.agent.agent_type.clone(), first.display_source)));
        create_selected.set(false);
    }
    let action = {
        let value = pending_action.read();
        *value
    };
    if let Some(action) = action {
        pending_action.set(None);
        let total = selectable.len() + usize::from(has_create);
        let current = if create_selected.get() {
            0
        } else {
            selected_key
                .read()
                .as_ref()
                .and_then(|key| {
                    selectable.iter().position(|agent| {
                        (&agent.agent.agent_type, agent.display_source) == (&key.0, key.1)
                    })
                })
                .map(|index| index + usize::from(has_create))
                .unwrap_or(0)
        };
        match action {
            ListAction::Back => (props.on_back)(()),
            ListAction::Accept => {
                if create_selected.get() && has_create {
                    (props.on_create_new)(());
                } else if let Some(key) = selected_key.read().clone()
                    && let Some(agent) = selectable.iter().find(|agent| {
                        agent.agent.agent_type == key.0 && agent.display_source == key.1
                    })
                {
                    (props.on_select)(agent.agent.clone());
                }
            }
            ListAction::Previous | ListAction::Next if total > 0 => {
                let next = if matches!(action, ListAction::Previous) {
                    if current == 0 { total - 1 } else { current - 1 }
                } else if current + 1 == total {
                    0
                } else {
                    current + 1
                };
                if has_create && next == 0 {
                    create_selected.set(true);
                    selected_key.set(None);
                } else if let Some(agent) = selectable.get(next - usize::from(has_create)) {
                    create_selected.set(false);
                    selected_key.set(Some((agent.agent.agent_type.clone(), agent.display_source)));
                }
            }
            ListAction::Previous | ListAction::Next => {}
        }
    }

    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    for (name, context, action) in [
        ("select:previous", ContextName::Select, ListAction::Previous),
        ("select:next", ContextName::Select, ListAction::Next),
        ("select:accept", ContextName::Select, ListAction::Accept),
        ("confirm:no", ContextName::Confirmation, ListAction::Back),
    ] {
        let mut pending_action = pending_action;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            name,
            context,
            || true,
            move || {
                pending_action.set(Some(action));
                true
            },
        );
    }

    let settings = crate::utils::settings::get_initial_settings();
    let nyan = settings.nyan.as_deref();
    let render_agent = |resolved: &ResolvedAgent| -> AnyElement<'static> {
        let built_in = resolved.display_source == AgentDisplaySource::BuiltIn;
        let selected = !built_in
            && !create_selected.get()
            && selected_key.read().as_ref().is_some_and(|key| {
                key.0 == resolved.agent.agent_type && key.1 == resolved.display_source
            });
        let dim = (built_in || resolved.overridden_by.is_some()) && !selected;
        let color = selected.then_some(theme.suggestion);
        let model = effective_subagent_profile(
            resolved.agent.model.as_deref(),
            &resolved.agent.agent_type,
            nyan,
        );
        let memory = resolved
            .agent
            .memory
            .map(|memory| memory.official_name().to_string());
        let warning = resolved.overridden_by.map(|source| {
            format!(
                " {} shadowed by {}",
                crate::constants::figures::get().warning,
                source.override_label()
            )
        });
        element! { View(flex_direction: FlexDirection::Row) {
            Text(content: if built_in { "".to_string() } else if selected { "❯ ".to_string() } else { "  ".to_string() }, color: color, dim: dim)
            Text(content: resolved.agent.agent_type.clone(), color: color, dim: dim)
            Text(content: format!(" · {model}"), color: color, dim: true)
            #(memory.map(|memory| element! { Text(content: format!(" · {memory} memory"), color: color, dim: true) }))
            #(warning.map(|warning| element! { Text(content: warning, color: if selected { Some(theme.warning) } else { None }, dim: !selected) }))
        }}.into_any()
    };

    let create_row = has_create.then(|| element! { View(flex_direction: FlexDirection::Row, margin_bottom: 1u32) {
        Text(content: if create_selected.get() { "❯ Create new agent".to_string() } else { "  Create new agent".to_string() }, color: create_selected.get().then_some(theme.suggestion))
    }});
    let non_builtin_count = sorted
        .iter()
        .filter(|agent| agent.display_source != AgentDisplaySource::BuiltIn)
        .count();
    let has_no_agents = sorted.is_empty()
        || (source != AgentsListSource::Source(AgentDisplaySource::BuiltIn)
            && non_builtin_count == 0);
    let subtitle = if has_no_agents {
        "No agents found".to_string()
    } else {
        format!(
            "{} agents",
            sorted
                .iter()
                .filter(|agent| agent.overridden_by.is_none())
                .count()
        )
    };
    let last_change = props.changes.last().cloned();
    let failed = (!props.failed_files.is_empty()).then(|| {
        let rows = props
            .failed_files
            .iter()
            .map(|file| element! {
                Text(content: format!("  └ {}: {}", file.label(), file.error), color: theme.inactive, wrap: TextWrap::Wrap)
            })
            .collect::<Vec<_>>();
        element! { View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
            Text(content: format!("Failed to load {} agent(s):", props.failed_files.len()), color: theme.error)
            #(rows)
        }}
    });

    let body: AnyElement<'static> = if has_no_agents {
        let builtins = sorted
            .iter()
            .filter(|agent| agent.display_source == AgentDisplaySource::BuiltIn)
            .map(&render_agent)
            .collect::<Vec<_>>();
        element! { View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            #(create_row)
            Text(content: "No agents found. Create specialized subagents that Claude can delegate to.".to_string(), dim: true)
            Text(content: "Each subagent has its own context window, custom system prompt, and specific tools.".to_string(), dim: true)
            Text(content: "Try creating: Code Reviewer, Code Simplifier, Security Reviewer, Tech Lead, or UX Reviewer.".to_string(), dim: true)
            #((source != AgentsListSource::Source(AgentDisplaySource::BuiltIn) && !builtins.is_empty()).then(|| element! { View(flex_direction: FlexDirection::Column) {
                Divider()
                View(flex_direction: FlexDirection::Column, padding_left: 2u32) {
                    Text(content: "Built-in (always available):".to_string(), weight: Weight::Bold, dim: true)
                    #(builtins)
                }
            }}))
        }}.into_any()
    } else if source == AgentsListSource::All {
        let groups = GROUP_ORDER.into_iter().filter_map(|group| {
            let agents = sorted.iter().filter(|agent| agent.display_source == group).collect::<Vec<_>>();
            if agents.is_empty() { return None; }
            let rows = agents.into_iter().map(&render_agent).collect::<Vec<_>>();
            let path = sorted.iter().find(|agent| agent.display_source == group).and_then(|agent| agent.agent.base_dir.as_ref()).map(|path| path.display().to_string());
            Some(element! { View(flex_direction: FlexDirection::Column, margin_bottom: 1u32, padding_left: if group == AgentDisplaySource::BuiltIn { 2u32 } else { 0u32 }) {
                MixedText(contents: vec![
                    MixedTextContent::new(if group == AgentDisplaySource::BuiltIn { "Built-in agents" } else { group.label() }).weight(Weight::Bold),
                    MixedTextContent::new(if group == AgentDisplaySource::BuiltIn { " (always available)".to_string() } else { path.map(|path| format!(" ({path})")).unwrap_or_default() }).weight(Weight::Light),
                ])
                #(rows)
            }})
        }).collect::<Vec<_>>();
        element! { View(flex_direction: FlexDirection::Column) { #(create_row) #(groups) } }
            .into_any()
    } else {
        let rows = sorted.iter().map(&render_agent).collect::<Vec<_>>();
        element! { View(flex_direction: FlexDirection::Column) {
            #(create_row)
            #((source == AgentsListSource::Source(AgentDisplaySource::BuiltIn)).then(|| element! { Text(content: "Built-in agents are provided by default and cannot be modified.".to_string(), dim: true, italic: true) }))
            #(rows)
        }}.into_any()
    };

    element! {
        Dialog(
            title: source_title(source).to_string(), subtitle: Some(subtitle), hide_input_guide: true,
            on_cancel: move |_| pending_action.set(Some(ListAction::Back)),
        ) {
            #(last_change.map(|change| element! { View(margin_top: 1u32) { Text(content: change, dim: true) } }))
            #(failed)
            #(vec![body])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn resolved(name: &str, source: AgentDefinitionSource) -> ResolvedAgent {
        ResolvedAgent::from_agent(AgentDefinition::new(name, format!("Use {name}"), source))
    }

    #[test]
    fn all_view_groups_sources_dims_builtins_and_shows_shadowing() {
        let mut shadowed = resolved("reviewer", AgentDefinitionSource::UserSettings);
        shadowed.overridden_by = Some(AgentDisplaySource::Project);
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentsList(
                    source: Some(AgentsListSource::All),
                    agents: vec![
                        resolved("explore", AgentDefinitionSource::BuiltIn),
                        resolved("zeta", AgentDefinitionSource::ProjectSettings),
                        shadowed,
                    ],
                    on_create_new: move |_| {},
                    changes: vec!["Saved agent".to_string()],
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Agents"));
        assert!(text.contains("2 agents"));
        assert!(text.contains("User agents"));
        assert!(text.contains("Project agents"));
        assert!(text.contains("Built-in agents (always available)"));
        assert!(text.contains("shadowed by project"));
        assert!(text.contains("Saved agent"));
    }

    #[test]
    fn failed_files_show_above_the_list_and_rows_show_the_resolved_profile() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::settings::settings_cache::install_test_settings(
            serde_json::from_value(serde_json::json!({
                "nyan": {"models": {"ds-flash": {"model": "deepseek-chat"}},
                    "selection": {"main": "ds-flash"}}
            }))
            .unwrap(),
        );
        let mut agent = resolved("scout", AgentDefinitionSource::ProjectSettings);
        agent.agent.model = Some("@light:high".to_string());
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentsList(
                    source: Some(AgentsListSource::All),
                    agents: vec![agent],
                    on_create_new: move |_| {},
                    failed_files: vec![FailedAgentFile {
                        path: std::path::PathBuf::from("/p/.nyan/agents/bad.md"),
                        error: "Invalid \"model\" field: Unknown model \"sonnet\"".to_string(),
                        source: None,
                    }],
                )
            }
        }
        .render(Some(140))
        .to_string();
        crate::utils::settings::settings_cache::reset_settings_cache();
        let failed_at = text
            .find("Failed to load 1 agent(s):")
            .expect("failed header");
        assert!(
            text.contains("/p/.nyan/agents/bad.md: Invalid \"model\" field"),
            "{text}"
        );
        assert!(failed_at < text.find("Create new agent").unwrap(), "{text}");
        assert!(text.contains("scout · ds-flash"), "{text}");
    }

    #[test]
    fn empty_view_preserves_create_guidance_and_builtins() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AgentsList(
                    source: Some(AgentsListSource::Source(AgentDisplaySource::Project)),
                    agents: vec![resolved("explore", AgentDefinitionSource::BuiltIn)],
                    on_create_new: move |_| {},
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("No agents found"));
        assert!(text.contains("❯ Create new agent"));
        assert!(text.contains("Each subagent has its own context window"));
        assert!(text.contains("Built-in (always available):"));
    }

    #[test]
    fn action_navigation_moves_from_create_to_first_agent_and_selects() {
        let selected = Arc::new(Mutex::new(Vec::<String>::new()));
        let selected_handler = Arc::clone(&selected);
        let selected_wait = Arc::clone(&selected);
        let events = stream::iter(vec![KeyCode::Down, KeyCode::Enter])
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
                        AgentsList(
                            source: Some(AgentsListSource::All),
                            agents: vec![resolved("reviewer", AgentDefinitionSource::ProjectSettings)],
                            on_create_new: move |_| {},
                            on_select: move |agent: AgentDefinition| selected_handler.lock().expect("select mutex").push(agent.agent_type),
                        )
                    }
                }
            };
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            for _ in 0..25 {
                let _ = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if !selected_wait.lock().expect("select mutex").is_empty() {
                    break;
                }
            }
        });
        assert_eq!(*selected.lock().expect("select mutex"), vec!["reviewer"]);
    }
}
