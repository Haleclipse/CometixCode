//! Parsed Bash command abstraction.
//!
//! Maps to: CC `utils/bash/ParsedCommand.ts`. Tree-sitter builds preserve byte
//! ranges for pipes/redirections; the external fallback delegates to the
//! source-shaped legacy helpers in `commands.rs`.

use super::tree_sitter_analysis::{TreeSitterAnalysis, analyze_command};
use tree_sitter::Node;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputRedirection {
    pub target: String,
    pub operator: String,
}

pub trait IParsedCommand {
    fn original_command(&self) -> &str;
    fn get_pipe_segments(&self) -> Vec<String>;
    fn without_output_redirections(&self) -> String;
    fn get_output_redirections(&self) -> Vec<OutputRedirection>;
    fn get_tree_sitter_analysis(&self) -> Option<&TreeSitterAnalysis>;
}

/// Maps to CC `RegexParsedCommand_DEPRECATED`.
#[derive(Clone, Debug)]
pub struct RegexParsedCommandDeprecated {
    original_command: String,
}

impl RegexParsedCommandDeprecated {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            original_command: command.into(),
        }
    }
}

impl IParsedCommand for RegexParsedCommandDeprecated {
    fn original_command(&self) -> &str {
        &self.original_command
    }

    fn get_pipe_segments(&self) -> Vec<String> {
        let parts = super::commands::split_command_with_operators(&self.original_command);
        let mut segments = Vec::new();
        let mut current = Vec::new();
        for part in parts {
            if part == "|" {
                if !current.is_empty() {
                    segments.push(std::mem::take(&mut current).join(" "));
                }
            } else {
                current.push(part);
            }
        }
        if !current.is_empty() {
            segments.push(current.join(" "));
        }
        if segments.is_empty() {
            vec![self.original_command.clone()]
        } else {
            segments
        }
    }

    fn without_output_redirections(&self) -> String {
        if !self.original_command.contains('>') {
            return self.original_command.clone();
        }
        let extracted = super::commands::extract_output_redirections(&self.original_command);
        if extracted.redirections.is_empty() {
            self.original_command.clone()
        } else {
            extracted.command_without_redirections
        }
    }

    fn get_output_redirections(&self) -> Vec<OutputRedirection> {
        super::commands::extract_output_redirections(&self.original_command).redirections
    }

    fn get_tree_sitter_analysis(&self) -> Option<&TreeSitterAnalysis> {
        None
    }
}

#[derive(Clone, Debug)]
struct RedirectionNode {
    output: OutputRedirection,
    start_byte: usize,
    end_byte: usize,
}

#[derive(Clone, Debug)]
struct TreeSitterParsedCommand {
    original_command: String,
    pipe_positions: Vec<usize>,
    redirection_nodes: Vec<RedirectionNode>,
    tree_sitter_analysis: TreeSitterAnalysis,
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text<'a>(node: Node<'_>, command: &'a str) -> Option<&'a str> {
    command.get(node.start_byte()..node.end_byte())
}

fn visit_nodes(node: Node<'_>, visitor: &mut impl FnMut(Node<'_>)) {
    visitor(node);
    for child in children(node) {
        visit_nodes(child, visitor);
    }
}

fn extract_pipe_positions(root: Node<'_>) -> Vec<usize> {
    let mut positions = Vec::new();
    visit_nodes(root, &mut |node| {
        if node.kind() == "pipeline" {
            positions.extend(
                children(node)
                    .into_iter()
                    .filter(|child| child.kind() == "|")
                    .map(|child| child.start_byte()),
            );
        }
    });
    positions.sort_unstable();
    positions
}

