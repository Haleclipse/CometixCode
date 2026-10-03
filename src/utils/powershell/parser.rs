//! Maps to: CC `utils/powershell/parser.ts`.
//!
//! Spawns the detected PowerShell executable to run
//! `[System.Management.Automation.Language.Parser]::ParseInput` and projects
//! the resulting .NET AST into the structures the permission validators
//! consume.
//!
//! Every failure mode — no PowerShell on the machine, spawn error, timeout,
//! non-zero exit, empty stdout, malformed JSON, over-long input — produces
//! `valid: false` with empty `statements`. Consumers treat that as
//! "cannot validate", never as "safe", so a host without PowerShell installed
//! degrades to prompting rather than auto-allowing.

use crate::utils::process_env::JsTruthy;
use serde::{Deserialize, Deserializer};
use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Public types describing the parsed output returned to callers.
// These map to System.Management.Automation.Language AST classes.
// ---------------------------------------------------------------------------

/// Maps to: CC `parser.ts:17-20#PipelineElementType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineElementType {
    CommandAst,
    CommandExpressionAst,
    ParenExpressionAst,
}

impl PipelineElementType {
    pub fn as_str(self) -> &'static str {
        match self {
            PipelineElementType::CommandAst => "CommandAst",
            PipelineElementType::CommandExpressionAst => "CommandExpressionAst",
            PipelineElementType::ParenExpressionAst => "ParenExpressionAst",
        }
    }
}

/// Maps to: CC `parser.ts:27-35#CommandElementType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandElementType {
    ScriptBlock,
    SubExpression,
    ExpandableString,
    MemberInvocation,
    Variable,
    StringConstant,
    Parameter,
    Other,
}

impl CommandElementType {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandElementType::ScriptBlock => "ScriptBlock",
            CommandElementType::SubExpression => "SubExpression",
            CommandElementType::ExpandableString => "ExpandableString",
            CommandElementType::MemberInvocation => "MemberInvocation",
            CommandElementType::Variable => "Variable",
            CommandElementType::StringConstant => "StringConstant",
            CommandElementType::Parameter => "Parameter",
            CommandElementType::Other => "Other",
        }
    }
}

/// Maps to: CC `parser.ts:43-46#CommandElementChild`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandElementChild {
    pub element_type: CommandElementType,
    pub text: String,
}

/// Maps to: CC `parser.ts:52-67#StatementType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementType {
    PipelineAst,
    PipelineChainAst,
    AssignmentStatementAst,
    IfStatementAst,
    ForStatementAst,
    ForEachStatementAst,
    WhileStatementAst,
    DoWhileStatementAst,
    DoUntilStatementAst,
    SwitchStatementAst,
    TryStatementAst,
    TrapStatementAst,
    FunctionDefinitionAst,
    DataStatementAst,
    UnknownStatementAst,
}

/// Maps to: CC `parser.ts:76#ParsedCommandElement.nameType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandNameType {
    Cmdlet,
    Application,
    Unknown,
}

/// Maps to: CC `parser.ts:72-95#ParsedCommandElement`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ParsedCommandElement {
    /// The command/cmdlet name (e.g., "Get-ChildItem", "git")
    pub name: String,
    /// The command name type: cmdlet, application (exe), or unknown
    pub name_type: Option<CommandNameType>,
    /// The AST element type from PowerShell's parser
    pub element_type: Option<PipelineElementType>,
    /// All arguments as strings (includes flags like "-Recurse")
    pub args: Vec<String>,
    /// The full text of this command element
    pub text: String,
    /// AST node types for each element in this command
    pub element_types: Option<Vec<CommandElementType>>,
    /// Child nodes of each argument, aligned with `args`
    pub children: Option<Vec<Option<Vec<CommandElementChild>>>>,
    /// Redirections on this command element
    pub redirections: Option<Vec<ParsedRedirection>>,
}

/// Maps to: CC `parser.ts:102#ParsedRedirection.operator`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectionOperator {
    Output,
    OutputAppend,
    Error,
    ErrorAppend,
    All,
    AllAppend,
    Merge,
}

impl RedirectionOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            RedirectionOperator::Output => ">",
            RedirectionOperator::OutputAppend => ">>",
            RedirectionOperator::Error => "2>",
            RedirectionOperator::ErrorAppend => "2>>",
            RedirectionOperator::All => "*>",
            RedirectionOperator::AllAppend => "*>>",
            RedirectionOperator::Merge => "2>&1",
        }
    }
}

/// Maps to: CC `parser.ts:100-107#ParsedRedirection`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedRedirection {
    pub operator: RedirectionOperator,
    pub target: String,
    pub is_merging: bool,
}

/// Maps to: CC `parser.ts:135-140#ParsedStatement.securityPatterns`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SecurityPatterns {
    pub has_member_invocations: bool,
    pub has_sub_expressions: bool,
    pub has_expandable_strings: bool,
    pub has_script_blocks: bool,
}

/// Maps to: CC `parser.ts:113-141#ParsedStatement`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedStatement {
    pub statement_type: StatementType,
    pub commands: Vec<ParsedCommandElement>,
    pub redirections: Vec<ParsedRedirection>,
    pub text: String,
    pub nested_commands: Option<Vec<ParsedCommandElement>>,
    pub security_patterns: Option<SecurityPatterns>,
}

/// Maps to: CC `parser.ts:146-151#ParsedVariable`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedVariable {
    pub path: String,
    pub is_splatted: bool,
}

/// Maps to: CC `parser.ts:156-159#ParseError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub error_id: String,
}

/// Maps to: CC `parser.ts:164-197#ParsedPowerShellCommand`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedPowerShellCommand {
    /// Whether the command parsed successfully (no syntax errors)
    pub valid: bool,
    pub errors: Vec<ParseError>,
    /// Top-level statements, separated by `;` or newlines
    pub statements: Vec<ParsedStatement>,
    pub variables: Vec<ParsedVariable>,
    /// Whether the token stream contains a stop-parsing (`--%`) token
    pub has_stop_parsing: bool,
    pub original_command: String,
    /// All .NET type literals found anywhere in the AST
    pub type_literals: Vec<String>,
    /// Whether the command contains `using module` / `using assembly`
    pub has_using_statements: bool,
    /// Whether the command contains `#Requires` directives
    pub has_script_requirements: bool,
}

// ---------------------------------------------------------------------------

/// Maps to: CC `parser.ts:207#DEFAULT_PARSE_TIMEOUT_MS`.
const DEFAULT_PARSE_TIMEOUT_MS: u64 = 5_000;

/// Maps to: CC `parser.ts:208-215#getParseTimeoutMs`.
fn get_parse_timeout_ms() -> u64 {
    crate::utils::process_env::var("CLAUDE_CODE_PWSH_PARSE_TIMEOUT_MS")
        .truthy()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|parsed| *parsed > 0)
        .unwrap_or(DEFAULT_PARSE_TIMEOUT_MS)
}

/// Maps to: CC `parser.ts:315-568#PARSE_SCRIPT_BODY`.
///
/// The command is passed via a Base64-encoded `$EncodedCommand` variable to
/// avoid here-string injection. Comments stay out of the script body: every
/// character here consumes the Windows `CreateProcess` argv budget that
/// [`windows_max_command_length`] is derived from.
pub const PARSE_SCRIPT_BODY: &str = r#"
if (-not $EncodedCommand) {
    Write-Output '{"valid":false,"errors":[{"message":"No command provided","errorId":"NoInput"}],"statements":[],"variables":[],"hasStopParsing":false,"originalCommand":""}'
    exit 0
}

$Command = [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String($EncodedCommand))

$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseInput(
    $Command,
    [ref]$tokens,
    [ref]$parseErrors
)

$allVariables = [System.Collections.ArrayList]::new()

function Get-RawCommandElements {
    param([System.Management.Automation.Language.CommandAst]$CmdAst)
    $elems = [System.Collections.ArrayList]::new()
    foreach ($ce in $CmdAst.CommandElements) {
        $ceData = @{ type = $ce.GetType().Name; text = $ce.Extent.Text }
        if ($ce.PSObject.Properties['Value'] -and $null -ne $ce.Value -and $ce.Value -is [string]) {
            $ceData.value = $ce.Value
        }
        if ($ce -is [System.Management.Automation.Language.CommandExpressionAst]) {
            $ceData.expressionType = $ce.Expression.GetType().Name
        }
        $a=$ce.Argument;if($a){$ceData.children=@(@{type=$a.GetType().Name;text=$a.Extent.Text})}
        [void]$elems.Add($ceData)
    }
    return $elems
}

function Get-RawRedirections {
    param($Redirections)
    $result = [System.Collections.ArrayList]::new()
    foreach ($redir in $Redirections) {
        $redirData = @{ type = $redir.GetType().Name }
        if ($redir -is [System.Management.Automation.Language.FileRedirectionAst]) {
            $redirData.append = [bool]$redir.Append
            $redirData.fromStream = $redir.FromStream.ToString()
            $redirData.locationText = $redir.Location.Extent.Text
        }
        [void]$result.Add($redirData)
    }
    return $result
}

