//! @nyan: tree-sitter symbol table for the retrieval index (docs/nyan/retrieval.md
//! 4.5). One walk per file produces definitions (with parent scope and line
//! range) and identifier occurrences (with call/enclosing-definition), which is
//! enough for definition lookup, file outline, references and callers.
//!
//! Languages without a grammar never reach this module; the caller records them
//! as `symbol_checked = false` and the result renders `(no symbol check)`.

use std::collections::HashSet;

use tree_sitter::{Node, Parser};

use super::{ReferenceRecord, SymbolRecord, grammar_for};

const MAX_DEPTH: usize = 400;
const MAX_REFERENCES: usize = 20_000;
const MAX_NAME_CHARS: usize = 200;

#[derive(Debug, Default)]
pub struct ParsedFile {
    pub symbols: Vec<SymbolRecord>,
    pub references: Vec<ReferenceRecord>,
    /// Set when the walk stopped early (depth or reference cap) so the symbol
    /// table is known to be incomplete rather than silently short.
    pub partial: Option<String>,
}

pub fn parse(text: &str, language: &str) -> Result<ParsedFile, String> {
    let grammar = grammar_for(language).ok_or_else(|| format!("no grammar for {language}"))?;
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|error| format!("grammar {language} rejected: {error}"))?;
    let tree = parser
        .parse(text, None)
        .ok_or_else(|| format!("tree-sitter returned no tree for {language}"))?;
    let mut walker = Walker {
        src: text.as_bytes(),
        language,
        out: ParsedFile::default(),
        definition_names: HashSet::new(),
        stack: Vec::new(),
    };
    walker.visit(tree.root_node(), 0);
    Ok(walker.out)
}

/// Inspect step 3: does `name` still exist as a definition whose range covers
/// `[start, end]`? Text chunks may be sub-windows of a long symbol, so the
/// range is contained, not equal.
pub fn symbol_covers(
    text: &str,
    language: &str,
    name: &str,
    start: usize,
    end: usize,
) -> Result<bool, String> {
    let parsed = parse(text, language)?;
    Ok(parsed
        .symbols
        .iter()
        .any(|symbol| symbol.name == name && symbol.start_line <= start && symbol.end_line >= end))
}

struct Walker<'a> {
    src: &'a [u8],
    language: &'a str,
    out: ParsedFile,
    definition_names: HashSet<usize>,
    /// Enclosing definitions, innermost last: (name, kind).
    stack: Vec<(String, &'static str)>,
}

impl Walker<'_> {
    fn visit(&mut self, node: Node, depth: usize) {
        if depth > MAX_DEPTH {
            self.out
                .partial
                .get_or_insert_with(|| format!("syntax tree deeper than {MAX_DEPTH} levels"));
            return;
        }
        if let Some((kind, name_node)) = definition(self.language, node, self.src)
            && let Some(name) = self.text_of(name_node)
        {
            self.definition_names.insert(name_node.id());
            self.out.symbols.push(SymbolRecord {
                name: name.clone(),
                kind: kind.to_string(),
                start_line: node.start_position().row + 1,
                end_line: end_line(node),
                parent: self.stack.last().map(|(parent, _)| parent.clone()),
            });
            self.stack.push((name, kind));
            self.visit_children(node, depth);
            self.stack.pop();
            return;
        }
        self.record_reference(node);
        self.visit_children(node, depth);
    }

    fn visit_children(&mut self, node: Node, depth: usize) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, depth + 1);
        }
    }

    fn record_reference(&mut self, node: Node) {
        let kind = node.kind();
        let is_reference = match self.language {
            "markdown" => false,
            "bash" => kind == "command_name",
            _ => matches!(
                kind,
                "identifier" | "type_identifier" | "field_identifier" | "property_identifier"
            ),
        };
        if !is_reference || self.definition_names.contains(&node.id()) {
            return;
        }
        if self.out.references.len() >= MAX_REFERENCES {
            self.out.partial.get_or_insert_with(|| {
                format!("more than {MAX_REFERENCES} identifier occurrences")
            });
            return;
        }
        let Some(name) = self.text_of(node) else {
            return;
        };
        let enclosing = self
            .stack
            .iter()
            .rev()
            .find(|(_, kind)| matches!(*kind, "function" | "method"))
            .map(|(name, _)| name.clone());
        self.out.references.push(ReferenceRecord {
            name,
            line: node.start_position().row + 1,
            call: self.language == "bash" || is_callee(node),
            enclosing,
        });
    }

    fn text_of(&self, node: Node) -> Option<String> {
        let text = node.utf8_text(self.src).ok()?.trim();
        if text.is_empty() {
            return None;
        }
        Some(text.chars().take(MAX_NAME_CHARS).collect())
    }
}

