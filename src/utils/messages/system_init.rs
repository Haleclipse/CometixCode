//! SDK `system/init` message construction.
//!
//! Maps to CC `utils/messages/systemInit.ts:1-95`.

fn mcp_server_statuses(state: &crate::state::app_state_store::McpState) -> Vec<serde_json::Value> {
    state
        .clients
        .iter()
        .map(|server| {
            let status = server.connection_type().as_str();
            serde_json::json!({"name": server.name(), "status": status})
        })
        .collect()
}

fn sdk_api_key_source() -> &'static str {
    match crate::utils::auth::get_anthropic_api_key_with_source(
        crate::utils::auth::GetAnthropicApiKeyOptions {
            skip_retrieving_key_from_api_key_helper: true,
        },
    )
    .source
    {
        crate::utils::auth::ApiKeySource::None => "none",
        crate::utils::auth::ApiKeySource::AnthropicApiKey => "ANTHROPIC_API_KEY",
        crate::utils::auth::ApiKeySource::ApiKeyHelper => "apiKeyHelper",
    }
}

/// Maps to CC `utils/messages/systemInit.ts:53-95`
/// `buildSystemInitMessage(...)`.
pub(crate) fn build_system_init_message(
    context: &crate::tool::ToolUseContext,
) -> serde_json::Value {
    let cwd = context
        .cwd_override
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let tools = context
        .tools
        .iter()
        .map(|tool| {
            if tool.name == "Agent" {
                "Task".to_string()
            } else {
                tool.name.clone()
            }
        })
        .collect::<Vec<_>>();
    let mcp_servers = mcp_server_statuses(&context.mcp_state);
    let commands = context
        .commands
        .iter()
        .filter(|command| command.user_invocable)
        .map(|command| crate::commands::get_command_name(command).to_string())
        .collect::<Vec<_>>();
    let agents = context
        .agent_definitions
        .active_agents
        .iter()
        .map(|agent| agent.agent_type.clone())
        .collect::<Vec<_>>();
    let settings = crate::utils::settings::get_initial_settings();
    serde_json::json!({
        "type": "system",
        "subtype": "init",
        "cwd": cwd,
        "session_id": crate::bootstrap::state::get_session_id(),
        "tools": tools,
        "mcp_servers": mcp_servers,
        "model": context.main_loop_model.clone().unwrap_or_else(
            crate::utils::model::model::get_main_loop_model
        ),
        "permissionMode": crate::utils::permissions::permission_mode::to_external_permission_mode(
            context.tool_permission_context.mode
        ),
        "slash_commands": commands,
        "apiKeySource": sdk_api_key_source(),
        "betas": [],
        "claude_code_version": env!("CARGO_PKG_VERSION"),
        "output_style": settings.output_style.as_deref().unwrap_or(
            crate::constants::output_styles::DEFAULT_OUTPUT_STYLE_NAME
        ),
        "agents": agents,
        "skills": [],
        "plugins": [],
        "uuid": uuid::Uuid::new_v4().to_string(),
    })
}
