//! MCP server-instruction delta reconstruction.
//!
//! Maps to CC `utils/mcpInstructionsDelta.ts:1-133`.

use crate::types::message::Message;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpInstructionsDelta {
    pub added_names: Vec<String>,
    pub added_blocks: Vec<String>,
    pub removed_names: Vec<String>,
}

/// Maps to CC `utils/mcpInstructionsDelta.ts:29-44`
/// `isMcpInstructionsDeltaEnabled()`.
pub fn is_mcp_instructions_delta_enabled() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_MCP_INSTR_DELTA").as_deref(),
    ) {
        return true;
    }
    if crate::utils::env_utils::is_env_defined_falsy(
        crate::utils::process_env::var("CLAUDE_CODE_MCP_INSTR_DELTA").as_deref(),
    ) {
        return false;
    }
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    )
    // GrowthBook's external source default is false; no cohort is fabricated.
}

/// Maps to CC `utils/mcpInstructionsDelta.ts:46-133`
/// `getMcpInstructionsDelta(...)` for server-authored instructions.
pub fn get_mcp_instructions_delta(
    connected_names: &BTreeSet<String>,
    instruction_blocks: &BTreeMap<String, String>,
    messages: &[Message],
) -> Option<McpInstructionsDelta> {
    let mut announced = BTreeSet::new();
    for message in messages {
        let Message::Attachment(crate::types::message::AttachmentMessage {
            attachment:
                crate::types::message::Attachment::McpInstructionsDelta {
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

    let added = instruction_blocks
        .iter()
        .filter(|(name, _)| connected_names.contains(*name) && !announced.contains(*name))
        .map(|(name, instructions)| (name.clone(), format!("## {name}\n{instructions}")))
        .collect::<Vec<_>>();
    let removed_names = announced
        .into_iter()
        .filter(|name| !connected_names.contains(name))
        .collect::<Vec<_>>();
    if added.is_empty() && removed_names.is_empty() {
        return None;
    }
    Some(McpInstructionsDelta {
        added_names: added.iter().map(|(name, _)| name.clone()).collect(),
        added_blocks: added.into_iter().map(|(_, block)| block).collect(),
        removed_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_delta_reconstructs_added_and_disconnected_servers() {
        let prior = Message::Attachment(crate::types::message::AttachmentMessage::new(
            serde_json::json!({
                "type":"mcp_instructions_delta",
                "addedNames":["old"],
                "addedBlocks":["## old\nold instructions"],
                "removedNames":[]
            }),
        ));
        let connected = ["new".to_string()].into_iter().collect();
        let blocks = [("new".to_string(), "new instructions".to_string())]
            .into_iter()
            .collect();
        let delta = get_mcp_instructions_delta(&connected, &blocks, &[prior]).unwrap();
        assert_eq!(delta.added_names, vec!["new"]);
        assert_eq!(delta.added_blocks, vec!["## new\nnew instructions"]);
        assert_eq!(delta.removed_names, vec!["old"]);
    }
}
