//! Maps to: CC `utils/bash/ast.ts`.
//!
//! Fail-closed tree-sitter Bash projection used by permission/security
//! consumers. Only syntax for which we can recover trustworthy static argv is
//! accepted; every unknown node becomes `TooComplex` and therefore requires
//! normal user approval.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::Node;

pub use super::parser::has_parse_error;

const CMDSUB_PLACEHOLDER: &str = "__CMDSUB_OUTPUT__";
const VAR_PLACEHOLDER: &str = "__TRACKED_VAR__";

static SIMPLE_EXPANSION_IN_TEXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$[A-Za-z_]").expect("valid simple-expansion detection regex"));
static VARIABLE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid Bash variable-name regex")
});
static PS4_REFERENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{[A-Za-z_][A-Za-z0-9_]*\}").expect("valid PS4 reference regex")
});
static PS4_SAFE_REMAINDER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9 _+:./=\[\]-]*$").expect("valid PS4 safe-character regex")
});
static JQ_SYSTEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsystem\s*\(").expect("valid jq system regex"));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    pub op: String,
    pub target: String,
    pub fd: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleCommand {
    pub argv: Vec<String>,
    pub env_vars: Vec<EnvironmentVariable>,
    pub redirects: Vec<Redirect>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseForSecurityResult {
    Simple {
        commands: Vec<SimpleCommand>,
    },
    ParseUnavailable,
    TooComplex {
        reason: String,
        node_type: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticCheckResult {
    Ok,
    Unsafe { reason: String },
}

fn semantic_error(reason: impl Into<String>) -> SemanticCheckResult {
    SemanticCheckResult::Unsafe {
        reason: reason.into(),
    }
}

/// Maps to CC `utils/bash/ast.ts#checkSemantics`.
pub fn check_semantics(commands: &[SimpleCommand]) -> SemanticCheckResult {
    const ZSH_DANGEROUS: &[&str] = &[
        "zmodload", "emulate", "sysopen", "sysread", "syswrite", "sysseek", "zpty", "ztcp",
        "zsocket", "zf_rm", "zf_mv", "zf_ln", "zf_chmod", "zf_chown", "zf_mkdir", "zf_rmdir",
        "zf_chgrp",
    ];
    const EVAL_LIKE: &[&str] = &[
        "eval",
        "source",
        ".",
        "exec",
        "command",
        "builtin",
        "fc",
        "coproc",
        "noglob",
        "nocorrect",
        "trap",
        "enable",
        "mapfile",
        "readarray",
        "hash",
        "bind",
        "complete",
        "compgen",
        "alias",
        "let",
    ];
    for command in commands {
        let mut argv = command.argv.as_slice();
        loop {
            match argv.first().map(String::as_str) {
                Some("time" | "nohup") => argv = &argv[1..],
                Some("timeout") => {
                    let mut index = 1usize;
                    while let Some(arg) = argv.get(index).map(String::as_str) {
                        if matches!(
                            arg,
                            "--foreground" | "--preserve-status" | "--verbose" | "-v"
                        ) {
                            index += 1;
                        } else if arg.starts_with("--kill-after=") || arg.starts_with("--signal=") {
                            index += 1;
                        } else if matches!(arg, "--kill-after" | "--signal" | "-k" | "-s") {
                            if argv.get(index + 1).is_none() {
                                return semantic_error(format!(
                                    "timeout with {arg} flag cannot be statically analyzed"
                                ));
                            }
                            index += 2;
                        } else if (arg.starts_with("-k") || arg.starts_with("-s")) && arg.len() > 2
                        {
                            index += 1;
                        } else if arg.starts_with('-') {
                            return semantic_error(format!(
                                "timeout with {arg} flag cannot be statically analyzed"
                            ));
                        } else {
                            break;
                        }
                    }
                    let Some(duration) = argv.get(index) else {
                        break;
                    };
                    if !is_static_timeout_duration(duration) {
                        return semantic_error(format!(
                            "timeout duration '{duration}' cannot be statically analyzed"
                        ));
                    }
                    argv = &argv[index + 1..];
                }
                Some("nice") => {
                    if argv.get(1).is_some_and(|arg| arg == "-n")
                        && argv.get(2).is_some_and(|arg| is_signed_integer(arg))
                    {
                        argv = &argv[3..];
                    } else if argv.get(1).is_some_and(|arg| {
                        arg.starts_with('-') && arg.len() > 1 && is_signed_integer(arg)
                    }) {
                        argv = &argv[2..];
                    } else if argv.get(1).is_some_and(|arg| arg.contains(['$', '(', '`'])) {
                        return semantic_error(format!(
                            "nice argument '{}' contains an expansion",
                            argv[1]
                        ));
                    } else {
                        argv = &argv[1..];
                    }
                }
                Some("env") => {
                    let mut index = 1usize;
                    while let Some(arg) = argv.get(index).map(String::as_str) {
                        if arg.contains('=') && !arg.starts_with('-')
                            || matches!(arg, "-i" | "-0" | "-v")
                        {
                            index += 1;
                        } else if arg == "-u" && argv.get(index + 1).is_some() {
                            index += 2;
                        } else if arg.starts_with('-') {
                            return semantic_error(format!(
                                "env with {arg} flag cannot be statically analyzed"
                            ));
                        } else {
                            break;
                        }
                    }
                    if index < argv.len() {
                        argv = &argv[index..];
                    } else {
                        break;
                    }
                }
                Some("stdbuf") => {
                    let mut index = 1usize;
                    while let Some(arg) = argv.get(index).map(String::as_str) {
                        if matches!(arg, "-i" | "-o" | "-e") && argv.get(index + 1).is_some() {
                            index += 2;
                        } else if ((arg.starts_with("-i")
                            || arg.starts_with("-o")
                            || arg.starts_with("-e"))
                            && arg.len() > 2)
                            || arg.starts_with("--input=")
                            || arg.starts_with("--output=")
                            || arg.starts_with("--error=")
                        {
                            index += 1;
                        } else if arg.starts_with('-') {
                            return semantic_error(format!(
                                "stdbuf with {arg} flag cannot be statically analyzed"
                            ));
                        } else {
                            break;
                        }
                    }
                    if index > 1 && index < argv.len() {
                        argv = &argv[index..];
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        let Some(name) = argv.first().map(String::as_str) else {
            continue;
        };
        if name.is_empty() || name.contains("__CMDSUB_OUTPUT__") || name.contains("__TRACKED_VAR__")
        {
            return semantic_error("Command name is runtime-determined");
        }
        if name
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, '-' | '|' | '&'))
        {
            return semantic_error("Command appears to be an incomplete fragment");
        }
        if super::bash_parser::SHELL_KEYWORDS.contains(&name) {
            return semantic_error(format!("Shell keyword '{name}' as command name"));
        }
        if command
            .argv
            .iter()
            .chain(command.env_vars.iter().map(|variable| &variable.value))
            .chain(command.redirects.iter().map(|redirect| &redirect.target))
            .any(|arg| {
                arg.split_inclusive('\n')
                    .skip(1)
                    .any(|line| line.trim_start_matches([' ', '\t', '\n']).starts_with('#'))
            })
        {
            return semantic_error(
                "Newline followed by # inside a quoted argument can hide path arguments",
            );
        }
        if ZSH_DANGEROUS.contains(&name) {
            return semantic_error(format!("Zsh builtin '{name}' can bypass security checks"));
        }
        if EVAL_LIKE.contains(&name) {
            let safe_command_query = name == "command"
                && argv
                    .get(1)
                    .is_some_and(|arg| matches!(arg.as_str(), "-v" | "-V"));
            let safe_fc = name == "fc"
                && !argv.iter().skip(1).any(|arg| {
                    arg.starts_with('-') && arg[1..].chars().any(|ch| matches!(ch, 'e' | 's'))
                });
            let safe_compgen = name == "compgen"
                && !argv.iter().skip(1).any(|arg| {
                    arg.starts_with('-') && arg[1..].chars().any(|ch| matches!(ch, 'C' | 'F' | 'W'))
                });
            if !safe_command_query && !safe_fc && !safe_compgen {
                return semantic_error(format!("'{name}' evaluates arguments as shell code"));
            }
        }
        if name == "jq"
            && argv
                .iter()
                .any(|arg| arg.contains("system(") || jq_argument_reads_or_executes_file(arg))
        {
            return semantic_error("jq argument can execute code or read arbitrary files");
        }
        if argv
            .iter()
            .chain(command.redirects.iter().map(|redirect| &redirect.target))
            .any(|arg| arg.contains("/proc/") && arg.contains("/environ"))
        {
            return semantic_error("Accesses /proc/*/environ which may expose secrets");
        }
        if has_subscript_eval_operand(name, argv) {
            return semantic_error(format!(
                "'{name}' NAME operand contains an array subscript evaluated by Bash"
            ));
        }
    }
    SemanticCheckResult::Ok
}

fn is_signed_integer(value: &str) -> bool {
    let value = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())
}

fn is_static_timeout_duration(value: &str) -> bool {
    let numeric = value
        .strip_suffix('s')
        .or_else(|| value.strip_suffix('m'))
        .or_else(|| value.strip_suffix('h'))
        .or_else(|| value.strip_suffix('d'))
        .unwrap_or(value);
    let mut dot = false;
    !numeric.is_empty()
        && numeric.chars().all(|ch| {
            if ch == '.' && !dot {
                dot = true;
                true
            } else {
                ch.is_ascii_digit()
            }
        })
        && !numeric.starts_with('.')
        && !numeric.ends_with('.')
}

fn has_subscript_eval_operand(name: &str, argv: &[String]) -> bool {
    let flags: &[&str] = match name {
        "test" | "[" | "[[" => &["-v", "-R"],
        "printf" => &["-v"],
        "read" => &["-a"],
        "unset" => &["-v"],
        "wait" => &["-p"],
        _ => &[],
    };
    for (index, argument) in argv.iter().enumerate().skip(1) {
        if flags.contains(&argument.as_str())
            && argv.get(index + 1).is_some_and(|name| name.contains('['))
        {
            return true;
        }
        if argument.starts_with('-')
            && !argument.starts_with("--")
            && argument.len() > 2
            && !argument.contains('[')
            && flags.iter().any(|flag| {
                flag.len() == 2 && argument[1..].contains(flag.chars().nth(1).unwrap_or_default())
            })
            && argv.get(index + 1).is_some_and(|name| name.contains('['))
        {
            return true;
        }
        if flags.iter().any(|flag| {
            flag.len() == 2
                && argument.starts_with(flag)
                && argument.len() > 2
                && argument.contains('[')
        }) {
            return true;
        }
    }

    if name == "[[" {
        const ARITHMETIC_COMPARISONS: &[&str] = &["-eq", "-ne", "-lt", "-le", "-gt", "-ge"];
        for index in 2..argv.len() {
            if ARITHMETIC_COMPARISONS.contains(&argv[index].as_str())
                && (argv
                    .get(index.wrapping_sub(1))
                    .is_some_and(|arg| arg.contains('['))
                    || argv.get(index + 1).is_some_and(|arg| arg.contains('[')))
            {
                return true;
            }
        }
    }

    if !matches!(name, "read" | "unset") {
        return false;
    }
    const READ_DATA_FLAGS: &[&str] = &["-p", "-d", "-n", "-N", "-t", "-u", "-i"];
    let mut skip_next = false;
    for argument in argv.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if argument.starts_with('-') {
            if name == "read" {
                if READ_DATA_FLAGS.contains(&argument.as_str()) {
                    skip_next = true;
                } else if argument.starts_with('-')
                    && !argument.starts_with("--")
                    && argument.len() > 2
                {
                    for (position, flag) in argument[1..].chars().enumerate() {
                        if READ_DATA_FLAGS
                            .iter()
                            .any(|candidate| candidate.chars().nth(1) == Some(flag))
                        {
                            skip_next = position + 2 == argument.chars().count();
                            break;
                        }
                    }
                }
            }
            continue;
        }
        if argument.contains('[') {
            return true;
        }
    }
    false
}

fn jq_argument_reads_or_executes_file(argument: &str) -> bool {
    if let Some(rest) = argument
        .strip_prefix("-f")
        .or_else(|| argument.strip_prefix("-L"))
    {
        return rest.is_empty()
            || rest
                .chars()
                .next()
                .is_some_and(|character| !character.is_ascii_alphabetic());
    }
    ["--from-file", "--rawfile", "--slurpfile", "--library-path"]
        .iter()
        .any(|flag| argument == *flag || argument.starts_with(&format!("{flag}=")))
}

fn too_complex(node: Node<'_>, reason: impl Into<String>) -> ParseForSecurityResult {
    ParseForSecurityResult::TooComplex {
        reason: reason.into(),
        node_type: Some(node.kind().to_string()),
    }
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.byte_range())
}

fn has_precheck_differential(command: &str) -> Option<&'static str> {
    if command
        .chars()
        .any(|ch| matches!(ch as u32, 0x00..=0x08 | 0x0b..=0x1f | 0x7f))
    {
        return Some("Contains control characters");
    }
    if command.chars().any(|ch| {
        matches!(
            ch as u32,
            0x00a0 | 0x1680 | 0x2000..=0x200b | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
        )
    }) {
        return Some("Contains Unicode whitespace");
    }
    let bytes = command.as_bytes();
    for index in 0..bytes.len().saturating_sub(1) {
        if bytes[index] != b'\\' {
            continue;
        }
        if matches!(bytes[index + 1], b' ' | b'\t')
            || (bytes[index + 1] == b'\n'
                && index > 0
                && !matches!(bytes[index - 1], b' ' | b'\t' | b'\n' | b'\\'))
        {
            return Some("Contains backslash-escaped whitespace");
        }
    }
    if command.contains("~[") {
        return Some("Contains zsh ~[ dynamic directory syntax");
    }
    let starts_zsh_equals = command.char_indices().any(|(index, ch)| {
        ch == '='
            && command[index + 1..]
                .chars()
                .next()
                .is_some_and(|next| next == '_' || next.is_ascii_alphabetic())
            && (index == 0
                || command[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|prev| prev.is_ascii_whitespace() || ";&|".contains(prev)))
    });
    if starts_zsh_equals {
        return Some("Contains zsh =cmd equals expansion");
    }
    if brace_with_quote_after_masking(command) {
        return Some("Contains brace with quote character (expansion obfuscation)");
    }
    None
}

/// Maps to CC `maskBracesInQuotedContexts` + `BRACE_WITH_QUOTE_RE`.
fn brace_with_quote_after_masking(command: &str) -> bool {
    if !command.contains('{') {
        return false;
    }
    let chars = command.chars().collect::<Vec<_>>();
    let mut masked = String::with_capacity(command.len());
    let mut single = false;
    let mut double = false;
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if single {
            if character == '\'' {
                single = false;
            }
            masked.push(if character == '{' { ' ' } else { character });
            index += 1;
            continue;
        }
        if double {
            if character == '\\'
                && chars
                    .get(index + 1)
                    .is_some_and(|next| matches!(*next, '"' | '\\'))
            {
                masked.push(character);
                masked.push(chars[index + 1]);
                index += 2;
                continue;
            }
            if character == '"' {
                double = false;
            }
            masked.push(if character == '{' { ' ' } else { character });
            index += 1;
            continue;
        }
        if character == '\\' && index + 1 < chars.len() {
            masked.push(character);
            masked.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if character == '\'' {
            single = true;
        } else if character == '"' {
            double = true;
        }
        masked.push(character);
        index += 1;
    }

    masked.split('{').skip(1).any(|suffix| {
        suffix
            .split_once('}')
            .is_some_and(|(inside, _)| inside.contains(['\'', '"']))
    })
}

pub fn parse_for_security(command: &str) -> ParseForSecurityResult {
    // Maps to parseForSecurity's exact empty-string short circuit. Whitespace
    // still reaches the parser and differential pre-checks.
    if command.is_empty() {
        return ParseForSecurityResult::Simple {
            commands: Vec::new(),
        };
    }
    let parsed = super::parser::parse_command_raw(command);
    if matches!(&parsed, super::parser::ParseCommandRawResult::Unavailable) {
        return ParseForSecurityResult::ParseUnavailable;
    }
    if let Some(reason) = has_precheck_differential(command) {
        return ParseForSecurityResult::TooComplex {
            reason: reason.to_string(),
            node_type: None,
        };
    }
    if command.trim().is_empty() {
        return ParseForSecurityResult::Simple {
            commands: Vec::new(),
        };
    }
    let tree = match parsed {
        super::parser::ParseCommandRawResult::Parsed(tree) => tree,
        super::parser::ParseCommandRawResult::ParseAborted => {
            return ParseForSecurityResult::TooComplex {
                reason: "Parser aborted (timeout or resource limit)".to_string(),
                node_type: Some("PARSE_ABORT".to_string()),
            };
        }
        super::parser::ParseCommandRawResult::Unavailable => unreachable!(),
    };
    let root = tree.root_node();
    if root.has_error() {
        return ParseForSecurityResult::TooComplex {
            reason: "Bash parser produced an ERROR node".to_string(),
            node_type: Some("ERROR".to_string()),
        };
    }
    let mut commands = Vec::new();
    let mut variable_scope = HashMap::new();
    match collect_commands_scoped(root, command, &mut commands, &mut variable_scope) {
        Ok(()) => ParseForSecurityResult::Simple { commands },
        Err(error) => error,
    }
}

/// Maps to CC `walkProgram` / `collectCommands` (`ast.ts:462-960`).
///
/// The scope model is security-sensitive: sequential `&&`/`;` clauses carry
/// literal assignments, while pipelines, `||`, and background clauses fork
/// from the incoming snapshot so conditional/subshell assignments cannot hide
/// a later runtime path or flag.
fn collect_commands_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    match node.kind() {
        "command" => collect_simple_command_scoped(node, source, commands, variable_scope),
        "redirected_statement" => {
            collect_redirected_statement(node, source, commands, variable_scope)
        }
        "comment" => Ok(()),
        "program" | "list" | "pipeline" => {
            collect_structural_commands(node, source, commands, variable_scope)
        }
        "negated_command" => {
            for child in all_children(node) {
                if child.kind() != "!" {
                    return collect_commands_scoped(child, source, commands, variable_scope);
                }
            }
            Ok(())
        }
        "declaration_command" => {
            collect_declaration_command(node, source, commands, variable_scope)
        }
        "variable_assignment" => {
            let assignment =
                parse_variable_assignment_scoped(node, source, commands, variable_scope)?;
            apply_variable_to_scope(variable_scope, &assignment);
            Ok(())
        }
        "for_statement" => collect_for_statement(node, source, commands, variable_scope),
        "if_statement" | "while_statement" => {
            collect_conditional_statement(node, source, commands, variable_scope)
        }
        "subshell" => {
            let mut inner_scope = variable_scope.clone();
            for child in all_children(node) {
                if matches!(child.kind(), "(" | ")") {
                    continue;
                }
                collect_commands_scoped(child, source, commands, &mut inner_scope)?;
            }
            Ok(())
        }
        "test_command" => collect_test_command(node, source, commands, variable_scope),
        "unset_command" => collect_unset_command(node, source, commands, variable_scope),
        other => Err(too_complex(
            node,
            format!("Unsupported Bash syntax node: {other}"),
        )),
    }
}

fn all_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn is_separator(kind: &str) -> bool {
    matches!(kind, "&&" | "||" | "|" | ";" | "&" | "|&" | "\n")
}

fn collect_structural_commands(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let incoming = variable_scope.clone();
    let pipeline = node.kind() == "pipeline";
    let mut working = incoming.clone();
    let mut propagates = !pipeline;

    for child in all_children(node) {
        let kind = child.kind();
        if is_separator(kind) {
            if matches!(kind, "||" | "|" | "|&" | "&") {
                if propagates {
                    *variable_scope = working.clone();
                }
                working = incoming.clone();
                propagates = false;
            } else if pipeline {
                working = incoming.clone();
            }
            continue;
        }
        collect_commands_scoped(child, source, commands, &mut working)?;
    }

    if propagates {
        *variable_scope = working;
    }
    Ok(())
}

fn collect_redirected_statement(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let mut redirects = Vec::new();
    let mut body = None;
    for child in all_children(node) {
        match child.kind() {
            "file_redirect" => redirects.push(parse_redirect_scoped(
                child,
                source,
                commands,
                variable_scope,
            )?),
            "heredoc_redirect" => {
                validate_heredoc_redirect(child, source)?;
            }
            "command"
            | "pipeline"
            | "list"
            | "negated_command"
            | "declaration_command"
            | "unset_command" => body = Some(child),
            kind if is_separator(kind) => {}
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported redirected statement child: {other}"),
                ));
            }
        }
    }

    let Some(body) = body else {
        commands.push(SimpleCommand {
            argv: Vec::new(),
            env_vars: Vec::new(),
            redirects,
            text: node_text(node, source).unwrap_or_default().to_string(),
        });
        return Ok(());
    };
    let before = commands.len();
    collect_commands_scoped(body, source, commands, variable_scope)?;
    if commands.len() > before
        && !redirects.is_empty()
        && let Some(last) = commands.last_mut()
    {
        last.redirects.extend(redirects);
    }
    Ok(())
}

