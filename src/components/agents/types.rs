//! Maps to: CC `components/agents/types.ts:1-27`.

use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};

pub const AGENT_FOLDER_NAME: &str = ".nyan";
pub const AGENTS_DIR: &str = "agents";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentListSource {
    All,
    BuiltIn,
    Source(AgentDefinitionSource),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentModeState {
    MainMenu,
    ListAgents {
        source: AgentListSource,
    },
    AgentMenu {
        agent: AgentDefinition,
        previous: Box<AgentModeState>,
    },
    ViewAgent {
        agent: AgentDefinition,
        previous: Box<AgentModeState>,
    },
    CreateAgent,
    EditAgent {
        agent: AgentDefinition,
        previous: Box<AgentModeState>,
    },
    DeleteConfirm {
        agent: AgentDefinition,
        previous: Box<AgentModeState>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentValidationResult {
    pub is_valid: bool,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}