/// tree-sitter ends a node at column 0 of the following line when it swallows
/// the trailing newline (Markdown sections); that line is not part of it.
fn end_line(node: Node) -> usize {
    let start = node.start_position();
    let end = node.end_position();
    if end.column == 0 && end.row > start.row {
        end.row
    } else {
        end.row + 1
    }
}

/// The definition kind and the node holding its name, if `node` defines one.
fn definition<'t>(language: &str, node: Node<'t>, src: &[u8]) -> Option<(&'static str, Node<'t>)> {
    let named = |kind: &'static str| node.child_by_field_name("name").map(|name| (kind, name));
    match (language, node.kind()) {
        ("rust", "function_item") => named(if inside_impl_or_trait(node) {
            "method"
        } else {
            "function"
        }),
        ("rust", "struct_item") => named("struct"),
        ("rust", "union_item") => named("union"),
        ("rust", "enum_item") => named("enum"),
        ("rust", "trait_item") => named("trait"),
        ("rust", "mod_item") => named("module"),
        ("rust", "type_item") => named("type"),
        ("rust", "const_item") => named("const"),
        ("rust", "static_item") => named("static"),
        ("rust", "macro_definition") => named("macro"),
        ("rust", "impl_item") => {
            let target = node.child_by_field_name("type")?;
            let target = match target.kind() {
                "generic_type" => target.child_by_field_name("type").unwrap_or(target),
                _ => target,
            };
            Some(("impl", target))
        }

        (
            "typescript" | "javascript",
            "function_declaration" | "generator_function_declaration",
        ) => named("function"),
        ("typescript" | "javascript", "class_declaration" | "abstract_class_declaration") => {
            named("class")
        }
        ("typescript" | "javascript", "interface_declaration") => named("interface"),
        ("typescript" | "javascript", "type_alias_declaration") => named("type"),
        ("typescript" | "javascript", "enum_declaration") => named("enum"),
        ("typescript" | "javascript", "method_definition" | "abstract_method_signature") => {
            named("method")
        }
        ("typescript" | "javascript", "variable_declarator") => {
            let value = node.child_by_field_name("value")?;
            matches!(
                value.kind(),
                "arrow_function" | "function_expression" | "function"
            )
            .then(|| named("function"))
            .flatten()
        }

        ("python", "function_definition") => named(if inside_kind(node, "class_definition") {
            "method"
        } else {
            "function"
        }),
        ("python", "class_definition") => named("class"),

        ("go", "function_declaration") => named("function"),
        ("go", "method_declaration") => named("method"),
        ("go", "type_spec") => named("type"),

        ("java", "class_declaration" | "record_declaration") => named("class"),
        ("java", "interface_declaration") => named("interface"),
        ("java", "enum_declaration") => named("enum"),
        ("java", "method_declaration" | "constructor_declaration") => named("method"),

        ("cpp", "class_specifier") => named("class"),
        ("cpp", "struct_specifier") => named("struct"),
        ("cpp", "enum_specifier") => named("enum"),
        ("cpp", "namespace_definition") => named("namespace"),
        ("cpp", "function_definition") => {
            cpp_declarator_name(node.child_by_field_name("declarator")?)
                .map(|name| ("function", name))
        }

        ("bash", "function_definition") => named("function"),

        ("markdown", "section") => heading_content(node, src).map(|name| ("heading", name)),
        _ => None,
    }
}

fn inside_impl_or_trait(node: Node) -> bool {
    inside_kind(node, "impl_item") || inside_kind(node, "trait_item")
}

/// Nearest enclosing `kind`, stopping at the first enclosing function so a
/// nested helper inside a method is not mislabelled.
fn inside_kind(node: Node, kind: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == kind {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn cpp_declarator_name(mut node: Node) -> Option<Node> {
    loop {
        match node.kind() {
            "identifier" | "field_identifier" | "type_identifier" | "destructor_name"
            | "operator_name" => return Some(node),
            "qualified_identifier" => node = node.child_by_field_name("name")?,
            _ => node = node.child_by_field_name("declarator")?,
        }
    }
}

fn heading_content<'t>(section: Node<'t>, _src: &[u8]) -> Option<Node<'t>> {
    let mut cursor = section.walk();
    let heading = section
        .children(&mut cursor)
        .find(|child| matches!(child.kind(), "atx_heading" | "setext_heading"))?;
    if let Some(content) = heading.child_by_field_name("heading_content") {
        return Some(content);
    }
    let mut cursor = heading.walk();
    heading
        .children(&mut cursor)
        .find(|child| child.kind() == "inline")
}

