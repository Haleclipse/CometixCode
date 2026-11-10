//! MCP service-layer boundaries.
//! Maps to: CC `services/mcp/`.
//!
//! The UI slice consumes snapshot state, while the runtime slice maps CC's
//! `@modelcontextprotocol/sdk` client usage to the official Rust SDK
//! (`modelcontextprotocol/rust-sdk`, crate `rmcp`). OAuth/settings writes
//! still live in their official modules and are not hidden inside command/UI
//! code.

pub mod auth;
pub mod channel_allowlist;
pub mod channel_notification;
pub mod channel_permissions;
pub mod client;
pub mod config;
pub mod elicitation_handler;
pub mod env_expansion;
pub mod headers_helper;
pub mod in_process_transport;
pub mod mcp_connection_manager;
pub mod mcp_string_utils;
pub mod normalization;
pub mod oauth_port;
pub mod sdk_control_transport;
pub mod types;
pub mod use_manage_mcp_connections;
pub mod utils;
pub mod vscode_sdk_mcp;
pub mod xaa;
pub mod xaa_idp_login;
