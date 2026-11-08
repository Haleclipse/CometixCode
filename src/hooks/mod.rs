//! Maps to: CC hooks/ (85 files) — reusable stateful logic.
//! CC's custom hooks encapsulate cross-cutting concerns:
//!   useDoublePress    — time-based double-tap detection
//!   useExitOnCtrlCD   — double Ctrl-C/D exit with confirmation
//!   useTypeahead      — slash command suggestions + navigation
//!   useSearchInput    — filtered list search state
//!   useElapsedTime    — timer display for spinners
//! iocraft equivalent: functions that take `&mut Hooks` and call
//! use_state/use_terminal_events/use_future internally, returning
//! state + handlers — same pattern as React custom hooks.

pub mod file_suggestions;
pub mod notifs;
pub mod render_placeholder;
pub mod tool_permission;
pub mod unified_suggestions;
pub mod use_arrow_key_history;
pub mod use_blink;
pub mod use_can_use_tool;
pub mod use_cancel_request;
pub mod use_command_keybindings;
pub mod use_command_queue;
pub mod use_credential_status;
pub mod use_diff_data;
pub mod use_double_press;
pub mod use_exit;
pub mod use_global_keybindings;
pub mod use_history_search;
pub mod use_ide_selection;
pub mod use_inbox_poller;
pub mod use_log_messages;
pub mod use_memory_usage;
pub mod use_merged_commands;
pub mod use_plugin_recommendation_base;
pub mod use_scheduled_tasks;
pub mod use_search_input;
pub mod use_settings_change;
pub mod use_swarm_permission_poller;
pub mod use_text_input;
pub mod use_turn_diffs;
pub mod use_typeahead;
pub mod use_update_notification;

pub mod use_manage_plugins;
