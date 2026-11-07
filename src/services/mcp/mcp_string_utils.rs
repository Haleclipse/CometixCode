//! Maps to: CC `services/mcp/mcpStringUtils.ts`.

use super::normalization::normalize_name_for_mcp;
use crate::types::tools::McpToolInfo;
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpNameInfo {
    pub server_name: String,
    pub tool_name: Option<String>,
}

/// Maps to: CC `mcpInfoFromString(toolString)`.
pub fn mcp_info_from_string(tool_string: &str) -> Option<McpNameInfo> {
    let mut parts = tool_string.split("__");
    let mcp_part = parts.next()?;
    let server_name = parts.next()?;
    if mcp_part != "mcp" || server_name.is_empty() {
        return None;
    }
    let rest = parts.collect::<Vec<_>>();
    let tool_name = (!rest.is_empty()).then(|| rest.join("__"));
    Some(McpNameInfo {
        server_name: server_name.to_string(),
        tool_name,
    })
}

/// Whether `spec` names the whole MCP server of `tool_name` — `mcp__server`
/// or `mcp__server__*` — as permission rules do (CC `toolMatchesRule`).
/// Agent `tools` / `disallowedTools` share this matching.
pub fn mcp_server_spec_matches(spec: &str, tool_name: &str) -> bool {
    let Some(spec_info) = mcp_info_from_string(spec) else {
        return false;
    };
    let Some(tool_info) = mcp_info_from_string(tool_name) else {
        return false;
    };
    matches!(spec_info.tool_name.as_deref(), None | Some("*"))
        && spec_info.server_name == tool_info.server_name
}

/// Maps to: CC `getMcpPrefix(serverName)`.
pub fn get_mcp_prefix(server_name: &str) -> String {
    format!("mcp__{}__", normalize_name_for_mcp(server_name))
}

/// Maps to: CC `buildMcpToolName(serverName, toolName)`.
pub fn build_mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "{}{}",
        get_mcp_prefix(server_name),
        normalize_name_for_mcp(tool_name)
    )
}

/// Maps to: CC `getToolNameForPermissionCheck(tool)`.
pub fn get_tool_name_for_permission_check<'a>(
    name: &'a str,
    mcp_info: Option<&McpToolInfo>,
) -> Cow<'a, str> {
    match mcp_info {
        Some(info) => Cow::Owned(build_mcp_tool_name(&info.server_name, &info.tool_name)),
        None => Cow::Borrowed(name),
    }
}

/// Maps to: CC `getMcpDisplayName(fullName, serverName)`.
pub fn get_mcp_display_name(full_name: &str, server_name: &str) -> String {
    let prefix = get_mcp_prefix(server_name);
    full_name.replacen(&prefix, "", 1)
}

/// Maps to: CC `extractMcpToolDisplayName(userFacingName)`.
pub fn extract_mcp_tool_display_name(user_facing_name: &str) -> String {
    let trimmed = user_facing_name.trim();
    let without_suffix = trimmed
        .strip_suffix("(MCP)")
        .map(str::trim_end)
        .unwrap_or(trimmed)
        .trim();

    if let Some(index) = without_suffix.find(" - ") {
        return without_suffix[index + 3..].trim().to_string();
    }

    without_suffix.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_string_utils_match_official_name_shapes() {
        assert_eq!(get_mcp_prefix("my server"), "mcp__my_server__");
        assert_eq!(
            build_mcp_tool_name("my server", "add comment"),
            "mcp__my_server__add_comment"
        );
        assert_eq!(
            get_mcp_display_name("mcp__my_server__add_comment", "my server"),
            "add_comment"
        );
        assert_eq!(
            extract_mcp_tool_display_name("github - Add comment to issue (MCP)"),
            "Add comment to issue"
        );
    }

    #[test]
    fn permission_check_name_prefers_mcp_info_over_the_display_name() {
        let info = McpToolInfo {
            server_name: "my server".to_string(),
            tool_name: "Write".to_string(),
        };

        assert_eq!(
            get_tool_name_for_permission_check("Write", Some(&info)),
            "mcp__my_server__Write"
        );
        assert_eq!(
            get_tool_name_for_permission_check("mcp__my_server__Write", Some(&info)),
            "mcp__my_server__Write"
        );
        assert_eq!(get_tool_name_for_permission_check("Write", None), "Write");
    }

    #[test]
    fn mcp_info_from_string_preserves_double_underscores_in_tool_name() {
        let info = mcp_info_from_string("mcp__server__tool__part").expect("info");
        assert_eq!(info.server_name, "server");
        assert_eq!(info.tool_name.as_deref(), Some("tool__part"));
        assert!(mcp_info_from_string("Write").is_none());
    }
}
