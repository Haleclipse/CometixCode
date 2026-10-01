//! Maps to: CC `tools/PowerShellTool/readOnlyValidation.ts`.
//!
//! Cmdlets are case-insensitive; all matching is done in lowercase.

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::utils::powershell::parser::{
    CommandElementType, CommandNameType, ParsedCommandElement, ParsedPowerShellCommand,
    ParsedStatement, PipelineElementType, StatementType, derive_security_flags,
    get_pipeline_segments, is_null_redirection_target, is_powershell_parameter,
};
use crate::utils::shell::read_only_command_validation::{
    EXTERNAL_READONLY_COMMANDS, ValidateFlagsOptions, additional_command_is_dangerous,
    command_config, validate_flags,
};

use super::common_parameters::is_common_parameter;

const WINDOWS_PATHEXT: [&str; 4] = [".exe", ".cmd", ".bat", ".com"];

/// Maps to: CC `readOnlyValidation.ts:32-37#DOTNET_READ_ONLY_FLAGS`.
const DOTNET_READ_ONLY_FLAGS: [&str; 4] = ["--version", "--info", "--list-runtimes", "--list-sdks"];

/// Extra validation for an allowlisted cmdlet. Maps to the
/// `additionalCommandIsDangerousCallback` field of CC's `CommandConfig`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdditionalCheck {
    /// Maps to: CC `readOnlyValidation.ts:76#argLeaksValue`.
    ArgLeaksValue,
    /// Maps to: CC `readOnlyValidation.ts:705-712` — reject any positional.
    IpconfigRejectsPositionals,
    /// Maps to: CC `readOnlyValidation.ts:749-755` — reject any positional.
    HostnameRejectsPositionals,
    /// Maps to: CC `readOnlyValidation.ts:777-791` — verb must be `print`.
    RouteVerbMustBePrint,
}

/// Maps to: CC `readOnlyValidation.ts:39-56#CommandConfig`.
///
/// CC also allows a `regex` constraint on the original command; no entry in
/// `CMDLET_ALLOWLIST` uses it, so it is not modelled here.
struct CmdletConfig {
    /// Safe subcommands or flags for this command
    safe_flags: &'static [&'static str],
    /// When true, all flags are allowed regardless of `safe_flags`. Without it,
    /// an empty `safe_flags` rejects all flags (positional args only).
    allow_all_flags: bool,
    additional_check: Option<AdditionalCheck>,
}

impl CmdletConfig {
    const fn flags(safe_flags: &'static [&'static str]) -> Self {
        Self {
            safe_flags,
            allow_all_flags: false,
            additional_check: None,
        }
    }

    const fn all_flags() -> Self {
        Self {
            safe_flags: &[],
            allow_all_flags: true,
            additional_check: None,
        }
    }

    const fn external() -> Self {
        Self {
            safe_flags: &[],
            allow_all_flags: false,
            additional_check: None,
        }
    }

    const fn with_check(mut self, check: AdditionalCheck) -> Self {
        self.additional_check = Some(check);
        self
    }
}