function Get-SecurityPatterns($A) {
    $p = @{}
    foreach ($n in $A.FindAll({ param($x)
        $x -is [System.Management.Automation.Language.MemberExpressionAst] -or
        $x -is [System.Management.Automation.Language.SubExpressionAst] -or
        $x -is [System.Management.Automation.Language.ArrayExpressionAst] -or
        $x -is [System.Management.Automation.Language.ExpandableStringExpressionAst] -or
        $x -is [System.Management.Automation.Language.ScriptBlockExpressionAst] -or
        $x -is [System.Management.Automation.Language.ParenExpressionAst]
    }, $true)) { switch ($n.GetType().Name) {
        'InvokeMemberExpressionAst' { $p.hasMemberInvocations = $true }
        'MemberExpressionAst' { $p.hasMemberInvocations = $true }
        'SubExpressionAst' { $p.hasSubExpressions = $true }
        'ArrayExpressionAst' { $p.hasSubExpressions = $true }
        'ParenExpressionAst' { $p.hasSubExpressions = $true }
        'ExpandableStringExpressionAst' { $p.hasExpandableStrings = $true }
        'ScriptBlockExpressionAst' { $p.hasScriptBlocks = $true }
    }}
    if ($p.Count -gt 0) { return $p }
    return $null
}

$varExprs = $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.VariableExpressionAst] }, $true)
foreach ($v in $varExprs) {
    [void]$allVariables.Add(@{
        path = $v.VariablePath.ToString()
        isSplatted = [bool]$v.Splatted
    })
}

$typeLiterals = [System.Collections.ArrayList]::new()
foreach ($t in $ast.FindAll({ param($n)
    $n -is [System.Management.Automation.Language.TypeExpressionAst] -or
    $n -is [System.Management.Automation.Language.TypeConstraintAst]
}, $true)) { [void]$typeLiterals.Add($t.TypeName.FullName) }

$hasStopParsing = $false
$tk = [System.Management.Automation.Language.TokenKind]
foreach ($tok in $tokens) {
    if ($tok.Kind -eq $tk::MinusMinus) { $hasStopParsing = $true; break }
    if ($tok.Kind -eq $tk::Generic -and ($tok.Text -replace '[–—―]','-') -eq '--%') {
        $hasStopParsing = $true; break
    }
}

$statements = [System.Collections.ArrayList]::new()

function Process-BlockStatements {
    param($Block)
    if (-not $Block) { return }

    foreach ($stmt in $Block.Statements) {
        $statement = @{
            type = $stmt.GetType().Name
            text = $stmt.Extent.Text
        }

        if ($stmt -is [System.Management.Automation.Language.PipelineAst]) {
            $elements = [System.Collections.ArrayList]::new()
            foreach ($element in $stmt.PipelineElements) {
                $elemData = @{
                    type = $element.GetType().Name
                    text = $element.Extent.Text
                }

                if ($element -is [System.Management.Automation.Language.CommandAst]) {
                    $elemData.commandElements = @(Get-RawCommandElements -CmdAst $element)
                    $elemData.redirections = @(Get-RawRedirections -Redirections $element.Redirections)
                } elseif ($element -is [System.Management.Automation.Language.CommandExpressionAst]) {
                    $elemData.expressionType = $element.Expression.GetType().Name
                    $elemData.redirections = @(Get-RawRedirections -Redirections $element.Redirections)
                }

                [void]$elements.Add($elemData)
            }
            $statement.elements = @($elements)

            $allNestedCmds = $stmt.FindAll(
                { param($node) $node -is [System.Management.Automation.Language.CommandAst] },
                $true
            )
            $nestedCmds = [System.Collections.ArrayList]::new()
            foreach ($cmd in $allNestedCmds) {
                if ($cmd.Parent -eq $stmt) { continue }
                $nested = @{
                    type = $cmd.GetType().Name
                    text = $cmd.Extent.Text
                    commandElements = @(Get-RawCommandElements -CmdAst $cmd)
                    redirections = @(Get-RawRedirections -Redirections $cmd.Redirections)
                }
                [void]$nestedCmds.Add($nested)
            }
            if ($nestedCmds.Count -gt 0) {
                $statement.nestedCommands = @($nestedCmds)
            }
            $r = $stmt.FindAll({param($n) $n -is [System.Management.Automation.Language.FileRedirectionAst]}, $true)
            if ($r.Count -gt 0) {
                $rr = @(Get-RawRedirections -Redirections $r)
                $statement.redirections = if ($statement.redirections) { @($statement.redirections) + $rr } else { $rr }
            }
        } else {
            $nestedCmdAsts = $stmt.FindAll(
                { param($node) $node -is [System.Management.Automation.Language.CommandAst] },
                $true
            )
            $nested = [System.Collections.ArrayList]::new()
            foreach ($cmd in $nestedCmdAsts) {
                [void]$nested.Add(@{
                    type = 'CommandAst'
                    text = $cmd.Extent.Text
                    commandElements = @(Get-RawCommandElements -CmdAst $cmd)
                    redirections = @(Get-RawRedirections -Redirections $cmd.Redirections)
                })
            }
            if ($nested.Count -gt 0) {
                $statement.nestedCommands = @($nested)
            }
            $r = $stmt.FindAll({param($n) $n -is [System.Management.Automation.Language.FileRedirectionAst]}, $true)
            if ($r.Count -gt 0) { $statement.redirections = @(Get-RawRedirections -Redirections $r) }
        }

        $sp = Get-SecurityPatterns $stmt
        if ($sp) { $statement.securityPatterns = $sp }

        [void]$statements.Add($statement)
    }

    if ($Block.Traps) {
        foreach ($trap in $Block.Traps) {
            $statement = @{
                type = 'TrapStatementAst'
                text = $trap.Extent.Text
            }
            $nestedCmdAsts = $trap.FindAll(
                { param($node) $node -is [System.Management.Automation.Language.CommandAst] },
                $true
            )
            $nestedCmds = [System.Collections.ArrayList]::new()
            foreach ($cmd in $nestedCmdAsts) {
                $nested = @{
                    type = $cmd.GetType().Name
                    text = $cmd.Extent.Text
                    commandElements = @(Get-RawCommandElements -CmdAst $cmd)
                    redirections = @(Get-RawRedirections -Redirections $cmd.Redirections)
                }
                [void]$nestedCmds.Add($nested)
            }
            if ($nestedCmds.Count -gt 0) {
                $statement.nestedCommands = @($nestedCmds)
            }
            $r = $trap.FindAll({param($n) $n -is [System.Management.Automation.Language.FileRedirectionAst]}, $true)
            if ($r.Count -gt 0) { $statement.redirections = @(Get-RawRedirections -Redirections $r) }
            $sp = Get-SecurityPatterns $trap
            if ($sp) { $statement.securityPatterns = $sp }
            [void]$statements.Add($statement)
        }
    }
}

Process-BlockStatements -Block $ast.BeginBlock
Process-BlockStatements -Block $ast.ProcessBlock
Process-BlockStatements -Block $ast.EndBlock
Process-BlockStatements -Block $ast.CleanBlock
Process-BlockStatements -Block $ast.DynamicParamBlock

if ($ast.ParamBlock) {
  $pb = $ast.ParamBlock
  $pn = [System.Collections.ArrayList]::new()
  foreach ($c in $pb.FindAll({param($n) $n -is [System.Management.Automation.Language.CommandAst]}, $true)) {
    [void]$pn.Add(@{type='CommandAst';text=$c.Extent.Text;commandElements=@(Get-RawCommandElements -CmdAst $c);redirections=@(Get-RawRedirections -Redirections $c.Redirections)})
  }
  $pr = $pb.FindAll({param($n) $n -is [System.Management.Automation.Language.FileRedirectionAst]}, $true)
  $ps = Get-SecurityPatterns $pb
  if ($pn.Count -gt 0 -or $pr.Count -gt 0 -or $ps) {
    $st = @{type='ParamBlockAst';text=$pb.Extent.Text}
    if ($pn.Count -gt 0) { $st.nestedCommands = @($pn) }
    if ($pr.Count -gt 0) { $st.redirections = @(Get-RawRedirections -Redirections $pr) }
    if ($ps) { $st.securityPatterns = $ps }
    [void]$statements.Add($st)
  }
}

$hasUsingStatements = $ast.UsingStatements -and $ast.UsingStatements.Count -gt 0
$hasScriptRequirements = $ast.ScriptRequirements -ne $null

$output = @{
    valid = ($parseErrors.Count -eq 0)
    errors = @($parseErrors | ForEach-Object {
        @{
            message = $_.Message
            errorId = $_.ErrorId
        }
    })
    statements = @($statements)
    variables = @($allVariables)
    hasStopParsing = $hasStopParsing
    originalCommand = $Command
    typeLiterals = @($typeLiterals)
    hasUsingStatements = [bool]$hasUsingStatements
    hasScriptRequirements = [bool]$hasScriptRequirements
}

$output | ConvertTo-Json -Depth 10 -Compress
"#;

