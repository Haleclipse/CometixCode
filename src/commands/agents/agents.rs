//! Maps to: CC `commands/agents/agents.tsx:1-16`.

use crate::components::agents::agents_menu::AgentsMenu;
use crate::components::agents::tool_selector::AgentToolOption;
use crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct AgentsCommandProps<'a> {
    pub tools: Vec<AgentToolOption>,
    pub on_done: HandlerMut<'a, String>,
}

#[component]
pub fn AgentsCommand<'a>(
    props: &mut AgentsCommandProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let definitions = hooks.use_state(|| {
        get_agent_definitions_with_overrides(&crate::bootstrap::state::get_original_cwd())
    });
    let mut pending = hooks.use_state(|| None::<String>);
    let done = { pending.read().clone() };
    if let Some(done) = done {
        pending.set(None);
        (props.on_done)(done);
    }
    let snapshot = definitions.read().clone();
    let mut exit = pending;
    element! {
        AgentsMenu(
            tools: props.tools.clone(), all_agents: snapshot.all_agents, active_agents: snapshot.active_agents,
            on_exit: move |message| exit.set(Some(message)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_boundary_loads_readonly_definitions_and_renders_menu() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) { AgentsCommand() }
        }.render(Some(100)).to_string();
        assert!(text.contains("Agents"));
        assert!(text.contains("Create new agent"));
    }
}
