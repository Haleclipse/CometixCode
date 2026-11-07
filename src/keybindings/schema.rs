//! Maps to: CC `keybindings/schema.ts`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Valid user-configurable context names. This intentionally follows CC's
/// schema list; runtime-only Rust L1 contexts are not accepted in JSON config.
pub const KEYBINDING_CONTEXTS: &[&str] = &[
    "Global",
    "Chat",
    "Autocomplete",
    "Confirmation",
    "Help",
    "Transcript",
    "HistorySearch",
    "Task",
    "ThemePicker",
    "Settings",
    "Tabs",
    "Attachments",
    "Footer",
    "MessageSelector",
    "DiffDialog",
    "ModelPicker",
    "Select",
    "Plugin",
];

pub const KEYBINDING_CONTEXT_DESCRIPTIONS: &[(&str, &str)] = &[
    ("Global", "Active everywhere, regardless of focus"),
    ("Chat", "When the chat input is focused"),
    ("Autocomplete", "When autocomplete menu is visible"),
    (
        "Confirmation",
        "When a confirmation/permission dialog is shown",
    ),
    ("Help", "When the help overlay is open"),
    ("Transcript", "When viewing the transcript"),
    ("HistorySearch", "When searching command history (ctrl+r)"),
    ("Task", "When a task/agent is running in the foreground"),
    ("ThemePicker", "When the theme picker is open"),
    ("Settings", "When the settings menu is open"),
    ("Tabs", "When tab navigation is active"),
    (
        "Attachments",
        "When navigating image attachments in a select dialog",
    ),
    ("Footer", "When footer indicators are focused"),
    (
        "MessageSelector",
        "When the message selector (rewind) is open",
    ),
    ("DiffDialog", "When the diff dialog is open"),
    ("ModelPicker", "When the model picker is open"),
    ("Select", "When a select/list component is focused"),
    ("Plugin", "When the plugin dialog is open"),
];

pub const KEYBINDING_ACTIONS: &[&str] = &[
    "app:interrupt",
    "app:exit",
    "app:toggleTodos",
    "app:toggleTranscript",
    "app:toggleBrief",
    "app:toggleTeammatePreview",
    "app:toggleTerminal",
    "app:redraw",
    "app:globalSearch",
    "app:quickOpen",
    "history:search",
    "history:previous",
    "history:next",
    "chat:cancel",
    "chat:killAgents",
    "chat:cycleMode",
    "chat:modelPicker",
    "chat:submit",
    "chat:newline",
    "chat:undo",
    "chat:externalEditor",
    "chat:stash",
    "chat:imagePaste",
    "chat:messageActions",
    "autocomplete:accept",
    "autocomplete:dismiss",
    "autocomplete:previous",
    "autocomplete:next",
    "confirm:yes",
    "confirm:no",
    "confirm:previous",
    "confirm:next",
    "confirm:nextField",
    "confirm:previousField",
    "confirm:cycleMode",
    "confirm:toggle",
    "confirm:toggleExplanation",
    "tabs:next",
    "tabs:previous",
    "transcript:toggleShowAll",
    "transcript:exit",
    "historySearch:next",
    "historySearch:accept",
    "historySearch:cancel",
    "historySearch:execute",
    "task:background",
    "theme:toggleSyntaxHighlighting",
    "help:dismiss",
    "attachments:next",
    "attachments:previous",
    "attachments:remove",
    "attachments:exit",
    "footer:up",
    "footer:down",
    "footer:next",
    "footer:previous",
    "footer:openSelected",
    "footer:clearSelection",
    "footer:close",
    "messageSelector:up",
    "messageSelector:down",
    "messageSelector:top",
    "messageSelector:bottom",
    "messageSelector:select",
    "diff:dismiss",
    "diff:previousSource",
    "diff:nextSource",
    "diff:back",
    "diff:viewDetails",
    "diff:previousFile",
    "diff:nextFile",
    "modelPicker:decreaseEffort",
    "modelPicker:increaseEffort",
    "modelPicker:setDefault",
    "select:next",
    "select:previous",
    "select:accept",
    "select:cancel",
    "plugin:toggle",
    "plugin:install",
    "permission:toggleDebug",
    "settings:search",
    "settings:retry",
    "settings:close",
    "voice:pushToTalk",
];