/// True when `node` is the called name of a call expression, either directly
/// (`foo()`) or as the member of a callee (`value.foo()`, `pkg.foo()`).
fn is_callee(node: Node) -> bool {
    let is =
        |candidate: Option<Node>, target: Node| candidate.is_some_and(|c| c.id() == target.id());
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "call_expression" | "call" => is(parent.child_by_field_name("function"), node),
        "method_invocation" => is(parent.child_by_field_name("name"), node),
        "macro_invocation" => is(parent.child_by_field_name("macro"), node),
        "field_expression" | "member_expression" | "attribute" | "selector_expression" => {
            let member = parent
                .child_by_field_name("field")
                .or_else(|| parent.child_by_field_name("property"))
                .or_else(|| parent.child_by_field_name("attribute"));
            is(member, node)
                && parent.parent().is_some_and(|grand| {
                    matches!(grand.kind(), "call_expression" | "call")
                        && is(grand.child_by_field_name("function"), parent)
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(parsed: &ParsedFile) -> Vec<(&str, &str)> {
        parsed
            .symbols
            .iter()
            .map(|symbol| (symbol.kind.as_str(), symbol.name.as_str()))
            .collect()
    }

    #[test]
    fn rust_definitions_scopes_and_callers() {
        let source = "struct Foo;\nimpl Foo {\n    fn run(&self) {\n        helper();\n        self.step();\n    }\n}\nfn helper() {}\n";
        let parsed = parse(source, "rust").unwrap();
        assert_eq!(
            names(&parsed),
            vec![
                ("struct", "Foo"),
                ("impl", "Foo"),
                ("method", "run"),
                ("function", "helper")
            ]
        );
        let run = &parsed.symbols[2];
        assert_eq!((run.start_line, run.end_line), (3, 6));
        assert_eq!(run.parent.as_deref(), Some("Foo"));
        let helper_call = parsed
            .references
            .iter()
            .find(|reference| reference.name == "helper" && reference.call)
            .unwrap();
        assert_eq!(helper_call.enclosing.as_deref(), Some("run"));
        assert!(
            parsed
                .references
                .iter()
                .any(|reference| reference.name == "step" && reference.call)
        );
        // The definition's own name is not an occurrence of itself.
        assert!(
            !parsed
                .references
                .iter()
                .any(|reference| reference.name == "helper" && reference.line == 8)
        );
    }

    #[test]
    fn python_methods_and_typescript_arrow_functions() {
        let python = parse(
            "class A:\n    def go(self):\n        pass\n\ndef top():\n    A().go()\n",
            "python",
        )
        .unwrap();
        assert_eq!(
            names(&python),
            vec![("class", "A"), ("method", "go"), ("function", "top")]
        );
        assert!(python.references.iter().any(|r| r.name == "go" && r.call));

        let typescript = parse(
            "export const load = async () => {\n  return fetchAll();\n};\ninterface Shape { area(): number }\n",
            "typescript",
        )
        .unwrap();
        assert!(names(&typescript).contains(&("function", "load")));
        assert!(names(&typescript).contains(&("interface", "Shape")));
    }

    #[test]
    fn go_cpp_java_bash_and_markdown() {
        let go = parse(
            "package p\ntype T struct{}\nfunc (t T) Do() {}\nfunc Free() {}\n",
            "go",
        )
        .unwrap();
        assert_eq!(
            names(&go),
            vec![("type", "T"), ("method", "Do"), ("function", "Free")]
        );
        let cpp = parse(
            "namespace n {\nclass K {};\nint K::run(int x) { return x; }\n}\n",
            "cpp",
        )
        .unwrap();
        assert!(names(&cpp).contains(&("namespace", "n")));
        assert!(names(&cpp).contains(&("class", "K")));
        assert!(names(&cpp).contains(&("function", "run")));
        let java = parse("class A { void m() { n(); } }\n", "java").unwrap();
        assert_eq!(names(&java), vec![("class", "A"), ("method", "m")]);
        let bash = parse("build() {\n  make all\n}\n", "bash").unwrap();
        assert_eq!(names(&bash), vec![("function", "build")]);
        assert!(bash.references.iter().any(|r| r.name == "make" && r.call));
        let markdown = parse("# Title\ntext\n## Sub\nmore\n# Next\n", "markdown").unwrap();
        assert_eq!(
            names(&markdown),
            vec![
                ("heading", "Title"),
                ("heading", "Sub"),
                ("heading", "Next")
            ]
        );
        assert_eq!(
            (markdown.symbols[0].start_line, markdown.symbols[0].end_line),
            (1, 4)
        );
    }

    #[test]
    fn unsupported_language_is_an_error_not_an_empty_table() {
        assert!(parse("anything", "unknown").is_err());
    }

    #[test]
    fn symbol_covers_accepts_sub_windows_and_rejects_moved_symbols() {
        let source = "fn a() {\n  1;\n  2;\n}\n";
        assert!(symbol_covers(source, "rust", "a", 2, 3).unwrap());
        assert!(!symbol_covers(source, "rust", "b", 1, 4).unwrap());
        assert!(!symbol_covers("\n\nfn a() {}\n", "rust", "a", 1, 1).unwrap());
    }
}
