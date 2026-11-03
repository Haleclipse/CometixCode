//! Claude.ai managed MCP connector bookkeeping.
//! Maps to: CC `services/mcp/claudeai.ts`.
//!
//! The connector discovery (`fetchClaudeAIMcpConfigsIfEligible`) authenticated
//! with the claude.ai OAuth token and fetched `/v1/mcp_servers`. Anthropic is
//! API-key only, so there is no token and the discovery is removed; only the
//! "ever connected" bookkeeping for already-configured connectors remains.

/// Maps to: CC `services/mcp/claudeai.ts#markClaudeAiMcpConnected`.
pub fn mark_claude_ai_mcp_connected(name: &str) -> anyhow::Result<()> {
    crate::utils::config::save_global_config(|config| {
        let seen = config
            .claude_ai_mcp_ever_connected
            .get_or_insert_with(Vec::new);
        if !seen.iter().any(|existing| existing == name) {
            seen.push(name.to_string());
        }
    })
}

/// Maps to: CC `services/mcp/claudeai.ts#hasClaudeAiMcpEverConnected`.
pub fn has_claude_ai_mcp_ever_connected(name: &str) -> bool {
    crate::utils::config::load_global_config()
        .claude_ai_mcp_ever_connected
        .unwrap_or_default()
        .iter()
        .any(|existing| existing == name)
}
