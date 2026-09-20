//! Service-layer owners mirroring official `src/services/`.
//!
//! Live API, quota, compact, hook, MCP, prompt-suggestion, and tool services sit
//! between `query` and REPL-owned state; retained components only consume their
//! typed snapshots/events.

pub mod analytics;
pub mod api;
pub mod claude_ai_limits;
pub mod compact;
pub mod context_collapse;
pub mod diagnostic_tracking;
pub mod hooks;
pub mod lsp;
pub mod mcp;
pub mod mcp_server_approval;
pub mod mock_rate_limits;
pub mod oauth;
pub mod policy_limits;
pub mod prompt_suggestion;
pub mod rate_limit_messages;
pub mod rate_limit_mocking;
pub mod team_memory_sync;
pub mod tips;
pub mod token_estimation;
pub mod tool_use_summary;
pub mod tools;

pub mod plugins;
pub mod proxy_channel;