// ---------------------------------------------------------------------------
// Windows CreateProcess has a 32,767 char command-line limit. The encoding
// chain is:
//   command (N UTF-8 bytes) -> Base64 (~4N/3 chars) -> $EncodedCommand = '...'\n
//   -> full script (wrapper + PARSE_SCRIPT_BODY) -> UTF-16LE (2x bytes)
//   -> Base64 (4/3x chars) -> -EncodedCommand argv
//
// SECURITY: the budget is in UTF-8 BYTES, not characters. A BMP character in
// U+0800-U+FFFF is one UTF-16 code unit but three UTF-8 bytes, so a
// character-based gate under-reports by up to 3x and lets the final argv
// overflow on Windows. CreateProcess then fails, the parse reports
// valid:false, and deny rules silently downgrade to ask.
//
// Unix argv limits are 2MB+ (ARG_MAX) with ~128KB per argument, so applying
// the Windows-derived ceiling there would regress commands in the ~1K-4.5K
// range that currently parse. The limit is therefore platform-gated.
// ---------------------------------------------------------------------------
const WINDOWS_ARGV_CAP: usize = 32_767;
const FIXED_ARGV_OVERHEAD: usize = 200;
/// `"$EncodedCommand = ''\n"` wrapper around the user command's base64.
const ENCODED_CMD_WRAPPER: usize = 21;
const SAFETY_MARGIN: usize = 100;
const UNIX_MAX_COMMAND_LENGTH: usize = 4_500;

/// Maps to: CC `parser.ts:627-630#WINDOWS_MAX_COMMAND_LENGTH`.
///
/// Derived from [`PARSE_SCRIPT_BODY`] so it cannot drift as the script grows.
/// Unit: UTF-8 bytes.
pub fn windows_max_command_length() -> usize {
    let script_chars_budget = ((WINDOWS_ARGV_CAP - FIXED_ARGV_OVERHEAD) as f64 * 3.0) / 8.0;
    let cmd_b64_budget =
        script_chars_budget - PARSE_SCRIPT_BODY.chars().count() as f64 - ENCODED_CMD_WRAPPER as f64;
    let raw = ((cmd_b64_budget * 3.0) / 4.0).floor();
    if raw <= SAFETY_MARGIN as f64 {
        return 0;
    }
    raw as usize - SAFETY_MARGIN
}

/// Maps to: CC `parser.ts:638-641#MAX_COMMAND_LENGTH`. Unit: UTF-8 bytes.
pub fn max_command_length() -> usize {
    if cfg!(target_os = "windows") {
        windows_max_command_length()
    } else {
        UNIX_MAX_COMMAND_LENGTH
    }
}

/// Maps to: CC `parser.ts:653-663#makeInvalidResult`.
fn make_invalid_result(command: &str, message: String, error_id: &str) -> ParsedPowerShellCommand {
    ParsedPowerShellCommand {
        valid: false,
        errors: vec![ParseError {
            message,
            error_id: error_id.to_string(),
        }],
        statements: Vec::new(),
        variables: Vec::new(),
        has_stop_parsing: false,
        original_command: command.to_string(),
        type_literals: Vec::new(),
        has_using_statements: false,
        has_script_requirements: false,
    }
}

/// Maps to: CC `parser.ts:669-680#toUtf16LeBase64`.
fn to_utf16le_base64(text: &str) -> String {
    use base64::Engine as _;
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Maps to: CC `parser.ts:687-697#buildParseScript`.
fn build_parse_script(command: &str) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(command.as_bytes());
    format!("$EncodedCommand = '{encoded}'\n{PARSE_SCRIPT_BODY}")
}

// ---------------------------------------------------------------------------
// Raw types describing the PowerShell script's JSON output.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            OneOrMany::Many(items) => items,
            OneOrMany::One(item) => vec![item],
        }
    }
}

/// Maps to: CC `parser.ts:703-708#ensureArray`. PowerShell 5.1's
/// `ConvertTo-Json` may unwrap single-element arrays into plain objects.
fn ensure_array<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<OneOrMany<T>>::deserialize(deserializer)?
        .map(OneOrMany::into_vec)
        .unwrap_or_default())
}

/// `ensure_array` for keys where absence is meaningful (`RawStatement.elements`
/// distinguishes a pipeline with no elements from a non-pipeline statement).
fn optional_ensure_array<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<OneOrMany<T>>::deserialize(deserializer)?.map(OneOrMany::into_vec))
}

/// Maps to: CC `parser.ts:226-232#RawCommandElement`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommandElement {
    #[serde(rename = "type", default)]
    node_type: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    value: Option<serde_json::Value>,
    #[serde(default)]
    expression_type: Option<String>,
    #[serde(default, deserialize_with = "ensure_array")]
    children: Vec<RawChildElement>,
}

#[derive(Deserialize)]
struct RawChildElement {
    #[serde(rename = "type", default)]
    node_type: String,
    #[serde(default)]
    text: String,
}

/// Maps to: CC `parser.ts:234-239#RawRedirection`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRedirection {
    #[serde(rename = "type", default)]
    node_type: String,
    #[serde(default)]
    append: Option<bool>,
    #[serde(default)]
    from_stream: Option<String>,
    #[serde(default)]
    location_text: Option<String>,
}

/// Maps to: CC `parser.ts:241-247#RawPipelineElement`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPipelineElement {
    #[serde(rename = "type", default)]
    node_type: String,
    #[serde(default)]
    text: String,
    #[serde(default, deserialize_with = "ensure_array")]
    command_elements: Vec<RawCommandElement>,
    #[serde(default, deserialize_with = "ensure_array")]
    redirections: Vec<RawRedirection>,
    #[serde(default)]
    expression_type: Option<String>,
}

/// Maps to: CC `parser.ts:255-261#RawStatement.securityPatterns`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSecurityPatterns {
    #[serde(default)]
    has_member_invocations: Option<bool>,
    #[serde(default)]
    has_sub_expressions: Option<bool>,
    #[serde(default)]
    has_expandable_strings: Option<bool>,
    #[serde(default)]
    has_script_blocks: Option<bool>,
}

/// Maps to: CC `parser.ts:249-262#RawStatement`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStatement {
    #[serde(rename = "type", default)]
    node_type: String,
    #[serde(default)]
    text: String,
    #[serde(default, deserialize_with = "optional_ensure_array")]
    elements: Option<Vec<RawPipelineElement>>,
    #[serde(default, deserialize_with = "ensure_array")]
    nested_commands: Vec<RawPipelineElement>,
    #[serde(default, deserialize_with = "ensure_array")]
    redirections: Vec<RawRedirection>,
    #[serde(default)]
    security_patterns: Option<RawSecurityPatterns>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawParseError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    error_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVariable {
    #[serde(default)]
    path: String,
    #[serde(default)]
    is_splatted: bool,
}

/// Maps to: CC `parser.ts:264-274#RawParsedOutput`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawParsedOutput {
    #[serde(default)]
    valid: bool,
    #[serde(default, deserialize_with = "ensure_array")]
    errors: Vec<RawParseError>,
    #[serde(default, deserialize_with = "ensure_array")]
    statements: Vec<RawStatement>,
    #[serde(default, deserialize_with = "ensure_array")]
    variables: Vec<RawVariable>,
    #[serde(default)]
    has_stop_parsing: bool,
    #[serde(default)]
    original_command: String,
    #[serde(default, deserialize_with = "ensure_array")]
    type_literals: Vec<String>,
    #[serde(default)]
    has_using_statements: bool,
    #[serde(default)]
    has_script_requirements: bool,
}

// ---------------------------------------------------------------------------
// Raw -> public projection.
// ---------------------------------------------------------------------------

/// Maps to: CC `parser.ts:712-745#mapStatementType`.
pub fn map_statement_type(raw_type: &str) -> StatementType {
    match raw_type {
        "PipelineAst" => StatementType::PipelineAst,
        "PipelineChainAst" => StatementType::PipelineChainAst,
        "AssignmentStatementAst" => StatementType::AssignmentStatementAst,
        "IfStatementAst" => StatementType::IfStatementAst,
        "ForStatementAst" => StatementType::ForStatementAst,
        "ForEachStatementAst" => StatementType::ForEachStatementAst,
        "WhileStatementAst" => StatementType::WhileStatementAst,
        "DoWhileStatementAst" => StatementType::DoWhileStatementAst,
        "DoUntilStatementAst" => StatementType::DoUntilStatementAst,
        "SwitchStatementAst" => StatementType::SwitchStatementAst,
        "TryStatementAst" => StatementType::TryStatementAst,
        "TrapStatementAst" => StatementType::TrapStatementAst,
        "FunctionDefinitionAst" => StatementType::FunctionDefinitionAst,
        "DataStatementAst" => StatementType::DataStatementAst,
        _ => StatementType::UnknownStatementAst,
    }
}

/// Maps to: CC `parser.ts:749-796#mapElementType`.
///
/// `ArrayExpressionAst` (`@()`) is a sibling of `SubExpressionAst`, not a
/// subclass, and both evaluate arbitrary pipelines with side effects, so both
/// map to `SubExpression`.
pub fn map_element_type(raw_type: &str, expression_type: Option<&str>) -> CommandElementType {
    match raw_type {
        "ScriptBlockExpressionAst" => CommandElementType::ScriptBlock,
        "SubExpressionAst" | "ArrayExpressionAst" => CommandElementType::SubExpression,
        "ExpandableStringExpressionAst" => CommandElementType::ExpandableString,
        "InvokeMemberExpressionAst" | "MemberExpressionAst" => CommandElementType::MemberInvocation,
        "VariableExpressionAst" => CommandElementType::Variable,
        "StringConstantExpressionAst" | "ConstantExpressionAst" => {
            CommandElementType::StringConstant
        }
        "CommandParameterAst" => CommandElementType::Parameter,
        "ParenExpressionAst" => CommandElementType::SubExpression,
        "CommandExpressionAst" => match expression_type {
            Some(expression_type) => map_element_type(expression_type, None),
            None => CommandElementType::Other,
        },
        _ => CommandElementType::Other,
    }
}