fn collect_declaration_command(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let mut argv = Vec::new();
    for child in all_children(node) {
        match child.kind() {
            "export" | "local" | "readonly" | "declare" | "typeset" => {
                argv.push(child.kind().to_string());
            }
            "word" | "number" | "raw_string" | "string" | "concatenation" => {
                let arg = resolve_argument_scoped(child, source, commands, variable_scope, false)?;
                if matches!(
                    argv.first().map(String::as_str),
                    Some("declare" | "typeset" | "local")
                ) && arg.starts_with('-')
                    && arg
                        .chars()
                        .skip(1)
                        .any(|flag| matches!(flag, 'n' | 'i' | 'a' | 'A'))
                {
                    return Err(too_complex(
                        child,
                        format!(
                            "declare flag {arg} changes assignment semantics (nameref/integer/array)"
                        ),
                    ));
                }
                if matches!(
                    argv.first().map(String::as_str),
                    Some("declare" | "typeset" | "local")
                ) && !arg.starts_with('-')
                    && arg.split('=').next().is_some_and(|name| name.contains('['))
                {
                    return Err(too_complex(
                        child,
                        format!("declare positional '{arg}' contains array subscript"),
                    ));
                }
                argv.push(arg);
            }
            "variable_assignment" => {
                let assignment =
                    parse_variable_assignment_scoped(child, source, commands, variable_scope)?;
                apply_variable_to_scope(variable_scope, &assignment);
                argv.push(format!("{}={}", assignment.name, assignment.value));
            }
            "variable_name" => argv.push(node_text(child, source).unwrap_or_default().to_string()),
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported declaration child: {other}"),
                ));
            }
        }
    }
    commands.push(SimpleCommand {
        argv,
        env_vars: Vec::new(),
        redirects: Vec::new(),
        text: node_text(node, source).unwrap_or_default().to_string(),
    });
    Ok(())
}