fn extract_redirection_nodes(root: Node<'_>, command: &str) -> Vec<RedirectionNode> {
    let mut redirections = Vec::new();
    visit_nodes(root, &mut |node| {
        if node.kind() != "file_redirect" {
            return;
        }
        let node_children = children(node);
        let operator = node_children
            .iter()
            .find(|child| matches!(child.kind(), ">" | ">>"));
        let target = node_children.iter().find(|child| child.kind() == "word");
        if let (Some(operator), Some(target)) = (operator, target) {
            redirections.push(RedirectionNode {
                output: OutputRedirection {
                    target: node_text(*target, command).unwrap_or_default().to_string(),
                    operator: operator.kind().to_string(),
                },
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
    });
    redirections
}

impl IParsedCommand for TreeSitterParsedCommand {
    fn original_command(&self) -> &str {
        &self.original_command
    }

    fn get_pipe_segments(&self) -> Vec<String> {
        if self.pipe_positions.is_empty() {
            return vec![self.original_command.clone()];
        }
        let bytes = self.original_command.as_bytes();
        let mut segments = Vec::new();
        let mut start = 0usize;
        for position in &self.pipe_positions {
            if let Ok(segment) = std::str::from_utf8(&bytes[start..*position]) {
                let segment = segment.trim();
                if !segment.is_empty() {
                    segments.push(segment.to_string());
                }
            }
            start = position.saturating_add(1);
        }
        if let Ok(segment) = std::str::from_utf8(&bytes[start..]) {
            let segment = segment.trim();
            if !segment.is_empty() {
                segments.push(segment.to_string());
            }
        }
        segments
    }

    fn without_output_redirections(&self) -> String {
        if self.redirection_nodes.is_empty() {
            return self.original_command.clone();
        }
        let mut sorted = self.redirection_nodes.clone();
        sorted.sort_unstable_by(|left, right| right.start_byte.cmp(&left.start_byte));
        let mut result = self.original_command.clone();
        for redirection in sorted {
            if redirection.start_byte <= redirection.end_byte
                && redirection.end_byte <= result.len()
                && result.is_char_boundary(redirection.start_byte)
                && result.is_char_boundary(redirection.end_byte)
            {
                result.replace_range(redirection.start_byte..redirection.end_byte, "");
            }
        }
        result.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn get_output_redirections(&self) -> Vec<OutputRedirection> {
        self.redirection_nodes
            .iter()
            .map(|redirection| redirection.output.clone())
            .collect()
    }

    fn get_tree_sitter_analysis(&self) -> Option<&TreeSitterAnalysis> {
        Some(&self.tree_sitter_analysis)
    }
}

/// Maps to CC `buildParsedCommandFromRoot(command, root)`.
pub fn build_parsed_command_from_root(command: &str, root: Node<'_>) -> Box<dyn IParsedCommand> {
    Box::new(TreeSitterParsedCommand {
        original_command: command.to_string(),
        pipe_positions: extract_pipe_positions(root),
        redirection_nodes: extract_redirection_nodes(root, command),
        tree_sitter_analysis: analyze_command(root, command),
    })
}

/// Maps to the CC `ParsedCommand` singleton.
pub struct ParsedCommand;

impl ParsedCommand {
    /// Maps to CC `ParsedCommand.parse(command)`.
    pub fn parse(command: &str) -> Option<Box<dyn IParsedCommand>> {
        if command.is_empty() {
            return None;
        }
        if crate::utils::build_profile::build_audience().is_internal()
            && let super::parser::ParseCommandRawResult::Parsed(tree) =
                super::parser::parse_command_raw(command)
        {
            return Some(build_parsed_command_from_root(command, tree.root_node()));
        }
        Some(Box::new(RegexParsedCommandDeprecated::new(command)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_fallback_segments_and_removes_output_redirection() {
        let parsed = RegexParsedCommandDeprecated::new("printf 'a|b' | grep a > out");
        assert_eq!(parsed.get_pipe_segments(), ["printf 'a|b'", "grep a > out"]);
        assert_eq!(
            parsed.without_output_redirections(),
            "printf \"a|b\" | grep a"
        );
        assert_eq!(
            parsed.get_output_redirections(),
            [OutputRedirection {
                target: "out".to_string(),
                operator: ">".to_string(),
            }]
        );
    }

    #[test]
    fn tree_sitter_builder_uses_utf8_byte_offsets() {
        let command = "printf '—' | cat > out";
        let tree = crate::utils::bash::bash_parser::parse(command)
            .expect("parser")
            .expect("tree");
        let parsed = build_parsed_command_from_root(command, tree.root_node());
        assert_eq!(parsed.get_pipe_segments(), ["printf '—'", "cat > out"]);
        assert_eq!(parsed.without_output_redirections(), "printf '—' | cat");
    }
}