/// Maps to: CC `parser.ts:800-810#classifyCommandName`.
pub fn classify_command_name(name: &str) -> CommandNameType {
    if is_verb_noun_name(name) {
        return CommandNameType::Cmdlet;
    }
    if name.contains(['.', '\\', '/']) {
        return CommandNameType::Application;
    }
    CommandNameType::Unknown
}

/// `/^[A-Za-z]+-[A-Za-z][A-Za-z0-9_]*$/`
fn is_verb_noun_name(name: &str) -> bool {
    let Some((verb, noun)) = name.split_once('-') else {
        return false;
    };
    if verb.is_empty() || !verb.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let mut noun_chars = noun.chars();
    let Some(first) = noun_chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic() && noun_chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Maps to: CC `parser.ts:814-826#stripModulePrefix`.
pub fn strip_module_prefix(name: &str) -> String {
    let Some(index) = name.rfind('\\') else {
        return name.to_string();
    };
    let has_drive_letter = {
        let mut chars = name.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
            && matches!(chars.next(), Some(':'))
    };
    if has_drive_letter
        || name.starts_with("\\\\")
        || name.starts_with(".\\")
        || name.starts_with("..\\")
    {
        return name.to_string();
    }
    name[index + 1..].to_string()
}

/// `/^['"]|['"]$/g` — strips at most one leading and one trailing quote.
fn strip_edge_quotes(value: &str) -> String {
    let trimmed = value.strip_prefix(['\'', '"']).unwrap_or(value);
    trimmed
        .strip_suffix(['\'', '"'])
        .unwrap_or(trimmed)
        .to_string()
}

fn is_string_literal_type(node_type: &str) -> bool {
    node_type == "StringConstantExpressionAst" || node_type == "ExpandableStringExpressionAst"
}

/// Maps to: CC `parser.ts:830-935#transformCommandAst`.
fn transform_command_ast(raw: &RawPipelineElement) -> ParsedCommandElement {
    let mut name = String::new();
    let mut args: Vec<String> = Vec::new();
    let mut element_types: Vec<CommandElementType> = Vec::new();
    let mut children: Vec<Option<Vec<CommandElementChild>>> = Vec::new();
    let mut has_children = false;

    // SECURITY: nameType is computed from the raw name, BEFORE
    // `strip_module_prefix`. `scripts\Get-Process` classifies as 'application'
    // (it contains a separator) which is the correct answer, since PowerShell
    // resolves it as a file path. After stripping it would classify as
    // 'cmdlet' and allowlist checks would trust it.
    let mut name_type = CommandNameType::Unknown;
    if let Some(first) = raw.command_elements.first() {
        // SECURITY: only trust `.value` for string-literal element types with a
        // string-typed value. A numeric `ConstantExpressionAst` (e.g. `& 1`)
        // emits an integer, so fall back to `.text` for anything else.
        let raw_name_unstripped = if is_string_literal_type(&first.node_type) {
            first
                .value
                .as_ref()
                .and_then(|value| value.as_str())
                .unwrap_or(&first.text)
        } else {
            &first.text
        };
        // SECURITY: strip surrounding quotes at the source so every downstream
        // reader of `name` (deny-rule matching, git-safety cmdlet lookup,
        // `resolve_to_canonical`) sees the bare cmdlet name.
        let raw_name = strip_edge_quotes(raw_name_unstripped);
        // SECURITY: PowerShell built-in cmdlet names are ASCII-only. .NET
        // OrdinalIgnoreCase folds U+017F and U+0131 onto ASCII letters while
        // Rust's `to_lowercase` does not, so a non-ASCII name is forced to
        // 'application' to gate every auto-allow path.
        name_type = if raw_name.chars().any(|c| (c as u32) >= 0x80) {
            CommandNameType::Application
        } else {
            classify_command_name(&raw_name)
        };
        name = strip_module_prefix(&raw_name);
        element_types.push(map_element_type(
            &first.node_type,
            first.expression_type.as_deref(),
        ));

        for element in raw.command_elements.iter().skip(1) {
            // Use the resolved `.value` for string constants (strips quotes,
            // resolves backtick escapes) but keep raw `.text` for parameters,
            // where `.value` loses the dash prefix.
            let value = if is_string_literal_type(&element.node_type) {
                element
                    .value
                    .as_ref()
                    .and_then(|value| value.as_str())
                    .unwrap_or(&element.text)
            } else {
                &element.text
            };
            args.push(value.to_string());
            element_types.push(map_element_type(
                &element.node_type,
                element.expression_type.as_deref(),
            ));
            if element.children.is_empty() {
                children.push(None);
            } else {
                has_children = true;
                children.push(Some(
                    element
                        .children
                        .iter()
                        .map(|child| CommandElementChild {
                            element_type: map_element_type(&child.node_type, None),
                            text: child.text.clone(),
                        })
                        .collect(),
                ));
            }
        }
    }

    ParsedCommandElement {
        name,
        name_type: Some(name_type),
        element_type: Some(PipelineElementType::CommandAst),
        args,
        text: raw.text.clone(),
        element_types: Some(element_types),
        children: has_children.then_some(children),
        redirections: (!raw.redirections.is_empty())
            .then(|| raw.redirections.iter().map(transform_redirection).collect()),
    }
}

/// Maps to: CC `parser.ts:939-958#transformExpressionElement`.
fn transform_expression_element(raw: &RawPipelineElement) -> ParsedCommandElement {
    let element_type = if raw.node_type == "ParenExpressionAst" {
        PipelineElementType::ParenExpressionAst
    } else {
        PipelineElementType::CommandExpressionAst
    };
    ParsedCommandElement {
        name: raw.text.clone(),
        name_type: Some(CommandNameType::Unknown),
        element_type: Some(element_type),
        args: Vec::new(),
        text: raw.text.clone(),
        element_types: Some(vec![map_element_type(
            &raw.node_type,
            raw.expression_type.as_deref(),
        )]),
        children: None,
        redirections: None,
    }
}

/// Maps to: CC `parser.ts:962-998#transformRedirection`.
fn transform_redirection(raw: &RawRedirection) -> ParsedRedirection {
    if raw.node_type == "MergingRedirectionAst" {
        return ParsedRedirection {
            operator: RedirectionOperator::Merge,
            target: String::new(),
            is_merging: true,
        };
    }

    let append = raw.append.unwrap_or(false);
    let from_stream = raw.from_stream.as_deref().unwrap_or("Output");
    let operator = match (append, from_stream) {
        (true, "Error") => RedirectionOperator::ErrorAppend,
        (true, "All") => RedirectionOperator::AllAppend,
        (true, _) => RedirectionOperator::OutputAppend,
        (false, "Error") => RedirectionOperator::Error,
        (false, "All") => RedirectionOperator::All,
        (false, _) => RedirectionOperator::Output,
    };

    ParsedRedirection {
        operator,
        target: raw.location_text.clone().unwrap_or_default(),
        is_merging: false,
    }
}

/// Maps to: CC `parser.ts:1002-1103#transformStatement`.
fn transform_statement(raw: &RawStatement) -> ParsedStatement {
    let statement_type = map_statement_type(&raw.node_type);
    let mut commands: Vec<ParsedCommandElement> = Vec::new();
    let mut redirections: Vec<ParsedRedirection> = Vec::new();

    if let Some(elements) = raw.elements.as_ref() {
        for element in elements {
            if element.node_type == "CommandAst" {
                commands.push(transform_command_ast(element));
            } else {
                commands.push(transform_expression_element(element));
            }
            // SECURITY: `CommandExpressionAst` also carries `.Redirections`
            // (inherited from `CommandBaseAst`). `1 > /tmp/evil.txt` is a
            // CommandExpressionAst with a FileRedirectionAst, so both branches
            // must harvest redirections or `get_file_redirections` misses it.
            for redirection in &element.redirections {
                redirections.push(transform_redirection(redirection));
            }
        }
        // The PipelineAst branch of the script also does a deep FindAll for
        // FileRedirectionAst to catch redirections hidden inside colon-bound
        // paren arguments and hashtable value statements. That re-discovers the
        // direct-element redirections captured above, so dedupe by
        // (operator, target).
        let mut seen: Vec<(RedirectionOperator, String)> = redirections
            .iter()
            .map(|redirection| (redirection.operator, redirection.target.clone()))
            .collect();
        for redirection in &raw.redirections {
            let transformed = transform_redirection(redirection);
            let key = (transformed.operator, transformed.target.clone());
            if !seen.contains(&key) {
                seen.push(key);
                redirections.push(transformed);
            }
        }
    } else {
        // Non-pipeline statement: add a synthetic command entry with full text.
        commands.push(ParsedCommandElement {
            name: raw.text.clone(),
            name_type: Some(CommandNameType::Unknown),
            element_type: Some(PipelineElementType::CommandExpressionAst),
            args: Vec::new(),
            text: raw.text.clone(),
            element_types: None,
            children: None,
            redirections: None,
        });
        // SECURITY: the script's else-branch does a direct recursive FindAll on
        // FileRedirectionAst to catch expression redirections inside control
        // flow. In `if ($x) { 1 > /tmp/evil }` the literal `1` with its attached
        // redirection is a CommandExpressionAst — a sibling of CommandAst, not a
        // subclass — so `nested_commands` never contains it.
        for redirection in &raw.redirections {
            redirections.push(transform_redirection(redirection));
        }
    }

    ParsedStatement {
        statement_type,
        commands,
        redirections,
        text: raw.text.clone(),
        nested_commands: (!raw.nested_commands.is_empty()).then(|| {
            raw.nested_commands
                .iter()
                .map(transform_command_ast)
                .collect()
        }),
        security_patterns: raw
            .security_patterns
            .as_ref()
            .map(|patterns| SecurityPatterns {
                has_member_invocations: patterns.has_member_invocations.unwrap_or(false),
                has_sub_expressions: patterns.has_sub_expressions.unwrap_or(false),
                has_expandable_strings: patterns.has_expandable_strings.unwrap_or(false),
                has_script_blocks: patterns.has_script_blocks.unwrap_or(false),
            }),
    }
}

/// Maps to: CC `parser.ts:1106-1126#transformRawOutput`.
fn transform_raw_output(raw: RawParsedOutput) -> ParsedPowerShellCommand {
    ParsedPowerShellCommand {
        valid: raw.valid,
        errors: raw
            .errors
            .into_iter()
            .map(|error| ParseError {
                message: error.message,
                error_id: error.error_id,
            })
            .collect(),
        statements: raw.statements.iter().map(transform_statement).collect(),
        variables: raw
            .variables
            .into_iter()
            .map(|variable| ParsedVariable {
                path: variable.path,
                is_splatted: variable.is_splatted,
            })
            .collect(),
        has_stop_parsing: raw.has_stop_parsing,
        original_command: raw.original_command,
        type_literals: raw.type_literals,
        has_using_statements: raw.has_using_statements,
        has_script_requirements: raw.has_script_requirements,
    }
}

// ---------------------------------------------------------------------------
// Spawn.
// ---------------------------------------------------------------------------

struct SpawnOutcome {
    stdout: String,
    stderr: String,
    code: i32,
    timed_out: bool,
}

fn spawn_pwsh(program: &str, args: &[&str], timeout: Duration) -> Result<SpawnOutcome, String> {
    fn drain<R: std::io::Read>(mut reader: R) -> Vec<u8> {
        let mut captured = Vec::new();
        let _ = reader.read_to_end(&mut captured);
        captured
    }

    let mut command = std::process::Command::new(program);
    // CC `parser.ts` execa(pwshPath, ...) inherits process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let mut child = command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("failed to capture pwsh output streams".to_string());
    };
    let (stdout_tx, stdout_rx) = std::sync::mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = stdout_tx.send(drain(stdout));
    });
    std::thread::spawn(move || {
        let _ = stderr_tx.send(drain(stderr));
    });

    let start = std::time::Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() > timeout => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    };

    Ok(SpawnOutcome {
        stdout: String::from_utf8_lossy(&stdout_rx.recv().unwrap_or_default()).to_string(),
        stderr: String::from_utf8_lossy(&stderr_rx.recv().unwrap_or_default()).to_string(),
        code: status.and_then(|status| status.code()).unwrap_or(1),
        timed_out,
    })
}