fn collect_for_statement(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let loop_variable = node
        .child_by_field_name("variable")
        .ok_or_else(|| too_complex(node, "For statement has no static loop variable"))?;
    let body = node
        .child_by_field_name("body")
        .ok_or_else(|| too_complex(node, "For statement has no body"))?;
    let variable_name = node_text(loop_variable, source).unwrap_or_default();
    if !VARIABLE_NAME_RE.is_match(variable_name) || matches!(variable_name, "PS4" | "IFS") {
        return Err(too_complex(loop_variable, "Unsafe for-loop variable name"));
    }

    for child in named_children(node) {
        if child.id() == loop_variable.id() || child.id() == body.id() {
            continue;
        }
        if child.kind() == "command_substitution" {
            collect_command_substitution_scoped(child, source, commands, variable_scope)?;
        } else {
            let _ = resolve_argument_scoped(child, source, commands, variable_scope, false)?;
        }
    }

    variable_scope.insert(variable_name.to_string(), VAR_PLACEHOLDER.to_string());
    let mut body_scope = variable_scope.clone();
    collect_do_group(body, source, commands, &mut body_scope)
}

fn collect_do_group(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    for child in all_children(node) {
        if matches!(child.kind(), "do" | "done" | ";" | "\n") {
            continue;
        }
        collect_commands_scoped(child, source, commands, variable_scope)?;
    }
    Ok(())
}

