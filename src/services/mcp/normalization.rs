//! Maps to: CC `services/mcp/normalization.ts`. @nyan: no claude.ai connector
//! name prefix, so there is no underscore collapsing for it.

/// Maps to: CC `normalizeNameForMCP(name)`.
pub fn normalize_name_for_mcp(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_name_for_mcp_matches_official_rules() {
        assert_eq!(
            normalize_name_for_mcp("my server.example"),
            "my_server_example"
        );
        assert_eq!(normalize_name_for_mcp("abc-DEF_123"), "abc-DEF_123");
        assert_eq!(
            normalize_name_for_mcp("claude.ai  a..b  "),
            "claude_ai__a__b__"
        );
    }
}
