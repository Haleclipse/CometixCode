//! Maps to: CC `commands/context/index.ts`.

pub mod context;
pub mod context_noninteractive;

use crate::tool::ToolUseContext;
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::utils::system_prompt::CliSystemPromptOverrides;

pub const NAME: &str = "context";
pub const INTERACTIVE_DESCRIPTION: &str = "Visualize current context usage as a colored grid";
pub const NON_INTERACTIVE_DESCRIPTION: &str = "Show current context usage";

/// Strongly typed payload handed from the synchronous descriptor boundary to
/// the REPL-owned worker. Maps to CC's `LocalJSXCommandContext` fields consumed
/// by `commands/context/context.tsx#call`.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextCommandRequest {
    pub context: ToolUseContext,
    pub terminal_width: Option<u16>,
    pub system_prompt_overrides: CliSystemPromptOverrides,
    pub main_thread_agent_definition: Option<AgentDefinition>,
}

impl Eq for ContextCommandRequest {}

impl ContextCommandRequest {
    pub fn new(context: &ToolUseContext) -> Self {
        Self {
            context: context.clone(),
            terminal_width: None,
            system_prompt_overrides: CliSystemPromptOverrides::default(),
            main_thread_agent_definition: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    struct InteractiveRestore(bool);

    impl Drop for InteractiveRestore {
        fn drop(&mut self) {
            crate::bootstrap::state::set_is_interactive(self.0);
        }
    }

    #[test]
    fn context_index_selects_interactive_and_noninteractive_descriptors_like_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let previous = crate::bootstrap::state::get_is_interactive();
        let _restore = InteractiveRestore(previous);
        let cwd = std::env::current_dir().unwrap();

        crate::bootstrap::state::set_is_interactive(true);
        let interactive = crate::commands::get_commands(&cwd)
            .into_iter()
            .find(|command| command.name == NAME)
            .expect("interactive context command");
        assert_eq!(interactive.kind, crate::commands::CommandKind::LocalUi);
        assert_eq!(interactive.description, INTERACTIVE_DESCRIPTION);
        assert!(interactive.call.is_some());
        assert!(!interactive.supports_non_interactive);

        crate::bootstrap::state::set_is_interactive(false);
        let noninteractive = crate::commands::get_commands(&cwd)
            .into_iter()
            .find(|command| command.name == NAME)
            .expect("noninteractive context command");
        assert_eq!(noninteractive.kind, crate::commands::CommandKind::Local);
        assert_eq!(noninteractive.description, NON_INTERACTIVE_DESCRIPTION);
        assert!(noninteractive.call.is_some());
        assert!(noninteractive.supports_non_interactive);
        assert!(!noninteractive.is_hidden);
    }
}