fn collect_conditional_statement(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    if node.kind() == "while_statement" {
        if let Some(condition) = node.child_by_field_name("condition") {
            let before = commands.len();
            collect_commands_scoped(condition, source, commands, variable_scope)?;
            track_read_variables_from_condition(&commands[before..], variable_scope)?;
        }
        if let Some(body) = node.child_by_field_name("body") {
            let mut body_scope = variable_scope.clone();
            collect_do_group(body, source, commands, &mut body_scope)?;
        }
        return Ok(());
    }

    let mut after_then = false;
    for child in all_children(node) {
        match child.kind() {
            "if" | "fi" | "else" | "elif" | ";" | "\n" => continue,
            "then" => {
                after_then = true;
                continue;
            }
            "elif_clause" | "else_clause" => {
                let mut branch_scope = variable_scope.clone();
                for branch_child in all_children(child) {
                    if matches!(branch_child.kind(), "elif" | "else" | "then" | ";" | "\n") {
                        continue;
                    }
                    collect_commands_scoped(branch_child, source, commands, &mut branch_scope)?;
                }
            }
            _ if after_then => {
                let mut branch_scope = variable_scope.clone();
                collect_commands_scoped(child, source, commands, &mut branch_scope)?;
            }
            _ => {
                let before = commands.len();
                collect_commands_scoped(child, source, commands, variable_scope)?;
                track_read_variables_from_condition(&commands[before..], variable_scope)?;
            }
        }
    }
    Ok(())
}

fn track_read_variables_from_condition(
    commands: &[SimpleCommand],
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    for command in commands {
        if command.argv.first().map(String::as_str) != Some("read") {
            continue;
        }
        for argument in command.argv.iter().skip(1) {
            if argument.starts_with('-') || !VARIABLE_NAME_RE.is_match(argument) {
                continue;
            }
            if variable_scope
                .get(argument)
                .is_some_and(|value| !contains_placeholder(value))
            {
                return Err(ParseForSecurityResult::TooComplex {
                    reason: format!(
                        "'read {argument}' in condition may not overwrite a tracked literal"
                    ),
                    node_type: Some("if_statement".to_string()),
                });
            }
            variable_scope.insert(argument.clone(), VAR_PLACEHOLDER.to_string());
        }
    }
    Ok(())
}

fn collect_test_command(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let mut argv = vec!["[[".to_string()];
    for child in all_children(node) {
        if matches!(child.kind(), "[[" | "]]" | "[" | "]") {
            continue;
        }
        walk_test_expression(child, source, &mut argv, commands, variable_scope)?;
    }
    commands.push(SimpleCommand {
        argv,
        env_vars: Vec::new(),
        redirects: Vec::new(),
        text: node_text(node, source).unwrap_or_default().to_string(),
    });
    Ok(())
}

fn walk_test_expression(
    node: Node<'_>,
    source: &str,
    argv: &mut Vec<String>,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    match node.kind() {
        "unary_expression"
        | "binary_expression"
        | "negated_expression"
        | "parenthesized_expression" => {
            for child in all_children(node) {
                walk_test_expression(child, source, argv, commands, variable_scope)?;
            }
        }
        "test_operator" | "!" | "(" | ")" | "&&" | "||" | "==" | "=" | "!=" | "<" | ">" | "=~"
        | "regex" | "extglob_pattern" => {
            argv.push(node_text(node, source).unwrap_or_default().to_string());
        }
        _ => argv.push(resolve_argument_scoped(
            node,
            source,
            commands,
            variable_scope,
            false,
        )?),
    }
    Ok(())
}

fn collect_unset_command(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let mut argv = Vec::new();
    for child in all_children(node) {
        match child.kind() {
            "unset" => argv.push("unset".to_string()),
            "variable_name" => {
                let name = node_text(child, source).unwrap_or_default().to_string();
                variable_scope.remove(&name);
                argv.push(name);
            }
            "word" => argv.push(resolve_argument_scoped(
                child,
                source,
                commands,
                variable_scope,
                false,
            )?),
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported unset child: {other}"),
                ));
            }
        }
    }
    commands.push(SimpleCommand {
        argv,
        env_vars: Vec::new(),
        redirects: Vec::new(),
        text: node_text(node, source).unwrap_or_default().to_string(),
    });
    Ok(())
}

fn collect_simple_command_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let mut argv = Vec::new();
    let mut env_vars = Vec::new();
    let mut redirects = Vec::new();
    let mut inner_commands = Vec::new();

    for child in named_children(node) {
        match child.kind() {
            "variable_assignment" => {
                let assignment = parse_variable_assignment_scoped(
                    child,
                    source,
                    &mut inner_commands,
                    variable_scope,
                )?;
                env_vars.push(EnvironmentVariable {
                    name: assignment.name,
                    value: assignment.value,
                });
            }
            "command_name" => {
                let argument = named_children(child).first().copied().unwrap_or(child);
                argv.push(resolve_argument_scoped(
                    argument,
                    source,
                    &mut inner_commands,
                    variable_scope,
                    false,
                )?);
            }
            "word"
            | "number"
            | "raw_string"
            | "string"
            | "concatenation"
            | "arithmetic_expansion"
            | "simple_expansion" => {
                argv.push(resolve_argument_scoped(
                    child,
                    source,
                    &mut inner_commands,
                    variable_scope,
                    false,
                )?);
            }
            "file_redirect" => redirects.push(parse_redirect_scoped(
                child,
                source,
                &mut inner_commands,
                variable_scope,
            )?),
            "herestring_redirect" => {
                validate_herestring_redirect(child, source, &mut inner_commands, variable_scope)?
            }
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported command child: {other}"),
                ));
            }
        }
    }

    let raw_text = node_text(node, source).unwrap_or_default();
    let text = if SIMPLE_EXPANSION_IN_TEXT_RE.is_match(raw_text) || raw_text.contains('\n') {
        argv.iter()
            .map(|argument| shell_escape_argument(argument))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        raw_text.to_string()
    };
    commands.extend(inner_commands);
    commands.push(SimpleCommand {
        argv,
        env_vars,
        redirects,
        text,
    });
    Ok(())
}

