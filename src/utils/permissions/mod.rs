//! Maps to: CC `utils/permissions/`.
//! Current nyan scope is UI-only/mock: updates are applied to an
//! in-memory `ToolPermissionContext` and are never persisted to settings or
//! session files.

pub mod auto_mode_state;
pub mod bash_classifier;
pub mod bypass_permissions_killswitch;
pub mod classifier_decision;
pub mod classifier_shared;
pub mod dangerous_patterns;
pub mod decision;
pub mod denial_tracking;
pub mod filesystem;
pub mod get_next_permission_mode;
pub mod path_validation;
pub mod permission_explainer;
pub mod permission_mode;
pub mod permission_prompt_tool_result_schema;
pub mod permission_result;
pub mod permission_rule;
pub mod permission_rule_parser;
pub mod permission_setup;
pub mod permission_update;
pub mod permission_update_schema;
pub mod permissions;
pub mod permissions_loader;
pub mod shadowed_rule_detection;
pub mod shell_rule_matching;
pub mod yolo_classifier;
