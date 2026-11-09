//! Plugin utilities.
//!
//! Maps to: CC `utils/plugins/`.

pub mod git_availability;
pub mod hint_recommendation;
pub mod install_counts;
pub mod installed_plugins_manager;
pub mod load_plugin_agents;
pub mod load_plugin_hooks;
pub mod lsp_plugin_integration;
pub mod marketplace_helpers;
pub mod marketplace_manager;
pub mod mcp_plugin_integration;
pub mod mcpb_handler;
pub mod official_marketplace;
pub mod orphaned_plugin_filter;
pub mod plugin_directories;
pub mod plugin_identifier;
pub mod plugin_installation_helpers;
pub mod plugin_loader;
pub mod plugin_options_storage;
pub mod schemas;
pub mod validate_plugin;

pub mod add_dir_plugin_settings;
pub mod official_marketplace_gcs;

pub mod cache_utils;
pub mod dependency_resolver;
pub mod parse_marketplace_input;
pub mod plugin_flagging;
pub mod plugin_versioning;

pub mod zip_cache;

pub mod plugin_blocklist;

pub mod plugin_autoupdate;

pub mod plugin_startup_check;
pub mod refresh;

pub mod walk_plugin_markdown;

pub mod load_plugin_commands;