/// Maps to: CC `KeybindingBlockSchema`'s inferred block type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeybindingSchemaBlock {
    pub context: String,
    pub bindings: BTreeMap<String, Option<String>>,
}

/// Maps to: CC `KeybindingsSchemaType`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeybindingsSchemaType {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(rename = "$docs", default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    pub bindings: Vec<KeybindingSchemaBlock>,
}

/// Rust schema-object projection of CC `KeybindingBlockSchema`.
pub struct KeybindingBlockSchema;

impl KeybindingBlockSchema {
    pub fn parse(value: serde_json::Value) -> Result<KeybindingSchemaBlock, String> {
        let block = serde_json::from_value::<KeybindingSchemaBlock>(value)
            .map_err(|error| error.to_string())?;
        if !is_valid_context(&block.context) {
            return Err(format!("Unknown context \"{}\"", block.context));
        }
        for (key, action) in &block.bindings {
            if let Some(action) = action
                && !is_valid_action(action)
            {
                return Err(format!("Invalid action \"{action}\" for \"{key}\""));
            }
        }
        Ok(block)
    }
}

/// Rust schema-object projection of CC `KeybindingsSchema`.
pub struct KeybindingsSchema;

impl KeybindingsSchema {
    pub fn parse(value: serde_json::Value) -> Result<KeybindingsSchemaType, String> {
        let config = serde_json::from_value::<KeybindingsSchemaType>(value)
            .map_err(|error| error.to_string())?;
        for block in &config.bindings {
            KeybindingBlockSchema::parse(
                serde_json::to_value(block).map_err(|error| error.to_string())?,
            )?;
        }
        Ok(config)
    }
}

pub fn is_valid_context(value: &str) -> bool {
    KEYBINDING_CONTEXTS.contains(&value)
}

pub fn is_valid_action(value: &str) -> bool {
    KEYBINDING_ACTIONS.contains(&value) || is_command_action(value)
}

pub fn is_command_action(value: &str) -> bool {
    let Some(command) = value.strip_prefix("command:") else {
        return false;
    };
    !command.is_empty()
        && command.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, ':' | '-' | '_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_context_and_action_catalogs_match_source_boundaries() {
        assert_eq!(KEYBINDING_CONTEXTS.first(), Some(&"Global"));
        assert_eq!(KEYBINDING_CONTEXTS.last(), Some(&"Plugin"));
        assert!(!is_valid_context("Scroll"));
        assert!(is_valid_action("chat:submit"));
        assert!(is_valid_action("command:compact"));
        assert!(!is_valid_action("command:bad name"));
    }

    #[test]
    fn schema_objects_accept_wrapper_null_unbind_and_command_actions() {
        let parsed = KeybindingsSchema::parse(serde_json::json!({
            "$schema": "https://example.test/keybindings.json",
            "$docs": "https://example.test/docs",
            "bindings": [{
                "context": "Chat",
                "bindings": {
                    "ctrl+k": "command:compact",
                    "ctrl+x": null
                }
            }]
        }))
        .unwrap();
        assert_eq!(parsed.bindings.len(), 1);
        assert_eq!(
            parsed.bindings[0].bindings.get("ctrl+k"),
            Some(&Some("command:compact".to_string()))
        );
        assert_eq!(parsed.bindings[0].bindings.get("ctrl+x"), Some(&None));
        assert!(
            KeybindingsSchema::parse(serde_json::json!({
                "bindings": [{"context": "Unknown", "bindings": {}}]
            }))
            .is_err()
        );
        assert!(
            KeybindingsSchema::parse(serde_json::json!({
                "bindings": [{"context": "Chat", "bindings": {"x": "not:an-action"}}]
            }))
            .is_err()
        );
    }

    #[test]
    fn every_context_has_one_description() {
        assert_eq!(
            KEYBINDING_CONTEXTS.len(),
            KEYBINDING_CONTEXT_DESCRIPTIONS.len()
        );
        for context in KEYBINDING_CONTEXTS {
            assert_eq!(
                KEYBINDING_CONTEXT_DESCRIPTIONS
                    .iter()
                    .filter(|(name, _)| name == context)
                    .count(),
                1
            );
        }
    }
}