#[derive(Clone, Debug)]
struct ScopedAssignment {
    name: String,
    value: String,
    append: bool,
}

fn parse_variable_assignment_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<ScopedAssignment, ParseForSecurityResult> {
    let mut name = None;
    let mut value = String::new();
    let mut append = false;
    for child in all_children(node) {
        match child.kind() {
            "variable_name" => {
                name = Some(node_text(child, source).unwrap_or_default().to_string())
            }
            "=" => {}
            "+=" => append = true,
            "command_substitution" => {
                collect_command_substitution_scoped(child, source, commands, variable_scope)?;
                value = CMDSUB_PLACEHOLDER.to_string();
            }
            "simple_expansion" => {
                value = resolve_simple_expansion(child, source, variable_scope, true)?;
            }
            _ => {
                value = resolve_argument_scoped(child, source, commands, variable_scope, false)?;
            }
        }
    }
    let Some(name) = name else {
        return Err(too_complex(node, "Variable assignment without name"));
    };
    if !VARIABLE_NAME_RE.is_match(&name) {
        return Err(too_complex(
            node,
            format!("Invalid variable name (bash treats as command): {name}"),
        ));
    }
    if name == "IFS" {
        return Err(too_complex(node, "IFS assignment changes word-splitting"));
    }
    if name == "PS4" {
        if append || contains_placeholder(&value) {
            return Err(too_complex(
                node,
                "PS4 assignment cannot be statically verified",
            ));
        }
        let without_refs = PS4_REFERENCE_RE.replace_all(&value, "");
        if !PS4_SAFE_REMAINDER_RE.is_match(&without_refs) {
            return Err(too_complex(
                node,
                "PS4 value is outside the safe character set",
            ));
        }
    }
    if value.contains('~') {
        return Err(too_complex(
            node,
            "Tilde in assignment value may expand at assignment time",
        ));
    }
    Ok(ScopedAssignment {
        name,
        value,
        append,
    })
}

fn apply_variable_to_scope(
    variable_scope: &mut HashMap<String, String>,
    assignment: &ScopedAssignment,
) {
    let combined = if assignment.append {
        format!(
            "{}{}",
            variable_scope
                .get(&assignment.name)
                .map(String::as_str)
                .unwrap_or_default(),
            assignment.value
        )
    } else {
        assignment.value.clone()
    };
    variable_scope.insert(
        assignment.name.clone(),
        if contains_placeholder(&combined) {
            VAR_PLACEHOLDER.to_string()
        } else {
            combined
        },
    );
}

fn collect_command_substitution_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let mut inner_scope = variable_scope.clone();
    for child in all_children(node) {
        if matches!(child.kind(), "$(" | "`" | ")") {
            continue;
        }
        collect_commands_scoped(child, source, commands, &mut inner_scope)?;
    }
    Ok(())
}

fn parse_redirect_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<Redirect, ParseForSecurityResult> {
    let mut fd = None;
    let mut op = None;
    let mut target = None;
    for child in all_children(node) {
        match child.kind() {
            "file_descriptor" => {
                fd = node_text(child, source).and_then(|text| text.parse::<u32>().ok());
            }
            ">" | ">>" | "<" | ">&" | "<&" | ">|" | "&>" | "&>>" => {
                op = Some(child.kind().to_string());
            }
            "word" | "number" | "raw_string" | "string" | "concatenation" => {
                if child.kind() == "number" && child.child_count() > 0 {
                    return Err(too_complex(child, "Redirect number contains expansion"));
                }
                target = Some(resolve_argument_scoped(
                    child,
                    source,
                    commands,
                    variable_scope,
                    false,
                )?);
            }
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported redirect child: {other}"),
                ));
            }
        }
    }
    match (op, target) {
        (Some(op), Some(target)) => Ok(Redirect { op, target, fd }),
        _ => Err(too_complex(node, "Unrecognized redirect shape")),
    }
}

fn validate_heredoc_redirect(node: Node<'_>, source: &str) -> Result<(), ParseForSecurityResult> {
    let mut start = None;
    for child in all_children(node) {
        match child.kind() {
            "heredoc_start" => start = node_text(child, source),
            "heredoc_body" => {
                for content in named_children(child) {
                    if content.kind() != "heredoc_content" {
                        return Err(too_complex(content, "Dynamic heredoc body"));
                    }
                }
            }
            "<<" | "<<-" | "heredoc_end" | "file_descriptor" => {}
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported heredoc child: {other}"),
                ));
            }
        }
    }
    let quoted = start.is_some_and(|delimiter| {
        (delimiter.starts_with('\'') && delimiter.ends_with('\''))
            || (delimiter.starts_with('"') && delimiter.ends_with('"'))
            || delimiter.starts_with('\\')
    });
    if quoted {
        Ok(())
    } else {
        Err(too_complex(
            node,
            "Heredoc with unquoted delimiter undergoes shell expansion",
        ))
    }
}

fn validate_herestring_redirect(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    for child in all_children(node) {
        if child.kind() == "<<<" {
            continue;
        }
        let content = resolve_argument_scoped(child, source, commands, variable_scope, false)?;
        if content
            .split_inclusive('\n')
            .skip(1)
            .any(|line| line.trim_start_matches([' ', '\t', '\n']).starts_with('#'))
        {
            return Err(too_complex(
                child,
                "Here-string contains newline-comment ambiguity",
            ));
        }
    }
    Ok(())
}

fn resolve_argument_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
    inside_string: bool,
) -> Result<String, ParseForSecurityResult> {
    let text = node_text(node, source).unwrap_or_default();
    match node.kind() {
        "command_name" => {
            let children = named_children(node);
            match children.as_slice() {
                [child] => resolve_argument_scoped(*child, source, commands, variable_scope, false),
                [] => resolve_unquoted_word(node, text),
                _ => Err(too_complex(node, "Ambiguous command name")),
            }
        }
        "word" => {
            if node.child_count() > 0 {
                return Err(too_complex(node, "Word contains an expansion child"));
            }
            resolve_unquoted_word(node, text)
        }
        "number" => {
            if node.child_count() > 0 {
                Err(too_complex(node, "Number node contains expansion"))
            } else {
                Ok(text.to_string())
            }
        }
        "variable_name" | "file_descriptor" | "string_content" => Ok(text.to_string()),
        "raw_string" => Ok(text
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .unwrap_or(text)
            .to_string()),
        "string" => resolve_string_scoped(node, source, commands, variable_scope),
        "concatenation" => {
            if has_brace_expansion(text) {
                return Err(too_complex(node, "Brace expansion"));
            }
            let mut result = String::new();
            for child in named_children(node) {
                result.push_str(&resolve_argument_scoped(
                    child,
                    source,
                    commands,
                    variable_scope,
                    false,
                )?);
            }
            Ok(result)
        }
        "simple_expansion" => resolve_simple_expansion(node, source, variable_scope, inside_string),
        "arithmetic_expansion" => {
            validate_arithmetic(node, source)?;
            Ok(text.to_string())
        }
        "command_substitution" => Err(too_complex(
            node,
            "Bare command substitution can hide a path or flag",
        )),
        other => Err(too_complex(
            node,
            format!("Dynamic Bash argument node: {other}"),
        )),
    }
}

