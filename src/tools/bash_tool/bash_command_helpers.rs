//! Maps to: CC `tools/BashTool/bashCommandHelpers.ts`.
//!
//! Redirection stripping for textual permission-rule matching. Permission
//! aggregation itself consumes the verified security AST in `bash_permissions`.

/// Maps to CC local `buildSegmentWithoutRedirections(segmentCommand)`.
pub fn build_segment_without_redirections(segment_command: &str) -> String {
    if !segment_command.contains('>') {
        return segment_command.to_string();
    }
    crate::utils::bash::parsed_command::ParsedCommand::parse(segment_command)
        .map(|parsed| parsed.without_output_redirections())
        .unwrap_or_else(|| segment_command.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_matching_strips_only_output_redirections() {
        assert_eq!(
            build_segment_without_redirections("grep foo file.txt > out.txt"),
            "grep foo file.txt"
        );
        assert_eq!(build_segment_without_redirections("echo hi"), "echo hi");
        assert_eq!(
            build_segment_without_redirections("echo 'x > y'"),
            "echo 'x > y'"
        );
    }
}
