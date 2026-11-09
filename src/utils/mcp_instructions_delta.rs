//! MCP server-instruction delta reconstruction.
//!
//! Maps to CC `utils/mcpInstructionsDelta.ts:1-130`.

use crate::services::mcp::types::McpServerConnection;
use crate::types::message::Message;
use std::collections::BTreeSet;

/// Maps to CC `utils/mcpInstructionsDelta.ts:10-16` `McpInstructionsDelta`.
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
        crate::utils::process_env::var("NYAN_MCP_INSTR_DELTA").as_deref(),
    ) {
        return true;
    }
    if crate::utils::env_utils::is_env_defined_falsy(
        crate::utils::process_env::var("NYAN_MCP_INSTR_DELTA").as_deref(),
    ) {
        return false;
    }
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    )
    // GrowthBook's external source default is false; no cohort is fabricated.
}

/// Maps to CC `utils/mcpInstructionsDelta.ts:46-130`
/// `getMcpInstructionsDelta(mcpClients, messages, clientSideInstructions)`:
/// the connected servers with instructions, diffed by name against what the
/// conversation has announced.
///
/// @nyan: no client-side instructions. CC's only producer was the Claude in
/// Chrome ToolSearch hint, which nyan does not ship.
pub fn get_mcp_instructions_delta(
    mcp_clients: &[McpServerConnection],
    messages: &[Message],
) -> Option<McpInstructionsDelta> {
    let mut announced = BTreeSet::new();
    for message in messages {
        let Message::Attachment(attachment) = message else {
            continue;
        };
        let crate::types::message::Attachment::McpInstructionsDelta {
            added_names,
            removed_names,
            ..
        } = &attachment.attachment
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

    let connected = mcp_clients
        .iter()
        .filter_map(McpServerConnection::as_connected)
        .collect::<Vec<_>>();
    let connected_names = connected
        .iter()
        .map(|client| client.name.as_str())
        .collect::<std::collections::HashSet<_>>();

    let mut added = connected
        .iter()
        .filter_map(|client| {
            let instructions = client.instructions.as_deref().filter(|s| !s.is_empty())?;
            Some((
                client.name.as_str(),
                format!("## {}\n{instructions}", client.name),
            ))
        })
        .filter(|(name, _)| !announced.contains(*name))
        .collect::<Vec<_>>();
    let mut removed_names = announced
        .into_iter()
        .filter(|name| !connected_names.contains(name.as_str()))
        .collect::<Vec<_>>();
    if added.is_empty() && removed_names.is_empty() {
        return None;
    }

    added.sort_by(|(a, _), (b, _)| crate::tools::grep_tool::javascript_locale_compare(a, b));
    // `Array.prototype.sort()` with no comparator: UTF-16 code unit order.
    removed_names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Some(McpInstructionsDelta {
        added_names: added.iter().map(|(name, _)| name.to_string()).collect(),
        added_blocks: added.into_iter().map(|(_, block)| block).collect(),
        removed_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announced(added: &[&str], removed: &[&str]) -> Message {
        Message::Attachment(crate::types::message::AttachmentMessage::new(
            serde_json::json!({
                "type": "mcp_instructions_delta",
                "addedNames": added,
                "addedBlocks": added.iter().map(|name| format!("## {name}\n")).collect::<Vec<_>>(),
                "removedNames": removed,
            }),
        ))
    }

    fn stdio(command: &str) -> crate::services::mcp::types::ScopedMcpServerConfig {
        crate::services::mcp::types::ScopedMcpServerConfig::stdio_for_test(command)
    }

    #[test]
    fn instruction_delta_reconstructs_added_and_disconnected_servers() {
        let clients = [McpServerConnection::connected_with_instructions_for_test(
            "new",
            "new instructions",
        )];
        let delta = get_mcp_instructions_delta(&clients, &[announced(&["old"], &[])]).unwrap();
        assert_eq!(delta.added_names, vec!["new"]);
        assert_eq!(delta.added_blocks, vec!["## new\nnew instructions"]);
        assert_eq!(delta.removed_names, vec!["old"]);
    }

    /// CC `:72-82`: only connected records with non-empty instructions
    /// announce.
    #[test]
    fn instruction_delta_reads_only_connected_records_with_instructions() {
        let clients = [
            McpServerConnection::connected_with_instructions_for_test("docs", "Read first."),
            McpServerConnection::connected_for_test("bare", stdio("bare")),
            McpServerConnection::connected_with_instructions_for_test("empty", ""),
            McpServerConnection::failed("down", stdio("down"), None),
        ];
        let delta = get_mcp_instructions_delta(&clients, &[]).unwrap();
        assert_eq!(delta.added_names, vec!["docs"]);
        assert_eq!(delta.added_blocks, vec!["## docs\nRead first."]);
        assert!(delta.removed_names.is_empty());
    }

    /// CC `:124-129`: added names sort with `localeCompare`, removed names
    /// with the default sort (UTF-16 code units, so `Z` before `a`).
    #[test]
    fn instruction_delta_sorts_added_by_locale_and_removed_by_code_unit() {
        let clients = [
            McpServerConnection::connected_with_instructions_for_test("Beta", "b"),
            McpServerConnection::connected_with_instructions_for_test("alpha", "a"),
        ];
        let delta =
            get_mcp_instructions_delta(&clients, &[announced(&["zed", "Zed"], &[])]).unwrap();
        assert_eq!(delta.added_names, vec!["alpha", "Beta"]);
        assert_eq!(delta.removed_names, vec!["Zed", "zed"]);

        // Nothing new and nothing gone: no delta.
        let settled = [announced(&["alpha", "Beta"], &[])];
        assert_eq!(get_mcp_instructions_delta(&clients, &settled), None);
    }

    /// CC `:63-71`: a later removal takes the name out of the announced set,
    /// so a server that has reconnected is announced again.
    #[test]
    fn instruction_delta_announces_a_reconnected_server_again() {
        let clients = [McpServerConnection::connected_with_instructions_for_test(
            "docs",
            "Read first.",
        )];
        let history = [announced(&["docs"], &[]), announced(&[], &["docs"])];
        let delta = get_mcp_instructions_delta(&clients, &history).unwrap();
        assert_eq!(delta.added_names, vec!["docs"]);
        assert!(delta.removed_names.is_empty());
    }
}