/// Maps to: CC `parser.ts:1136-1261#parsePowerShellCommandImpl`.
fn parse_powershell_command_impl(command: &str) -> ParsedPowerShellCommand {
    // SECURITY: the ceiling is a UTF-8 BYTE budget. Measuring characters
    // under-reports multibyte input by up to 3x and lets the Windows argv
    // overflow, which fails the spawn and degrades deny rules to ask.
    let command_bytes = command.len();
    let limit = max_command_length();
    if command_bytes > limit {
        crate::utils::debug::log_for_debugging(&format!(
            "PowerShell parser: command too long ({command_bytes} bytes, max {limit})"
        ));
        return make_invalid_result(
            command,
            format!(
                "Command too long for parsing ({command_bytes} bytes). Maximum supported length is {limit} bytes."
            ),
            "CommandTooLong",
        );
    }

    let Some(pwsh_path) = crate::utils::shell::powershell_detection::get_cached_powershell_path()
    else {
        return make_invalid_result(
            command,
            "PowerShell is not available".to_string(),
            "NoPowerShell",
        );
    };

    // `-EncodedCommand` takes a Base64-encoded UTF-16LE string, which avoids
    // stdin interactive-mode prompts, command-line escaping, and temp files.
    let encoded_script = to_utf16le_base64(&build_parse_script(command));
    let args = [
        "-NoProfile",
        "-NonInteractive",
        "-NoLogo",
        "-EncodedCommand",
        encoded_script.as_str(),
    ];

    // One retry on timeout: on loaded runners the spawn plus JIT plus
    // ParseInput occasionally exceeds the budget. A double timeout is reported
    // as PwshTimeout.
    let parse_timeout = Duration::from_millis(get_parse_timeout_ms());
    let mut outcome = None;
    for attempt in 0..2 {
        match spawn_pwsh(&pwsh_path, &args, parse_timeout) {
            Ok(result) => {
                let timed_out = result.timed_out;
                outcome = Some(result);
                if !timed_out {
                    break;
                }
                crate::utils::debug::log_for_debugging(&format!(
                    "PowerShell parser: pwsh timed out after {}ms (attempt {})",
                    parse_timeout.as_millis(),
                    attempt + 1
                ));
            }
            Err(error) => {
                crate::utils::debug::log_for_debugging(&format!(
                    "PowerShell parser: failed to spawn pwsh: {error}"
                ));
                return make_invalid_result(
                    command,
                    format!("Failed to spawn PowerShell: {error}"),
                    "PwshSpawnError",
                );
            }
        }
    }

    let outcome = outcome.expect("spawn loop records an outcome or returns early");
    if outcome.timed_out {
        return make_invalid_result(
            command,
            format!(
                "pwsh timed out after {}ms (2 attempts)",
                parse_timeout.as_millis()
            ),
            "PwshTimeout",
        );
    }

    if outcome.code != 0 {
        crate::utils::debug::log_for_debugging(&format!(
            "PowerShell parser: pwsh exited with code {}, stderr: {}",
            outcome.code, outcome.stderr
        ));
        return make_invalid_result(
            command,
            format!("pwsh exited with code {}: {}", outcome.code, outcome.stderr),
            "PwshError",
        );
    }

    let trimmed = outcome.stdout.trim();
    if trimmed.is_empty() {
        crate::utils::debug::log_for_debugging("PowerShell parser: empty stdout from pwsh");
        return make_invalid_result(
            command,
            "No output from PowerShell parser".to_string(),
            "EmptyOutput",
        );
    }

    match serde_json::from_str::<RawParsedOutput>(trimmed) {
        Ok(raw) => transform_raw_output(raw),
        Err(_) => {
            crate::utils::debug::log_for_debugging(&format!(
                "PowerShell parser: invalid JSON output: {}",
                trimmed.chars().take(200).collect::<String>()
            ));
            make_invalid_result(
                command,
                "Invalid JSON from PowerShell parser".to_string(),
                "InvalidJson",
            )
        }
    }
}

/// Maps to: CC `parser.ts:1267-1273#TRANSIENT_ERROR_IDS`.
///
/// Transient process failures are not cached so a later call can retry.
/// Deterministic failures (`CommandTooLong`, syntax errors from a successful
/// parse, `NoPowerShell`) stay cached since retrying reproduces them.
const TRANSIENT_ERROR_IDS: [&str; 5] = [
    "PwshSpawnError",
    "PwshError",
    "PwshTimeout",
    "EmptyOutput",
    "InvalidJson",
];

const PARSE_CACHE_CAPACITY: usize = 256;

struct ParseCache {
    entries: HashMap<String, ParsedPowerShellCommand>,
    order: VecDeque<String>,
}

static PARSE_CACHE: LazyLock<Mutex<ParseCache>> = LazyLock::new(|| {
    Mutex::new(ParseCache {
        entries: HashMap::new(),
        order: VecDeque::new(),
    })
});

/// Maps to: CC `parser.ts:1275-1294#parsePowerShellCommand`.
///
/// Results are memoized by command string with the source's 256-entry bound.
pub fn parse_powershell_command(command: &str) -> ParsedPowerShellCommand {
    {
        let mut cache = PARSE_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.entries.get(command).cloned() {
            if let Some(index) = cache.order.iter().position(|key| key == command) {
                cache.order.remove(index);
            }
            cache.order.push_back(command.to_string());
            return cached;
        }
    }

    let result = parse_powershell_command_impl(command);
    let is_transient = !result.valid
        && result
            .errors
            .first()
            .is_some_and(|error| TRANSIENT_ERROR_IDS.contains(&error.error_id.as_str()));
    if !is_transient {
        let mut cache = PARSE_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache
            .entries
            .insert(command.to_string(), result.clone())
            .is_none()
        {
            cache.order.push_back(command.to_string());
        }
        while cache.order.len() > PARSE_CACHE_CAPACITY {
            if let Some(evicted) = cache.order.pop_front() {
                cache.entries.remove(&evicted);
            }
        }
    }
    result
}