/// Maps to: CC `readOnlyValidation.ts:129-882#CMDLET_ALLOWLIST`.
///
/// Rust's `HashMap` has no JavaScript prototype chain, preserving the source
/// table's null-prototype lookup behavior for attacker-controlled names.
///
/// SECURITY omissions carried over from the source: `select-xml` (XXE via
/// DOCTYPE SYSTEM), `test-json` (`$ref` fetches external URLs), `get-command`
/// and `get-help` (module autoload runs `.psm1` init code), `get-wmiobject`
/// and `get-ciminstance` (Win32_PingStatus sends ICMP), `get-clipboard`
/// (exposes copied secrets), and `netsh` (grammar too complex to allowlist).
static CMDLET_ALLOWLIST: LazyLock<HashMap<&'static str, CmdletConfig>> = LazyLock::new(|| {
    HashMap::from([
        // ── Filesystem (read-only) ───────────────────────────────────────────
        (
            "get-childitem",
            CmdletConfig::flags(&[
                "-Path",
                "-LiteralPath",
                "-Filter",
                "-Include",
                "-Exclude",
                "-Recurse",
                "-Depth",
                "-Name",
                "-Force",
                "-Attributes",
                "-Directory",
                "-File",
                "-Hidden",
                "-ReadOnly",
                "-System",
            ]),
        ),
        (
            "get-content",
            CmdletConfig::flags(&[
                "-Path",
                "-LiteralPath",
                "-TotalCount",
                "-Head",
                "-Tail",
                "-Raw",
                "-Encoding",
                "-Delimiter",
                "-ReadCount",
            ]),
        ),
        (
            "get-item",
            CmdletConfig::flags(&["-Path", "-LiteralPath", "-Force", "-Stream"]),
        ),
        (
            "get-itemproperty",
            CmdletConfig::flags(&["-Path", "-LiteralPath", "-Name"]),
        ),
        (
            "test-path",
            CmdletConfig::flags(&[
                "-Path",
                "-LiteralPath",
                "-PathType",
                "-Filter",
                "-Include",
                "-Exclude",
                "-IsValid",
                "-NewerThan",
                "-OlderThan",
            ]),
        ),
        (
            "resolve-path",
            CmdletConfig::flags(&["-Path", "-LiteralPath", "-Relative"]),
        ),
        (
            "get-filehash",
            CmdletConfig::flags(&["-Path", "-LiteralPath", "-Algorithm", "-InputStream"]),
        ),
        (
            "get-acl",
            CmdletConfig::flags(&[
                "-Path",
                "-LiteralPath",
                "-Audit",
                "-Filter",
                "-Include",
                "-Exclude",
            ]),
        ),
        // ── Navigation (read-only, only changes the working directory) ───────
        (
            "set-location",
            CmdletConfig::flags(&["-Path", "-LiteralPath", "-PassThru", "-StackName"]),
        ),
        (
            "push-location",
            CmdletConfig::flags(&["-Path", "-LiteralPath", "-PassThru", "-StackName"]),
        ),
        (
            "pop-location",
            CmdletConfig::flags(&["-PassThru", "-StackName"]),
        ),
        // ── Text searching/filtering (read-only) ─────────────────────────────
        (
            "select-string",
            CmdletConfig::flags(&[
                "-Path",
                "-LiteralPath",
                "-Pattern",
                "-InputObject",
                "-SimpleMatch",
                "-CaseSensitive",
                "-Quiet",
                "-List",
                "-NotMatch",
                "-AllMatches",
                "-Encoding",
                "-Context",
                "-Raw",
                "-NoEmphasis",
            ]),
        ),
        // ── Data conversion (pure transforms, no side effects) ───────────────
        (
            "convertto-json",
            CmdletConfig::flags(&[
                "-InputObject",
                "-Depth",
                "-Compress",
                "-EnumsAsStrings",
                "-AsArray",
            ]),
        ),
        (
            "convertfrom-json",
            CmdletConfig::flags(&["-InputObject", "-Depth", "-AsHashtable", "-NoEnumerate"]),
        ),
        (
            "convertto-csv",
            CmdletConfig::flags(&[
                "-InputObject",
                "-Delimiter",
                "-NoTypeInformation",
                "-NoHeader",
                "-UseQuotes",
            ]),
        ),
        (
            "convertfrom-csv",
            CmdletConfig::flags(&["-InputObject", "-Delimiter", "-Header", "-UseCulture"]),
        ),
        (
            "convertto-xml",
            CmdletConfig::flags(&["-InputObject", "-Depth", "-As", "-NoTypeInformation"]),
        ),
        (
            "convertto-html",
            CmdletConfig::flags(&[
                "-InputObject",
                "-Property",
                "-Head",
                "-Title",
                "-Body",
                "-Pre",
                "-Post",
                "-As",
                "-Fragment",
            ]),
        ),
        (
            "format-hex",
            CmdletConfig::flags(&[
                "-Path",
                "-LiteralPath",
                "-InputObject",
                "-Encoding",
                "-Count",
                "-Offset",
            ]),
        ),
        // ── Object inspection and manipulation (read-only) ───────────────────
        (
            "get-member",
            CmdletConfig::flags(&[
                "-InputObject",
                "-MemberType",
                "-Name",
                "-Static",
                "-View",
                "-Force",
            ]),
        ),
        (
            "get-unique",
            CmdletConfig::flags(&["-InputObject", "-AsString", "-CaseInsensitive", "-OnType"]),
        ),
        (
            "compare-object",
            CmdletConfig::flags(&[
                "-ReferenceObject",
                "-DifferenceObject",
                "-Property",
                "-SyncWindow",
                "-CaseSensitive",
                "-Culture",
                "-ExcludeDifferent",
                "-IncludeEqual",
                "-PassThru",
            ]),
        ),
        (
            "join-string",
            CmdletConfig::flags(&[
                "-InputObject",
                "-Property",
                "-Separator",
                "-OutputPrefix",
                "-OutputSuffix",
                "-SingleQuote",
                "-DoubleQuote",
                "-FormatString",
            ]),
        ),
        (
            "get-random",
            CmdletConfig::flags(&[
                "-InputObject",
                "-Minimum",
                "-Maximum",
                "-Count",
                "-SetSeed",
                "-Shuffle",
            ]),
        ),
        // ── Path utilities (read-only) ───────────────────────────────────────
        (
            "convert-path",
            CmdletConfig::flags(&["-Path", "-LiteralPath"]),
        ),
        (
            // -Resolve removed: it touches the filesystem to verify the joined
            // path exists, but that path was never validated.
            "join-path",
            CmdletConfig::flags(&["-Path", "-ChildPath", "-AdditionalChildPath"]),
        ),
        (
            "split-path",
            CmdletConfig::flags(&[
                "-Path",
                "-LiteralPath",
                "-Qualifier",
                "-NoQualifier",
                "-Parent",
                "-Leaf",
                "-LeafBase",
                "-Extension",
                "-IsAbsolute",
            ]),
        ),
        // ── Additional system info (read-only) ───────────────────────────────
        ("get-hotfix", CmdletConfig::flags(&["-Id", "-Description"])),
        (
            "get-itempropertyvalue",
            CmdletConfig::flags(&["-Path", "-LiteralPath", "-Name"]),
        ),
        ("get-psprovider", CmdletConfig::flags(&["-PSProvider"])),
        // ── Process/system info ──────────────────────────────────────────────
        (
            "get-process",
            CmdletConfig::flags(&[
                "-Name",
                "-Id",
                "-Module",
                "-FileVersionInfo",
                "-IncludeUserName",
            ]),
        ),
        (
            "get-service",
            CmdletConfig::flags(&[
                "-Name",
                "-DisplayName",
                "-DependentServices",
                "-RequiredServices",
                "-Include",
                "-Exclude",
            ]),
        ),
        ("get-computerinfo", CmdletConfig::all_flags()),
        ("get-host", CmdletConfig::all_flags()),
        (
            "get-date",
            CmdletConfig::flags(&["-Date", "-Format", "-UFormat", "-DisplayHint", "-AsUTC"]),
        ),
        (
            "get-location",
            CmdletConfig::flags(&["-PSProvider", "-PSDrive", "-Stack", "-StackName"]),
        ),
        (
            "get-psdrive",
            CmdletConfig::flags(&["-Name", "-PSProvider", "-Scope"]),
        ),
        (
            "get-module",
            CmdletConfig::flags(&[
                "-Name",
                "-ListAvailable",
                "-All",
                "-FullyQualifiedName",
                "-PSEdition",
            ]),
        ),
        (
            "get-alias",
            CmdletConfig::flags(&["-Name", "-Definition", "-Scope", "-Exclude"]),
        ),
        ("get-history", CmdletConfig::flags(&["-Id", "-Count"])),
        ("get-culture", CmdletConfig::all_flags()),
        ("get-uiculture", CmdletConfig::all_flags()),
        (
            "get-timezone",
            CmdletConfig::flags(&["-Name", "-Id", "-ListAvailable"]),
        ),
        ("get-uptime", CmdletConfig::all_flags()),
        // ── Output and misc (no side effects) ────────────────────────────────
        (
            "write-output",
            CmdletConfig::flags(&["-InputObject", "-NoEnumerate"])
                .with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "write-host",
            CmdletConfig::flags(&[
                "-Object",
                "-NoNewline",
                "-Separator",
                "-ForegroundColor",
                "-BackgroundColor",
            ])
            .with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "start-sleep",
            CmdletConfig::flags(&["-Seconds", "-Milliseconds", "-Duration"])
                .with_check(AdditionalCheck::ArgLeaksValue),
        ),
        // Format-*/Measure-Object/Select-Object/... moved here from
        // SAFE_OUTPUT_CMDLETS after review found they all accept
        // calculated-property hashtables. `allow_all_flags` keeps their
        // display-only flags working while `arg_leaks_value` blocks the
        // dangerous argument *values*.
        (
            "format-table",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "format-list",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "format-wide",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "format-custom",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "measure-object",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "select-object",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "sort-object",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "group-object",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "where-object",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "out-string",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        (
            "out-host",
            CmdletConfig::all_flags().with_check(AdditionalCheck::ArgLeaksValue),
        ),
        // ── Network info (read-only) ─────────────────────────────────────────
        (
            "get-netadapter",
            CmdletConfig::flags(&[
                "-Name",
                "-InterfaceDescription",
                "-InterfaceIndex",
                "-Physical",
            ]),
        ),
        (
            "get-netipaddress",
            CmdletConfig::flags(&[
                "-InterfaceIndex",
                "-InterfaceAlias",
                "-AddressFamily",
                "-Type",
            ]),
        ),
        (
            "get-netipconfiguration",
            CmdletConfig::flags(&["-InterfaceIndex", "-InterfaceAlias", "-Detailed", "-All"]),
        ),
        (
            "get-netroute",
            CmdletConfig::flags(&[
                "-InterfaceIndex",
                "-InterfaceAlias",
                "-AddressFamily",
                "-DestinationPrefix",
            ]),
        ),
        (
            // -CimSession/-ThrottleLimit excluded: -CimSession connects to a
            // remote host.
            "get-dnsclientcache",
            CmdletConfig::flags(&["-Entry", "-Name", "-Type", "-Status", "-Section", "-Data"]),
        ),
        (
            "get-dnsclient",
            CmdletConfig::flags(&["-InterfaceIndex", "-InterfaceAlias"]),
        ),
        // ── Event log (read-only) ────────────────────────────────────────────
        (
            "get-eventlog",
            CmdletConfig::flags(&[
                "-LogName",
                "-Newest",
                "-After",
                "-Before",
                "-EntryType",
                "-Index",
                "-InstanceId",
                "-Message",
                "-Source",
                "-UserName",
                "-AsBaseObject",
                "-List",
            ]),
        ),
        (
            // -FilterXml/-FilterHashtable removed: -FilterXml accepts XML with
            // DOCTYPE external entities (XXE). -FilterXPath is kept.
            "get-winevent",
            CmdletConfig::flags(&[
                "-LogName",
                "-ListLog",
                "-ListProvider",
                "-ProviderName",
                "-Path",
                "-MaxEvents",
                "-FilterXPath",
                "-Force",
                "-Oldest",
            ]),
        ),
        // ── WMI/CIM ──────────────────────────────────────────────────────────
        (
            // get-cimclass stays — it only lists class metadata, with no
            // instance enumeration.
            "get-cimclass",
            CmdletConfig::flags(&[
                "-ClassName",
                "-Namespace",
                "-MethodName",
                "-PropertyName",
                "-QualifierName",
            ]),
        ),
        // ── External commands with shared validation ─────────────────────────
        ("git", CmdletConfig::external()),
        ("gh", CmdletConfig::external()),
        ("docker", CmdletConfig::external()),
        ("dotnet", CmdletConfig::external()),
        // ── Windows-specific system commands ─────────────────────────────────
        (
            // On macOS `ipconfig set <iface> <mode>` writes system config, and
            // safeFlags only validates flags, so every positional is rejected.
            "ipconfig",
            CmdletConfig::flags(&["/all", "/displaydns", "/allcompartments"])
                .with_check(AdditionalCheck::IpconfigRejectsPositionals),
        ),
        (
            "netstat",
            CmdletConfig::flags(&[
                "-a", "-b", "-e", "-f", "-n", "-o", "-p", "-q", "-r", "-s", "-t", "-x", "-y",
            ]),
        ),
        ("systeminfo", CmdletConfig::flags(&["/FO", "/NH"])),
        (
            "tasklist",
            CmdletConfig::flags(&["/M", "/SVC", "/V", "/FI", "/FO", "/NH"]),
        ),
        ("where.exe", CmdletConfig::all_flags()),
        (
            // `hostname NAME` and `hostname -F FILE` SET the hostname on
            // Linux/macOS, so positionals are rejected.
            "hostname",
            CmdletConfig::flags(&["-a", "-d", "-f", "-i", "-I", "-s", "-y", "-A"])
                .with_check(AdditionalCheck::HostnameRejectsPositionals),
        ),
        (
            "whoami",
            CmdletConfig::flags(&[
                "/user", "/groups", "/claims", "/priv", "/logonid", "/all", "/fo", "/nh",
            ]),
        ),
        ("ver", CmdletConfig::all_flags()),
        ("arp", CmdletConfig::flags(&["-a", "-g", "-v", "-N"])),
        (
            "route",
            CmdletConfig::flags(&["print", "PRINT", "-4", "-6"])
                .with_check(AdditionalCheck::RouteVerbMustBePrint),
        ),
        ("getmac", CmdletConfig::flags(&["/FO", "/NH", "/V"])),
        // ── Cross-platform CLI tools ─────────────────────────────────────────
        (
            // `file -C` compiles a magic database and WRITES to disk, so only
            // introspection flags are allowed.
            "file",
            CmdletConfig::flags(&[
                "-b",
                "--brief",
                "-i",
                "--mime",
                "-L",
                "--dereference",
                "--mime-type",
                "--mime-encoding",
                "-z",
                "--uncompress",
                "-p",
                "--preserve-date",
                "-k",
                "--keep-going",
                "-r",
                "--raw",
                "-v",
                "--version",
                "-0",
                "--print0",
                "-s",
                "--special-files",
                "-l",
                "-F",
                "--separator",
                "-e",
                "-P",
                "-N",
                "--no-pad",
                "-E",
                "--extension",
            ]),
        ),
        ("tree", CmdletConfig::flags(&["/F", "/A", "/Q", "/L"])),
        (
            // Flag matching strips ':' before comparison (`/C:pattern` → `/C`),
            // so these entries must NOT include the trailing colon.
            "findstr",
            CmdletConfig::flags(&[
                "/B", "/E", "/L", "/R", "/S", "/I", "/X", "/V", "/N", "/M", "/O", "/P", "/C", "/G",
                "/D", "/A",
            ]),
        ),
    ])
});

