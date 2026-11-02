//! Maps to: CC `utils/treeify.ts`.
//!
//! Nyan keeps the same tree character and traversal semantics needed by
//! `ValidationErrorsList`. ANSI colorization is intentionally omitted here;
//! callers apply iocraft `Text` colors at render time.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeValue {
    Node(TreeNode),
    Text(String),
    Null,
}

pub type TreeNode = BTreeMap<String, TreeValue>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeifyOptions {
    pub show_values: bool,
    pub hide_functions: bool,
}

impl Default for TreeifyOptions {
    fn default() -> Self {
        Self {
            show_values: true,
            hide_functions: false,
        }
    }
}

const BRANCH: &str = "├";
const LAST_BRANCH: &str = "└";
const LINE: &str = "│";
const EMPTY: &str = " ";

/// Maps to: CC `utils/treeify.ts` `treeify`.
pub fn treeify(obj: &TreeNode, options: TreeifyOptions) -> String {
    if obj.is_empty() {
        return "(empty)".to_string();
    }

    if obj.len() == 1
        && let Some((key, TreeValue::Text(value))) = obj.iter().next()
        && key.trim().is_empty()
    {
        return format!("{LAST_BRANCH} {value}");
    }

    let mut lines = Vec::new();
    grow_branch(obj, "", 0, &options, &mut lines);
    lines.join("\n")
}

fn grow_branch(
    node: &TreeNode,
    prefix: &str,
    depth: usize,
    options: &TreeifyOptions,
    lines: &mut Vec<String>,
) {
    let entries: Vec<_> = node.iter().collect();
    for (index, (key, value)) in entries.iter().enumerate() {
        let is_last_key = index == entries.len().saturating_sub(1);
        let node_prefix = if depth == 0 && index == 0 {
            "".to_string()
        } else {
            prefix.to_string()
        };
        let tree_char = if is_last_key { LAST_BRANCH } else { BRANCH };
        let key_text = if key.trim().is_empty() {
            String::new()
        } else {
            (*key).clone()
        };
        let mut line = node_prefix.clone() + tree_char;
        if !key_text.is_empty() {
            line.push(' ');
            line.push_str(&key_text);
        }
        let should_add_colon = !key.trim().is_empty();

        match value {
            TreeValue::Node(child) => {
                lines.push(line);
                let continuation = if is_last_key { EMPTY } else { LINE };
                let next_prefix = node_prefix + continuation + " ";
                grow_branch(child, &next_prefix, depth + 1, options, lines);
            }
            TreeValue::Text(value) => {
                if options.show_values {
                    line.push_str(separator(should_add_colon, !line.is_empty()));
                    line.push_str(value);
                }
                lines.push(line);
            }
            TreeValue::Null => {
                if options.show_values {
                    line.push_str(separator(should_add_colon, !line.is_empty()));
                    line.push_str("undefined");
                }
                lines.push(line);
            }
        }
    }
}

fn separator(should_add_colon: bool, has_line: bool) -> &'static str {
    if should_add_colon {
        ": "
    } else if has_line {
        " "
    } else {
        ""
    }
}

/// Maps to: CC `ValidationErrorsList.tsx` `setWith(tree, path, message, Object)`.
pub fn set_dot_path(tree: &mut TreeNode, path: &str, value: impl Into<String>) {
    let value = value.into();
    if path.is_empty() {
        tree.insert(String::new(), TreeValue::Text(value));
        return;
    }

    let mut node = tree;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        let is_last = parts.peek().is_none();
        if is_last {
            node.insert(part.to_string(), TreeValue::Text(value));
            return;
        }

        let entry = node
            .entry(part.to_string())
            .or_insert_with(|| TreeValue::Node(TreeNode::new()));
        if !matches!(entry, TreeValue::Node(_)) {
            *entry = TreeValue::Node(TreeNode::new());
        }
        let TreeValue::Node(child) = entry else {
            unreachable!("entry was normalized to a node")
        };
        node = child;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treeify_empty_and_root_error_match_official_shape() {
        assert_eq!(
            treeify(&TreeNode::new(), TreeifyOptions::default()),
            "(empty)"
        );

        let mut tree = TreeNode::new();
        set_dot_path(&mut tree, "", "Invalid or malformed JSON");
        assert_eq!(
            treeify(&tree, TreeifyOptions::default()),
            "└ Invalid or malformed JSON"
        );
    }

    #[test]
    fn treeify_nested_dot_paths_match_official_tree_characters() {
        let mut tree = TreeNode::new();
        set_dot_path(&mut tree, "env.DEBUG", "Expected string");
        set_dot_path(&mut tree, "permissions.defaultMode", "Invalid value");

        let text = treeify(&tree, TreeifyOptions::default());

        assert!(text.contains("├ env"), "tree=\n{text}");
        assert!(text.contains("│ └ DEBUG: Expected string"), "tree=\n{text}");
        assert!(text.contains("└ permissions"), "tree=\n{text}");
        assert!(
            text.contains("  └ defaultMode: Invalid value"),
            "tree=\n{text}"
        );
    }
}