/// Clears the memoized parse results. Only for testing.
pub fn reset_parse_cache() {
    let mut cache = PARSE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.entries.clear();
    cache.order.clear();
}

// ---------------------------------------------------------------------------
// Analysis helpers — derived from the parsed AST structure.
// ---------------------------------------------------------------------------

/// Maps to: CC `parser.ts:1303-1318#SecurityFlags`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SecurityFlags {
    pub has_sub_expressions: bool,
    pub has_script_blocks: bool,
    pub has_splatting: bool,
    pub has_expandable_strings: bool,
    pub has_member_invocations: bool,
    pub has_assignments: bool,
    pub has_stop_parsing: bool,
}

/// Maps to: CC `utils/powershell/parser.ts:1326-1452` `COMMON_ALIASES`.
///
/// Rust's `HashMap` has no JavaScript prototype chain, preserving the source
/// table's null-prototype lookup behavior for attacker-controlled names.
pub static COMMON_ALIASES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("ls", "Get-ChildItem"),
        ("dir", "Get-ChildItem"),
        ("gci", "Get-ChildItem"),
        ("cat", "Get-Content"),
        ("type", "Get-Content"),
        ("gc", "Get-Content"),
        ("cd", "Set-Location"),
        ("sl", "Set-Location"),
        ("chdir", "Set-Location"),
        ("pushd", "Push-Location"),
        ("popd", "Pop-Location"),
        ("pwd", "Get-Location"),
        ("gl", "Get-Location"),
        ("gi", "Get-Item"),
        ("gp", "Get-ItemProperty"),
        ("ni", "New-Item"),
        ("mkdir", "New-Item"),
        ("md", "New-Item"),
        ("ri", "Remove-Item"),
        ("del", "Remove-Item"),
        ("rd", "Remove-Item"),
        ("rmdir", "Remove-Item"),
        ("rm", "Remove-Item"),
        ("erase", "Remove-Item"),
        ("mi", "Move-Item"),
        ("mv", "Move-Item"),
        ("move", "Move-Item"),
        ("ci", "Copy-Item"),
        ("cp", "Copy-Item"),
        ("copy", "Copy-Item"),
        ("cpi", "Copy-Item"),
        ("si", "Set-Item"),
        ("rni", "Rename-Item"),
        ("ren", "Rename-Item"),
        ("ps", "Get-Process"),
        ("gps", "Get-Process"),
        ("kill", "Stop-Process"),
        ("spps", "Stop-Process"),
        ("start", "Start-Process"),
        ("saps", "Start-Process"),
        ("sajb", "Start-Job"),
        ("ipmo", "Import-Module"),
        ("echo", "Write-Output"),
        ("write", "Write-Output"),
        ("sleep", "Start-Sleep"),
        ("help", "Get-Help"),
        ("man", "Get-Help"),
        ("gcm", "Get-Command"),
        ("gsv", "Get-Service"),
        ("gv", "Get-Variable"),
        ("sv", "Set-Variable"),
        ("h", "Get-History"),
        ("history", "Get-History"),
        ("iex", "Invoke-Expression"),
        ("iwr", "Invoke-WebRequest"),
        ("irm", "Invoke-RestMethod"),
        ("icm", "Invoke-Command"),
        ("ii", "Invoke-Item"),
        ("nsn", "New-PSSession"),
        ("etsn", "Enter-PSSession"),
        ("exsn", "Exit-PSSession"),
        ("gsn", "Get-PSSession"),
        ("rsn", "Remove-PSSession"),
        ("cls", "Clear-Host"),
        ("clear", "Clear-Host"),
        ("select", "Select-Object"),
        ("where", "Where-Object"),
        ("foreach", "ForEach-Object"),
        ("%", "ForEach-Object"),
        ("?", "Where-Object"),
        ("measure", "Measure-Object"),
        ("ft", "Format-Table"),
        ("fl", "Format-List"),
        ("fw", "Format-Wide"),
        ("oh", "Out-Host"),
        ("ogv", "Out-GridView"),
        ("ac", "Add-Content"),
        ("clc", "Clear-Content"),
        ("tee", "Tee-Object"),
        ("epcsv", "Export-Csv"),
        ("sp", "Set-ItemProperty"),
        ("rp", "Remove-ItemProperty"),
        ("cli", "Clear-Item"),
        ("epal", "Export-Alias"),
        ("sls", "Select-String"),
    ])
});

const DIRECTORY_CHANGE_CMDLETS: [&str; 3] = ["set-location", "push-location", "pop-location"];
const DIRECTORY_CHANGE_ALIASES: [&str; 5] = ["cd", "sl", "chdir", "pushd", "popd"];

/// Maps to: CC `parser.ts:1467-1480#getAllCommandNames`.
pub fn get_all_command_names(parsed: &ParsedPowerShellCommand) -> Vec<String> {
    get_all_commands(parsed)
        .into_iter()
        .map(|command| command.name.to_lowercase())
        .collect()
}

/// Maps to: CC `parser.ts:1486-1501#getAllCommands`.
pub fn get_all_commands(parsed: &ParsedPowerShellCommand) -> Vec<&ParsedCommandElement> {
    let mut commands = Vec::new();
    for statement in &parsed.statements {
        commands.extend(statement.commands.iter());
        if let Some(nested) = statement.nested_commands.as_ref() {
            commands.extend(nested.iter());
        }
    }
    commands
}

/// Maps to: CC `parser.ts:1507-1527#getAllRedirections`.
pub fn get_all_redirections(parsed: &ParsedPowerShellCommand) -> Vec<&ParsedRedirection> {
    let mut redirections = Vec::new();
    for statement in &parsed.statements {
        redirections.extend(statement.redirections.iter());
        if let Some(nested) = statement.nested_commands.as_ref() {
            for command in nested {
                if let Some(command_redirections) = command.redirections.as_ref() {
                    redirections.extend(command_redirections.iter());
                }
            }
        }
    }
    redirections
}

/// Maps to: CC `parser.ts:1533-1539#getVariablesByScope`.
pub fn get_variables_by_scope<'a>(
    parsed: &'a ParsedPowerShellCommand,
    scope: &str,
) -> Vec<&'a ParsedVariable> {
    let prefix = format!("{}:", scope.to_lowercase());
    parsed
        .variables
        .iter()
        .filter(|variable| variable.path.to_lowercase().starts_with(&prefix))
        .collect()
}

/// Maps to: CC `parser.ts:1545-1571#hasCommandNamed`.
pub fn has_command_named(parsed: &ParsedPowerShellCommand, name: &str) -> bool {
    let lower_name = name.to_lowercase();
    let canonical_from_alias = COMMON_ALIASES
        .get(lower_name.as_str())
        .map(|alias| alias.to_lowercase());

    for command_name in get_all_command_names(parsed) {
        if command_name == lower_name {
            return true;
        }
        let canonical = COMMON_ALIASES
            .get(command_name.as_str())
            .map(|alias| alias.to_lowercase());
        if canonical.as_deref() == Some(lower_name.as_str()) {
            return true;
        }
        if let Some(canonical_from_alias) = canonical_from_alias.as_deref() {
            if command_name == canonical_from_alias {
                return true;
            }
            if canonical.as_deref() == Some(canonical_from_alias) {
                return true;
            }
        }
    }
    false
}

/// Maps to: CC `parser.ts:1578-1588#hasDirectoryChange`.
pub fn has_directory_change(parsed: &ParsedPowerShellCommand) -> bool {
    get_all_command_names(parsed).into_iter().any(|name| {
        DIRECTORY_CHANGE_CMDLETS.contains(&name.as_str())
            || DIRECTORY_CHANGE_ALIASES.contains(&name.as_str())
    })
}

/// Maps to: CC `parser.ts:1594-1602#isSingleCommand`.
pub fn is_single_command(parsed: &ParsedPowerShellCommand) -> bool {
    parsed.statements.len() == 1
        && parsed.statements.first().is_some_and(|statement| {
            statement.commands.len() == 1
                && statement
                    .nested_commands
                    .as_ref()
                    .is_none_or(|nested| nested.is_empty())
        })
}

/// Maps to: CC `parser.ts:1608-1614#commandHasArg`.
pub fn command_has_arg(command: &ParsedCommandElement, arg: &str) -> bool {
    let lower_arg = arg.to_lowercase();
    command
        .args
        .iter()
        .any(|candidate| candidate.to_lowercase() == lower_arg)
}

/// Maps to: CC `parser.ts:1627-1632#PS_TOKENIZER_DASH_CHARS`.
///
/// `SpecialCharacters.IsDash` accepts exactly these four: ASCII hyphen-minus,
/// en-dash, em-dash, and horizontal bar. These are tokenizer-level and apply
/// to every cmdlet parameter.
pub const PS_TOKENIZER_DASH_CHARS: [char; 4] = ['-', '\u{2013}', '\u{2014}', '\u{2015}'];

