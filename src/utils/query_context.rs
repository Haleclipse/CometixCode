//! Maps to CC `utils/queryContext.ts` shared API cache-prefix preparation.
//! `buildSideQuestionFallbackParams` remains unported.

use crate::services::api::request::SystemPrompt;
use crate::services::mcp::types::McpServerConnection;
use crate::types::tools::Tool;
use std::collections::BTreeMap;

/// Maps to: CC `utils/queryContext.ts:44-74` `fetchSystemPromptParts`.
/// The synchronous owners use the established sync-context carrier; the three
/// tuple fields map in order to `defaultSystemPrompt`, `userContext`, and
/// `systemContext` in the source return object.
/// A custom prompt, including an empty string, suppresses default system prompt
/// and system context fetching, while user context is always fetched.
pub fn fetch_system_prompt_parts(
    tools: &[Tool],
    main_loop_model: &str,
    additional_working_directories: &[String],
    mcp_clients: &[McpServerConnection],
    custom_system_prompt: Option<&str>,
) -> (
    SystemPrompt,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
) {
    let default_system_prompt = if custom_system_prompt.is_some() {
        Vec::new()
    } else {
        crate::constants::prompts::get_system_prompt(
            tools,
            main_loop_model,
            additional_working_directories,
            mcp_clients,
        )
    };
    let user_context = crate::context::get_user_context();
    let system_context = if custom_system_prompt.is_some() {
        BTreeMap::new()
    } else {
        crate::context::get_system_context()
    };
    (default_system_prompt, user_context, system_context)
}