fn resolve_string_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<String, ParseForSecurityResult> {
    let text = node_text(node, source).unwrap_or_default();
    let children = all_children(node);
    if children.is_empty() {
        return Ok(text
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(text)
            .to_string());
    }

    let mut result = String::new();
    let mut cursor = None;
    let mut dynamic = false;
    let mut literal = false;
    for child in children {
        if let Some(previous) = cursor
            && child.start_byte() > previous
            && child.kind() != "\""
        {
            let gap = source.get(previous..child.start_byte()).unwrap_or_default();
            let newline_count = gap.bytes().filter(|byte| *byte == b'\n').count();
            if newline_count > 0 {
                result.push_str(&"\n".repeat(newline_count));
                literal = true;
            }
        }
        cursor = Some(child.end_byte());
        match child.kind() {
            "\"" => {}
            "string_content" => {
                result.push_str(&resolve_double_quoted_content(
                    node_text(child, source).unwrap_or_default(),
                ));
                literal = true;
            }
            "$" => {
                result.push('$');
                literal = true;
            }
            "command_substitution" => {
                if let Some(body) = extract_safe_cat_heredoc(child, source)? {
                    let trimmed = body.trim_end_matches('\n');
                    if !trimmed.contains('\n') {
                        result.push_str(trimmed);
                    }
                    literal = true;
                } else {
                    collect_command_substitution_scoped(child, source, commands, variable_scope)?;
                    result.push_str(CMDSUB_PLACEHOLDER);
                    dynamic = true;
                }
            }
            "simple_expansion" => {
                let value = resolve_simple_expansion(child, source, variable_scope, true)?;
                if value == VAR_PLACEHOLDER {
                    dynamic = true;
                } else {
                    literal = true;
                }
                result.push_str(&value);
            }
            "arithmetic_expansion" => {
                validate_arithmetic(child, source)?;
                result.push_str(node_text(child, source).unwrap_or_default());
                literal = true;
            }
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported double-quoted child: {other}"),
                ));
            }
        }
    }
    if dynamic && !literal {
        return Err(too_complex(
            node,
            "Runtime value is the entire quoted argument",
        ));
    }
    Ok(result)
}

/// Maps to CC `extractSafeCatHeredoc` (`ast.ts:1721-1775`).
fn extract_safe_cat_heredoc(
    substitution: Node<'_>,
    source: &str,
) -> Result<Option<String>, ParseForSecurityResult> {
    let statements = named_children(substitution);
    let [statement] = statements.as_slice() else {
        return Ok(None);
    };
    if statement.kind() != "redirected_statement" {
        return Ok(None);
    }

    let mut saw_cat = false;
    let mut body = None;
    for child in named_children(*statement) {
        match child.kind() {
            "command" => {
                let command_children = named_children(child);
                if command_children.len() != 1
                    || command_children[0].kind() != "command_name"
                    || node_text(command_children[0], source) != Some("cat")
                {
                    return Ok(None);
                }
                saw_cat = true;
            }
            "heredoc_redirect" => {
                if validate_heredoc_redirect(child, source).is_err() {
                    return Ok(None);
                }
                body = named_children(child)
                    .into_iter()
                    .find(|node| node.kind() == "heredoc_body")
                    .and_then(|node| node_text(node, source))
                    .map(ToOwned::to_owned);
            }
            _ => return Ok(None),
        }
    }
    let Some(body) = body.filter(|_| saw_cat) else {
        return Ok(None);
    };
    if body.contains("/proc/") && body.contains("/environ") || JQ_SYSTEM_RE.is_match(&body) {
        return Err(too_complex(
            substitution,
            "Quoted cat heredoc contains security-sensitive content",
        ));
    }
    Ok(Some(body))
}

fn resolve_simple_expansion(
    node: Node<'_>,
    source: &str,
    variable_scope: &HashMap<String, String>,
    inside_string: bool,
) -> Result<String, ParseForSecurityResult> {
    const SAFE_ENV_VARS: &[&str] = &[
        "HOME",
        "PWD",
        "OLDPWD",
        "USER",
        "LOGNAME",
        "SHELL",
        "PATH",
        "HOSTNAME",
        "UID",
        "EUID",
        "PPID",
        "RANDOM",
        "SECONDS",
        "LINENO",
        "TMPDIR",
        "BASH_VERSION",
        "BASHPID",
        "SHLVL",
        "HISTFILE",
        "IFS",
    ];
    const SPECIAL_VARS: &[&str] = &["?", "$", "!", "#", "0", "-"];

    let mut variable_name = None;
    let mut special = false;
    for child in named_children(node) {
        if child.kind() == "variable_name" {
            variable_name = node_text(child, source).map(ToOwned::to_owned);
            break;
        }
        if child.kind() == "special_variable_name" {
            variable_name = node_text(child, source).map(ToOwned::to_owned);
            special = true;
            break;
        }
    }
    let Some(variable_name) = variable_name else {
        return Err(too_complex(node, "Simple expansion has no variable name"));
    };
    if let Some(value) = variable_scope.get(&variable_name) {
        if contains_placeholder(value) {
            return if inside_string {
                Ok(VAR_PLACEHOLDER.to_string())
            } else {
                Err(too_complex(
                    node,
                    "Bare tracked variable has a runtime value",
                ))
            };
        }
        if !inside_string
            && (value.is_empty()
                || value
                    .chars()
                    .any(|character| matches!(character, ' ' | '\t' | '\n' | '*' | '?' | '[')))
        {
            return Err(too_complex(
                node,
                "Bare variable undergoes word splitting or glob expansion",
            ));
        }
        return Ok(value.clone());
    }
    if inside_string
        && (SAFE_ENV_VARS.contains(&variable_name.as_str())
            || special
                && (SPECIAL_VARS.contains(&variable_name.as_str())
                    || variable_name
                        .chars()
                        .all(|character| character.is_ascii_digit())))
    {
        return Ok(VAR_PLACEHOLDER.to_string());
    }
    Err(too_complex(node, "Untracked variable expansion"))
}

fn validate_arithmetic(node: Node<'_>, source: &str) -> Result<(), ParseForSecurityResult> {
    for child in all_children(node) {
        if child.child_count() == 0 {
            let text = node_text(child, source).unwrap_or_default();
            if !is_safe_arithmetic_leaf(text) {
                return Err(too_complex(
                    child,
                    format!("Arithmetic expansion references variable or non-literal: {text}"),
                ));
            }
            continue;
        }
        if matches!(
            child.kind(),
            "binary_expression"
                | "unary_expression"
                | "ternary_expression"
                | "parenthesized_expression"
        ) {
            validate_arithmetic(child, source)?;
        } else {
            return Err(too_complex(child, "Unsupported arithmetic expression"));
        }
    }
    Ok(())
}

fn is_safe_arithmetic_leaf(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    if text.chars().all(|character| character.is_ascii_digit()) {
        return true;
    }
    if text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return true;
    }
    if let Some((base, digits)) = text.split_once('#') {
        return !base.is_empty()
            && base.chars().all(|c| c.is_ascii_digit())
            && !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_alphanumeric());
    }
    text.chars()
        .all(|character| "-+*/%^&|~!<>=?:(),$".contains(character))
}

fn has_brace_expansion(text: &str) -> bool {
    text.split('{').skip(1).any(|suffix| {
        suffix
            .split_once('}')
            .is_some_and(|(inside, _)| inside.contains(',') || inside.contains(".."))
    })
}

fn contains_placeholder(value: &str) -> bool {
    value.contains(CMDSUB_PLACEHOLDER) || value.contains(VAR_PLACEHOLDER)
}

fn shell_escape_argument(argument: &str) -> String {
    if argument.is_empty()
        || argument
            .chars()
            .any(|character| "\"'\\ \t\n$`;|&<>(){}*?[]~#".contains(character))
    {
        format!("'{}'", argument.replace('\'', "'\\''"))
    } else {
        argument.to_string()
    }
}