/// Maps to: CC `parser.ts:1647-1655#isPowerShellParameter`.
///
/// When the AST element type is available it is authoritative — the parser
/// maps `CommandParameterAst` to `Parameter` regardless of which dash
/// character was typed, and a quoted `"-Path"` is a StringConstant.
pub fn is_powershell_parameter(arg: &str, element_type: Option<CommandElementType>) -> bool {
    if let Some(element_type) = element_type {
        return element_type == CommandElementType::Parameter;
    }
    arg.chars()
        .next()
        .is_some_and(|first| PS_TOKENIZER_DASH_CHARS.contains(&first))
}

/// Maps to: CC `parser.ts:1663-1684#commandHasArgAbbreviation`.
pub fn command_has_arg_abbreviation(
    command: &ParsedCommandElement,
    full_param: &str,
    min_prefix: &str,
) -> bool {
    let lower_full = full_param.to_lowercase();
    let lower_min = min_prefix.to_lowercase();
    command.args.iter().any(|arg| {
        let param_part = match arg
            .char_indices()
            .skip(1)
            .find_map(|(index, character)| (character == ':').then_some(index))
        {
            Some(colon_index) => &arg[..colon_index],
            None => arg.as_str(),
        };
        let lower = param_part.replace('`', "").to_lowercase();
        lower.starts_with(&lower_min)
            && lower_full.starts_with(&lower)
            && lower.len() <= lower_full.len()
    })
}

/// Maps to: CC `parser.ts:1690-1694#getPipelineSegments`.
pub fn get_pipeline_segments(parsed: &ParsedPowerShellCommand) -> &[ParsedStatement] {
    &parsed.statements
}

/// Maps to: CC `parser.ts:1703-1706#isNullRedirectionTarget`.
///
/// `> $null` discards output like `/dev/null` and is not a filesystem write.
pub fn is_null_redirection_target(target: &str) -> bool {
    let target = target.trim().to_lowercase();
    target == "$null" || target == "${null}"
}

/// Maps to: CC `parser.ts:1713-1719#getFileRedirections`.
pub fn get_file_redirections(parsed: &ParsedPowerShellCommand) -> Vec<&ParsedRedirection> {
    get_all_redirections(parsed)
        .into_iter()
        .filter(|redirection| {
            !redirection.is_merging && !is_null_redirection_target(&redirection.target)
        })
        .collect()
}

