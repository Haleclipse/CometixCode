//! Service-layer owners mirroring official `src/services/`.
//!
//! Live API, compact, hook, MCP, prompt-suggestion, and tool services sit
//! between `query` and REPL-owned state; retained components only consume their
//! typed snapshots/events.

pub mod api;
/// @nyan: idle memory consolidation (memory 4.4 / auto-dream).
pub mod auto_dream;
pub mod cache_warmer;
pub mod compact;
pub mod context_collapse;
/// @nyan: quick classification backend for the four modules and host-runtime
/// decision points (docs/nyan/decision-provider.md).
pub mod decision;
pub mod diagnostic_tracking;
/// @nyan: turn-end and pre-compact memory extraction (memory 4.3).
pub mod extract_memories;
pub mod hooks;
pub mod lsp;
pub mod mcp;
pub mod mcp_server_approval;
/// @nyan: shadow-mode Decision Provider seam for the memory points (C4).
pub mod memory_decision;
pub mod policy_limits;
pub mod prompt_suggestion;
/// @nyan: provider-neutral request IR and adapters (provider redesign PR 4).
pub mod provider;
/// @nyan: shared Qwen3-Reranker client (retrieval 4.9, slice G0).
pub mod rerank;
/// @nyan: deterministic-first code retrieval library and optional semantic branch.
pub mod retrieval;
/// @nyan: shared Ready ⇄ Down(until) state machine for self-hosted services
/// and the Decision Provider backend (README §1.9).
pub mod service_health;
pub mod team_memory_sync;
pub mod tips;
pub mod token_estimation;
pub mod tool_use_summary;
pub mod tools;
/// @nyan: deterministic provider-backed web search and SSRF-safe fetch.
pub mod web;

pub mod plugins;
