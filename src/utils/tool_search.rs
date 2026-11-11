//! Tool Search utilities.
//!
//! Maps to CC `utils/toolSearch.ts`. This module owns message-history scans and
//! request-time helpers for dynamic tool loading; tool metadata and prompts stay
//! in `tools/tool_search_tool/*`, and SDK projection stays in `utils/api.rs`.

use crate::types::message::{Message, ToolResultContentBlock, UserContent};
use std::collections::BTreeSet;

/// Maps to CC `utils/toolSearch.ts` `extractDiscoveredToolNames(...)`.
///
/// Scans user `tool_result` blocks and compact-boundary carry-forward metadata
/// so `services/api/request.rs` can include only discovered deferred tools in
/// the next request.
pub fn extract_discovered_tool_names(messages: &[Message]) -> BTreeSet<String> {
    let mut discovered = BTreeSet::new();

    for message in messages {
        if let Message::System(crate::types::message::SystemMessage::CompactBoundary {
            compact_metadata,
            ..
        }) = message
        {
            if let Some(metadata) = compact_metadata {
                discovered.extend(metadata.pre_compact_discovered_tools.iter().cloned());
            }
            continue;
        }

        let Message::User(user) = message else {
            continue;
        };
        for content in &user.content {
            let UserContent::ToolResult(result) = content else {
                continue;
            };
            for block in &result.content_blocks {
                if let ToolResultContentBlock::ToolReference { tool_name } = block {
                    discovered.insert(tool_name.clone());
                }
            }
        }
    }

    discovered
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredToolsDelta {
    pub added_names: Vec<String>,
    pub added_lines: Vec<String>,
    pub removed_names: Vec<String>,
}

/// Maps to CC `utils/toolSearch.ts:646-706` `getDeferredToolsDelta(...)`.
pub fn get_deferred_tools_delta(
    tools: &[crate::types::tools::Tool],
    messages: &[Message],
) -> Option<DeferredToolsDelta> {
    let mut announced = BTreeSet::new();
    for message in messages {
        let Message::Attachment(crate::types::message::AttachmentMessage {
            attachment:
                crate::types::message::Attachment::DeferredToolsDelta {
                    added_names,
                    removed_names,
                    ..
                },
            ..
        }) = message
        else {
            continue;
        };
        for name in added_names {
            announced.insert(name.clone());
        }
        for name in removed_names {
            announced.remove(name);
        }
    }

    let deferred = tools
        .iter()
        .filter(|tool| crate::tools::tool_search_tool::prompt::is_deferred_tool(tool))
        .collect::<Vec<_>>();
    let deferred_names = deferred
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    let pool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut added_names = deferred
        .iter()
        .filter(|tool| !announced.contains(&tool.name))
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let mut removed_names = announced
        .into_iter()
        .filter(|name| {
            !deferred_names.contains(name.as_str()) && !pool_names.contains(name.as_str())
        })
        .collect::<Vec<_>>();
    added_names.sort();
    removed_names.sort();
    if added_names.is_empty() && removed_names.is_empty() {
        return None;
    }
    Some(DeferredToolsDelta {
        // `formatDeferredToolLine` is exactly `tool.name` in this source.
        added_lines: added_names.clone(),
        added_names,
        removed_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::ToolUseId;
    use crate::types::message::{ToolResult, UserMessage};

    #[test]
    fn extract_discovered_tool_names_reads_tool_reference_blocks_like_official() {
        let messages = vec![
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![UserContent::Text("hello".to_string())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![UserContent::ToolResult(ToolResult {
                    tool_use_id: ToolUseId("toolu_search".to_string()),
                    content: "{}".to_string(),
                    is_error: false,
                    content_blocks: vec![
                        ToolResultContentBlock::ToolReference {
                            tool_name: "WebFetch".to_string(),
                        },
                        ToolResultContentBlock::ToolReference {
                            tool_name: "ReadMcpResourceTool".to_string(),
                        },
                    ],
                    tool_use_result: None,
                })],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];

        let discovered = extract_discovered_tool_names(&messages);

        assert_eq!(
            discovered,
            ["ReadMcpResourceTool".to_string(), "WebFetch".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn extract_discovered_tool_names_reads_compact_boundary_carry_forward() {
        let messages = vec![Message::System(
            crate::types::message::SystemMessage::compact_boundary(Some(
                crate::types::message::CompactMetadata {
                    pre_compact_discovered_tools: vec![
                        "WebFetch".to_string(),
                        "ReadMcpResourceTool".to_string(),
                    ],
                    ..Default::default()
                },
            )),
        )];

        let discovered = extract_discovered_tool_names(&messages);

        assert_eq!(
            discovered,
            ["ReadMcpResourceTool".to_string(), "WebFetch".to_string()]
                .into_iter()
                .collect()
        );
    }
}