/// Maps to: CC `parser.ts:1728-1802#deriveSecurityFlags`.
pub fn derive_security_flags(parsed: &ParsedPowerShellCommand) -> SecurityFlags {
    let mut flags = SecurityFlags {
        has_stop_parsing: parsed.has_stop_parsing,
        ..SecurityFlags::default()
    };

    fn check_elements(command: &ParsedCommandElement, flags: &mut SecurityFlags) {
        let Some(element_types) = command.element_types.as_ref() else {
            return;
        };
        for element_type in element_types {
            match element_type {
                CommandElementType::ScriptBlock => flags.has_script_blocks = true,
                CommandElementType::SubExpression => flags.has_sub_expressions = true,
                CommandElementType::ExpandableString => flags.has_expandable_strings = true,
                CommandElementType::MemberInvocation => flags.has_member_invocations = true,
                _ => {}
            }
        }
    }

    for statement in &parsed.statements {
        if statement.statement_type == StatementType::AssignmentStatementAst {
            flags.has_assignments = true;
        }
        for command in &statement.commands {
            check_elements(command, &mut flags);
        }
        if let Some(nested) = statement.nested_commands.as_ref() {
            for command in nested {
                check_elements(command, &mut flags);
            }
        }
        // securityPatterns is a belt-and-suspenders check that catches patterns
        // elementTypes may miss (member invocations inside assignments,
        // subexpressions in non-pipeline statements).
        if let Some(patterns) = statement.security_patterns.as_ref() {
            flags.has_member_invocations |= patterns.has_member_invocations;
            flags.has_sub_expressions |= patterns.has_sub_expressions;
            flags.has_expandable_strings |= patterns.has_expandable_strings;
            flags.has_script_blocks |= patterns.has_script_blocks;
        }
    }

    if parsed.variables.iter().any(|variable| variable.is_splatted) {
        flags.has_splatting = true;
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_raw(json: &str) -> ParsedPowerShellCommand {
        transform_raw_output(serde_json::from_str::<RawParsedOutput>(json).expect("valid raw json"))
    }

    #[test]
    fn common_aliases_match_official_single_hop_entries_and_omissions() {
        assert_eq!(COMMON_ALIASES.get("gc"), Some(&"Get-Content"));
        assert_eq!(COMMON_ALIASES.get("sls"), Some(&"Select-String"));
        assert_eq!(COMMON_ALIASES.get("where"), Some(&"Where-Object"));
        assert_eq!(COMMON_ALIASES.get("md"), Some(&"New-Item"));
        assert!(!COMMON_ALIASES.contains_key("sort"));
        assert!(!COMMON_ALIASES.contains_key("curl"));
        assert!(!COMMON_ALIASES.contains_key("__proto__"));
    }

    #[test]
    fn missing_powershell_degrades_to_an_invalid_parse_rather_than_a_valid_one() {
        let result = make_invalid_result(
            "Get-ChildItem",
            "PowerShell is not available".to_string(),
            "NoPowerShell",
        );
        assert!(!result.valid);
        assert!(result.statements.is_empty());
        assert!(result.variables.is_empty());
        assert!(!result.has_stop_parsing);
        assert_eq!(result.errors[0].error_id, "NoPowerShell");
        // Every AST consumer refuses to auto-allow on `!valid`.
        assert!(get_pipeline_segments(&result).is_empty());
        assert_eq!(derive_security_flags(&result), SecurityFlags::default());
    }

    #[test]
    fn parse_of_any_command_is_invalid_when_no_powershell_is_installed() {
        reset_parse_cache();
        let result = parse_powershell_command("Get-ChildItem ./src");
        if crate::utils::shell::powershell_detection::get_cached_powershell_path().is_none() {
            assert!(!result.valid);
            assert_eq!(result.errors[0].error_id, "NoPowerShell");
            assert!(result.statements.is_empty());
        } else {
            assert_eq!(result.original_command, "Get-ChildItem ./src");
        }
    }

    #[test]
    fn command_length_budget_is_derived_from_the_script_body() {
        assert_eq!(UNIX_MAX_COMMAND_LENGTH, 4_500);
        let windows_limit = windows_max_command_length();
        assert!(
            windows_limit > 0 && windows_limit < UNIX_MAX_COMMAND_LENGTH,
            "windows limit={windows_limit}"
        );
        if !cfg!(target_os = "windows") {
            assert_eq!(max_command_length(), UNIX_MAX_COMMAND_LENGTH);
        }
    }

    #[test]
    fn over_long_commands_fail_closed_before_any_spawn() {
        reset_parse_cache();
        let command = "a".repeat(max_command_length() + 1);
        let result = parse_powershell_command(&command);
        assert!(!result.valid);
        assert_eq!(result.errors[0].error_id, "CommandTooLong");
    }

    #[test]
    fn parse_script_body_keeps_the_unicode_dash_normalization_class() {
        assert!(PARSE_SCRIPT_BODY.contains("'[\u{2013}\u{2014}\u{2015}]','-'"));
        assert!(PARSE_SCRIPT_BODY.starts_with('\n'));
        assert!(PARSE_SCRIPT_BODY.ends_with("$output | ConvertTo-Json -Depth 10 -Compress\n"));
    }

    #[test]
    fn utf16le_base64_matches_the_encoded_command_contract() {
        assert_eq!(to_utf16le_base64("hi"), "aABpAA==");
        assert!(build_parse_script("Get-Date").starts_with("$EncodedCommand = 'R2V0LURhdGU='\n"));
    }

    #[test]
    fn statement_and_element_types_map_like_the_official_tables() {
        assert_eq!(
            map_statement_type("PipelineAst"),
            StatementType::PipelineAst
        );
        assert_eq!(
            map_statement_type("ParamBlockAst"),
            StatementType::UnknownStatementAst
        );
        assert_eq!(
            map_element_type("ArrayExpressionAst", None),
            CommandElementType::SubExpression
        );
        assert_eq!(
            map_element_type("ParenExpressionAst", None),
            CommandElementType::SubExpression
        );
        assert_eq!(
            map_element_type("ConstantExpressionAst", None),
            CommandElementType::StringConstant
        );
        assert_eq!(
            map_element_type("CommandExpressionAst", Some("SubExpressionAst")),
            CommandElementType::SubExpression
        );
        assert_eq!(
            map_element_type("CommandExpressionAst", None),
            CommandElementType::Other
        );
    }

    #[test]
    fn command_name_classification_and_module_prefix_follow_the_source_rules() {
        assert_eq!(
            classify_command_name("Get-ChildItem"),
            CommandNameType::Cmdlet
        );
        assert_eq!(
            classify_command_name("node.exe"),
            CommandNameType::Application
        );
        assert_eq!(classify_command_name("git"), CommandNameType::Unknown);
        assert_eq!(
            strip_module_prefix("Microsoft.PowerShell.Utility\\Invoke-Expression"),
            "Invoke-Expression"
        );
        assert_eq!(strip_module_prefix("C:\\tools\\x.exe"), "C:\\tools\\x.exe");
        assert_eq!(strip_module_prefix(".\\run.ps1"), ".\\run.ps1");
        assert_eq!(strip_module_prefix("\\\\host\\share"), "\\\\host\\share");
    }

    #[test]
    fn module_qualified_names_keep_the_pre_strip_name_type() {
        let element = transform_command_ast(&serde_json::from_str::<RawPipelineElement>(
            r#"{"type":"CommandAst","text":"scripts\\Get-Process","commandElements":[
                {"type":"StringConstantExpressionAst","text":"scripts\\Get-Process","value":"scripts\\Get-Process"}]}"#,
        )
        .expect("valid element"));
        assert_eq!(element.name, "Get-Process");
        assert_eq!(element.name_type, Some(CommandNameType::Application));
    }

    #[test]
    fn non_ascii_cmdlet_names_are_forced_to_application() {
        let element = transform_command_ast(
            &serde_json::from_str::<RawPipelineElement>(
                r#"{"type":"CommandAst","text":"\u017ftart-proce\u017f\u017f","commandElements":[
                {"type":"StringConstantExpressionAst","text":"x","value":"\u017ftart-proce\u017f\u017f"}]}"#,
            )
            .expect("valid element"),
        );
        assert_eq!(element.name_type, Some(CommandNameType::Application));
    }

    #[test]
    fn quoted_command_names_are_stripped_at_the_source() {
        let element = transform_command_ast(
            &serde_json::from_str::<RawPipelineElement>(
                r#"{"type":"CommandAst","text":"& 'Invoke-Expression' 'x'","commandElements":[
                {"type":"BareWordAst","text":"'Invoke-Expression'"}]}"#,
            )
            .expect("valid element"),
        );
        assert_eq!(element.name, "Invoke-Expression");
    }

    #[test]
    fn single_element_arrays_unwrapped_by_powershell_5_are_rehydrated() {
        let parsed = parse_raw(
            r#"{"valid":true,"errors":[],"statements":{"type":"PipelineAst","text":"Get-Date",
              "elements":{"type":"CommandAst","text":"Get-Date","commandElements":{
                "type":"StringConstantExpressionAst","text":"Get-Date","value":"Get-Date"}}},
              "variables":[],"hasStopParsing":false,"originalCommand":"Get-Date"}"#,
        );
        assert_eq!(parsed.statements.len(), 1);
        assert_eq!(parsed.statements[0].commands.len(), 1);
        assert_eq!(parsed.statements[0].commands[0].name, "Get-Date");
    }

    #[test]
    fn non_pipeline_statements_get_a_synthetic_expression_command() {
        let parsed = parse_raw(
            r#"{"valid":true,"errors":[],"statements":[{"type":"IfStatementAst","text":"if ($t) { Get-Date }",
              "nestedCommands":[{"type":"CommandAst","text":"Get-Date","commandElements":[
                {"type":"StringConstantExpressionAst","text":"Get-Date","value":"Get-Date"}]}]}],
              "variables":[],"hasStopParsing":false,"originalCommand":"x"}"#,
        );
        let statement = &parsed.statements[0];
        assert_eq!(statement.statement_type, StatementType::IfStatementAst);
        assert_eq!(
            statement.commands[0].element_type,
            Some(PipelineElementType::CommandExpressionAst)
        );
        assert_eq!(
            statement.nested_commands.as_ref().unwrap()[0].name,
            "Get-Date"
        );
    }

    #[test]
    fn statement_redirections_are_deduped_against_element_redirections() {
        let parsed = parse_raw(
            r#"{"valid":true,"errors":[],"statements":[{"type":"PipelineAst","text":"x > /tmp/a",
              "elements":[{"type":"CommandAst","text":"x","commandElements":[
                {"type":"StringConstantExpressionAst","text":"x","value":"x"}],
                "redirections":[{"type":"FileRedirectionAst","append":false,"fromStream":"Output","locationText":"/tmp/a"}]}],
              "redirections":[{"type":"FileRedirectionAst","append":false,"fromStream":"Output","locationText":"/tmp/a"},
                              {"type":"FileRedirectionAst","append":true,"fromStream":"Error","locationText":"/tmp/b"}]}],
              "variables":[],"hasStopParsing":false,"originalCommand":"x"}"#,
        );
        let redirections = &parsed.statements[0].redirections;
        assert_eq!(redirections.len(), 2);
        assert_eq!(redirections[0].operator, RedirectionOperator::Output);
        assert_eq!(redirections[1].operator, RedirectionOperator::ErrorAppend);
        assert_eq!(get_file_redirections(&parsed).len(), 2);
    }

    #[test]
    fn null_redirection_targets_are_not_filesystem_writes() {
        assert!(is_null_redirection_target(" $NULL "));
        assert!(is_null_redirection_target("${null}"));
        assert!(!is_null_redirection_target("${ null }"));
        assert!(!is_null_redirection_target("/tmp/x"));
    }

    #[test]
    fn colon_bound_parameter_children_are_mapped_through_element_types() {
        let element = transform_command_ast(
            &serde_json::from_str::<RawPipelineElement>(
                r#"{"type":"CommandAst","text":"Write-Output -InputObject:$env:SECRET","commandElements":[
                {"type":"StringConstantExpressionAst","text":"Write-Output","value":"Write-Output"},
                {"type":"CommandParameterAst","text":"-InputObject:$env:SECRET",
                 "children":[{"type":"VariableExpressionAst","text":"$env:SECRET"}]}]}"#,
            )
            .expect("valid element"),
        );
        let children = element.children.expect("children populated");
        assert_eq!(
            children[0].as_ref().unwrap()[0].element_type,
            CommandElementType::Variable
        );
        assert_eq!(
            element.element_types.unwrap()[1],
            CommandElementType::Parameter
        );
    }

    #[test]
    fn security_flags_merge_element_types_variables_and_security_patterns() {
        let parsed = parse_raw(
            r#"{"valid":true,"errors":[],"statements":[{"type":"AssignmentStatementAst","text":"$x = $y.Invoke()",
              "securityPatterns":{"hasMemberInvocations":true}}],
              "variables":[{"path":"splat","isSplatted":true}],"hasStopParsing":true,"originalCommand":"x"}"#,
        );
        let flags = derive_security_flags(&parsed);
        assert!(flags.has_assignments);
        assert!(flags.has_member_invocations);
        assert!(flags.has_splatting);
        assert!(flags.has_stop_parsing);
        assert!(!flags.has_script_blocks);
    }

    #[test]
    fn parameter_detection_prefers_the_ast_element_type() {
        assert!(is_powershell_parameter(
            "\u{2013}Path",
            Some(CommandElementType::Parameter)
        ));
        assert!(!is_powershell_parameter(
            "-Path",
            Some(CommandElementType::StringConstant)
        ));
        assert!(is_powershell_parameter("\u{2015}Path", None));
        assert!(!is_powershell_parameter("Path", None));
    }

    #[test]
    fn arg_abbreviation_matching_strips_colon_values_and_backticks() {
        let command = ParsedCommandElement {
            args: vec!["-en:b64".to_string(), "-Member`Name".to_string()],
            ..ParsedCommandElement::default()
        };
        assert!(command_has_arg_abbreviation(
            &command,
            "-encodedcommand",
            "-en"
        ));
        assert!(command_has_arg_abbreviation(&command, "-membername", "-me"));
        assert!(!command_has_arg_abbreviation(&command, "-recurse", "-re"));
    }

    #[test]
    fn alias_aware_command_lookup_and_directory_change_detection() {
        let parsed = parse_raw(
            r#"{"valid":true,"errors":[],"statements":[{"type":"PipelineAst","text":"rm x",
              "elements":[{"type":"CommandAst","text":"rm x","commandElements":[
                {"type":"StringConstantExpressionAst","text":"rm","value":"rm"},
                {"type":"StringConstantExpressionAst","text":"x","value":"x"}]}]},
              {"type":"PipelineAst","text":"cd /tmp","elements":[{"type":"CommandAst","text":"cd /tmp",
                "commandElements":[{"type":"StringConstantExpressionAst","text":"cd","value":"cd"}]}]}],
              "variables":[],"hasStopParsing":false,"originalCommand":"x"}"#,
        );
        assert!(has_command_named(&parsed, "Remove-Item"));
        assert!(has_command_named(&parsed, "del"));
        assert!(!has_command_named(&parsed, "Get-Date"));
        assert!(has_directory_change(&parsed));
        assert!(!is_single_command(&parsed));
        assert_eq!(get_all_commands(&parsed).len(), 2);
    }

    #[test]
    fn transient_failures_are_not_memoized_but_deterministic_ones_are() {
        assert!(TRANSIENT_ERROR_IDS.contains(&"PwshTimeout"));
        assert!(TRANSIENT_ERROR_IDS.contains(&"InvalidJson"));
        assert!(!TRANSIENT_ERROR_IDS.contains(&"CommandTooLong"));
        assert!(!TRANSIENT_ERROR_IDS.contains(&"NoPowerShell"));
    }
}
