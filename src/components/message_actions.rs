//! Maps to: CC `components/messageActions.tsx`.
//!
//! Official message actions own navigability classification, copy text
//! extraction, action labels, and the footer bar. React keybinding/ref wiring is
//! intentionally not duplicated here; fullscreen integration can call these
//! pure helpers and render `MessageActionsBar` at the official boundary.

use crate::constants::figures;
use crate::utils::messages::{
    INTERRUPT_MESSAGE, INTERRUPT_MESSAGE_FOR_TOOL_USE, NO_RESPONSE_REQUESTED, is_empty_message_text,
};
use iocraft::prelude::*;
use serde_json::Value;
const CANCEL_MESSAGE: &str = "The user doesn't want to take this action right now. STOP what you are doing and wait for the user to tell you how to proceed.";
const REJECT_MESSAGE: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";

/// Maps to CC `MessageActionsSelectedContext` for nested message leaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageActionsSelectedContext(pub bool);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigableType {
    User,
    Assistant,
    GroupedToolUse,
    CollapsedReadSearch,
    System,
    Attachment,
}

impl NavigableType {
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::GroupedToolUse => "grouped_tool_use",
            Self::CollapsedReadSearch => "collapsed_read_search",
            Self::System => "system",
            Self::Attachment => "attachment",
        }
    }
}

/// Maps to: CC `messageActions.tsx#NAVIGABLE_TYPES`.
pub const NAVIGABLE_TYPES: &[NavigableType] = &[
    NavigableType::User,
    NavigableType::Assistant,
    NavigableType::GroupedToolUse,
    NavigableType::CollapsedReadSearch,
    NavigableType::System,
    NavigableType::Attachment,
];

#[derive(Clone, Debug, PartialEq)]
pub enum NavigableMessage {
    User {
        uuid: String,
        text: String,
        is_meta: bool,
        is_compact_summary: bool,
    },
    AssistantText {
        uuid: String,
        text: String,
    },
    AssistantToolUse {
        uuid: String,
        name: String,
        input: Value,
    },
    GroupedToolUse {
        uuid: String,
        tool_name: String,
        input: Value,
        results: Vec<String>,
    },
    CollapsedReadSearch {
        uuid: String,
        results: Vec<String>,
    },
    System {
        uuid: String,
        subtype: String,
        content: Option<String>,
        error: Option<String>,
    },
    Attachment {
        uuid: String,
        attachment_type: String,
        prompt: Option<String>,
    },
}

