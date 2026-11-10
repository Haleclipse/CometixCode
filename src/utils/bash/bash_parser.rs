//! Native tree-sitter Bash parser loader/runtime.
//!
//! Maps to: CC `utils/bash/bashParser.ts` parser initialization,
//! `SHELL_KEYWORDS`, `PARSE_TIMEOUT_MICROS`, and `MAX_NODES`.
//!
//! Rust links `tree-sitter-bash` directly for every build; security consumers
//! fail closed if parsing exceeds its time, node, or depth budget.

use tree_sitter::{Node, Parser, Tree};

pub const PARSE_TIMEOUT_MICROS: u64 = 50_000;
pub const MAX_NODES: usize = 50_000;
const MAX_DEPTH: usize = 128;

pub const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "in", "time", "coproc",
];

fn parser() -> Result<Parser, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|error| format!("failed to initialize Bash parser: {error}"))?;
    #[allow(deprecated)]
    parser.set_timeout_micros(PARSE_TIMEOUT_MICROS);
    Ok(parser)
}

fn exceeds_node_budget(root: Node<'_>) -> bool {
    let mut stack = vec![(root, 0)];
    let mut count = 0usize;
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        if count > MAX_NODES || depth > MAX_DEPTH {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor).map(|child| (child, depth + 1)));
    }
    false
}

/// Rust equivalent of the native parser module's `parse(command)`.
pub fn parse(command: &str) -> Result<Option<Tree>, String> {
    let mut parser = parser()?;
    let Some(tree) = parser.parse(command, None) else {
        return Ok(None);
    };
    if exceeds_node_budget(tree.root_node()) {
        return Ok(None);
    }
    Ok(Some(tree))
}
