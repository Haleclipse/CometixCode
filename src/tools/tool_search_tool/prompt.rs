//! Maps to CC `tools/ToolSearchTool/constants.ts` and `prompt.ts`.

pub const TOOL_SEARCH_TOOL_NAME: &str = "ToolSearch";

const PROMPT_HEAD: &str =
    "Fetches full schema definitions for deferred tools so they can be called.\n\n";
const PROMPT_TAIL: &str = " Until fetched, only the name is known — there is no parameter schema, so the tool cannot be invoked. This tool takes a query, matches it against the deferred tool list, and returns the matched tools' complete JSONSchema definitions inside a <functions> block. Once a tool's schema appears in that result, it is callable exactly like any tool defined at the top of the prompt.\n\nResult format: each matched tool appears as one <function>{\"description\": \"...\", \"name\": \"...\", \"parameters\": {...}}</function> line inside the <functions> block — the same encoding as the tool list at the top of this prompt.\n\nQuery forms:\n- \"select:Read,Edit,Grep\" — fetch these exact tools by name\n- \"notebook jupyter\" — keyword search, up to max_results best matches\n- \"+slack send\" — require \"slack\" in the name, rank by remaining terms";

/// Maps to CC `getToolLocationHint()`.
/// Cometix uses the hardcoded feature-switch collection rather than GrowthBook.
fn tool_location_hint() -> &'static str {
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    ) || crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::DeferredToolsDelta,
    ) {
        "Deferred tools appear by name in <system-reminder> messages."
    } else {
        "Deferred tools appear by name in <available-deferred-tools> messages."
    }
}

pub fn get_prompt() -> String {
    format!("{PROMPT_HEAD}{}{PROMPT_TAIL}", tool_location_hint())
}

/// Maps to CC `tools/ToolSearchTool/prompt.ts` `isDeferredTool(...)`.
///
/// Built-in behavior metadata is consumed from the active ToolCall registry;
/// the name fallback preserves deferred tools whose behavior port has not yet
/// implemented `should_defer`. `ToolSearch`, `Agent`, and
/// `Brief`/`SendUserMessage` remain eager like CC.
pub fn is_deferred_tool(tool: &crate::types::tools::Tool) -> bool {
    if tool.name == TOOL_SEARCH_TOOL_NAME || tool.name == "Agent" || tool.name == "SendUserMessage"
    {
        return false;
    }
    if tool.is_mcp {
        return true;
    }
    if crate::services::tools::tool_execution::find_tool_call(&tool.name)
        .is_some_and(|call| call.should_defer())
    {
        return true;
    }
    matches!(
        tool.name.as_str(),
        "AskUserQuestion"
            | "NotebookEdit"
            | "ExitPlanMode"
            | "EnterPlanMode"
            | "EnterWorktree"
            | "ExitWorktree"
            | "TaskOutput"
            | "TaskStop"
            | "TaskCreate"
            | "TaskGet"
            | "TaskUpdate"
            | "TaskList"
            | "TodoWrite"
            | "LSP"
            | "ListMcpResourcesTool"
            | "ReadMcpResourceTool"
            | "Config"
            | "WebFetch"
            | "WebSearch"
            | "SendMessage"
            | "TeamCreate"
            | "TeamDelete"
            | "CronCreate"
            | "CronDelete"
            | "CronList"
            | "RemoteTrigger"
    )
}

/// Maps to CC `utils/toolSearch.ts` `isToolSearchToolAvailable(...)`.
pub fn is_tool_search_tool_available(tools: &[crate::types::tools::Tool]) -> bool {
    tools
        .iter()
        .any(|tool| crate::types::tools::tool_matches_name(tool, TOOL_SEARCH_TOOL_NAME))
}

/// Maps to CC `utils/toolSearch.ts` `modelSupportsToolReference(...)`.
pub fn model_supports_tool_reference(model: &str) -> bool {
    !model.to_ascii_lowercase().contains("haiku")
}

/// Maps to CC `utils/toolSearch.ts` `isToolSearchEnabled(...)` request-time gate.
///
/// Cometix keeps the official hard requirements (beta kill switch, model
/// support, and ToolSearchTool availability). The upstream `tst-auto` token
/// threshold path is approximated as enabled once explicitly requested; full
/// token-count based auto mode belongs beside future context-analysis ports.
pub fn is_tool_search_enabled_for_request(
    model: &str,
    tools: &[crate::types::tools::Tool],
) -> bool {
    is_tool_search_enabled_optimistic()
        && model_supports_tool_reference(model)
        && is_tool_search_tool_available(tools)
}

/// Maps to CC `utils/toolSearch.ts` `isToolSearchEnabledOptimistic()`.
pub fn is_tool_search_enabled_optimistic() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS")
            .as_deref(),
    ) {
        return false;
    }
    if let Some(value) = crate::utils::process_env::var("ENABLE_TOOL_SEARCH") {
        let normalized = value.trim().to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "0" | "false" | "no" | "off" | "auto:100"
        ) {
            return false;
        }
    }
    true
}