impl NavigableMessage {
    pub fn navigable_type(&self) -> NavigableType {
        match self {
            Self::User { .. } => NavigableType::User,
            Self::AssistantText { .. } | Self::AssistantToolUse { .. } => NavigableType::Assistant,
            Self::GroupedToolUse { .. } => NavigableType::GroupedToolUse,
            Self::CollapsedReadSearch { .. } => NavigableType::CollapsedReadSearch,
            Self::System { .. } => NavigableType::System,
            Self::Attachment { .. } => NavigableType::Attachment,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MessageToolCall {
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimaryInput {
    pub tool_name: &'static str,
    pub label: &'static str,
    pub field: &'static str,
}

/// Maps to: CC `messageActions.tsx#PRIMARY_INPUT`.
pub const PRIMARY_INPUTS: &[PrimaryInput] = &[
    PrimaryInput {
        tool_name: "Read",
        label: "path",
        field: "file_path",
    },
    PrimaryInput {
        tool_name: "Edit",
        label: "path",
        field: "file_path",
    },
    PrimaryInput {
        tool_name: "Write",
        label: "path",
        field: "file_path",
    },
    PrimaryInput {
        tool_name: "NotebookEdit",
        label: "path",
        field: "notebook_path",
    },
    PrimaryInput {
        tool_name: "Bash",
        label: "command",
        field: "command",
    },
    PrimaryInput {
        tool_name: "Grep",
        label: "pattern",
        field: "pattern",
    },
    PrimaryInput {
        tool_name: "Glob",
        label: "pattern",
        field: "pattern",
    },
    PrimaryInput {
        tool_name: "WebFetch",
        label: "url",
        field: "url",
    },
    PrimaryInput {
        tool_name: "WebSearch",
        label: "query",
        field: "query",
    },
    PrimaryInput {
        tool_name: "Task",
        label: "prompt",
        field: "prompt",
    },
    PrimaryInput {
        tool_name: "Agent",
        label: "prompt",
        field: "prompt",
    },
    PrimaryInput {
        tool_name: "Tmux",
        label: "command",
        field: "args",
    },
];

pub fn primary_input_for_tool(tool_name: &str) -> Option<PrimaryInput> {
    PRIMARY_INPUTS
        .iter()
        .copied()
        .find(|input| input.tool_name == tool_name)
}

fn extract_primary_input(primary: PrimaryInput, input: &Value) -> Option<String> {
    if primary.tool_name == "Tmux" {
        return input.get("args").and_then(|args| {
            args.as_array().map(|items| {
                format!(
                    "tmux {}",
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })
        });
    }
    input
        .get(primary.field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn is_synthetic_message(text: &str) -> bool {
    matches!(
        text,
        INTERRUPT_MESSAGE
            | INTERRUPT_MESSAGE_FOR_TOOL_USE
            | CANCEL_MESSAGE
            | REJECT_MESSAGE
            | NO_RESPONSE_REQUESTED
    )
}

/// Maps to: CC `messageActions.tsx#stripSystemReminders`.
pub fn strip_system_reminders(text: &str) -> String {
    const CLOSE: &str = "</system-reminder>";
    let mut t = text.trim_start().to_string();
    while t.starts_with("<system-reminder>") {
        let Some(end) = t.find(CLOSE) else {
            break;
        };
        t = t[end + CLOSE.len()..].trim_start().to_string();
    }
    t
}

/// Maps to: CC `messageActions.tsx#toolCallOf`.
pub fn tool_call_of(msg: &NavigableMessage) -> Option<MessageToolCall> {
    match msg {
        NavigableMessage::AssistantToolUse { name, input, .. } => Some(MessageToolCall {
            name: name.clone(),
            input: input.clone(),
        }),
        NavigableMessage::GroupedToolUse {
            tool_name, input, ..
        } => Some(MessageToolCall {
            name: tool_name.clone(),
            input: input.clone(),
        }),
        _ => None,
    }
}

/// Maps to: CC `messageActions.tsx#isNavigableMessage`.
pub fn is_navigable_message(msg: &NavigableMessage) -> bool {
    match msg {
        NavigableMessage::AssistantText { text, .. } => {
            !is_empty_message_text(text) && !is_synthetic_message(text)
        }
        NavigableMessage::AssistantToolUse { name, .. } => primary_input_for_tool(name).is_some(),
        NavigableMessage::User {
            text,
            is_meta,
            is_compact_summary,
            ..
        } => {
            !*is_meta
                && !*is_compact_summary
                && !is_synthetic_message(text)
                && !strip_system_reminders(text).starts_with('<')
        }
        NavigableMessage::System { subtype, .. } => !matches!(
            subtype.as_str(),
            "api_metrics"
                | "stop_hook_summary"
                | "turn_duration"
                | "memory_saved"
                | "agents_killed"
                | "away_summary"
                | "thinking"
        ),
        NavigableMessage::GroupedToolUse { .. } | NavigableMessage::CollapsedReadSearch { .. } => {
            true
        }
        NavigableMessage::Attachment {
            attachment_type, ..
        } => matches!(
            attachment_type.as_str(),
            "queued_command"
                | "diagnostics"
                | "hook_blocking_error"
                | "hook_error_during_execution"
        ),
    }
}

/// Maps to: CC `messageActions.tsx#copyTextOf`.
pub fn copy_text_of(msg: &NavigableMessage) -> String {
    match msg {
        NavigableMessage::User { text, .. } => strip_system_reminders(text),
        NavigableMessage::AssistantText { text, .. } => text.clone(),
        NavigableMessage::AssistantToolUse { .. } => tool_call_of(msg)
            .and_then(|tc| {
                primary_input_for_tool(&tc.name).and_then(|p| extract_primary_input(p, &tc.input))
            })
            .unwrap_or_default(),
        NavigableMessage::GroupedToolUse { results, .. }
        | NavigableMessage::CollapsedReadSearch { results, .. } => results
            .iter()
            .filter(|text| !text.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n"),
        NavigableMessage::System {
            subtype,
            content,
            error,
            ..
        } => content
            .clone()
            .or_else(|| error.clone())
            .unwrap_or_else(|| subtype.clone()),
        NavigableMessage::Attachment {
            attachment_type,
            prompt,
            ..
        } => {
            if attachment_type == "queued_command" {
                prompt.clone().unwrap_or_default()
            } else {
                format!("[{attachment_type}]")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageActionsState {
    pub uuid: String,
    pub msg_type: NavigableType,
    pub expanded: bool,
    pub tool_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageActionDef {
    pub key: &'static str,
    pub base_label: &'static str,
    pub stays: bool,
}

/// Maps to: CC `messageActions.tsx#MESSAGE_ACTIONS` labels/applicability.
pub fn applicable_message_actions(cursor: &MessageActionsState) -> Vec<MessageActionDef> {
    let mut actions = Vec::new();
    if matches!(
        cursor.msg_type,
        NavigableType::GroupedToolUse
            | NavigableType::CollapsedReadSearch
            | NavigableType::Attachment
            | NavigableType::System
    ) {
        actions.push(MessageActionDef {
            key: "enter",
            base_label: if cursor.expanded {
                "collapse"
            } else {
                "expand"
            },
            stays: true,
        });
    }
    if cursor.msg_type == NavigableType::User {
        actions.push(MessageActionDef {
            key: "enter",
            base_label: "edit",
            stays: false,
        });
    }
    if NAVIGABLE_TYPES.contains(&cursor.msg_type) {
        actions.push(MessageActionDef {
            key: "c",
            base_label: "copy",
            stays: false,
        });
    }
    if matches!(
        cursor.msg_type,
        NavigableType::GroupedToolUse | NavigableType::Assistant
    ) && let Some(tool_name) = cursor.tool_name.as_deref()
        && let Some(primary) = primary_input_for_tool(tool_name)
    {
        actions.push(MessageActionDef {
            key: "p",
            base_label: primary.label,
            stays: false,
        });
    }
    actions
}

pub fn message_action_label(action: MessageActionDef) -> String {
    if action.key == "p" {
        format!("copy {}", action.base_label)
    } else {
        action.base_label.to_string()
    }
}

#[derive(Default, Props)]
pub struct MessageActionsBarProps {
    pub cursor: Option<MessageActionsState>,
}

/// Maps to: CC `components/messageActions.tsx#MessageActionsBar`.
#[component]
pub fn MessageActionsBar(props: &MessageActionsBarProps) -> impl Into<AnyElement<'static>> {
    let Some(cursor) = props.cursor.clone() else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };
    let actions = applicable_message_actions(&cursor);
    let fig = figures::get();

    element! {
        View(flex_direction: FlexDirection::Column, flex_shrink: 0.0f32, padding_top: 1u32, padding_bottom: 1u32) {
            View(border_style: BorderStyle::Single, border_top: true, border_bottom: false, border_left: false, border_right: false, border_color: Color::Grey) {}
            View(flex_direction: FlexDirection::Row, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                #(actions.into_iter().enumerate().flat_map(|(index, action)| {
                    let mut row = Vec::<AnyElement<'static>>::new();
                    if index > 0 {
                        row.push(element! { Text(content: " · ".to_string(), dim: true, wrap: TextWrap::NoWrap) }.into_any());
                    }
                    row.push(element! { Text(content: action.key.to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap) }.into_any());
                    row.push(element! { Text(content: format!(" {}", message_action_label(action)), dim: true, wrap: TextWrap::NoWrap) }.into_any());
                    row
                }).collect::<Vec<_>>())
                Text(content: " · ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                Text(content: format!("{}{}", fig.arrow_up, fig.arrow_down), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(content: " navigate · ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                Text(content: "esc".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(content: " back".to_string(), dim: true, wrap: TextWrap::NoWrap)
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, input: Value) -> NavigableMessage {
        NavigableMessage::AssistantToolUse {
            uuid: "a".to_string(),
            name: name.to_string(),
            input,
        }
    }

    #[test]
    fn message_actions_strip_system_reminders_matches_official_loop() {
        assert_eq!(
            strip_system_reminders(
                " <system-reminder>one</system-reminder>\n<system-reminder>two</system-reminder>Ask"
            ),
            "Ask"
        );
        assert_eq!(
            strip_system_reminders("<system-reminder>unterminated"),
            "<system-reminder>unterminated"
        );
    }

    #[test]
    fn message_actions_primary_input_matches_official_tools() {
        assert_eq!(primary_input_for_tool("Read").unwrap().label, "path");
        assert_eq!(primary_input_for_tool("Bash").unwrap().field, "command");
        assert_eq!(primary_input_for_tool("Tmux").unwrap().label, "command");
        assert!(primary_input_for_tool("Unknown").is_none());

        assert_eq!(
            copy_text_of(&tool(
                "Read",
                serde_json::json!({ "file_path": "/tmp/a.rs" })
            )),
            "/tmp/a.rs"
        );
        assert_eq!(
            copy_text_of(&tool("Tmux", serde_json::json!({ "args": ["ls", "-la"] }))),
            "tmux ls -la"
        );
    }

    #[test]
    fn message_actions_navigable_filter_matches_official_blocklists() {
        assert!(!is_navigable_message(&NavigableMessage::AssistantText {
            uuid: "a".to_string(),
            text: NO_RESPONSE_REQUESTED.to_string(),
        }));
        assert!(is_navigable_message(&NavigableMessage::AssistantText {
            uuid: "a".to_string(),
            text: "hello".to_string(),
        }));
        assert!(!is_navigable_message(&NavigableMessage::User {
            uuid: "u".to_string(),
            text: "<command-message>synthetic</command-message>".to_string(),
            is_meta: false,
            is_compact_summary: false,
        }));
        assert!(!is_navigable_message(&NavigableMessage::System {
            uuid: "s".to_string(),
            subtype: "turn_duration".to_string(),
            content: None,
            error: None,
        }));
        assert!(is_navigable_message(&NavigableMessage::Attachment {
            uuid: "att".to_string(),
            attachment_type: "queued_command".to_string(),
            prompt: Some("do it".to_string()),
        }));
    }

    #[test]
    fn message_actions_copy_text_matches_official_shapes() {
        assert_eq!(
            copy_text_of(&NavigableMessage::User {
                uuid: "u".to_string(),
                text: "<system-reminder>one</system-reminder>actual".to_string(),
                is_meta: false,
                is_compact_summary: false,
            }),
            "actual"
        );
        assert_eq!(
            copy_text_of(&NavigableMessage::GroupedToolUse {
                uuid: "g".to_string(),
                tool_name: "Agent".to_string(),
                input: serde_json::json!({ "prompt": "check" }),
                results: vec!["one".to_string(), "".to_string(), "two".to_string()],
            }),
            "one\n\ntwo"
        );
        assert_eq!(
            copy_text_of(&NavigableMessage::Attachment {
                uuid: "a".to_string(),
                attachment_type: "diagnostics".to_string(),
                prompt: None,
            }),
            "[diagnostics]"
        );
    }

    #[test]
    fn message_actions_applicable_labels_match_official_order() {
        let user = MessageActionsState {
            uuid: "u".to_string(),
            msg_type: NavigableType::User,
            expanded: false,
            tool_name: None,
        };
        let labels = applicable_message_actions(&user)
            .into_iter()
            .map(message_action_label)
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["edit", "copy"]);

        let tool = MessageActionsState {
            uuid: "a".to_string(),
            msg_type: NavigableType::Assistant,
            expanded: false,
            tool_name: Some("Bash".to_string()),
        };
        let labels = applicable_message_actions(&tool)
            .into_iter()
            .map(message_action_label)
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["copy", "copy command"]);
    }

    #[test]
    fn message_actions_bar_renders_action_hints_and_navigation() {
        let text = element! {
            MessageActionsBar(cursor: Some(MessageActionsState {
                uuid: "a".to_string(),
                msg_type: NavigableType::Assistant,
                expanded: false,
                tool_name: Some("Read".to_string()),
            }))
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("c copy"), "canvas=\n{text}");
        assert!(text.contains("p copy path"), "canvas=\n{text}");
        assert!(text.contains("↑↓ navigate"), "canvas=\n{text}");
        assert!(text.contains("esc back"), "canvas=\n{text}");
    }
}
