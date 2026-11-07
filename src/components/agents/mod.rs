//! Maps to: CC `components/agents/**`.

pub mod agent_detail;
pub mod agent_file_utils;
pub mod agent_navigation_footer;
pub mod agents_list;
pub mod agents_menu;
pub mod color_picker;
pub mod generate_agent;
pub mod model_selector;
pub mod new_agent_creation;
pub mod tool_selector;
pub mod types;
pub mod utils;
pub mod validate_agent;

pub use agent_detail::AgentDetail;
pub use agent_navigation_footer::AgentNavigationFooter;
pub use agents_list::{AgentDisplaySource, AgentsList, AgentsListSource, ResolvedAgent};
pub use agents_menu::AgentsMenu;
pub use color_picker::ColorPicker;
pub use model_selector::ModelSelector;
pub use tool_selector::{AgentToolOption, ToolSelector};
pub use validate_agent::{AgentValidationInput, validate_agent, validate_agent_type};