#[allow(dead_code)]
fn collect_commands(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
) -> Result<(), ParseForSecurityResult> {
    match node.kind() {
        "command" => collect_simple_command(node, source, commands),
        "program" | "list" | "pipeline" | "negated_command" | "subshell" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_commands(child, source, commands)?;
            }
            Ok(())
        }
        "redirected_statement" => {
            let Some(body) = node.child_by_field_name("body") else {
                return Err(too_complex(node, "Redirected statement has no body"));
            };
            let start = commands.len();
            collect_commands(body, source, commands)?;
            let mut redirects = Vec::new();
            let mut nested = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.id() == body.id() {
                    continue;
                }
                match child.kind() {
                    "file_redirect" => {
                        redirects.push(parse_redirect(child, source, &mut nested)?);
                    }
                    "heredoc_redirect" | "herestring_redirect" => {
                        return Err(too_complex(child, "Here redirects require approval"));
                    }
                    _ => {}
                }
            }
            if !nested.is_empty() {
                return Err(too_complex(
                    node,
                    "Command substitution in redirect target requires approval",
                ));
            }
            for command in &mut commands[start..] {
                command.redirects.extend(redirects.iter().cloned());
            }
            Ok(())
        }
        "comment" => Ok(()),
        "variable_assignment" => collect_nested_command_substitutions(node, source, commands),
        other => Err(too_complex(
            node,
            format!("Unsupported Bash syntax node: {other}"),
        )),
    }
}

fn collect_nested_command_substitutions(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
) -> Result<(), ParseForSecurityResult> {
    if node.kind() == "command_substitution" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_commands(child, source, commands)?;
        }
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_nested_command_substitutions(child, source, commands)?;
    }
    Ok(())
}

fn collect_simple_command(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
) -> Result<(), ParseForSecurityResult> {
    let mut argv = Vec::new();
    let mut env_vars = Vec::new();
    let mut redirects = Vec::new();
    let mut nested = Vec::new();
    let Some(name) = node.child_by_field_name("name") else {
        return Err(too_complex(node, "Command has no static name"));
    };
    argv.push(resolve_argument(name, source, &mut nested)?);

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.id() == name.id() {
            continue;
        }
        if matches!(
            child.kind(),
            "variable_assignment" | "file_redirect" | "herestring_redirect"
        ) {
            if child.kind() == "variable_assignment" {
                env_vars.push(parse_variable_assignment(child, source, &mut nested)?);
            } else if child.kind() == "herestring_redirect" {
                return Err(too_complex(child, "Here-string redirects require approval"));
            } else {
                redirects.push(parse_redirect(child, source, &mut nested)?);
            }
            continue;
        }
        argv.push(resolve_argument(child, source, &mut nested)?);
    }
    commands.extend(nested);
    commands.push(SimpleCommand {
        argv,
        env_vars,
        redirects,
        text: node_text(node, source).unwrap_or_default().to_string(),
    });
    Ok(())
}

fn parse_variable_assignment(
    node: Node<'_>,
    source: &str,
    nested: &mut Vec<SimpleCommand>,
) -> Result<EnvironmentVariable, ParseForSecurityResult> {
    let Some(name) = node.child_by_field_name("name") else {
        return Err(too_complex(node, "Variable assignment has no name"));
    };
    if name.kind() != "variable_name" {
        return Err(too_complex(node, "Array assignments require approval"));
    }
    let value = match node.child_by_field_name("value") {
        Some(value) => resolve_argument(value, source, nested)?,
        None => String::new(),
    };
    Ok(EnvironmentVariable {
        name: node_text(name, source).unwrap_or_default().to_string(),
        value,
    })
}

fn parse_redirect(
    node: Node<'_>,
    source: &str,
    nested: &mut Vec<SimpleCommand>,
) -> Result<Redirect, ParseForSecurityResult> {
    let descriptor = node
        .child_by_field_name("descriptor")
        .and_then(|descriptor| node_text(descriptor, source))
        .and_then(|descriptor| descriptor.parse::<u32>().ok());
    let destination = node.child_by_field_name("destination").or_else(|| {
        let mut cursor = node.walk();

        node.named_children(&mut cursor)
            .find(|child| child.kind() != "file_descriptor")
    });
    let Some(destination) = destination else {
        return Err(too_complex(node, "Redirect has no static destination"));
    };
    let target = resolve_argument(destination, source, nested)?;
    let mut cursor = node.walk();
    let op = node
        .children(&mut cursor)
        .map(|child| child.kind())
        .find(|kind| {
            matches!(
                *kind,
                ">" | ">>" | "<" | ">&" | "< &" | "<&" | ">|" | "&>" | "&>>"
            )
        })
        .unwrap_or_default()
        .to_string();
    if op.is_empty() {
        return Err(too_complex(node, "Redirect has an unknown operator"));
    }
    Ok(Redirect {
        op,
        target,
        fd: descriptor,
    })
}

fn resolve_argument(
    node: Node<'_>,
    source: &str,
    nested: &mut Vec<SimpleCommand>,
) -> Result<String, ParseForSecurityResult> {
    let text = node_text(node, source).unwrap_or_default();
    match node.kind() {
        "command_name" => {
            let mut cursor = node.walk();
            let mut children = node.named_children(&mut cursor);
            match (children.next(), children.next()) {
                (Some(child), None) => resolve_argument(child, source, nested),
                (None, _) => resolve_unquoted_word(node, text),
                _ => Err(too_complex(node, "Ambiguous command name")),
            }
        }
        "word" | "number" | "variable_name" => resolve_unquoted_word(node, text),
        "string_content" => Ok(resolve_double_quoted_content(text)),
        "raw_string" => Ok(text
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .unwrap_or(text)
            .to_string()),
        "string" => {
            let mut result = String::new();
            let mut cursor = node.walk();
            let children = node.named_children(&mut cursor).collect::<Vec<_>>();
            if children.is_empty() {
                return Ok(text
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(text)
                    .to_string());
            }
            let mut byte_cursor = node.start_byte().saturating_add(1);
            for child in children {
                if child.start_byte() > byte_cursor {
                    result.extend(
                        source[byte_cursor..child.start_byte()]
                            .chars()
                            .filter(|ch| *ch == '\n'),
                    );
                }
                if child.kind() == "command_substitution" {
                    let mut cursor = child.walk();
                    for inner in child.named_children(&mut cursor) {
                        collect_commands(inner, source, nested)?;
                    }
                    result.push_str("__CMDSUB_OUTPUT__");
                } else {
                    result.push_str(&resolve_argument(child, source, nested)?);
                }
                byte_cursor = child.end_byte();
            }
            let content_end = node.end_byte().saturating_sub(1);
            if content_end > byte_cursor {
                result.extend(
                    source[byte_cursor..content_end]
                        .chars()
                        .filter(|ch| *ch == '\n'),
                );
            }
            if result == "__CMDSUB_OUTPUT__" {
                return Err(too_complex(
                    node,
                    "Command substitution is the entire argument",
                ));
            }
            Ok(result)
        }
        "concatenation" => {
            let mut result = String::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                result.push_str(&resolve_argument(child, source, nested)?);
            }
            Ok(result)
        }
        "command_substitution" => Err(too_complex(
            node,
            "Bare command substitution can hide a path or flag",
        )),
        "file_descriptor" => Ok(text.to_string()),
        other => Err(too_complex(
            node,
            format!("Dynamic Bash argument node: {other}"),
        )),
    }
}

fn resolve_double_quoted_content(text: &str) -> String {
    let mut resolved = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && chars
                .peek()
                .is_some_and(|next| matches!(*next, '$' | '`' | '"' | '\\' | '\n'))
        {
            if let Some(next) = chars.next()
                && next != '\n'
            {
                resolved.push(next);
            }
        } else {
            resolved.push(ch);
        }
    }
    resolved
}