/// Maps to: CC `readOnlyValidation.ts:888-917#SAFE_OUTPUT_CMDLETS`.
///
/// `out-string`/`out-host` are deliberately absent — both accept
/// `-InputObject`, which leaks the same way `Write-Output` does. They live in
/// `CMDLET_ALLOWLIST` with the `arg_leaks_value` guard instead.
const SAFE_OUTPUT_CMDLETS: [&str; 1] = ["out-null"];

/// Maps to: CC `readOnlyValidation.ts:931-943#PIPELINE_TAIL_CMDLETS`.
const PIPELINE_TAIL_CMDLETS: [&str; 11] = [
    "format-table",
    "format-list",
    "format-wide",
    "format-custom",
    "measure-object",
    "select-object",
    "sort-object",
    "group-object",
    "where-object",
    "out-string",
    "out-host",
];

/// Maps to: CC `readOnlyValidation.ts:964#SAFE_EXTERNAL_EXES`.
const SAFE_EXTERNAL_EXES: [&str; 1] = ["where.exe"];

/// `/[$(@{[]/` — the expression metacharacters that mark an argument's extent
/// text as carrying a runtime-evaluated value.
fn has_expression_metachar(value: &str) -> bool {
    value.contains(['$', '(', '@', '{', '['])
}

/// Maps to: CC `readOnlyValidation.ts:76-115#argLeaksValue`.
///
/// Shared guard for cmdlets that print or coerce their arguments to
/// stdout/stderr. `Write-Output $env:SECRET` prints it directly; `Start-Sleep
/// $env:SECRET` leaks it through the type-coercion error message.
///
/// Two checks: an elementTypes whitelist of StringConstant plus Parameter, and
/// a colon-bound parameter value check. `-InputObject:$env:SECRET` is a SINGLE
/// `CommandParameterAst` whose VariableExpressionAst is a `.Argument` child
/// rather than a separate element, so the whitelist alone would pass it.
pub fn arg_leaks_value(element: Option<&ParsedCommandElement>) -> bool {
    let Some(element) = element else {
        return false;
    };
    let arg_types: Vec<CommandElementType> = element
        .element_types
        .as_ref()
        .map(|types| types.iter().skip(1).copied().collect())
        .unwrap_or_default();
    let args = &element.args;
    let children = element.children.as_ref();

    for (index, arg_type) in arg_types.iter().enumerate() {
        if *arg_type != CommandElementType::StringConstant
            && *arg_type != CommandElementType::Parameter
        {
            // ArrayLiteralAst (`Select-Object Name, Id`) maps to 'Other' and
            // the parse script only populates children for
            // CommandParameterAst.Argument, so fall back to the extent text: a
            // comma-list of bare identifiers has no metacharacter, while
            // `Name, $x` still rejects on `$`.
            if !has_expression_metachar(args.get(index).map(String::as_str).unwrap_or_default()) {
                continue;
            }
            return true;
        }
        if *arg_type == CommandElementType::Parameter {
            match children
                .and_then(|children| children.get(index))
                .cloned()
                .flatten()
            {
                Some(param_children) => {
                    if param_children
                        .iter()
                        .any(|child| child.element_type != CommandElementType::StringConstant)
                    {
                        return true;
                    }
                }
                None => {
                    // Fallback for parsers that do not populate children.
                    let arg = args.get(index).map(String::as_str).unwrap_or_default();
                    if let Some(colon_index) = arg.find(':') {
                        if colon_index > 0 && has_expression_metachar(&arg[colon_index + 1..]) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Maps to: CC `tools/PowerShellTool/readOnlyValidation.ts:984-996`
/// `resolveToCanonical`.
///
/// Strips Windows executable extensions from path-free names so `git.exe`
/// canonicalises to `git` and triggers the git-safety guards. SECURITY: only
/// strips when the name has no path separator — `scripts\git.exe` runs a local
/// script, not PATH-resolved git, and must NOT canonicalise to `git`.
pub fn resolve_to_canonical(name: &str) -> String {
    let mut lower = name.to_lowercase();
    if !lower.contains('\\') && !lower.contains('/') {
        if let Some(extension) = WINDOWS_PATHEXT
            .into_iter()
            .find(|extension| lower.ends_with(extension))
        {
            lower.truncate(lower.len() - extension.len());
        }
    }
    crate::utils::powershell::parser::COMMON_ALIASES
        .get(lower.as_str())
        .map(|alias| alias.to_lowercase())
        .unwrap_or(lower)
}

/// Maps to: CC `readOnlyValidation.ts:1017-1033#isCwdChangingCmdlet`.
///
/// Covers two classes: cwd-changing cmdlets (Set-Location/Push-Location/
/// Pop-Location and aliases) and PSDrive-creating cmdlets (New-PSDrive), both
/// of which alter path resolution for later statements in the same compound.
pub fn is_cwd_changing_cmdlet(name: &str) -> bool {
    let canonical = resolve_to_canonical(name);
    canonical == "set-location"
        || canonical == "push-location"
        || canonical == "pop-location"
        || canonical == "new-psdrive"
        // ndr/mount alias New-PSDrive on Windows only. On POSIX `mount` is
        // mount(8), so treating it as PSDrive-creating would false-positive.
        || (cfg!(target_os = "windows") && (canonical == "ndr" || canonical == "mount"))
}

/// Maps to: CC `readOnlyValidation.ts:1038-1041#isSafeOutputCommand`.
pub fn is_safe_output_command(name: &str) -> bool {
    SAFE_OUTPUT_CMDLETS.contains(&resolve_to_canonical(name).as_str())
}

/// Maps to: CC `readOnlyValidation.ts:1052-1061#isAllowlistedPipelineTail`.
///
/// Narrow fallback for `is_safe_output_command` call sites that still need the
/// "skip harmless pipeline tail" behavior for Format-Table / Select-Object and
/// friends. Does NOT match the full allowlist — only the migrated transformers.
pub fn is_allowlisted_pipeline_tail(cmd: &ParsedCommandElement, original_command: &str) -> bool {
    if !PIPELINE_TAIL_CMDLETS.contains(&resolve_to_canonical(&cmd.name).as_str()) {
        return false;
    }
    is_allowlisted_command(cmd, original_command)
}

/// Maps to: CC `readOnlyValidation.ts:1072-1082#isProvablySafeStatement`.
///
/// Fail-closed gate for read-only auto-allow. Returns true ONLY for a
/// PipelineAst whose every element is a CommandAst — the one statement shape
/// that can be fully validated. New AST types fall through to false by
/// construction.
pub fn is_provably_safe_statement(stmt: &ParsedStatement) -> bool {
    if stmt.statement_type != StatementType::PipelineAst {
        return false;
    }
    // Empty commands would vacuously pass the loop below. PowerShell guarantees
    // at least one pipeline element for valid source, but this gate is the
    // linchpin, so it defends against parser/JSON edge cases.
    if stmt.commands.is_empty() {
        return false;
    }
    stmt.commands
        .iter()
        .all(|cmd| cmd.element_type == Some(PipelineElementType::CommandAst))
}

/// Maps to: CC `readOnlyValidation.ts:1088-1101#lookupAllowlist`.
fn lookup_allowlist(name: &str) -> Option<&'static CmdletConfig> {
    let lower = name.to_lowercase();
    if let Some(config) = CMDLET_ALLOWLIST.get(lower.as_str()) {
        return Some(config);
    }
    let canonical = resolve_to_canonical(&lower);
    if canonical != lower {
        return CMDLET_ALLOWLIST.get(canonical.as_str());
    }
    None
}

static SUBEXPRESSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\(").expect("valid subexpression regex"));
static SPLATTING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^0-9A-Za-z_.])@[0-9A-Za-z_]+").expect("valid splat regex"));
static MEMBER_INVOCATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.[0-9A-Za-z_]+\s*\(").expect("valid member regex"));
static ASSIGNMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$[0-9A-Za-z_]+\s*[+\-*/]?=").expect("valid assignment regex"));

/// `/(?<!:)\/\//` — Rust's regex engine has no lookbehind, so the `://` carve
/// out is applied directly.
fn has_bare_double_slash(text: &str) -> bool {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(1)).any(|index| {
        bytes[index] == b'/' && bytes[index + 1] == b'/' && (index == 0 || bytes[index - 1] != b':')
    })
}

/// Maps to: CC `readOnlyValidation.ts:1112-1159#hasSyncSecurityConcerns`.
///
/// Regex pre-filter used by the sync read-only path before the cmdlet
/// allowlist check, mirroring BashTool's `checkReadOnlyConstraints`.
pub fn has_sync_security_concerns(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Subexpressions: $(...) can execute arbitrary code.
    if SUBEXPRESSION_RE.is_match(trimmed) {
        return true;
    }
    // Splatting: @variable passes arbitrary parameters. Real splatting is
    // token-start only, so `user@example.com` and `file.@{u}` do not match.
    if SPLATTING_RE.is_match(trimmed) {
        return true;
    }
    // Member invocations: .Method() can call arbitrary .NET methods.
    if MEMBER_INVOCATION_RE.is_match(trimmed) {
        return true;
    }
    // Assignments: $var = ... can modify state.
    if ASSIGNMENT_RE.is_match(trimmed) {
        return true;
    }
    // Stop-parsing symbol: --% passes everything raw to native commands.
    if trimmed.contains("--%") {
        return true;
    }
    // UNC paths can trigger network requests and leak NTLM/Kerberos credentials.
    if trimmed.contains("\\\\") || has_bare_double_slash(trimmed) {
        return true;
    }
    // Static method calls: [Type]::Method() can invoke arbitrary .NET methods.
    trimmed.contains("::")
}

/// Maps to: CC `readOnlyValidation.ts:1168-1305#isReadOnlyCommand`.
pub fn is_read_only_command(command: &str, parsed: Option<&ParsedPowerShellCommand>) -> bool {
    if command.trim().is_empty() {
        return false;
    }

    // Without a parsed AST, or when parsing failed, refuse to classify.
    let Some(parsed) = parsed.filter(|parsed| parsed.valid) else {
        return false;
    };

    // Reject script blocks and friends — the code inside cannot be verified.
    // `Get-Process | ForEach-Object { Remove-Item C:\foo }` looks like a safe
    // pipeline but the script block is destructive.
    let security = derive_security_flags(parsed);
    if security.has_script_blocks
        || security.has_sub_expressions
        || security.has_expandable_strings
        || security.has_splatting
        || security.has_member_invocations
        || security.has_assignments
        || security.has_stop_parsing
    {
        return false;
    }

    let segments = get_pipeline_segments(parsed);
    if segments.is_empty() {
        return false;
    }

    // SECURITY: block compound commands containing a cwd-changing cmdlet
    // alongside any other statement. `Set-Location ~; Get-Content
    // ./.ssh/id_rsa` has both cmdlets in the allowlist, so without this guard
    // the compound auto-allows while path validation resolved
    // `./.ssh/id_rsa` against the STALE validator cwd.
    let total_commands: usize = segments.iter().map(|segment| segment.commands.len()).sum();
    if total_commands > 1
        && segments.iter().any(|segment| {
            segment
                .commands
                .iter()
                .any(|cmd| is_cwd_changing_cmdlet(&cmd.name))
        })
    {
        return false;
    }

    // Every statement must independently be read-only.
    for pipeline in segments {
        if pipeline.commands.is_empty() {
            return false;
        }

        // Reject file redirections. `> $null` discards output and is not a
        // filesystem write, so it does not disqualify read-only status.
        if pipeline.redirections.iter().any(|redirection| {
            !redirection.is_merging && !is_null_redirection_target(&redirection.target)
        }) {
            return false;
        }

        let Some(first_cmd) = pipeline.commands.first() else {
            return false;
        };
        if !is_allowlisted_command(first_cmd, command) {
            return false;
        }

        // Remaining pipeline commands must be safe output cmdlets or
        // allowlisted with arg validation. The nameType gate catches
        // `scripts\Out-Null`, whose stripped name would match
        // SAFE_OUTPUT_CMDLETS while PowerShell runs `scripts\Out-Null.ps1`.
        for cmd in pipeline.commands.iter().skip(1) {
            if cmd.name_type == Some(CommandNameType::Application) {
                return false;
            }
            // SECURITY: `is_safe_output_command` is name-only, so only
            // zero-arg invocations short-circuit. `Out-String -InputObject:(rm
            // x)` evaluates the paren when Out-String runs.
            if is_safe_output_command(&cmd.name) && cmd.args.is_empty() {
                continue;
            }
            if !is_allowlisted_command(cmd, command) {
                return false;
            }
        }

        // SECURITY: reject statements with nested commands. Those are
        // CommandAst nodes inside script-block arguments, ParenExpressionAst
        // children of colon-bound parameters, or other non-top-level
        // positions — executable sub-pipelines that bypass the per-command
        // allowlist check above.
        if pipeline
            .nested_commands
            .as_ref()
            .is_some_and(|nested| !nested.is_empty())
        {
            return false;
        }
    }

    true
}

/// Maps to: CC `readOnlyValidation.ts:1310-1516#isAllowlistedCommand`.
pub fn is_allowlisted_command(cmd: &ParsedCommandElement, original_command: &str) -> bool {
    // SECURITY: nameType is computed from the raw, pre-strip name.
    // 'application' means it contained path characters — e.g.
    // `scripts\Get-Process`, `./git`, `node.exe`. PowerShell resolves those as
    // file paths, not as the cmdlet the stripped name matches, and the
    // allowlist was built for cmdlets rather than arbitrary scripts.
    if cmd.name_type == Some(CommandNameType::Application) {
        // Bypass for explicit safe .exe names. SECURITY: match the raw first
        // token of `cmd.text`, not `cmd.name` — `strip_module_prefix` collapses
        // `scripts\where.exe` to `where.exe` while `text` preserves the path.
        let raw_first_token = cmd
            .text
            .split(char::is_whitespace)
            .next()
            .unwrap_or_default()
            .to_lowercase();
        if !SAFE_EXTERNAL_EXES.contains(&raw_first_token.as_str()) {
            return false;
        }
    }

    let Some(config) = lookup_allowlist(&cmd.name) else {
        return false;
    };

    if let Some(check) = config.additional_check {
        if additional_check_is_dangerous(check, cmd) {
            return false;
        }
    }

    // SECURITY: whitelist arg elementTypes — only StringConstant and Parameter
    // are statically verifiable. Everything else expands at runtime:
    // `Get-Process $env:AWS_SECRET_ACCESS_KEY` errors with the secret in the
    // message, hashtable/convert/binary expressions leak the same way, and
    // subexpressions run arbitrary code.
    //
    // elementTypes missing means an untrusted or malformed element, so it fails
    // closed. elementTypes[0] is the command name; args start at index 1.
    let Some(element_types) = cmd.element_types.as_ref() else {
        return false;
    };
    for (index, element_type) in element_types.iter().enumerate().skip(1) {
        let arg = cmd
            .args
            .get(index - 1)
            .map(String::as_str)
            .unwrap_or_default();
        if *element_type != CommandElementType::StringConstant
            && *element_type != CommandElementType::Parameter
        {
            // ArrayLiteralAst (`Get-Process Name, Id`) maps to 'Other'. Every
            // leak vector has a metacharacter in its extent text; a bare
            // comma-list of identifiers has none.
            if !has_expression_metachar(arg) {
                continue;
            }
            return false;
        }
        // A colon-bound parameter (`-Flag:$env:SECRET`) is a SINGLE
        // CommandParameterAst, so the whitelist above passes. Query the
        // parser's children tree, which catches more than a string check —
        // `-InputObject:@{k=v}` has no `$`, and `-Name:('payload' > file)`
        // hides a redirection.
        if *element_type == CommandElementType::Parameter {
            match cmd
                .children
                .as_ref()
                .and_then(|children| children.get(index - 1))
                .cloned()
                .flatten()
            {
                Some(param_children) => {
                    if param_children
                        .iter()
                        .any(|child| child.element_type != CommandElementType::StringConstant)
                    {
                        return false;
                    }
                }
                None => {
                    if let Some(colon_index) = arg.find(':') {
                        if colon_index > 0 && has_expression_metachar(&arg[colon_index + 1..]) {
                            return false;
                        }
                    }
                }
            }
        }
    }

    let canonical = resolve_to_canonical(&cmd.name);

    if matches!(canonical.as_str(), "git" | "gh" | "docker" | "dotnet") {
        return is_external_command_safe(&canonical, &cmd.args, original_command);
    }

    // On Windows `/` is a valid flag prefix for native commands (`findstr /S`),
    // but PowerShell cmdlets always use `-` parameters, so `/tmp` is a path.
    // Cmdlets are detected by their Verb-Noun canonical name.
    let is_cmdlet = canonical.contains('-');

    if config.allow_all_flags {
        return true;
    }
    if config.safe_flags.is_empty() {
        // No safeFlags and allow_all_flags unset means "positional args only,
        // reject all flags" — the safe default. Commands must opt in.
        return !cmd
            .args
            .iter()
            .enumerate()
            .any(|(index, arg)| arg_is_flag(arg, index, cmd, is_cmdlet));
    }

    for (index, arg) in cmd.args.iter().enumerate() {
        if !arg_is_flag(arg, index, cmd, is_cmdlet) {
            continue;
        }
        // For cmdlets, normalize a Unicode dash to ASCII for the safeFlags
        // comparison. Native-exe safeFlags are stored with `/`, so leave those.
        let mut param_name = if is_cmdlet {
            format!("-{}", arg.chars().skip(1).collect::<String>())
        } else {
            arg.clone()
        };
        if let Some(colon_index) = param_name.find(':') {
            if colon_index > 0 {
                param_name.truncate(colon_index);
            }
        }

        // -ErrorAction/-Verbose/-Debug and friends are accepted by every cmdlet
        // via [CmdletBinding()] and only route streams, so they cannot make a
        // read-only cmdlet write.
        let param_lower = param_name.to_lowercase();
        if is_cmdlet && is_common_parameter(&param_lower) {
            continue;
        }
        if !config
            .safe_flags
            .iter()
            .any(|flag| flag.to_lowercase() == param_lower)
        {
            return false;
        }
    }

    true
}

/// SECURITY: elementTypes is the ground truth for parameter detection.
/// PowerShell's tokenizer accepts en-dash/em-dash/horizontal-bar as parameter
/// prefixes, which a raw `starts_with('-')` check misses. Native exes on
/// Windows additionally use the `/` argv convention, which the parser sees as a
/// positional rather than a CommandParameterAst.
fn arg_is_flag(arg: &str, index: usize, cmd: &ParsedCommandElement, is_cmdlet: bool) -> bool {
    if is_cmdlet {
        let element_type = cmd
            .element_types
            .as_ref()
            .and_then(|types| types.get(index + 1).copied());
        return is_powershell_parameter(arg, element_type);
    }
    arg.starts_with('-') || (cfg!(target_os = "windows") && arg.starts_with('/'))
}

fn additional_check_is_dangerous(check: AdditionalCheck, cmd: &ParsedCommandElement) -> bool {
    match check {
        AdditionalCheck::ArgLeaksValue => arg_leaks_value(Some(cmd)),
        AdditionalCheck::IpconfigRejectsPositionals => cmd
            .args
            .iter()
            .any(|arg| !arg.starts_with('/') && !arg.starts_with('-')),
        AdditionalCheck::HostnameRejectsPositionals => {
            cmd.args.iter().any(|arg| !arg.starts_with('-'))
        }
        AdditionalCheck::RouteVerbMustBePrint => {
            // route.exe syntax is `route [-f] [-p] [-4|-6] VERB [args...]`; the
            // first non-flag positional is the verb. A position-insensitive
            // `args.contains("print")` would accept `route add ... print`.
            cmd.args
                .iter()
                .find(|arg| !arg.starts_with('-'))
                .map(|verb| verb.to_lowercase() != "print")
                .unwrap_or(true)
        }
    }
}

// ---------------------------------------------------------------------------
// External command validation (git, gh, docker, dotnet) using shared configs
// ---------------------------------------------------------------------------

/// Maps to: CC `readOnlyValidation.ts:1522-1535#isExternalCommandSafe`.
fn is_external_command_safe(command: &str, args: &[String], raw_command: &str) -> bool {
    match command {
        "git" => is_git_safe(args, raw_command),
        "gh" => is_gh_safe(args, raw_command),
        "docker" => is_docker_safe(args, raw_command),
        "dotnet" => is_dotnet_safe(args),
        _ => false,
    }
}

/// Maps to: CC `readOnlyValidation.ts:1537-1553#DANGEROUS_GIT_GLOBAL_FLAGS`.
///
/// `--attr-source` is rejected outright rather than skipped: git treats the
/// token after its tree-ish value as a pathspec, so a skip-by-2 loop would read
/// the wrong token as the subcommand.
const DANGEROUS_GIT_GLOBAL_FLAGS: [&str; 7] = [
    "-c",
    "-C",
    "--exec-path",
    "--config-env",
    "--git-dir",
    "--work-tree",
    "--attr-source",
];

/// Maps to: CC `readOnlyValidation.ts:1566-1576#GIT_GLOBAL_FLAGS_WITH_VALUES`.
///
/// SECURITY: this set must be COMPLETE. A value-consuming global flag that is
/// missing here creates a parser differential where the validator reads the
/// value as the subcommand while git runs the NEXT token.
const GIT_GLOBAL_FLAGS_WITH_VALUES: [&str; 9] = [
    "-c",
    "-C",
    "--exec-path",
    "--config-env",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--shallow-file",
];

/// Maps to: CC `readOnlyValidation.ts:1582#DANGEROUS_GIT_SHORT_FLAGS_ATTACHED`.
const DANGEROUS_GIT_SHORT_FLAGS_ATTACHED: [&str; 2] = ["-c", "-C"];

/// Maps to: CC `readOnlyValidation.ts:1584-1701#isGitSafe`.
fn is_git_safe(args: &[String], raw_command: &str) -> bool {
    if args.is_empty() {
        return true;
    }

    // SECURITY: reject any arg containing `$`. Bare VariableExpressionAst
    // positionals reach here as literal text; the validator sees `$VAR` while
    // PowerShell expands it at runtime, so `git diff $VAR` with
    // `$VAR='--output=/tmp/evil'` would write a file.
    if args.iter().any(|arg| arg.contains('$')) {
        return false;
    }

    // Skip over global flags before the subcommand, rejecting dangerous ones.
    // Flags taking space-separated values must consume the next token so it is
    // not mistaken for the subcommand (`git --namespace foo status`).
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        if !arg.starts_with('-') {
            break;
        }
        // SECURITY: attached-form short flags. `-ccore.pager=sh` splits on `=`
        // to `-ccore.pager`, which is not in the dangerous set, so prefix
        // matching is required. The `!= '-'` guard applies only to `-c` (git
        // config keys never start with `-`); directory paths CAN start with
        // `-`, so `git -C-trap status` must reject.
        for short_flag in DANGEROUS_GIT_SHORT_FLAGS_ATTACHED {
            if arg.len() > short_flag.len()
                && arg.starts_with(short_flag)
                && (short_flag == "-C" || arg.as_bytes()[short_flag.len()] != b'-')
            {
                return false;
            }
        }
        let has_inline_value = arg.contains('=');
        let flag_name = if has_inline_value {
            arg.split('=').next().unwrap_or_default()
        } else {
            arg
        };
        if DANGEROUS_GIT_GLOBAL_FLAGS.contains(&flag_name) {
            return false;
        }
        if !has_inline_value && GIT_GLOBAL_FLAGS_WITH_VALUES.contains(&flag_name) {
            index += 2;
        } else {
            index += 1;
        }
    }

    if index >= args.len() {
        return true;
    }

    let mut words = vec!["git".to_string()];
    words.extend(args[index..].iter().map(|arg| arg.to_lowercase()));
    let Some((name, command_tokens, config)) = command_config(&words, false) else {
        return false;
    };
    let flag_args: Vec<String> = args[index + command_tokens - 1..].to_vec();

    // `git ls-remote URL` is a data-exfiltration vector (secrets encoded in the
    // hostname leak via DNS/HTTP), so URL-like positionals are rejected.
    if name == "git ls-remote"
        && flag_args.iter().any(|arg| {
            !arg.starts_with('-')
                && (arg.contains("://")
                    || arg.contains('@')
                    || arg.contains(':')
                    || arg.contains('$'))
        })
    {
        return false;
    }

    if additional_command_is_dangerous(name, "", &flag_args) {
        return false;
    }
    validate_flags(
        &flag_args,
        0,
        config,
        ValidateFlagsOptions {
            command_name: Some("git"),
            raw_command: Some(raw_command),
            xargs_target_commands: None,
        },
    )
}

/// Maps to: CC `readOnlyValidation.ts:1703-1757#isGhSafe`.
fn is_gh_safe(args: &[String], raw_command: &str) -> bool {
    // gh commands are network-dependent; only allow for ant users.
    if crate::utils::process_env::var("USER_TYPE").as_deref() != Some("ant") {
        return false;
    }

    if args.is_empty() {
        return true;
    }

    let mut words = vec!["gh".to_string()];
    words.extend(args.iter().map(|arg| arg.to_lowercase()));
    let Some((name, command_tokens, config)) = command_config(&words, false) else {
        return false;
    };
    let flag_args: Vec<String> = args[command_tokens - 1..].to_vec();

    // SECURITY: reject any arg containing `$`. All gh subcommands are
    // network-facing, so a variable argument is an exfiltration vector:
    // `gh search repos $env:SECRET_API_KEY` sends the secret to the API.
    if flag_args.iter().any(|arg| arg.contains('$')) {
        return false;
    }
    if additional_command_is_dangerous(name, "", &flag_args) {
        return false;
    }
    validate_flags(
        &flag_args,
        0,
        config,
        ValidateFlagsOptions {
            command_name: Some("gh"),
            raw_command: Some(raw_command),
            xargs_target_commands: None,
        },
    )
}

/// Maps to: CC `readOnlyValidation.ts:1759-1807#isDockerSafe`.
fn is_docker_safe(args: &[String], raw_command: &str) -> bool {
    if args.is_empty() {
        return true;
    }

    // SECURITY: blanket `$` rejection, checked BEFORE the fast path. The
    // earlier placement missed `docker ps --format $env:AWS_SECRET_ACCESS_KEY`,
    // which PowerShell expanded and docker echoed back in its error output.
    // args[0] (the subcommand slot) is checked too.
    if args.iter().any(|arg| arg.contains('$')) {
        return false;
    }

    let one_word_key = format!("docker {}", args[0].to_lowercase());
    if EXTERNAL_READONLY_COMMANDS.contains(&one_word_key.as_str()) {
        return true;
    }

    let mut words = vec!["docker".to_string()];
    words.extend(args.iter().map(|arg| arg.to_lowercase()));
    let Some((name, command_tokens, config)) = command_config(&words, false) else {
        return false;
    };
    let flag_args: Vec<String> = args[command_tokens - 1..].to_vec();

    if additional_command_is_dangerous(name, "", &flag_args) {
        return false;
    }
    validate_flags(
        &flag_args,
        0,
        config,
        ValidateFlagsOptions {
            command_name: Some("docker"),
            raw_command: Some(raw_command),
            xargs_target_commands: None,
        },
    )
}

/// Maps to: CC `readOnlyValidation.ts:1809-1823#isDotnetSafe`.
fn is_dotnet_safe(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    args.iter()
        .all(|arg| DOTNET_READ_ONLY_FLAGS.contains(&arg.to_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::powershell::parser::{
        CommandElementChild, ParsedRedirection, RedirectionOperator,
    };

    fn cmd(
        name: &str,
        args: &[&str],
        element_types: &[CommandElementType],
    ) -> ParsedCommandElement {
        let mut types = vec![CommandElementType::StringConstant];
        types.extend_from_slice(element_types);
        ParsedCommandElement {
            name: name.to_string(),
            name_type: Some(CommandNameType::Cmdlet),
            element_type: Some(PipelineElementType::CommandAst),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            text: format!("{name} {}", args.join(" ")).trim_end().to_string(),
            element_types: Some(types),
            children: None,
            redirections: None,
        }
    }

    fn literals(count: usize) -> Vec<CommandElementType> {
        vec![CommandElementType::StringConstant; count]
    }

    fn parsed(statements: Vec<ParsedStatement>) -> ParsedPowerShellCommand {
        ParsedPowerShellCommand {
            valid: true,
            errors: Vec::new(),
            statements,
            variables: Vec::new(),
            has_stop_parsing: false,
            original_command: String::new(),
            type_literals: Vec::new(),
            has_using_statements: false,
            has_script_requirements: false,
        }
    }

    fn statement(commands: Vec<ParsedCommandElement>) -> ParsedStatement {
        ParsedStatement {
            statement_type: StatementType::PipelineAst,
            commands,
            redirections: Vec::new(),
            text: String::new(),
            nested_commands: None,
            security_patterns: None,
        }
    }

    #[test]
    fn resolve_to_canonical_matches_official_alias_and_pathext_order() {
        assert_eq!(resolve_to_canonical("SLS"), "select-string");
        assert_eq!(resolve_to_canonical("git.EXE"), "git");
        assert_eq!(resolve_to_canonical("scripts\\git.exe"), "scripts\\git.exe");
        assert_eq!(resolve_to_canonical("./git.cmd"), "./git.cmd");
        // PATHEXT stripping happens before the source's single alias lookup.
        assert_eq!(resolve_to_canonical("where.exe"), "where-object");
        assert_eq!(resolve_to_canonical("sort"), "sort");
    }

    #[test]
    fn cwd_changing_cmdlets_cover_aliases_and_psdrive_creation() {
        for name in [
            "cd",
            "sl",
            "chdir",
            "pushd",
            "popd",
            "Set-Location",
            "New-PSDrive",
        ] {
            assert!(is_cwd_changing_cmdlet(name), "name={name:?}");
        }
        for name in ["Get-Content", "git"] {
            assert!(!is_cwd_changing_cmdlet(name), "name={name:?}");
        }
        // `mount` is mount(8) on POSIX and must not be treated as PSDrive.
        assert_eq!(is_cwd_changing_cmdlet("mount"), cfg!(target_os = "windows"));
    }

    #[test]
    fn safe_output_is_name_only_and_excludes_the_migrated_transformers() {
        assert!(is_safe_output_command("Out-Null"));
        assert!(!is_safe_output_command("Out-String"));
        assert!(!is_safe_output_command("Format-Table"));
        assert!(!is_safe_output_command("Write-Output"));
    }

    #[test]
    fn pipeline_tails_pass_only_when_their_arguments_validate() {
        assert!(is_allowlisted_pipeline_tail(
            &cmd("Format-Table", &["Name"], &literals(1)),
            "x | Format-Table Name"
        ));
        assert!(!is_allowlisted_pipeline_tail(
            &cmd(
                "Format-Table",
                &["$env:SECRET"],
                &[CommandElementType::Variable]
            ),
            "x | Format-Table $env:SECRET"
        ));
        // Not a migrated transformer.
        assert!(!is_allowlisted_pipeline_tail(
            &cmd("Get-Content", &["a.txt"], &literals(1)),
            "x | Get-Content a.txt"
        ));
    }

    #[test]
    fn provably_safe_statements_require_an_all_command_pipeline() {
        assert!(is_provably_safe_statement(&statement(vec![cmd(
            "Get-Date",
            &[],
            &[]
        )])));
        assert!(!is_provably_safe_statement(&statement(Vec::new())));
        let mut expression = statement(vec![ParsedCommandElement {
            element_type: Some(PipelineElementType::CommandExpressionAst),
            ..ParsedCommandElement::default()
        }]);
        assert!(!is_provably_safe_statement(&expression));
        expression.statement_type = StatementType::IfStatementAst;
        assert!(!is_provably_safe_statement(&expression));
    }

    #[test]
    fn arg_leaks_value_blocks_variables_hashtables_and_colon_bound_expressions() {
        assert!(arg_leaks_value(Some(&cmd(
            "Write-Output",
            &["$env:SECRET"],
            &[CommandElementType::Variable]
        ))));
        assert!(arg_leaks_value(Some(&cmd(
            "Format-Table",
            &["@{N='x';E={}}"],
            &[CommandElementType::Other]
        ))));
        assert!(arg_leaks_value(Some(&cmd(
            "Write-Output",
            &["-InputObject:$env:SECRET"],
            &[CommandElementType::Parameter]
        ))));
        // ArrayLiteralAst of bare identifiers has no metachar and is allowed.
        assert!(!arg_leaks_value(Some(&cmd(
            "Select-Object",
            &["Name, Id"],
            &[CommandElementType::Other]
        ))));
        assert!(!arg_leaks_value(Some(&cmd(
            "Write-Output",
            &["hello"],
            &literals(1)
        ))));
        assert!(!arg_leaks_value(None));
    }

    #[test]
    fn arg_leaks_value_prefers_the_children_tree_over_string_archaeology() {
        let mut command = cmd(
            "Write-Output",
            &["-InputObject:@{k=v}"],
            &[CommandElementType::Parameter],
        );
        command.children = Some(vec![Some(vec![CommandElementChild {
            element_type: CommandElementType::Other,
            text: "@{k=v}".to_string(),
        }])]);
        assert!(arg_leaks_value(Some(&command)));

        command.children = Some(vec![Some(vec![CommandElementChild {
            element_type: CommandElementType::StringConstant,
            text: "value".to_string(),
        }])]);
        assert!(!arg_leaks_value(Some(&command)));
    }

    #[test]
    fn sync_security_concerns_match_the_official_regex_set() {
        for command in [
            "Get-Content $(whoami)",
            "Get-Process @splat",
            "$x.Invoke()",
            "$x = 1",
            "git log --%",
            "Get-Content \\\\server\\share",
            "Get-Content //server/share",
            "[System.IO.File]::ReadAllText('x')",
        ] {
            assert!(has_sync_security_concerns(command), "command={command:?}");
        }
        for command in [
            "",
            "Get-ChildItem ./src",
            "Send-MailMessage user@example.com",
            "Invoke-WebRequest https://example.com",
        ] {
            assert!(!has_sync_security_concerns(command), "command={command:?}");
        }
    }

    #[test]
    fn allowlisted_cmdlets_validate_flags_and_reject_unknown_ones() {
        assert!(is_allowlisted_command(
            &cmd(
                "Get-Content",
                &["-Path", "a.txt"],
                &[
                    CommandElementType::Parameter,
                    CommandElementType::StringConstant
                ]
            ),
            "Get-Content -Path a.txt"
        ));
        assert!(!is_allowlisted_command(
            &cmd("Get-Content", &["-Wait"], &[CommandElementType::Parameter]),
            "Get-Content -Wait"
        ));
        assert!(!is_allowlisted_command(
            &cmd("Remove-Item", &["a.txt"], &literals(1)),
            "Remove-Item a.txt"
        ));
    }

    #[test]
    fn common_parameters_are_accepted_on_any_allowlisted_cmdlet() {
        assert!(is_allowlisted_command(
            &cmd(
                "Get-Content",
                &["a.txt", "-ErrorAction", "SilentlyContinue"],
                &[
                    CommandElementType::StringConstant,
                    CommandElementType::Parameter,
                    CommandElementType::StringConstant
                ]
            ),
            "Get-Content a.txt -ErrorAction SilentlyContinue"
        ));
    }

    #[test]
    fn missing_element_types_fail_closed() {
        let mut command = cmd("Get-Date", &[], &[]);
        command.element_types = None;
        assert!(!is_allowlisted_command(&command, "Get-Date"));
    }

    #[test]
    fn path_like_names_are_rejected_unless_explicitly_safe_executables() {
        let mut spoofed = cmd("Get-Process", &[], &[]);
        spoofed.name_type = Some(CommandNameType::Application);
        spoofed.text = "scripts\\Get-Process".to_string();
        assert!(!is_allowlisted_command(&spoofed, "scripts\\Get-Process"));

        let mut safe_exe = cmd("where.exe", &["git"], &literals(1));
        safe_exe.name_type = Some(CommandNameType::Application);
        safe_exe.text = "where.exe git".to_string();
        assert!(is_allowlisted_command(&safe_exe, "where.exe git"));

        let mut spoofed_exe = cmd("where.exe", &["git"], &literals(1));
        spoofed_exe.name_type = Some(CommandNameType::Application);
        spoofed_exe.text = "scripts\\where.exe git".to_string();
        assert!(!is_allowlisted_command(
            &spoofed_exe,
            "scripts\\where.exe git"
        ));
    }

    #[test]
    fn native_exe_positional_guards_follow_the_official_callbacks() {
        assert!(is_allowlisted_command(
            &cmd("route", &["print"], &literals(1)),
            "route print"
        ));
        assert!(!is_allowlisted_command(
            &cmd("route", &["add", "10.0.0.0", "print"], &literals(3)),
            "route add 10.0.0.0 print"
        ));
        assert!(!is_allowlisted_command(
            &cmd("hostname", &["newname"], &literals(1)),
            "hostname newname"
        ));
        assert!(is_allowlisted_command(
            &cmd("hostname", &[], &[]),
            "hostname"
        ));
        assert!(!is_allowlisted_command(
            &cmd("ipconfig", &["set", "en0", "DHCP"], &literals(3)),
            "ipconfig set en0 DHCP"
        ));
    }

    #[test]
    fn git_global_flags_that_can_run_code_are_rejected() {
        for args in [
            vec!["-c", "core.pager=sh", "log"],
            vec!["-ccore.pager=sh", "log"],
            vec!["-C-trap", "status"],
            vec!["--exec-path=/tmp", "status"],
            vec!["--attr-source", "HEAD~10", "log", "status"],
            vec!["diff", "$VAR"],
        ] {
            let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            assert!(!is_git_safe(&owned, "git"), "args={args:?}");
        }
        let safe: Vec<String> = ["--namespace", "foo", "status"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        assert!(is_git_safe(&safe, "git"));
        assert!(is_git_safe(&Vec::new(), "git"));
    }

    #[test]
    fn git_read_only_subcommands_are_allowed_and_writes_are_not() {
        let allow: Vec<String> = ["status"].iter().map(|arg| arg.to_string()).collect();
        assert!(is_git_safe(&allow, "git status"));
        let deny: Vec<String> = ["push"].iter().map(|arg| arg.to_string()).collect();
        assert!(!is_git_safe(&deny, "git push"));
    }

    #[test]
    fn git_ls_remote_rejects_url_shaped_positionals() {
        for arg in ["https://evil.com/x", "git@host:path", "$env:URL"] {
            let args: Vec<String> = vec!["ls-remote".to_string(), arg.to_string()];
            assert!(!is_git_safe(&args, "git ls-remote"), "arg={arg:?}");
        }
    }

    #[test]
    fn docker_fast_path_still_rejects_variable_arguments() {
        let safe: Vec<String> = vec!["ps".to_string()];
        assert!(is_docker_safe(&safe, "docker ps"));
        let leaky: Vec<String> = vec![
            "ps".to_string(),
            "--format".to_string(),
            "$env:AWS_SECRET_ACCESS_KEY".to_string(),
        ];
        assert!(!is_docker_safe(&leaky, "docker ps --format $env:X"));
        let write: Vec<String> = vec!["run".to_string(), "alpine".to_string()];
        assert!(!is_docker_safe(&write, "docker run alpine"));
    }

    #[test]
    fn dotnet_only_allows_its_informational_flags() {
        let safe: Vec<String> = vec!["--version".to_string()];
        assert!(is_dotnet_safe(&safe));
        let unsafe_args: Vec<String> = vec!["build".to_string()];
        assert!(!is_dotnet_safe(&unsafe_args));
        assert!(!is_dotnet_safe(&Vec::new()));
    }

    #[test]
    fn read_only_classification_requires_a_valid_parse() {
        assert!(!is_read_only_command("Get-Date", None));
        let invalid = ParsedPowerShellCommand {
            valid: false,
            ..parsed(vec![statement(vec![cmd("Get-Date", &[], &[])])])
        };
        assert!(!is_read_only_command("Get-Date", Some(&invalid)));
        assert!(!is_read_only_command("   ", Some(&parsed(Vec::new()))));
    }

    #[test]
    fn read_only_pipelines_allow_safe_tails_but_reject_writes() {
        let allowed = parsed(vec![statement(vec![
            cmd("Get-ChildItem", &["./src"], &literals(1)),
            cmd("Out-Null", &[], &[]),
        ])]);
        assert!(is_read_only_command(
            "Get-ChildItem ./src | Out-Null",
            Some(&allowed)
        ));

        let denied = parsed(vec![statement(vec![
            cmd("Get-ChildItem", &["./src"], &literals(1)),
            cmd("Remove-Item", &["x"], &literals(1)),
        ])]);
        assert!(!is_read_only_command(
            "Get-ChildItem ./src | Remove-Item x",
            Some(&denied)
        ));
    }

    #[test]
    fn safe_output_short_circuit_requires_zero_arguments() {
        let with_args = parsed(vec![statement(vec![
            cmd("Get-Process", &[], &[]),
            cmd(
                "Out-Null",
                &["-InputObject:(Remove-Item /tmp/x)"],
                &[CommandElementType::Parameter],
            ),
        ])]);
        assert!(!is_read_only_command(
            "Get-Process | Out-Null -InputObject:(...)",
            Some(&with_args)
        ));
    }

    #[test]
    fn compound_commands_with_a_cwd_change_are_never_read_only() {
        let compound = parsed(vec![
            statement(vec![cmd("Set-Location", &["~"], &literals(1))]),
            statement(vec![cmd("Get-Content", &["./.ssh/id_rsa"], &literals(1))]),
        ]);
        assert!(!is_read_only_command(
            "Set-Location ~; Get-Content ./.ssh/id_rsa",
            Some(&compound)
        ));
    }

    #[test]
    fn file_redirections_disqualify_read_only_but_null_sinks_do_not() {
        let mut redirected = parsed(vec![statement(vec![cmd("Get-Date", &[], &[])])]);
        redirected.statements[0].redirections = vec![ParsedRedirection {
            operator: RedirectionOperator::Output,
            target: "/tmp/x".to_string(),
            is_merging: false,
        }];
        assert!(!is_read_only_command(
            "Get-Date > /tmp/x",
            Some(&redirected)
        ));

        redirected.statements[0].redirections[0].target = "$null".to_string();
        assert!(is_read_only_command("Get-Date > $null", Some(&redirected)));
    }

    #[test]
    fn nested_commands_disqualify_read_only_classification() {
        let mut nested = parsed(vec![statement(vec![cmd("Get-Date", &[], &[])])]);
        nested.statements[0].nested_commands = Some(vec![cmd("Remove-Item", &["/"], &literals(1))]);
        assert!(!is_read_only_command("Get-Date", Some(&nested)));
    }
}
