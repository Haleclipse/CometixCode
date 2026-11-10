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
pub const MAX_COMMANDS_FOR_SECURITY_CHECK: usize = 50;
const MAX_PROJECTION_NODE_VISITS: usize = 4096;
const MAX_STATIC_VALUE_BYTES: usize = 10_000;

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
        "getopts",
        "readonly",
        "set",
        "shopt",
        "unalias",
        "sh",
        "bash",
        "zsh",
        "dash",
        "ksh",
        "fish",
        "csh",
        "tcsh",
    ];
    for command in commands {
        if command
            .env_vars
            .iter()
            .any(|variable| is_execution_environment_variable(&variable.name))
        {
            return semantic_error(
                "Environment assignment can change command execution or path resolution",
            );
        }
        if command
            .argv
            .iter()
            .chain(command.env_vars.iter().map(|variable| &variable.value))
            .chain(command.redirects.iter().map(|redirect| &redirect.target))
            .any(|argument| contains_placeholder(argument))
        {
            return semantic_error(
                "Runtime argument or redirect target cannot be statically verified",
            );
        }
        let mut argv = command.argv.as_slice();
        loop {
            if matches!(
                argv.first().and_then(|name| name.rsplit('/').next()),
                Some("env" | "export" | "readonly" | "local" | "declare" | "typeset")
            ) && argv.iter().skip(1).any(|argument| {
                argument.split_once('=').is_some_and(|(name, _)| {
                    is_execution_environment_variable(name.trim_end_matches('+'))
                })
            }) {
                return semantic_error(
                    "Environment assignment can change command execution or path resolution",
                );
            }
            match argv.first().and_then(|name| name.rsplit('/').next()) {
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
        let Some(name) = argv.first().and_then(|name| name.rsplit('/').next()) else {
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
        if argv.len() != command.argv.len()
            && matches!(
                name,
                "read" | "unset" | "export" | "local" | "declare" | "typeset"
            )
        {
            return semantic_error("Wrapped shell builtin changes variables outside tracked scope");
        }
        if name == "wait"
            && argv
                .iter()
                .skip(1)
                .any(|arg| arg.starts_with('-') && arg.contains('p'))
        {
            return semantic_error("wait -p changes variables at runtime");
        }
        if name == "[["
            && argv.iter().enumerate().any(|(index, argument)| {
                matches!(
                    argument.as_str(),
                    "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"
                ) && (index == 0
                    || !is_signed_integer(&argv[index - 1])
                    || argv
                        .get(index + 1)
                        .is_none_or(|value| !is_signed_integer(value)))
            })
        {
            return semantic_error("Arithmetic test operands cannot be statically verified");
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
        if matches!(name, "read" | "unset")
            && argv
                .iter()
                .skip(1)
                .any(|arg| is_execution_environment_variable(arg))
        {
            return semantic_error("Shell builtin can change command execution or path resolution");
        }
        if name == "printf"
            && argv
                .iter()
                .skip(1)
                .any(|arg| arg.starts_with('-') && arg.contains('v'))
        {
            return semantic_error("printf -v changes variables at runtime");
        }
        if has_subscript_eval_operand(name, argv) {
            return semantic_error(format!(
                "'{name}' NAME operand contains an array subscript evaluated by Bash"
            ));
        }
    }
    SemanticCheckResult::Ok
}

fn is_execution_environment_variable(name: &str) -> bool {
    name.starts_with("LD_")
        || name.starts_with("DYLD_")
        || matches!(
            name,
            "PATH" | "BASH_ENV" | "ENV" | "SHELLOPTS" | "BASHOPTS" | "CDPATH" | "GLOBIGNORE"
        )
}

fn is_runtime_variable(name: &str) -> bool {
    matches!(
        name,
        "RANDOM"
            | "SRANDOM"
            | "SECONDS"
            | "EPOCHSECONDS"
            | "EPOCHREALTIME"
            | "LINENO"
            | "BASH_COMMAND"
            | "BASH_SUBSHELL"
            | "BASH_REMATCH"
    )
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
    let mut visits_left = MAX_PROJECTION_NODE_VISITS;
    let mut iterations_left = MAX_COMMANDS_FOR_SECURITY_CHECK;
    if let Err(error) = check_projection_work(root, 1, &mut visits_left, &mut iterations_left) {
        return error;
    }
    let mut commands = Vec::new();
    let mut variable_scope = HashMap::new();
    match collect_commands_scoped(root, command, &mut commands, &mut variable_scope) {
        Ok(()) if commands.len() <= MAX_COMMANDS_FOR_SECURITY_CHECK => {
            ParseForSecurityResult::Simple { commands }
        }
        Ok(()) => too_complex(root, "Too many commands after static loop expansion"),
        Err(error) => error,
    }
}

// Precharge the complete expanded tree, not just emitted commands. Multiplicity
// carries through every subtree, including substitutions; neither an empty body
// nor a fresh command buffer can reset the shared, monotonically spent budget.
fn check_projection_work(
    node: Node<'_>,
    multiplicity: usize,
    visits_left: &mut usize,
    iterations_left: &mut usize,
) -> Result<(), ParseForSecurityResult> {
    *visits_left = visits_left
        .checked_sub(multiplicity)
        .ok_or_else(|| too_complex(node, "Bash projection node-visit budget exceeded"))?;
    let body = node.child_by_field_name("body");
    let iterations = if node.kind() == "for_statement" {
        if node.child(0).map(|keyword| keyword.kind()) != Some("for") {
            return Err(too_complex(
                node,
                "Only literal for statements can be expanded",
            ));
        }
        let variable = node.child_by_field_name("variable");
        let count = named_children(node)
            .into_iter()
            .filter(|child| Some(*child) != variable && Some(*child) != body)
            .count();
        let expanded = multiplicity
            .checked_mul(count)
            .ok_or_else(|| too_complex(node, "Bash projection iteration budget exceeded"))?;
        *iterations_left = iterations_left
            .checked_sub(expanded)
            .ok_or_else(|| too_complex(node, "Bash projection iteration budget exceeded"))?;
        expanded
    } else {
        multiplicity
    };
    for child in all_children(node) {
        let copies = if node.kind() == "for_statement" && Some(child) == body {
            iterations
        } else {
            multiplicity
        };
        check_projection_work(child, copies, visits_left, iterations_left)?;
    }
    if node.kind() == "do_group"
        && named_children(node)
            .iter()
            .all(|child| child.kind() == "comment")
    {
        return Err(too_complex(node, "Empty loop body is not valid Bash"));
    }
    Ok(())
}

/// Maps to CC `walkProgram` / `collectCommands` (`ast.ts:462-960`).
///
/// Literal assignments flow through sequential clauses. Conditional writes
/// become unknown on exit, and pipelines/background clauses fork the incoming
/// scope, so skipped or repeated assignments cannot hide a runtime path.
fn collect_commands_scoped(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    if commands.len() >= MAX_COMMANDS_FOR_SECURITY_CHECK {
        return Err(too_complex(
            node,
            "Too many commands after static loop expansion",
        ));
    }
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
            commands.push(SimpleCommand {
                argv: Vec::new(),
                env_vars: vec![EnvironmentVariable {
                    name: assignment.name,
                    value: assignment.value,
                }],
                redirects: Vec::new(),
                text: node_text(node, source).unwrap_or_default().to_string(),
            });
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
    let mut conditional = false;

    for child in all_children(node) {
        let kind = child.kind();
        if is_separator(kind) {
            conditional |= kind == "&&";
            if matches!(kind, "||" | "|" | "|&" | "&") {
                invalidate_changed_scope(variable_scope, &incoming, &working);
                if pipeline {
                    working = incoming.clone();
                } else {
                    // A failed left branch can still have changed variables.
                    let branch = working.clone();
                    invalidate_changed_scope(&mut working, &incoming, &branch);
                }
                propagates = false;
            } else if pipeline {
                working = incoming.clone();
            }
            continue;
        }
        collect_commands_scoped(child, source, commands, &mut working)?;
    }

    if propagates && !conditional {
        *variable_scope = working;
    } else {
        invalidate_changed_scope(variable_scope, &incoming, &working);
    }
    Ok(())
}

fn invalidate_changed_scope(
    scope: &mut HashMap<String, String>,
    incoming: &HashMap<String, String>,
    branch: &HashMap<String, String>,
) {
    for name in incoming.keys().chain(branch.keys()) {
        if incoming.get(name) != branch.get(name) {
            scope.insert(name.clone(), VAR_PLACEHOLDER.to_string());
        }
    }
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
    // Bash expands all declaration operands before the builtin applies any of
    // them. A preceding operand must not change a later operand's expansion.
    let mut argument_scope = variable_scope.clone();
    let mut options_ended = false;
    let mut saw_operand = false;
    for child in all_children(node) {
        match child.kind() {
            "local" => {
                return Err(too_complex(
                    child,
                    "Function-local declarations are not modeled",
                ));
            }
            "export" | "readonly" | "declare" | "typeset" => {
                argv.push(child.kind().to_string());
            }
            "word" | "number" | "raw_string" | "string" | "concatenation" => {
                let arg =
                    resolve_argument_scoped(child, source, commands, &mut argument_scope, false)?;
                if arg.starts_with(['-', '+']) {
                    // Only export marking is modeled. Conversion, readonly,
                    // array, nameref, query/function and combined options can
                    // change or suppress subsequent assignments.
                    let supported = !options_ended
                        && !saw_operand
                        && (arg == "--"
                            || arg == "-x"
                                && matches!(
                                    argv.first().map(String::as_str),
                                    Some("declare" | "typeset")
                                ));
                    if !supported {
                        return Err(too_complex(
                            child,
                            format!("Unmodeled declaration option: {arg}"),
                        ));
                    }
                    options_ended |= arg == "--";
                    argv.push(arg);
                    continue;
                }
                saw_operand = true;
                if arg.split('=').next().is_some_and(|name| name.contains('[')) {
                    return Err(too_complex(
                        child,
                        format!("Declaration positional '{arg}' contains array subscript"),
                    ));
                }
                if let Some((name, _)) = arg.split_once('=') {
                    let name = name.trim_end_matches('+');
                    if VARIABLE_NAME_RE.is_match(name) {
                        variable_scope.insert(name.to_string(), VAR_PLACEHOLDER.to_string());
                    }
                }
                argv.push(arg);
            }
            "variable_assignment" => {
                let assignment =
                    parse_variable_assignment_scoped(child, source, commands, &mut argument_scope)?;
                apply_variable_to_scope(variable_scope, &assignment);
                saw_operand = true;
                argv.push(format!(
                    "{}{}={}",
                    assignment.name,
                    if assignment.append { "+" } else { "" },
                    assignment.value
                ));
            }
            "variable_name" => {
                saw_operand = true;
                argv.push(node_text(child, source).unwrap_or_default().to_string());
            }
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
    // `select` shares this grammar node, but its value comes from stdin.
    if node.child(0).map(|keyword| keyword.kind()) != Some("for") {
        return Err(too_complex(
            node,
            "select reads its loop value from stdin at runtime",
        ));
    }
    let variable_name = node_text(loop_variable, source).unwrap_or_default();
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    if may_declare_loop_variable(root, source, variable_name)
        || !is_projectable_loop_variable(variable_name)
    {
        return Err(too_complex(
            loop_variable,
            "For-loop variable may rebind inherited or shell-special state",
        ));
    }

    let mut values = Vec::new();
    for child in named_children(node) {
        if child.id() == loop_variable.id() || child.id() == body.id() {
            continue;
        }
        // Only literal lists are expanded: no globbing, substitutions, or
        // implicit positional parameters whose values depend on the shell.
        let mut pending = vec![child];
        while let Some(part) = pending.pop() {
            if !matches!(
                part.kind(),
                "word" | "number" | "raw_string" | "string" | "concatenation" | "string_content"
            ) || part.kind() == "word"
                && node_text(part, source).is_some_and(|text| text.contains(['*', '?', '[', '~']))
            {
                return Err(too_complex(part, "For-loop list is not statically literal"));
            }
            pending.extend(named_children(part));
        }
        let value = resolve_argument_scoped(child, source, commands, variable_scope, false)?;
        // The Bash tool may run zsh, whose snapshot restores user options:
        // under GLOB_SUBST an unquoted `$var` re-expands `~`, `<1-9>`, `(a|b)`
        // and other patterns, and a leading `=` is command-path expansion.
        // Accept only characters that no supported shell expands.
        if !value
            .chars()
            .all(|ch| ch.is_alphanumeric() || " \t\n_./:,@%+-".contains(ch))
        {
            return Err(too_complex(child, "For-loop value may expand at runtime"));
        }
        values.push(value);
        if values.len() > MAX_COMMANDS_FOR_SECURITY_CHECK {
            return Err(too_complex(node, "Too many static for-loop iterations"));
        }
    }
    if values.is_empty() {
        return Err(too_complex(
            node,
            "For-loop requires an explicit nonempty literal list",
        ));
    }

    let mut body_scope = variable_scope.clone();
    for value in values {
        body_scope.insert(variable_name.to_string(), value);
        collect_do_group(body, source, commands, &mut body_scope)?;
    }
    // Even a literal list can exit early through break/continue. Do not guess
    // which value survives after the loop.
    variable_scope.insert(variable_name.to_string(), VAR_PLACEHOLDER.to_string());
    Ok(())
}

/// The only names a literal `for` may bind. A loop variable keeps an existing
/// export and any special meaning, so it can change `~`, `cd`, git/XDG/tool
/// configuration or zsh's tied `path` without changing the command text.
/// Only ordinary scalar names are accepted, not an open-ended class such as
/// lowercase identifiers. They must also be absent from both inherited
/// environment carriers and all declarations in the input. An unexported
/// ordinary scalar cannot change a child's environment. Session scripts can
/// introduce arbitrary attributes, so their presence prevents expansion.
const PROJECTABLE_LOOP_VARIABLES: &[&str] = &[
    "i", "j", "k", "x", "y", "f", "d", "p", "item", "file", "dir", "name", "value", "entry", "arg",
    "word", "pkg", "target", "src", "dst", "line", "part", "key", "cmd", "action",
];

fn is_projectable_loop_variable(name: &str) -> bool {
    PROJECTABLE_LOOP_VARIABLES.contains(&name)
        && crate::utils::process_env::var_os(name).is_none()
        && !crate::utils::session_env_vars::get_session_env_vars().contains_key(name)
        && crate::utils::session_environment::get_session_environment_script().is_none()
}

/// A declaration anywhere may leave an export or other attribute on the loop
/// variable. Resolve only literal operand names: a dynamic `export "$arg"`
/// cannot prove that it names a different variable, even across branches.
fn may_declare_loop_variable(node: Node<'_>, source: &str, name: &str) -> bool {
    let operands = named_children(node);
    // The grammar can split one shell operand (`f"il"e`) into adjacent
    // named nodes. Individual fragments cannot prove a different name.
    if node.kind() == "declaration_command"
        && (operands.windows(2).any(|pair| {
            source
                .get(pair[0].end_byte()..pair[1].start_byte())
                .is_none_or(|gap| gap.trim_start_matches("\\\n").is_empty())
        }) || operands.iter().copied().any(|operand| {
            if operand.kind() == "variable_assignment" {
                return named_children(operand)
                    .into_iter()
                    .find(|child| child.kind() == "variable_name")
                    .and_then(|child| node_text(child, source))
                    .is_none_or(|declared| declared == name);
            }
            if operand.kind() == "variable_name" {
                return node_text(operand, source).is_none_or(|declared| declared == name);
            }
            literal_declaration_operand(operand, source).is_none_or(|value| {
                value
                    .split('=')
                    .next()
                    .is_none_or(|declared| declared.trim_end_matches('+') == name)
            })
        }))
    {
        return true;
    }
    operands
        .into_iter()
        .any(|child| may_declare_loop_variable(child, source, name))
}

// This scan must never enter a command collector: a substitution containing a
// loop would scan the same root again. Unknown syntax may name any variable.
fn literal_declaration_operand(node: Node<'_>, source: &str) -> Option<String> {
    let text = node_text(node, source)?;
    let value = match node.kind() {
        "word" | "number"
            if node.child_count() == 0
                && !text.starts_with('=')
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_+-=".contains(&byte)) =>
        {
            text.to_string()
        }
        "raw_string" => text.strip_prefix('\'')?.strip_suffix('\'')?.to_string(),
        "string" => {
            let children = all_children(node);
            if children.len() < 2
                || children.first().map(Node::kind) != Some("\"")
                || children.last().map(Node::kind) != Some("\"")
                || children[1..children.len() - 1]
                    .iter()
                    .any(|child| !matches!(child.kind(), "string_content" | "$"))
            {
                return None;
            }
            resolve_double_quoted_content(text.strip_prefix('"')?.strip_suffix('"')?)
        }
        "concatenation" => {
            let mut value = String::new();
            for child in named_children(node) {
                value.push_str(&literal_declaration_operand(child, source)?);
                if value.len() > MAX_STATIC_VALUE_BYTES {
                    return None;
                }
            }
            value
        }
        _ => return None,
    };
    (value.len() <= MAX_STATIC_VALUE_BYTES).then_some(value)
}

fn collect_do_group(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<SimpleCommand>,
    variable_scope: &mut HashMap<String, String>,
) -> Result<(), ParseForSecurityResult> {
    let before = commands.len();
    let incoming = variable_scope.clone();
    for child in all_children(node) {
        if matches!(child.kind(), "do" | "done" | ";" | "\n") {
            continue;
        }
        collect_commands_scoped(child, source, commands, variable_scope)?;
    }
    if *variable_scope != incoming
        || commands[before..].iter().any(|command| {
            !command.env_vars.is_empty()
                || matches!(
                    command.argv.first().map(String::as_str),
                    Some(
                        "export" | "local" | "readonly" | "declare" | "typeset" | "unset" | "read"
                    )
                )
        })
    {
        return Err(too_complex(
            node,
            "Loop variable mutations require manual approval",
        ));
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
        let body = node
            .child_by_field_name("body")
            .ok_or_else(|| too_complex(node, "Loop has no body"))?;
        let incoming = variable_scope.clone();
        // The grammar repeats the condition field for semicolon/newline lists;
        // child_by_field_name would silently omit every condition after the first.
        for condition in named_children(node) {
            if condition.id() == body.id() {
                continue;
            }
            let before = commands.len();
            collect_commands_scoped(condition, source, commands, variable_scope)?;
            track_read_variables_from_condition(&commands[before..], variable_scope)?;
        }
        if *variable_scope != incoming {
            return Err(too_complex(
                node,
                "Loop condition changes variables at runtime",
            ));
        }
        let mut body_scope = variable_scope.clone();
        collect_do_group(body, source, commands, &mut body_scope)?;
        return Ok(());
    }

    let incoming = variable_scope.clone();
    let mut then_scope = incoming.clone();
    let mut after_then = false;
    for child in all_children(node) {
        match child.kind() {
            "if" | "fi" | "else" | "elif" | ";" | "\n" => continue,
            "then" => {
                then_scope = variable_scope.clone();
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
                invalidate_changed_scope(variable_scope, &incoming, &branch_scope);
            }
            _ if after_then => {
                collect_commands_scoped(child, source, commands, &mut then_scope)?;
            }
            _ => {
                let before = commands.len();
                collect_commands_scoped(child, source, commands, variable_scope)?;
                track_read_variables_from_condition(&commands[before..], variable_scope)?;
            }
        }
    }
    invalidate_changed_scope(variable_scope, &incoming, &then_scope);
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
            | "command_substitution"
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

    if argv.first().is_some_and(|name| {
        matches!(
            name.as_str(),
            "export" | "readonly" | "local" | "declare" | "typeset" | "unset"
        )
    }) {
        return Err(too_complex(
            node,
            "Obfuscated shell builtin changes untracked state",
        ));
    }
    let raw_text = node_text(node, source).unwrap_or_default();
    let text = if SIMPLE_EXPANSION_IN_TEXT_RE.is_match(raw_text) || raw_text.contains('\n') {
        env_vars
            .iter()
            .map(|variable| {
                format!(
                    "{}={}",
                    variable.name,
                    shell_escape_argument(&variable.value)
                )
            })
            .chain(argv.iter().map(|argument| shell_escape_argument(argument)))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        raw_text.to_string()
    };
    if argv.first().is_some_and(|name| name == "read") {
        variable_scope.insert("REPLY".to_string(), VAR_PLACEHOLDER.to_string());
        for argument in argv
            .iter()
            .skip(1)
            .filter(|arg| VARIABLE_NAME_RE.is_match(arg))
        {
            variable_scope.insert(argument.clone(), VAR_PLACEHOLDER.to_string());
        }
    }
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
                .unwrap_or(VAR_PLACEHOLDER),
            assignment.value
        )
    } else {
        assignment.value.clone()
    };
    variable_scope.insert(
        assignment.name.clone(),
        if combined.len() > MAX_STATIC_VALUE_BYTES
            || contains_placeholder(&combined)
            || is_runtime_variable(&assignment.name)
        {
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
                if result.len() > MAX_STATIC_VALUE_BYTES {
                    return Err(too_complex(node, "Static argument byte budget exceeded"));
                }
            }
            Ok(result)
        }
        "simple_expansion" => resolve_simple_expansion(node, source, variable_scope, inside_string),
        "arithmetic_expansion" => {
            validate_arithmetic(node, source)?;
            Ok(VAR_PLACEHOLDER.to_string())
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
    if !text.starts_with('"')
        || !text.ends_with('"')
        || children.first().map(Node::kind) != Some("\"")
        || children.last().map(Node::kind) != Some("\"")
    {
        return Err(too_complex(node, "Unrecognized double-quoted boundaries"));
    }

    let mut result = String::new();
    let mut pending_literal = String::new();
    let mut cursor = node.start_byte() + 1;
    let mut dynamic = false;
    let mut literal = false;
    for child in children[1..children.len() - 1].iter().copied() {
        let gap = source
            .get(cursor..child.start_byte())
            .ok_or_else(|| too_complex(node, "Invalid double-quoted source span"))?;
        if !gap.replace("\\\n", "").bytes().all(|byte| byte == b'\n') {
            return Err(too_complex(node, "Unmodeled double-quoted source gap"));
        }
        pending_literal.push_str(gap);
        cursor = child.end_byte();
        if matches!(child.kind(), "string_content" | "$") {
            pending_literal.push_str(node_text(child, source).unwrap_or_default());
            continue;
        }
        let decoded = resolve_double_quoted_content(&pending_literal);
        literal |= !decoded.is_empty();
        result.push_str(&decoded);
        pending_literal.clear();
        match child.kind() {
            "command_substitution" => {
                if let Some(body) = extract_safe_cat_heredoc(child, source)? {
                    collect_command_substitution_scoped(child, source, commands, variable_scope)?;
                    result.push_str(body.trim_end_matches('\n'));
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
                result.push_str(VAR_PLACEHOLDER);
                dynamic = true;
            }
            other => {
                return Err(too_complex(
                    child,
                    format!("Unsupported double-quoted child: {other}"),
                ));
            }
        }
        if result.len() > MAX_STATIC_VALUE_BYTES {
            return Err(too_complex(node, "Static argument byte budget exceeded"));
        }
    }
    let tail = source
        .get(cursor..node.end_byte() - 1)
        .ok_or_else(|| too_complex(node, "Invalid double-quoted tail span"))?;
    if !tail.replace("\\\n", "").bytes().all(|byte| byte == b'\n') {
        return Err(too_complex(node, "Unmodeled double-quoted tail gap"));
    }
    pending_literal.push_str(tail);
    let decoded = resolve_double_quoted_content(&pending_literal);
    literal |= !decoded.is_empty();
    result.push_str(&decoded);
    if result.len() > MAX_STATIC_VALUE_BYTES {
        return Err(too_complex(node, "Static argument byte budget exceeded"));
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
                if body.is_some()
                    || validate_heredoc_redirect(child, source).is_err()
                    || all_children(child).iter().any(|part| {
                        part.kind() == "<<-"
                            || part.kind() == "file_descriptor"
                                && node_text(*part, source) != Some("0")
                    })
                {
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
            "PATH=/untrusted; echo hello",
            "env PATH=/untrusted echo hello",
            "export PATH=/untrusted",
            "/usr/bin/env PATH=/untrusted echo hello",
            "echo \"prefix$HOME\"",
            "bash <<'EOF'\necho hello\nEOF",
            "/bin/bash -c 'echo hello'",
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
                vec![vec![], vec!["echo".to_string(), "bar".to_string()]],
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
                vec![
                    vec![],
                    vec!["printf".to_string(), "%s".to_string(), "a b".to_string()],
                ],
            ),
            ("cat <<< hello", vec![vec!["cat".to_string()]]),
            (
                "echo $((1+2))",
                vec![vec!["echo".to_string(), VAR_PLACEHOLDER.to_string()]],
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
            vec![
                vec!["cat".to_string()],
                vec![
                    "gh".to_string(),
                    "pr".to_string(),
                    "create".to_string(),
                    "--body".to_string(),
                    "## Summary\ntext".to_string(),
                ],
            ]
        );
        assert_eq!(
            argvs("rm \"$(cat <<'EOF'\n/etc/passwd\nEOF\n)\""),
            vec![
                vec!["cat".to_string()],
                vec!["rm".to_string(), "/etc/passwd".to_string()],
            ]
        );
    }

    #[test]
    fn double_quoted_newlines_preserve_every_literal_byte() {
        for (quoted, expected) in [
            ("\"safe\n\"", "safe\n"),
            ("\"\n\"", "\n"),
            ("\"\n\n\"", "\n\n"),
            ("\"\nα\nβ\n\"", "\nα\nβ\n"),
            ("\"\n\"'tail'", "\ntail"),
        ] {
            assert_eq!(argvs(&format!("cat {quoted}")), vec![vec!["cat", expected]]);
            assert_eq!(argvs(&format!("{quoted} arg")), vec![vec![expected, "arg"]]);
            assert_eq!(
                argvs(&format!("for dir in {quoted}; do cat \"$dir/file\"; done")),
                vec![vec!["cat".to_string(), format!("{expected}/file")]]
            );
            for operator in ["<", ">", ">>"] {
                let source = format!("cat {operator} {quoted}");
                let ParseForSecurityResult::Simple { commands } = parse_for_security(&source)
                else {
                    panic!("literal redirect should parse: {source:?}");
                };
                assert_eq!(
                    commands[0].redirects[0].target.as_bytes(),
                    expected.as_bytes()
                );
            }
        }
        assert_eq!(
            argvs("P=tail; cat \"\n$P\n\" \"prefix$P\n\""),
            vec![vec![], vec!["cat", "\ntail\n", "prefixtail\n"]]
        );
    }

    #[test]
    fn double_quoted_line_continuations_decode_without_dropping_literal_newlines() {
        for (quoted, expected, asks) in [
            ("\"safe\\\n\"", "safe", true),
            ("\"safe\\\\\n\"", "safe\\\n", false),
            ("\"safe\\\\\\\n\"", "safe\\", false),
        ] {
            let source = format!("cat {quoted}");
            // Keep the existing differential guard for a lone escaped LF;
            // verify the decoder independently as well as the public result.
            if asks {
                assert!(matches!(
                    parse_for_security(&source),
                    ParseForSecurityResult::TooComplex { .. }
                ));
            } else {
                assert_eq!(argvs(&source), vec![vec!["cat", expected]]);
            }
            let tree = super::super::bash_parser::parse(&source).unwrap().unwrap();
            let mut commands = Vec::new();
            collect_commands_scoped(
                tree.root_node(),
                &source,
                &mut commands,
                &mut HashMap::new(),
            )
            .unwrap();
            assert_eq!(commands[0].argv, ["cat", expected]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn static_projection_matches_bash_nul_delimited_argv() {
        for source in [
            "capture \"safe\n\"",
            "capture \"\n\" \"\n\n\" \"α\nβ\n\"",
            "capture \"a\\\\\\\"b\" '\n' \"\\$literal\n\"",
            "\"capture\n\" arg",
        ] {
            let output = std::process::Command::new("bash")
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    &format!("printf '%s\\0' {source}"),
                ])
                .env_remove("BASH_ENV")
                .output()
                .unwrap();
            assert!(output.status.success(), "{source:?}: {:?}", output.stderr);
            let actual = output
                .stdout
                .strip_suffix(&[0])
                .unwrap()
                .split(|byte| *byte == 0)
                .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(argvs(source), vec![actual], "{source:?}");
        }
        for source in [
            "declare action=push; capture \"$action\"",
            "typeset -x action=push; for item in 'a b' \"\n\"; do capture \"$action\" \"$item\"; done",
            "declare -- action=push; declare action+=ed; capture \"$action\"",
            "P=old; declare P=inside Q=\"$P\"; capture \"$P\" \"$Q\"",
            "P=old; typeset P=inside Q=\"$P\"; capture \"$P\" \"$Q\"",
            "P=old; export P=inside Q=\"$P\"; capture \"$P\" \"$Q\"",
        ] {
            let script = format!("capture() {{ printf '%s\\0' capture \"$@\"; }}; {source}");
            let output = std::process::Command::new("bash")
                .args(["--noprofile", "--norc", "-c", &script])
                .env_remove("BASH_ENV")
                .output()
                .unwrap();
            assert!(output.status.success(), "{source:?}: {:?}", output.stderr);
            let projected = argvs(source)
                .into_iter()
                .filter(|argv| argv.first().is_some_and(|name| name == "capture"))
                .flatten()
                .flat_map(|argument| argument.into_bytes().into_iter().chain([0]))
                .collect::<Vec<_>>();
            assert_eq!(projected, output.stdout, "{source:?}");
        }
    }

    #[test]
    fn declarations_require_explicitly_modeled_options() {
        for builtin in ["declare", "typeset"] {
            for option in [
                "-l", "-u", "-p", "-f", "-F", "-i", "-a", "-A", "-n", "-r", "-t", "-g", "-xl",
                "-ux", "-xp", "-pf", "+l", "+x", "--help", "-Z",
            ] {
                for source in [
                    format!(
                        "{builtin} {option} action; for action in PUSH; do git \"$action\" origin main; done"
                    ),
                    format!("P=/outside/file; {builtin} {option} P=inside; cat \"$P\""),
                    format!("P=/outside/file; {builtin} {option} P=inside; cat > \"$P\""),
                ] {
                    assert!(
                        matches!(
                            parse_for_security(&source),
                            ParseForSecurityResult::TooComplex { .. }
                        ),
                        "{source:?}"
                    );
                }
            }
            for option in ["", "-x", "--", "-x --"] {
                assert_eq!(
                    argvs(&format!("{builtin} {option} P=inside; cat \"$P\""))[1],
                    ["cat", "inside"]
                );
            }
        }
        for source in [
            "local P=inside; cat \"$P\"",
            "P=/outside/file; export -p P=inside; cat \"$P\"",
            "P=/outside/file; declare -- -p P=inside; cat \"$P\"",
            "P=/outside/file; declare P=inside -x; cat \"$P\"",
        ] {
            assert!(
                matches!(
                    parse_for_security(source),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "{source:?}"
            );
        }
    }

    #[test]
    fn projection_budget_is_shared_by_all_expanded_subtrees() {
        for source in [
            "for j in x; do # empty\ndone; for i in 1 2 3 4 5 6 7 8; do for j in 1 2 3 4 5 6 7 8; do # empty\ndone; done".to_string(),
            "for i in x; do # empty\ndone".to_string(),
            "while false; do # empty\ndone".to_string(),
            "for i in 1 2 3 4 5 6 7 8; do for j in 1 2 3 4 5 6 7 8; do :; done; done".to_string(),
            format!("for i in {}; do :; done; for j in x; do :; done", std::iter::repeat_n("x", 50).collect::<Vec<_>>().join(" ")),
            format!("echo \"first: $(for i in {}; do :; done)\" \"second: $(for j in {}; do :; done)\"", std::iter::repeat_n("x", 26).collect::<Vec<_>>().join(" "), std::iter::repeat_n("x", 25).collect::<Vec<_>>().join(" ")),
            format!("echo \"prefix: $(for i in {}; do echo \"nested: $(for j in x y; do :; done)\"; done)\"", std::iter::repeat_n("x", 20).collect::<Vec<_>>().join(" ")),
        ] {
            assert!(
                matches!(
                    parse_for_security(&source),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "{source:?}"
            );
        }
        let empty = "for i in x; do # empty\ndone";
        let tree = super::super::bash_parser::parse(empty).unwrap().unwrap();
        let mut visits_left = MAX_PROJECTION_NODE_VISITS;
        let mut iterations_left = MAX_COMMANDS_FOR_SECURITY_CHECK;
        assert!(
            check_projection_work(tree.root_node(), 1, &mut visits_left, &mut iterations_left)
                .is_err()
        );
        assert_eq!(iterations_left, MAX_COMMANDS_FOR_SECURITY_CHECK - 1);
        assert!(visits_left < MAX_PROJECTION_NODE_VISITS);
        let nested_empty =
            "for i in 1 2 3 4 5 6 7 8; do for j in 1 2 3 4 5 6 7 8; do # empty\ndone; done";
        assert!(
            matches!(parse_for_security(nested_empty), ParseForSecurityResult::TooComplex { reason, .. } if reason.contains("iteration budget"))
        );
        let tree = super::super::bash_parser::parse("echo x").unwrap().unwrap();
        assert!(check_projection_work(tree.root_node(), 1, &mut 1, &mut 50).is_err());
        let source = std::iter::repeat_n("echo x", 50)
            .collect::<Vec<_>>()
            .join("; ");
        assert_eq!(argvs(&source).len(), 50);
        assert!(matches!(
            parse_for_security(&format!("{source}; echo x")),
            ParseForSecurityResult::TooComplex { .. }
        ));
        assert!(matches!(
            parse_for_security(&format!("echo \"prefix: $({source})\"")),
            ParseForSecurityResult::TooComplex { .. }
        ));
        let wide = format!("echo {}", "x ".repeat(4500));
        assert!(
            matches!(parse_for_security(&wide), ParseForSecurityResult::TooComplex { reason, .. } if reason.contains("node-visit budget"))
        );
        for assignment in ["P=\"$P$P\";", "P=$P$P;", "P+=\"$P\";"] {
            let source = format!("P=x; {} cat \"$P\"", assignment.repeat(20));
            assert!(
                matches!(
                    parse_for_security(&source),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "{source:?}"
            );
        }
    }

    #[test]
    fn while_and_until_project_every_condition_command() {
        for keyword in ["while", "until"] {
            assert_eq!(
                argvs(&format!(
                    "{keyword} true; echo condition; do echo body; done"
                )),
                vec![
                    vec!["true"],
                    vec!["echo", "condition"],
                    vec!["echo", "body"]
                ]
            );
        }
    }

    #[test]
    fn literal_for_bindings_require_closed_names_and_static_declarations() {
        for name in [
            "HOME",
            "PWD",
            "OLDPWD",
            "GIT_DIR",
            "GIT_CONFIG_GLOBAL",
            "XDG_CONFIG_HOME",
            "LESSOPEN",
            "PAGER",
            "EDITOR",
            "HISTFILE",
            "NODE_OPTIONS",
            "PYTHONPATH",
            "PATH",
            "IFS",
            "PS4",
            "RANDOM",
            "REPLY",
            "path",
            "fpath",
            "status",
            "parameters",
            "arbitrary_lowercase",
            "NYAN_UNKNOWN_ENV",
        ] {
            let source = format!("for {name} in inside; do echo hello; done");
            assert!(
                matches!(
                    parse_for_security(&source),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "{source:?}"
            );
        }
        for declaration in [
            "export file",
            "export file=before",
            "export 'file=before'",
            "export \"file\"",
            "export f\\ile",
            "declare file",
            "typeset -x file",
            "arg=file; export \"$arg\"",
            "arg=file; declare -x \"$arg\"",
            "arg=file; typeset -x \"$arg\"",
            "false && export file",
        ] {
            let source = format!("{declaration}; for file in inside; do cat \"$file\"; done");
            assert!(
                matches!(
                    parse_for_security(&source),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "{source:?}"
            );
        }
        for value in [
            "'~'", "'=cat'", "'<1-9>'", "'(a|b)'", "'*.txt'", "'a?b'", "'[ab]'", "'a{b,c}'",
        ] {
            let source = format!("for file in {value}; do cat $file; done");
            assert!(
                matches!(
                    parse_for_security(&source),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "{source:?}"
            );
        }
    }

    #[test]
    fn declaration_name_scan_uses_only_static_literal_operands() {
        for builtin in ["export", "declare", "typeset"] {
            for (operand, may_declare) in [
                ("file", true),
                ("file=before", true),
                ("'file=before'", true),
                ("\"file\"", true),
                ("f\"il\"e", true),
                ("f\\\n\"ile\"", true),
                ("\"f\\\nile\"", true),
                ("other", false),
                ("other=before", false),
                ("'other=before'", false),
                ("\"other\"", false),
                ("o\"th\"er", true),
                ("\"o\\\nther\"", false),
                ("\"$arg\"", true),
                ("\"${arg}\"", true),
                ("\"$((1+2))\"", true),
                ("f\\ile", true),
                ("*", true),
                ("~", true),
                ("=echo", true),
                ("\"$(for i in x; do echo hello; done)\"", true),
            ] {
                let source = format!("{builtin} {operand}");
                let super::super::parser::ParseCommandRawResult::Parsed(tree) =
                    super::super::parser::parse_command_raw(&source)
                else {
                    panic!("expected declaration AST for {source:?}");
                };
                assert!(!tree.root_node().has_error(), "{source:?}");
                assert_eq!(
                    may_declare_loop_variable(tree.root_node(), &source, "file"),
                    may_declare,
                    "{source:?}: {}",
                    tree.root_node().to_sexp()
                );
            }
        }
    }

    #[test]
    fn declaration_substitution_loops_return_bounded_safety_errors() {
        let started = std::time::Instant::now();
        for builtin in ["export", "declare", "typeset"] {
            for operand in [
                "\"$(for i in x; do echo hello; done)\"",
                "\"other=$(for i in x; do echo hello; done)\"",
                "other\"$(for i in x; do echo hello; done)\"",
                "\"`for i in x; do echo hello; done`\"",
                "\"$(echo \"$(for i in x; do echo hello; done)\")\"",
                "\"$(for i in x; do for j in y; do echo hello; done; done)\"",
                "\"$(for i in x; do export other; echo hello; done)\"",
                "\"$(for i in x; do declare other; echo hello; done)\"",
                "\"$(for i in x; do typeset other; echo hello; done)\"",
            ] {
                let source = format!("{builtin} {operand}");
                assert!(
                    matches!(
                        parse_for_security(&source),
                        ParseForSecurityResult::TooComplex { .. }
                    ),
                    "{source:?}"
                );
            }
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn literal_for_bindings_reject_process_and_session_exports() {
        use crate::utils::session_env_vars::{
            delete_session_env_var, get_session_env_vars, set_session_env_var,
        };
        use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

        struct RestoreSessionVariable(Option<String>);
        impl Drop for RestoreSessionVariable {
            fn drop(&mut self) {
                if let Some(value) = self.0.take() {
                    set_session_env_var("file", value);
                } else {
                    delete_session_env_var("file");
                }
            }
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = RestoreSessionVariable(get_session_env_vars().get("file").cloned());
        delete_session_env_var("file");
        let _absent = EnvVarGuard::unset("file");
        let source = "for file in inside; do cat \"$file\"; done";
        assert!(matches!(
            parse_for_security(source),
            ParseForSecurityResult::Simple { .. }
        ));
        for value in ["", "before"] {
            let _inherited = EnvVarGuard::set("file", value);
            assert!(matches!(
                parse_for_security(source),
                ParseForSecurityResult::TooComplex { .. }
            ));
        }
        set_session_env_var("file", "");
        assert!(matches!(
            parse_for_security(source),
            ParseForSecurityResult::TooComplex { .. }
        ));
    }

    #[test]
    fn select_never_uses_literal_for_projection() {
        for source in [
            "select p in Cargo.toml; do cat \"$p\"; done",
            "echo 9 | select p in src; do cat \"$p/etc/hostname\"; done",
            "REPLY=status; echo push | select x in a; do git \"$REPLY\" origin main; break; done",
            "select i; do echo hello; done",
            "for i in x; do select p in Cargo.toml; do cat \"$p\"; done; done",
            "echo \"$(select p in a; do echo \"$p\"; done)\"",
        ] {
            assert!(
                matches!(
                    parse_for_security(source),
                    ParseForSecurityResult::TooComplex { .. }
                ),
                "{source:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejected_bindings_and_select_match_real_bash_runtime_behavior() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let source = "for HOME in /etc; do printf '%s\\0' ~/passwd; done";
        assert!(matches!(
            parse_for_security(source),
            ParseForSecurityResult::TooComplex { .. }
        ));
        let output = Command::new("bash")
            .args(["--noprofile", "--norc", "-c", source])
            .env_remove("BASH_ENV")
            .env("HOME", "/fixture/home")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"/etc/passwd\0");

        let source = "select p in src; do printf '%s\\0' \"$p/etc/hostname\"; break; done";
        assert!(matches!(
            parse_for_security(source),
            ParseForSecurityResult::TooComplex { .. }
        ));
        let mut child = Command::new("bash")
            .args(["--noprofile", "--norc", "-c", source])
            .env_remove("BASH_ENV")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"9\n").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"/etc/hostname\0");
    }

    #[test]
    fn literal_for_loops_check_every_value_with_a_bounded_projection() {
        assert_eq!(
            argvs("for i in 1 2 3; do echo progress-$i; sleep 1.2; done"),
            vec![
                vec!["echo", "progress-1"],
                vec!["sleep", "1.2"],
                vec!["echo", "progress-2"],
                vec!["sleep", "1.2"],
                vec!["echo", "progress-3"],
                vec!["sleep", "1.2"],
            ]
        );
        assert_eq!(
            argvs("for file in 'a b' \"c d\"; do cat \"$file\"; done"),
            vec![vec!["cat", "a b"], vec!["cat", "c d"]]
        );
        let values = std::iter::repeat_n("x", MAX_COMMANDS_FOR_SECURITY_CHECK)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            argvs(&format!("for i in {values}; do echo $i; done")).len(),
            MAX_COMMANDS_FOR_SECURITY_CHECK
        );
        for command in [
            format!("for i in {values} x; do echo $i; done"),
            format!("for i in {values}; do echo $i; pwd; done"),
        ] {
            assert!(matches!(
                parse_for_security(&command),
                ParseForSecurityResult::TooComplex { .. }
            ));
        }
    }

    #[test]
    fn assignments_in_one_branch_remain_visible_within_that_branch() {
        let ParseForSecurityResult::Simple { commands } =
            parse_for_security("P=inside; if true; then P=/etc/passwd; cat \"$P\"; fi")
        else {
            panic!("literal branch assignments should parse");
        };
        assert_eq!(commands.last().unwrap().argv, ["cat", "/etc/passwd"]);
        assert_eq!(commands[0].env_vars[0].name, "P");
        assert_eq!(commands[0].env_vars[0].value, "inside");
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
            "P=/etc/passwd; false && P=inside; cat \"$P\"",
            "P=inside; if true; then P=/etc/passwd; fi; cat \"$P\"",
            "P=inside; true || P=/etc/passwd; cat \"$P\"",
            "P=inside; while true; do cat \"$P\"; P=/etc/passwd; done",
            "P=inside; read P; cat \"$P\"",
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