fn resolve_unquoted_word(node: Node<'_>, text: &str) -> Result<String, ParseForSecurityResult> {
    let brace_expansion = text.split('{').skip(1).any(|suffix| {
        suffix
            .split_once('}')
            .is_some_and(|(inside, _)| inside.contains(',') || inside.contains(".."))
    });
    if brace_expansion {
        return Err(too_complex(node, "Word contains brace expansion syntax"));
    }
    let mut resolved = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let Some(next) = chars.next() else {
                return Err(too_complex(node, "Trailing escape in Bash word"));
            };
            resolved.push(next);
        } else {
            resolved.push(ch);
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argvs(command: &str) -> Vec<Vec<String>> {
        match parse_for_security(command) {
            ParseForSecurityResult::Simple { commands } => {
                commands.into_iter().map(|command| command.argv).collect()
            }
            other => panic!("expected simple parse, got {other:?}"),
        }
    }

    #[test]
    fn extracts_static_commands_across_operators_and_quotes() {
        assert_eq!(
            argvs("printf '%s' hi | git status && echo \"done\""),
            vec![
                vec!["printf", "%s", "hi"],
                vec!["git", "status"],
                vec!["echo", "done"],
            ]
        );
    }

    #[test]
    fn extracts_static_redirect_targets_and_descriptors() {
        let ParseForSecurityResult::Simple { commands } = parse_for_security("echo hi 2>&1 > out")
        else {
            panic!("redirect fixture parses");
        };
        assert_eq!(
            commands[0].redirects,
            vec![
                Redirect {
                    op: ">&".to_string(),
                    target: "1".to_string(),
                    fd: Some(2),
                },
                Redirect {
                    op: ">".to_string(),
                    target: "out".to_string(),
                    fd: None,
                },
            ]
        );
    }

    #[test]
    fn recursively_extracts_command_substitutions() {
        assert_eq!(
            argvs("echo \"status: $(git status)\""),
            vec![
                vec!["git", "status"],
                vec!["echo", "status: __CMDSUB_OUTPUT__"],
            ]
        );
        assert!(matches!(
            parse_for_security("cat \"$(printf /etc/passwd)\""),
            ParseForSecurityResult::TooComplex { .. }
        ));
    }

    #[test]
    fn semantic_checks_see_wrapped_eval_and_argv_hazards() {
        for command in [
            "eval 'echo bad'",
            "timeout -k 1 5 eval 'echo bad'",
            "env FOO=bar jq 'system(\"id\")'",
            "cat /proc/self/environ",
            "printf -v 'a[$(id)]' x",
        ] {
            let ParseForSecurityResult::Simple { commands } = parse_for_security(command) else {
                panic!("{command:?} should have a static argv parse");
            };
            assert!(
                matches!(
                    check_semantics(&commands),
                    SemanticCheckResult::Unsafe { .. }
                ),
                "{command:?} must fail semantic checks"
            );
        }
        let ParseForSecurityResult::Simple { commands } =
            parse_for_security("command -v git && fc -l && compgen -c")
        else {
            panic!("safe semantic fixture parses");
        };
        assert_eq!(check_semantics(&commands), SemanticCheckResult::Ok);
    }

    #[test]
    fn semantic_subscript_and_wrapper_attack_matrix_matches_cc_2_1_88() {
        for command in [
            "printf -v 'a[$(id)]' x",
            "printf -rv 'a[$(id)]' x",
            "printf '-va[$(id)]' x",
            "read 'a[$(id)]' <<< data",
            "read -p '[safe] ' variable",
            "[[ 'a[$(id)]' -eq 0 ]]",
            "jq -f=script.jq input.json",
            "nice $((0-5)) jq 'system(\"id\")'",
        ] {
            let ParseForSecurityResult::Simple { commands } = parse_for_security(command) else {
                panic!("semantic fixture should tokenize: {command:?}");
            };
            let semantics = check_semantics(&commands);
            if command == "read -p '[safe] ' variable" {
                assert_eq!(semantics, SemanticCheckResult::Ok, "{command:?}");
            } else {
                assert!(
                    matches!(semantics, SemanticCheckResult::Unsafe { .. }),
                    "expected semantic rejection: {command:?}"
                );
            }
        }
    }

    #[test]
    fn tracked_scope_control_flow_and_literal_expansions_match_cc_2_1_88() {
        let cases = [
            (
                "FOO=bar && echo \"$FOO\"",
                vec![vec!["echo".to_string(), "bar".to_string()]],
            ),
            (
                "if true; then echo hi; fi",
                vec![
                    vec!["true".to_string()],
                    vec!["echo".to_string(), "hi".to_string()],
                ],
            ),
            (
                "while false; do echo x; done",
                vec![
                    vec!["false".to_string()],
                    vec!["echo".to_string(), "x".to_string()],
                ],
            ),
            (
                "[[ -f foo ]] && cat foo",
                vec![
                    vec!["[[".to_string(), "-f".to_string(), "foo".to_string()],
                    vec!["cat".to_string(), "foo".to_string()],
                ],
            ),
            (
                "echo \"prefix$HOME\"",
                vec![vec![
                    "echo".to_string(),
                    "prefix__TRACKED_VAR__".to_string(),
                ]],
            ),
            (
                "X=\"a b\"; printf \"%s\" \"$X\"",
                vec![vec![
                    "printf".to_string(),
                    "%s".to_string(),
                    "a b".to_string(),
                ]],
            ),
            ("cat <<< hello", vec![vec!["cat".to_string()]]),
            (
                "echo $((1+2))",
                vec![vec!["echo".to_string(), "$((1+2))".to_string()]],
            ),
            (
                "declare X=foo",
                vec![vec!["declare".to_string(), "X=foo".to_string()]],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(argvs(source), expected, "source={source:?}");
        }

        assert_eq!(
            argvs("cat <<'EOF'\nhello\nEOF"),
            vec![vec!["cat".to_string()]]
        );
        assert_eq!(
            argvs("gh pr create --body \"$(cat <<'EOF'\n## Summary\ntext\nEOF\n)\""),
            vec![vec![
                "gh".to_string(),
                "pr".to_string(),
                "create".to_string(),
                "--body".to_string(),
                String::new(),
            ]]
        );
        assert_eq!(
            argvs("rm \"$(cat <<'EOF'\n/etc/passwd\nEOF\n)\""),
            vec![vec!["rm".to_string(), "/etc/passwd".to_string()]]
        );
    }

    #[test]
    fn scope_boundaries_and_dynamic_values_remain_fail_closed() {
        for command in [
            "FOO=bar echo \"$FOO\"",
            "true || FLAG=--safe && cmd $FLAG",
            "echo $HOME",
            "echo \"$HOME\"",
            "X=\"a b\"; printf '%s' $X",
            "cat <<EOF\nhello\nEOF",
            "x=1; echo $((x+2))",
            "X=(a b)",
            "f(){ echo hi; }; f",
            "echo {a'}',b}",
            "cat \"$(cat <<'EOF'\n/proc/self/environ\nEOF\n)\"",
        ] {
            assert!(
                matches!(
                    parse_for_security(command),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "expected fail-closed parse for {command:?}"
            );
        }
    }

    #[test]
    fn malformed_and_dynamic_syntax_fail_closed() {
        assert!(has_parse_error("echo 'unterminated"));
        assert!(matches!(
            parse_for_security("cat $UNTRUSTED"),
            ParseForSecurityResult::TooComplex { .. }
        ));
        assert!(matches!(
            parse_for_security("echo\u{00a0}git status"),
            ParseForSecurityResult::TooComplex { .. }
        ));
        assert_eq!(
            parse_for_security(&format!("echo {}", "x".repeat(10_001))),
            ParseForSecurityResult::ParseUnavailable
        );
    }
}
