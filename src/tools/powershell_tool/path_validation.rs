//! Maps to: CC `tools/PowerShellTool/pathValidation.ts`.
//!
//! Extracts file paths from PowerShell commands using the AST parser and
//! validates that they stay within the allowed project directories.
//!
//! SECURITY MODEL: any `-Param` that is not declared in a cmdlet's
//! `path_params` / `known_switches` / `known_value_params` sets forces
//! `has_unvalidatable_path_arg`, which the caller turns into an ask. That ends
//! the whack-a-mole where a missing switch caused the unknown-parameter
//! heuristic to swallow the next argument — potentially the positional path.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionBehavior, PermissionMode, PermissionRule, PermissionUpdate,
    PermissionUpdateDestination,
};
use crate::utils::fs_operations::{get_fs_implementation, safe_resolve_path};
use crate::utils::permissions::filesystem::{
    FilePermissionType, all_working_directories, matching_rule_for_input,
};
use crate::utils::permissions::path_validation::{
    FileOperationType, expand_tilde, format_directory_list, is_dangerous_removal_path,
    is_path_allowed,
};
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use crate::utils::permissions::permission_update::create_read_rule_suggestion;
use crate::utils::powershell::parser::{
    CommandElementType, ParsedCommandElement, ParsedPowerShellCommand, ParsedStatement,
    PipelineElementType, is_null_redirection_target, is_powershell_parameter,
};

use super::common_parameters::{COMMON_SWITCHES, COMMON_VALUE_PARAMS};
use super::read_only_validation::resolve_to_canonical;

/// Maps to: CC `pathValidation.ts:50#GLOB_PATTERN_REGEX`.
///
/// PowerShell wildcards are only `*`, `?`, `[`, `]` — braces are literal
/// characters with no brace expansion. Including `{}` mis-routed paths like
/// `./{x}/passwd` through glob-base truncation instead of full-path symlink
/// resolution.
const GLOB_PATTERN_CHARS: [char; 4] = ['*', '?', '[', ']'];

/// Maps to: CC `pathValidation.ts:88-122#CmdletPathConfig`.
struct CmdletPathConfig {
    operation_type: FileOperationType,
    /// Parameter names that accept file paths (validated against allowed dirs)
    path_params: &'static [&'static str],
    /// Switch parameters that take no value (next arg is NOT consumed)
    known_switches: &'static [&'static str],
    /// Value-taking parameters that are not paths (next arg IS consumed, not
    /// path-validated)
    known_value_params: &'static [&'static str],
    /// Parameters whose value PowerShell resolves relative to ANOTHER
    /// parameter rather than the cwd. Only simple leaf names are extractable.
    leaf_only_path_params: &'static [&'static str],
    /// Number of leading positional arguments that are not paths (e.g.
    /// Invoke-WebRequest's positional `-Uri`).
    positional_skip: usize,
    /// When true the cmdlet only writes to disk if a path parameter is present.
    optional_write: bool,
}

impl CmdletPathConfig {
    fn new(
        operation_type: FileOperationType,
        path_params: &'static [&'static str],
        known_switches: &'static [&'static str],
        known_value_params: &'static [&'static str],
    ) -> Self {
        Self {
            operation_type,
            path_params,
            known_switches,
            known_value_params,
            leaf_only_path_params: &[],
            positional_skip: 0,
            optional_write: false,
        }
    }

    fn with_leaf_only(mut self, params: &'static [&'static str]) -> Self {
        self.leaf_only_path_params = params;
        self
    }

    fn with_positional_skip(mut self, skip: usize) -> Self {
        self.positional_skip = skip;
        self
    }

    fn with_optional_write(mut self) -> Self {
        self.optional_write = true;
        self
    }
}

const PROVIDER_PATH_PARAMS: [&str; 4] = ["-path", "-literalpath", "-pspath", "-lp"];

/// Maps to: CC `pathValidation.ts:124-765#CMDLET_PATH_CONFIG`.
static CMDLET_PATH_CONFIG: LazyLock<HashMap<&'static str, CmdletPathConfig>> =
    LazyLock::new(|| {
        use FileOperationType::{Read, Write};
        HashMap::from([
            // ─── Write/create operations ─────────────────────────────────────────
            (
                "set-content",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-passthru",
                        "-force",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                        "-nonewline",
                        "-asbytestream",
                    ],
                    &[
                        "-value",
                        "-filter",
                        "-include",
                        "-exclude",
                        "-credential",
                        "-encoding",
                        "-stream",
                    ],
                ),
            ),
            (
                "add-content",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-passthru",
                        "-force",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                        "-nonewline",
                        "-asbytestream",
                    ],
                    &[
                        "-value",
                        "-filter",
                        "-include",
                        "-exclude",
                        "-credential",
                        "-encoding",
                        "-stream",
                    ],
                ),
            ),
            (
                "remove-item",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-recurse",
                        "-force",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                    ],
                    &["-filter", "-include", "-exclude", "-credential", "-stream"],
                ),
            ),
            (
                "clear-content",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &["-force", "-whatif", "-confirm", "-usetransaction"],
                    &["-filter", "-include", "-exclude", "-credential", "-stream"],
                ),
            ),
            (
                "out-file",
                CmdletPathConfig::new(
                    Write,
                    // `-Path` is PowerShell's documented alias for `-FilePath`; it
                    // must be here or `Out-File -Path:./x` falls to the unknown
                    // parameter branch and the value is never deny-checked.
                    &["-filepath", "-path", "-literalpath", "-pspath", "-lp"],
                    &[
                        "-append",
                        "-force",
                        "-noclobber",
                        "-nonewline",
                        "-whatif",
                        "-confirm",
                    ],
                    &["-inputobject", "-encoding", "-width"],
                ),
            ),
            (
                "tee-object",
                CmdletPathConfig::new(
                    Write,
                    &["-filepath", "-path", "-literalpath", "-pspath", "-lp"],
                    &["-append"],
                    &["-inputobject", "-variable", "-encoding"],
                ),
            ),
            (
                "export-csv",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-append",
                        "-force",
                        "-noclobber",
                        "-notypeinformation",
                        "-includetypeinformation",
                        "-useculture",
                        "-noheader",
                        "-whatif",
                        "-confirm",
                    ],
                    &[
                        "-inputobject",
                        "-delimiter",
                        "-encoding",
                        "-quotefields",
                        "-usequotes",
                    ],
                ),
            ),
            (
                "export-clixml",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &["-force", "-noclobber", "-whatif", "-confirm"],
                    &["-inputobject", "-depth", "-encoding"],
                ),
            ),
            (
                "new-item",
                // `-Name` is resolved by PowerShell relative to `-Path`, including
                // `..` traversal, while path validation resolves against the cwd.
                // Simple leaf names are extracted; anything path-like forces an ask.
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &["-force", "-whatif", "-confirm", "-usetransaction"],
                    &["-itemtype", "-value", "-credential", "-type"],
                )
                .with_leaf_only(&["-name"]),
            ),
            (
                "copy-item",
                // `-Path` (source) and `-Destination` are both extracted and both
                // validated as writes, which is blunt but strictly safer than
                // extracting neither.
                CmdletPathConfig::new(
                    Write,
                    &["-path", "-literalpath", "-pspath", "-lp", "-destination"],
                    &[
                        "-container",
                        "-force",
                        "-passthru",
                        "-recurse",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                    ],
                    &[
                        "-filter",
                        "-include",
                        "-exclude",
                        "-credential",
                        "-fromsession",
                        "-tosession",
                    ],
                ),
            ),
            (
                "move-item",
                CmdletPathConfig::new(
                    Write,
                    &["-path", "-literalpath", "-pspath", "-lp", "-destination"],
                    &[
                        "-force",
                        "-passthru",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                    ],
                    &["-filter", "-include", "-exclude", "-credential"],
                ),
            ),
            (
                "rename-item",
                // `-NewName` is leaf-only by documentation and Rename-Item rejects
                // `..` in it, so it belongs in known_value_params rather than
                // leaf_only_path_params.
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-force",
                        "-passthru",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                    ],
                    &["-newname", "-credential", "-filter", "-include", "-exclude"],
                ),
            ),
            (
                "set-item",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-force",
                        "-passthru",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                    ],
                    &["-value", "-credential", "-filter", "-include", "-exclude"],
                ),
            ),
            // ─── Read operations ─────────────────────────────────────────────────
            (
                "get-content",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-force",
                        "-usetransaction",
                        "-wait",
                        "-raw",
                        "-asbytestream",
                    ],
                    &[
                        "-readcount",
                        "-totalcount",
                        "-tail",
                        "-first",
                        "-head",
                        "-last",
                        "-filter",
                        "-include",
                        "-exclude",
                        "-credential",
                        "-delimiter",
                        "-encoding",
                        "-stream",
                    ],
                ),
            ),
            (
                "get-childitem",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-recurse",
                        "-force",
                        "-name",
                        "-usetransaction",
                        "-followsymlink",
                        "-directory",
                        "-file",
                        "-hidden",
                        "-readonly",
                        "-system",
                    ],
                    &[
                        "-filter",
                        "-include",
                        "-exclude",
                        "-depth",
                        "-attributes",
                        "-credential",
                    ],
                ),
            ),
            (
                "get-item",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-force", "-usetransaction"],
                    &["-filter", "-include", "-exclude", "-credential", "-stream"],
                ),
            ),
            (
                "get-itemproperty",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-usetransaction"],
                    &["-name", "-filter", "-include", "-exclude", "-credential"],
                ),
            ),
            (
                "get-itempropertyvalue",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-usetransaction"],
                    &["-name", "-filter", "-include", "-exclude", "-credential"],
                ),
            ),
            (
                "get-filehash",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &[],
                    &["-algorithm", "-inputstream"],
                ),
            ),
            (
                "get-acl",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-audit", "-allcentralaccesspolicies", "-usetransaction"],
                    &["-inputobject", "-filter", "-include", "-exclude"],
                ),
            ),
            (
                "format-hex",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-raw"],
                    &["-inputobject", "-encoding", "-count", "-offset"],
                ),
            ),
            (
                "test-path",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-isvalid", "-usetransaction"],
                    &[
                        "-filter",
                        "-include",
                        "-exclude",
                        "-pathtype",
                        "-credential",
                        "-olderthan",
                        "-newerthan",
                    ],
                ),
            ),
            (
                "resolve-path",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-relative", "-usetransaction", "-force"],
                    &["-credential", "-relativebasepath"],
                ),
            ),
            (
                "convert-path",
                CmdletPathConfig::new(Read, &PROVIDER_PATH_PARAMS, &["-usetransaction"], &[]),
            ),
            (
                "select-string",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-simplematch",
                        "-casesensitive",
                        "-quiet",
                        "-list",
                        "-notmatch",
                        "-allmatches",
                        "-noemphasis",
                        "-raw",
                    ],
                    &[
                        "-inputobject",
                        "-pattern",
                        "-include",
                        "-exclude",
                        "-encoding",
                        "-context",
                        "-culture",
                    ],
                ),
            ),
            (
                "set-location",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-passthru", "-usetransaction"],
                    &["-stackname"],
                ),
            ),
            (
                "push-location",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &["-passthru", "-usetransaction"],
                    &["-stackname"],
                ),
            ),
            (
                // Pop-Location has no -Path/-LiteralPath (it pops from the stack),
                // but the entry keeps it passing through path validation gracefully.
                "pop-location",
                CmdletPathConfig::new(
                    Read,
                    &[],
                    &["-passthru", "-usetransaction"],
                    &["-stackname"],
                ),
            ),
            (
                "select-xml",
                CmdletPathConfig::new(
                    Read,
                    &PROVIDER_PATH_PARAMS,
                    &[],
                    &["-xml", "-content", "-xpath", "-namespace"],
                ),
            ),
            (
                "get-winevent",
                CmdletPathConfig::new(
                    Read,
                    // Get-WinEvent only has -Path, no -LiteralPath.
                    &["-path"],
                    &["-force", "-oldest"],
                    &[
                        "-listlog",
                        "-logname",
                        "-listprovider",
                        "-providername",
                        "-maxevents",
                        "-computername",
                        "-credential",
                        "-filterxpath",
                        "-filterxml",
                        "-filterhashtable",
                    ],
                ),
            ),
            (
                "invoke-webrequest",
                // -OutFile is the write target; -InFile is a read source that
                // uploads a local file. Both are path params so Edit deny rules are
                // consulted for exfiltration attempts.
                CmdletPathConfig::new(
                    Write,
                    &["-outfile", "-infile"],
                    &[
                        "-allowinsecureredirect",
                        "-allowunencryptedauthentication",
                        "-disablekeepalive",
                        "-nobodyprogress",
                        "-passthru",
                        "-preservefileauthorizationmetadata",
                        "-resume",
                        "-skipcertificatecheck",
                        "-skipheadervalidation",
                        "-skiphttperrorcheck",
                        "-usebasicparsing",
                        "-usedefaultcredentials",
                    ],
                    &[
                        "-uri",
                        "-method",
                        "-body",
                        "-contenttype",
                        "-headers",
                        "-maximumredirection",
                        "-maximumretrycount",
                        "-proxy",
                        "-proxycredential",
                        "-retryintervalsec",
                        "-sessionvariable",
                        "-timeoutsec",
                        "-token",
                        "-transferencoding",
                        "-useragent",
                        "-websession",
                        "-credential",
                        "-authentication",
                        "-certificate",
                        "-certificatethumbprint",
                        "-form",
                        "-httpversion",
                    ],
                )
                .with_positional_skip(1)
                .with_optional_write(),
            ),
            (
                "invoke-restmethod",
                CmdletPathConfig::new(
                    Write,
                    &["-outfile", "-infile"],
                    &[
                        "-allowinsecureredirect",
                        "-allowunencryptedauthentication",
                        "-disablekeepalive",
                        "-followrellink",
                        "-nobodyprogress",
                        "-passthru",
                        "-preservefileauthorizationmetadata",
                        "-resume",
                        "-skipcertificatecheck",
                        "-skipheadervalidation",
                        "-skiphttperrorcheck",
                        "-usebasicparsing",
                        "-usedefaultcredentials",
                    ],
                    &[
                        "-uri",
                        "-method",
                        "-body",
                        "-contenttype",
                        "-headers",
                        "-maximumfollowrellink",
                        "-maximumredirection",
                        "-maximumretrycount",
                        "-proxy",
                        "-proxycredential",
                        "-responseheaderstvariable",
                        "-retryintervalsec",
                        "-sessionvariable",
                        "-statuscodevariable",
                        "-timeoutsec",
                        "-token",
                        "-transferencoding",
                        "-useragent",
                        "-websession",
                        "-credential",
                        "-authentication",
                        "-certificate",
                        "-certificatethumbprint",
                        "-form",
                        "-httpversion",
                    ],
                )
                .with_positional_skip(1)
                .with_optional_write(),
            ),
            (
                "expand-archive",
                CmdletPathConfig::new(
                    Write,
                    &[
                        "-path",
                        "-literalpath",
                        "-pspath",
                        "-lp",
                        "-destinationpath",
                    ],
                    &["-force", "-passthru", "-whatif", "-confirm"],
                    &[],
                ),
            ),
            (
                "compress-archive",
                CmdletPathConfig::new(
                    Write,
                    &[
                        "-path",
                        "-literalpath",
                        "-pspath",
                        "-lp",
                        "-destinationpath",
                    ],
                    &["-force", "-update", "-passthru", "-whatif", "-confirm"],
                    &["-compressionlevel"],
                ),
            ),
            (
                "set-itemproperty",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-passthru",
                        "-force",
                        "-whatif",
                        "-confirm",
                        "-usetransaction",
                    ],
                    &[
                        "-name",
                        "-value",
                        "-type",
                        "-filter",
                        "-include",
                        "-exclude",
                        "-credential",
                        "-inputobject",
                    ],
                ),
            ),
            (
                "new-itemproperty",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &["-force", "-whatif", "-confirm", "-usetransaction"],
                    &[
                        "-name",
                        "-value",
                        "-propertytype",
                        "-type",
                        "-filter",
                        "-include",
                        "-exclude",
                        "-credential",
                    ],
                ),
            ),
            (
                "remove-itemproperty",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &["-force", "-whatif", "-confirm", "-usetransaction"],
                    &["-name", "-filter", "-include", "-exclude", "-credential"],
                ),
            ),
            (
                "clear-item",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &["-force", "-whatif", "-confirm", "-usetransaction"],
                    &["-filter", "-include", "-exclude", "-credential"],
                ),
            ),
            (
                "export-alias",
                CmdletPathConfig::new(
                    Write,
                    &PROVIDER_PATH_PARAMS,
                    &[
                        "-append",
                        "-force",
                        "-noclobber",
                        "-passthru",
                        "-whatif",
                        "-confirm",
                    ],
                    &["-name", "-description", "-scope", "-as"],
                ),
            ),
        ])
    });

/// Maps to: CC `pathValidation.ts:772-782#matchesParam`.
///
/// Accounts for PowerShell's prefix matching (`-Lit` matches `-LiteralPath`).
fn matches_param(param_lower: &str, param_list: &[&str]) -> bool {
    param_list.iter().any(|candidate| {
        *candidate == param_lower
            || (param_lower.chars().count() > 1 && candidate.starts_with(param_lower))
    })
}

/// Maps to: CC `pathValidation.ts:793-803#hasComplexColonValue`.
///
/// True when a colon-syntax value contains expression constructs that mask the
/// real runtime path. The outer `CommandParameterAst` 'Parameter' element type
/// hides these from the AST walk, so they must be detected textually.
fn has_complex_colon_value(raw_value: &str) -> bool {
    raw_value.contains(',')
        || raw_value.starts_with('(')
        || raw_value.starts_with('[')
        || raw_value.contains('`')
        || raw_value.contains("@(")
        || raw_value.starts_with("@{")
        || raw_value.contains('$')
}

/// CC's `expandTilde` in `pathValidation.ts:820-829` accepts `~\` on every
/// platform; the shared Rust helper gates that form on Windows.
fn expand_tilde_any_separator(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~\\") {
        return expand_tilde(&format!("~/{rest}"));
    }
    expand_tilde(path)
}

fn strip_edge_quotes(path: &str) -> String {
    let trimmed = path.strip_prefix(['\'', '"']).unwrap_or(path);
    trimmed
        .strip_suffix(['\'', '"'])
        .unwrap_or(trimmed)
        .to_string()
}

/// Maps to: CC `tools/PowerShellTool/pathValidation.ts:840-846`
/// `isDangerousRemovalRawPath`.
///
/// `safeResolvePath` canonicalizes in ways that defeat `isDangerousRemovalPath`
/// (on Windows `/` becomes `C:\`; on macOS a `/var` home is rewritten to
/// `/private/var`), so the raw user-provided form is checked first.
pub fn is_dangerous_removal_raw_path(file_path: &str) -> bool {
    let expanded = expand_tilde_any_separator(&strip_edge_quotes(file_path)).replace('\\', "/");
    is_dangerous_removal_path(&expanded)
}

/// Maps to: CC `tools/PowerShellTool/pathValidation.ts:848-857`
/// `dangerousRemovalDeny`.
pub fn dangerous_removal_deny(path: &str) -> PermissionResult {
    PermissionResult::Deny {
        message: format!(
            "Remove-Item on system path '{path}' is blocked. This path is protected from removal."
        ),
        decision_reason: PermissionDecisionReason::Other {
            reason: "Removal targets a protected system path".to_string(),
        },
        tool_use_id: None,
    }
}

fn normalize_path_string(path: &str) -> String {
    let mut normalized = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized.display().to_string()
}

/// Maps to: CC `path.resolve(cwd, filePath)` as used throughout this module.
fn absolute_path(path: &str, cwd: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        normalize_path_string(path)
    } else {
        normalize_path_string(&Path::new(cwd).join(candidate).display().to_string())
    }
}

/// Maps to: CC `safeResolvePath(getFsImplementation(), absolutePath)`.
fn resolve_path(path: &str) -> (String, bool) {
    let resolved = safe_resolve_path(get_fs_implementation().as_ref(), Path::new(path));
    (
        resolved.resolved_path.display().to_string(),
        resolved.is_canonical,
    )
}

/// Maps to: CC `utils/path.ts#containsPathTraversal`.
fn contains_path_traversal(path: &str) -> bool {
    path.split(['/', '\\']).any(|part| part == "..")
}

/// Maps to: CC `pathValidation.ts:1266-1278#getGlobBaseDirectory`.
///
/// Keeps the trailing separator, unlike the shared BashTool helper.
fn get_glob_base_directory(file_path: &str) -> String {
    let Some(glob_index) = file_path.find(GLOB_PATTERN_CHARS) else {
        return file_path.to_string();
    };
    let before_glob = &file_path[..glob_index];
    let Some(last_sep_index) = before_glob.rfind(['/', '\\']) else {
        return ".".to_string();
    };
    let base = &before_glob[..last_sep_index + 1];
    if base.is_empty() {
        "/".to_string()
    } else {
        base.to_string()
    }
}

/// Maps to: CC `pathValidation.ts:984-1008#checkDenyRuleForGuessedPath`.
///
/// Best-effort deny check for paths obscured by `::` or backtick syntax. Only
/// ever matches deny rules; it never auto-allows.
fn check_deny_rule_for_guessed_path(
    stripped_path: &str,
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    operation_type: FileOperationType,
) -> Option<(String, PermissionRule)> {
    if stripped_path.is_empty() || stripped_path.contains('\0') {
        return None;
    }
    // A backtick can sit in front of the tilde, so tilde expansion is re-run
    // on the stripped guess.
    let absolute = absolute_path(&expand_tilde_any_separator(stripped_path), cwd);
    let (resolved_path, _) = resolve_path(&absolute);
    let permission_type = if operation_type == FileOperationType::Read {
        FilePermissionType::Read
    } else {
        FilePermissionType::Edit
    };
    matching_rule_for_input(
        &resolved_path,
        tool_permission_context,
        permission_type,
        PermissionBehavior::Deny,
        Path::new(cwd),
    )
    .map(|rule| (resolved_path, rule))
}

struct ResolvedPathCheck {
    allowed: bool,
    resolved_path: String,
    decision_reason: Option<PermissionDecisionReason>,
}

fn blocked(resolved_path: String, reason: &str) -> ResolvedPathCheck {
    ResolvedPathCheck {
        allowed: false,
        resolved_path,
        decision_reason: Some(PermissionDecisionReason::Other {
            reason: reason.to_string(),
        }),
    }
}

/// Maps to: CC `pathValidation.ts:1013-1264#validatePath`.
fn validate_path(
    file_path: &str,
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    operation_type: FileOperationType,
) -> ResolvedPathCheck {
    let clean_path = expand_tilde_any_separator(&strip_edge_quotes(file_path));

    // SECURITY: PowerShell Core normalizes backslashes to forward slashes on
    // every platform, so traversal patterns like `dir\..\..\etc\shadow` must be
    // normalized before resolution.
    let normalized_path = clean_path.replace('\\', "/");

    // SECURITY: backtick is PowerShell's escape character. It is a no-op in many
    // positions but defeats path checks like `isAbsolute`. Redirection targets
    // use raw extent text, which preserves the escapes.
    if normalized_path.contains('`') {
        let backtick_stripped = normalized_path.replace('`', "");
        if let Some((resolved_path, rule)) = check_deny_rule_for_guessed_path(
            &backtick_stripped,
            cwd,
            tool_permission_context,
            operation_type,
        ) {
            return ResolvedPathCheck {
                allowed: false,
                resolved_path,
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            };
        }
        return blocked(
            normalized_path,
            "Backtick escape characters in paths cannot be statically validated and require manual approval",
        );
    }

    // SECURITY: block module-qualified provider paths. PowerShell resolves
    // `Microsoft.PowerShell.Core\FileSystem::/etc/passwd` to `/etc/passwd`, and
    // the `::` separator does not match the simple provider-prefix regex.
    if let Some(provider_index) = normalized_path.find("::") {
        let after_provider = &normalized_path[provider_index + 2..];
        if let Some((resolved_path, rule)) = check_deny_rule_for_guessed_path(
            after_provider,
            cwd,
            tool_permission_context,
            operation_type,
        ) {
            return ResolvedPathCheck {
                allowed: false,
                resolved_path,
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            };
        }
        return blocked(
            normalized_path,
            "Module-qualified provider paths (::) cannot be statically validated and require manual approval",
        );
    }

    // SECURITY: block UNC paths — they can trigger network requests and leak
    // NTLM/Kerberos credentials.
    let lower = normalized_path.to_lowercase();
    if normalized_path.starts_with("//") || lower.contains("davwwwroot") || lower.contains("@ssl@")
    {
        return blocked(
            normalized_path,
            "UNC paths are blocked because they can trigger network requests and credential leakage",
        );
    }

    if normalized_path.contains('$') || normalized_path.contains('%') {
        return blocked(
            normalized_path,
            "Variable expansion syntax in paths requires manual approval",
        );
    }

    // SECURITY: block non-filesystem provider paths (env:, HKLM:, alias:, ...).
    // On POSIX any `<letters-or-digits>:` prefix is a PSDrive, since
    // single-letter drive paths have no native meaning there; on Windows 2+
    // characters are required so native drive letters pass through.
    if has_provider_path_prefix(&normalized_path) {
        let reason = format!(
            "Path '{normalized_path}' uses a non-filesystem provider and requires manual approval"
        );
        return blocked(normalized_path, &reason);
    }

    if normalized_path.contains(GLOB_PATTERN_CHARS) {
        if matches!(
            operation_type,
            FileOperationType::Write | FileOperationType::Create
        ) {
            return blocked(
                normalized_path,
                "Glob patterns are not allowed in write operations. Please specify an exact file path.",
            );
        }

        // For reads with traversal (e.g. `/project/*/../../../etc/shadow`),
        // resolve the full path including the glob characters so patterns that
        // escape the working directory via `..` are caught.
        if contains_path_traversal(&normalized_path) {
            let (resolved_path, is_canonical) = resolve_path(&absolute_path(&normalized_path, cwd));
            let paths = is_canonical.then(|| vec![resolved_path.clone()]);
            let result = is_path_allowed(
                &resolved_path,
                tool_permission_context,
                operation_type,
                paths.as_deref(),
            );
            return ResolvedPathCheck {
                allowed: result.allowed,
                resolved_path,
                decision_reason: result.decision_reason,
            };
        }

        // SECURITY: glob patterns for reads cannot be statically validated —
        // only the base directory is realpathed, so anything the glob expands
        // to (including symlinks) is never examined. Deny rules on the base
        // directory still fire; otherwise force an ask.
        let base_path = get_glob_base_directory(&normalized_path);
        let (resolved_path, _) = resolve_path(&absolute_path(&base_path, cwd));
        let permission_type = if operation_type == FileOperationType::Read {
            FilePermissionType::Read
        } else {
            FilePermissionType::Edit
        };
        if let Some(rule) = matching_rule_for_input(
            &resolved_path,
            tool_permission_context,
            permission_type,
            PermissionBehavior::Deny,
            Path::new(cwd),
        ) {
            return ResolvedPathCheck {
                allowed: false,
                resolved_path,
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            };
        }
        return blocked(
            resolved_path,
            "Glob patterns in paths cannot be statically validated — symlinks inside the glob expansion are not examined. Requires manual approval.",
        );
    }

    let (resolved_path, is_canonical) = resolve_path(&absolute_path(&normalized_path, cwd));
    let paths = is_canonical.then(|| vec![resolved_path.clone()]);
    let result = is_path_allowed(
        &resolved_path,
        tool_permission_context,
        operation_type,
        paths.as_deref(),
    );
    ResolvedPathCheck {
        allowed: result.allowed,
        resolved_path,
        decision_reason: result.decision_reason,
    }
}

/// `/^[a-z0-9]{2,}:/i` on Windows, `/^[a-z0-9]+:/i` on POSIX.
fn has_provider_path_prefix(path: &str) -> bool {
    let prefix_len = path
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .count();
    if prefix_len == 0 || path.chars().nth(prefix_len) != Some(':') {
        return false;
    }
    if cfg!(target_os = "windows") {
        prefix_len >= 2
    } else {
        true
    }
}

/// Maps to: CC `pathValidation.ts:1294#SAFE_PATH_ELEMENT_TYPES`.
///
/// Only element types with statically-known string values are safe for path
/// extraction. Variable and ExpandableString have runtime-determined values,
/// so excluding them here fails safe at the earliest gate.
fn is_safe_path_element_type(element_type: CommandElementType) -> bool {
    matches!(
        element_type,
        CommandElementType::StringConstant | CommandElementType::Parameter
    )
}

struct ExtractedPaths {
    paths: Vec<String>,
    operation_type: FileOperationType,
    has_unvalidatable_path_arg: bool,
    optional_write: bool,
}

/// Maps to: CC `pathValidation.ts:1304-1508#extractPathsFromCommand`.
fn extract_paths_from_command(cmd: &ParsedCommandElement) -> ExtractedPaths {
    let canonical = resolve_to_canonical(&cmd.name);
    let Some(config) = CMDLET_PATH_CONFIG.get(canonical.as_str()) else {
        return ExtractedPaths {
            paths: Vec::new(),
            operation_type: FileOperationType::Read,
            has_unvalidatable_path_arg: false,
            optional_write: false,
        };
    };

    let switch_params: Vec<&str> = config
        .known_switches
        .iter()
        .copied()
        .chain(COMMON_SWITCHES)
        .collect();
    let value_params: Vec<&str> = config
        .known_value_params
        .iter()
        .copied()
        .chain(COMMON_VALUE_PARAMS)
        .collect();

    let mut paths: Vec<String> = Vec::new();
    let args = &cmd.args;
    let element_types = cmd.element_types.as_ref();
    let mut has_unvalidatable_path_arg = false;
    let mut positionals_seen = 0usize;

    // elementTypes[0] is the command name; elementTypes[i+1] tracks args[i].
    let element_type_at = |arg_index: usize| -> Option<CommandElementType> {
        element_types.and_then(|types| types.get(arg_index + 1).copied())
    };
    let check_arg_element_type = |arg_index: usize, flag: &mut bool| {
        if let Some(element_type) = element_type_at(arg_index) {
            if !is_safe_path_element_type(element_type) {
                *flag = true;
            }
        }
    };

    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg.is_empty() {
            index += 1;
            continue;
        }

        // SECURITY: use elementTypes as ground truth. PowerShell's tokenizer
        // accepts en-dash/em-dash/horizontal-bar as parameter prefixes, which a
        // raw `starts_with('-')` check misses, and it correctly rejects a
        // quoted "-Include" (a StringConstant, not a parameter).
        if is_powershell_parameter(arg, element_type_at(index)) {
            // Handle colon syntax (`-Path:C:\secret`) and normalize a Unicode
            // dash to ASCII, since the param tables are stored with `-`.
            let characters: Vec<char> = arg.chars().collect();
            let colon_index = characters
                .iter()
                .enumerate()
                .skip(1)
                .find_map(|(position, character)| (*character == ':').then_some(position));
            let param_name: String = std::iter::once('-')
                .chain(
                    characters[1..colon_index.unwrap_or(characters.len())]
                        .iter()
                        .copied(),
                )
                .collect();
            let param_lower = param_name.to_lowercase();
            let colon_value =
                colon_index.map(|position| characters[position + 1..].iter().collect::<String>());

            if matches_param(&param_lower, config.path_params) {
                let mut value: Option<String> = None;
                if let Some(raw_value) = colon_value.as_ref() {
                    // SECURITY: comma-separated values (`-Path:safe.txt,/etc/passwd`)
                    // produce an ArrayLiteralExpressionAst inside the
                    // CommandParameterAst. PowerShell writes to every path while
                    // we only see a single string.
                    if has_complex_colon_value(raw_value) {
                        has_unvalidatable_path_arg = true;
                    } else {
                        value = Some(raw_value.clone());
                    }
                } else if let Some(next_value) = args.get(index + 1) {
                    if !is_powershell_parameter(next_value, element_type_at(index + 1)) {
                        value = Some(next_value.clone());
                        check_arg_element_type(index + 1, &mut has_unvalidatable_path_arg);
                        index += 1;
                    }
                }
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    paths.push(value);
                }
            } else if matches_param(&param_lower, config.leaf_only_path_params) {
                // Leaf-only path parameter (e.g. `New-Item -Name`). PowerShell
                // resolves it relative to another parameter, not the cwd, so
                // only simple leaf filenames are extractable.
                let mut value: Option<String> = None;
                if let Some(raw_value) = colon_value.as_ref() {
                    if has_complex_colon_value(raw_value) {
                        has_unvalidatable_path_arg = true;
                    } else {
                        value = Some(raw_value.clone());
                    }
                } else if let Some(next_value) = args.get(index + 1) {
                    if !is_powershell_parameter(next_value, element_type_at(index + 1)) {
                        value = Some(next_value.clone());
                        check_arg_element_type(index + 1, &mut has_unvalidatable_path_arg);
                        index += 1;
                    }
                }
                if let Some(value) = value {
                    if value.contains('/') || value.contains('\\') || value == "." || value == ".."
                    {
                        has_unvalidatable_path_arg = true;
                    } else {
                        paths.push(value);
                    }
                }
            } else if matches_param(&param_lower, &switch_params) {
                // Known switch parameter — takes no value, so the next arg is
                // not consumed. Colon syntax on a switch (`-Confirm:$false`) is
                // self-contained in one token and correctly falls through here.
            } else if matches_param(&param_lower, &value_params) {
                // Known value-taking non-path parameter. Consume its value and
                // check the element type, but do not validate it as a path: a
                // Variable elementType means the runtime value is unknowable.
                if let Some(raw_value) = colon_value.as_ref() {
                    if has_complex_colon_value(raw_value) {
                        has_unvalidatable_path_arg = true;
                    }
                } else if let Some(next_value) = args.get(index + 1) {
                    if !is_powershell_parameter(next_value, element_type_at(index + 1)) {
                        check_arg_element_type(index + 1, &mut has_unvalidatable_path_arg);
                        index += 1;
                    }
                }
            } else {
                // Unknown parameter: rather than guess whether it is a switch
                // (risking swallowing a positional path) or takes a value
                // (risking the same), flag the whole command as unvalidatable.
                has_unvalidatable_path_arg = true;
                // The bound value of a colon-syntax unknown parameter may still
                // be a filesystem path, so it is extracted for deny-rule
                // matching; without this the value stays trapped in the single
                // token and a deny silently downgrades to an ask.
                if let Some(raw_value) = colon_value.as_ref() {
                    if !has_complex_colon_value(raw_value) {
                        paths.push(raw_value.clone());
                    }
                }
            }
            index += 1;
            continue;
        }

        // Positional arguments are paths, except for the leading non-path
        // positionals declared by `positional_skip` (e.g. iwr's `-Uri`).
        if positionals_seen < config.positional_skip {
            positionals_seen += 1;
            index += 1;
            continue;
        }
        positionals_seen += 1;
        check_arg_element_type(index, &mut has_unvalidatable_path_arg);
        paths.push(arg.clone());
        index += 1;
    }

    ExtractedPaths {
        paths,
        operation_type: config.operation_type,
        has_unvalidatable_path_arg,
        optional_write: config.optional_write,
    }
}

fn passthrough(message: &str) -> PermissionResult {
    PermissionResult::Passthrough {
        message: message.to_string(),
        decision_reason: None,
        suggestions: Vec::new(),
        blocked_path: None,
        pending_classifier_check: None,
    }
}

fn ask(
    message: String,
    decision_reason: Option<PermissionDecisionReason>,
    blocked_path: Option<String>,
    suggestions: Vec<PermissionUpdate>,
) -> PermissionResult {
    PermissionResult::Ask {
        message,
        updated_input: None,
        decision_reason,
        suggestions,
        blocked_path,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

/// Maps to: CC `pathValidation.ts:1528-1567#checkPathConstraints`.
///
/// `compound_command_has_cd` reports whether the full compound command contains
/// a cwd-changing cmdlet. When true, relative paths in ANY statement cannot be
/// trusted: PowerShell executes statements sequentially and a `cd` in statement
/// N changes the cwd for statement N+1, while this validator resolves every
/// path against the stale process cwd.
///
/// Returns `ask` when a path command reaches outside the allowed directories,
/// `deny` when a deny rule blocks a path, and `passthrough` when no path
/// commands were found or every path validated.
pub fn check_path_constraints(
    parsed: &ParsedPowerShellCommand,
    tool_permission_context: &ToolPermissionContext,
    compound_command_has_cd: bool,
) -> PermissionResult {
    if !parsed.valid {
        return passthrough("Cannot validate paths for unparsed command");
    }

    // SECURITY: two-pass approach — check ALL statements so deny rules always
    // take precedence over ask. Otherwise an ask on statement 1 could return
    // before statement 2 is checked for deny rules, letting the user approve a
    // command that includes a denied path.
    let mut first_ask: Option<PermissionResult> = None;

    for statement in &parsed.statements {
        let result = check_path_constraints_for_statement(
            statement,
            tool_permission_context,
            compound_command_has_cd,
        );
        match result {
            PermissionResult::Deny { .. } => return result,
            PermissionResult::Ask { .. } if first_ask.is_none() => first_ask = Some(result),
            _ => {}
        }
    }

    first_ask.unwrap_or_else(|| passthrough("All path constraints validated successfully"))
}

/// Maps to: CC `pathValidation.ts:1569-2049#checkPathConstraintsForStatement`.
fn check_path_constraints_for_statement(
    statement: &ParsedStatement,
    tool_permission_context: &ToolPermissionContext,
    compound_command_has_cd: bool,
) -> PermissionResult {
    let cwd = cwd();
    let mut first_ask: Option<PermissionResult> = None;

    // SECURITY: BashTool parity — block path operations in compound commands
    // that change the working directory. `Set-Location ./.claude; Set-Content
    // ./settings.json '...'` looks like a write to /project/settings.json to
    // this validator while PowerShell writes /project/.claude/settings.json.
    //
    // Unlike BashTool, reads are blocked too: `Set-Location ~; Get-Content
    // ./.ssh/id_rsa` would otherwise bypass a `Read(~/.ssh/**)` deny rule.
    // Deny-rule matching below still runs (via first_ask, not an early return)
    // so explicit deny rules on the stale-resolved path are honored.
    if compound_command_has_cd {
        first_ask = Some(ask(
            "Compound command changes working directory (Set-Location/Push-Location/Pop-Location/New-PSDrive) — relative paths cannot be validated against the original cwd and require manual approval".to_string(),
            Some(PermissionDecisionReason::Other {
                reason: "Compound command contains cd with path operation — manual approval required to prevent path resolution bypass".to_string(),
            }),
            None,
            Vec::new(),
        ));
    }

    // SECURITY: a non-CommandAst pipeline element (string literal, variable,
    // array expression) is piped to downstream cmdlets, often binding to -Path.
    // `'/etc/passwd' | Remove-Item` gives Remove-Item no explicit args, so path
    // extraction returns nothing and the command would passthrough.
    let mut has_expression_pipeline_source = false;
    let mut pipeline_source_text: Option<String> = None;

    for cmd in &statement.commands {
        if cmd.element_type != Some(PipelineElementType::CommandAst) {
            has_expression_pipeline_source = true;
            pipeline_source_text = Some(cmd.text.clone());
            continue;
        }

        let extracted = extract_paths_from_command(cmd);
        let canonical = resolve_to_canonical(&cmd.name);

        if has_expression_pipeline_source {
            // Before falling back to ask, check whether the pipeline-source
            // text matches a deny rule, so `'.git/hooks/pre-commit' |
            // Remove-Item` denies rather than asks under `Edit(.git/**)`.
            if let Some(source_text) = pipeline_source_text.as_ref() {
                if let Some((resolved_path, rule)) = check_deny_rule_for_guessed_path(
                    &strip_edge_quotes(source_text),
                    &cwd,
                    tool_permission_context,
                    extracted.operation_type,
                ) {
                    return PermissionResult::Deny {
                        message: format!(
                            "{canonical} targeting '{resolved_path}' was blocked by a deny rule"
                        ),
                        decision_reason: PermissionDecisionReason::Rule { rule },
                        tool_use_id: None,
                    };
                }
            }
            first_ask.get_or_insert_with(|| {
                ask(
                    format!("{canonical} receives its path from a pipeline expression source that cannot be statically validated and requires manual approval"),
                    None,
                    None,
                    Vec::new(),
                )
            });
        }

        // SECURITY: array literals, subexpressions, and other complex argument
        // types cannot be statically validated. `-Path ./safe.txt, /etc/passwd`
        // produces a single 'Other' element whose combined text may resolve
        // inside the cwd while PowerShell writes to every path in the array.
        if extracted.has_unvalidatable_path_arg {
            first_ask.get_or_insert_with(|| {
                ask(
                    format!("{canonical} uses a parameter or complex path expression (array literal, subexpression, unknown parameter, etc.) that cannot be statically validated and requires manual approval"),
                    None,
                    None,
                    Vec::new(),
                )
            });
        }

        // SECURITY: a configured write cmdlet that extracted zero paths either
        // has no arguments at all or hid its path among them. Either way there
        // is no validated target, so ask. Reads, pop-location, and optionalWrite
        // cmdlets without their output parameter are exempt.
        if extracted.operation_type != FileOperationType::Read
            && !extracted.optional_write
            && extracted.paths.is_empty()
            && CMDLET_PATH_CONFIG.contains_key(canonical.as_str())
        {
            first_ask.get_or_insert_with(|| {
                ask(
                    format!("{canonical} is a write operation but no target path could be determined; requires manual approval"),
                    None,
                    None,
                    Vec::new(),
                )
            });
            continue;
        }

        // SECURITY: bash-parity hard deny for removal cmdlets on
        // system-critical paths. The user cannot approve deleting `/` or `~`.
        let is_removal = canonical == "remove-item";

        for file_path in &extracted.paths {
            // Check the RAW path first: `safe_resolve_path` canonicalizes `/`
            // to `C:\` on Windows and `/var/...` to `/private/var/...` on macOS,
            // which defeats the string comparisons in the removal check.
            if is_removal && is_dangerous_removal_raw_path(file_path) {
                return dangerous_removal_deny(file_path);
            }

            let check = validate_path(
                file_path,
                &cwd,
                tool_permission_context,
                extracted.operation_type,
            );

            // Also check the resolved path — catches symlinks that point at a
            // protected location.
            if is_removal && is_dangerous_removal_path(&check.resolved_path) {
                return dangerous_removal_deny(&check.resolved_path);
            }

            if !check.allowed {
                match blocked_path_outcome(
                    &canonical,
                    &check,
                    tool_permission_context,
                    extracted.operation_type,
                ) {
                    BlockedOutcome::Deny(result) => return result,
                    BlockedOutcome::Ask(result) => {
                        first_ask.get_or_insert(result);
                    }
                }
            }
        }
    }

    // Also check nested commands from control flow.
    if let Some(nested_commands) = statement.nested_commands.as_ref() {
        for cmd in nested_commands {
            let extracted = extract_paths_from_command(cmd);
            let canonical = resolve_to_canonical(&cmd.name);

            if extracted.has_unvalidatable_path_arg {
                first_ask.get_or_insert_with(|| {
                    ask(
                        format!("{canonical} uses a parameter or complex path expression (array literal, subexpression, unknown parameter, etc.) that cannot be statically validated and requires manual approval"),
                        None,
                        None,
                        Vec::new(),
                    )
                });
            }

            if extracted.operation_type != FileOperationType::Read
                && !extracted.optional_write
                && extracted.paths.is_empty()
                && CMDLET_PATH_CONFIG.contains_key(canonical.as_str())
            {
                first_ask.get_or_insert_with(|| {
                    ask(
                        format!("{canonical} is a write operation but no target path could be determined; requires manual approval"),
                        None,
                        None,
                        Vec::new(),
                    )
                });
                continue;
            }

            // SECURITY: mirror the main-loop removal hard-deny. Without it,
            // `if ($true) { Remove-Item / }` routes through nestedCommands and
            // downgrades deny to ask.
            let is_removal = canonical == "remove-item";

            for file_path in &extracted.paths {
                if is_removal && is_dangerous_removal_raw_path(file_path) {
                    return dangerous_removal_deny(file_path);
                }

                let check = validate_path(
                    file_path,
                    &cwd,
                    tool_permission_context,
                    extracted.operation_type,
                );

                if is_removal && is_dangerous_removal_path(&check.resolved_path) {
                    return dangerous_removal_deny(&check.resolved_path);
                }

                if !check.allowed {
                    match blocked_path_outcome(
                        &canonical,
                        &check,
                        tool_permission_context,
                        extracted.operation_type,
                    ) {
                        BlockedOutcome::Deny(result) => return result,
                        BlockedOutcome::Ask(result) => {
                            first_ask.get_or_insert(result);
                        }
                    }
                }
            }

            // Placed after the path loop so specific asks (with blockedPath and
            // suggestions) win.
            if has_expression_pipeline_source {
                first_ask.get_or_insert_with(|| {
                    ask(
                        format!("{canonical} appears inside a control-flow or chain statement where piped expression sources cannot be statically validated and requires manual approval"),
                        None,
                        None,
                        Vec::new(),
                    )
                });
            }
        }
    }

    // Check redirections on nested commands (e.g. from && / || chains).
    if let Some(nested_commands) = statement.nested_commands.as_ref() {
        for cmd in nested_commands {
            let Some(redirections) = cmd.redirections.as_ref() else {
                continue;
            };
            for redirection in redirections {
                if let Some(result) = check_redirection_target(
                    redirection.is_merging,
                    &redirection.target,
                    &cwd,
                    tool_permission_context,
                    &mut first_ask,
                ) {
                    return result;
                }
            }
        }
    }

    // Check file redirections.
    for redirection in &statement.redirections {
        if let Some(result) = check_redirection_target(
            redirection.is_merging,
            &redirection.target,
            &cwd,
            tool_permission_context,
            &mut first_ask,
        ) {
            return result;
        }
    }

    first_ask.unwrap_or_else(|| passthrough("All path constraints validated successfully"))
}

enum BlockedOutcome {
    Deny(PermissionResult),
    Ask(PermissionResult),
}

fn blocked_path_outcome(
    canonical: &str,
    check: &ResolvedPathCheck,
    tool_permission_context: &ToolPermissionContext,
    operation_type: FileOperationType,
) -> BlockedOutcome {
    let working_dirs = all_working_directories(tool_permission_context);
    let dir_list = format_directory_list(&working_dirs);
    let resolved_path = &check.resolved_path;
    let message = match check.decision_reason.as_ref() {
        Some(PermissionDecisionReason::Other { reason })
        | Some(PermissionDecisionReason::SafetyCheck { reason, .. }) => reason.clone(),
        _ => format!(
            "{canonical} targeting '{resolved_path}' was blocked. For security, Claude Code may only access files in the allowed working directories for this session: {dir_list}."
        ),
    };

    if let Some(reason @ PermissionDecisionReason::Rule { .. }) = check.decision_reason.as_ref() {
        return BlockedOutcome::Deny(PermissionResult::Deny {
            message,
            decision_reason: reason.clone(),
            tool_use_id: None,
        });
    }

    let mut suggestions: Vec<PermissionUpdate> = Vec::new();
    if !resolved_path.is_empty() {
        let directory = crate::utils::path::get_directory_for_path(resolved_path);
        if operation_type == FileOperationType::Read {
            if let Some(suggestion) =
                create_read_rule_suggestion(&directory, PermissionUpdateDestination::Session)
            {
                suggestions.push(suggestion);
            }
        } else {
            suggestions.push(PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: vec![directory],
            });
        }
    }

    if matches!(
        operation_type,
        FileOperationType::Write | FileOperationType::Create
    ) {
        suggestions.push(PermissionUpdate::SetMode {
            destination: PermissionUpdateDestination::Session,
            mode: PermissionMode::AcceptEdits,
        });
    }

    BlockedOutcome::Ask(ask(
        message,
        check.decision_reason.clone(),
        Some(resolved_path.clone()),
        suggestions,
    ))
}

fn check_redirection_target(
    is_merging: bool,
    target: &str,
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    first_ask: &mut Option<PermissionResult>,
) -> Option<PermissionResult> {
    if is_merging || target.is_empty() || is_null_redirection_target(target) {
        return None;
    }

    let check = validate_path(
        target,
        cwd,
        tool_permission_context,
        FileOperationType::Create,
    );
    if check.allowed {
        return None;
    }

    let working_dirs = all_working_directories(tool_permission_context);
    let dir_list = format_directory_list(&working_dirs);
    let resolved_path = &check.resolved_path;
    let message = match check.decision_reason.as_ref() {
        Some(PermissionDecisionReason::Other { reason })
        | Some(PermissionDecisionReason::SafetyCheck { reason, .. }) => reason.clone(),
        _ => format!(
            "Output redirection to '{resolved_path}' was blocked. For security, Claude Code may only write to files in the allowed working directories for this session: {dir_list}."
        ),
    };

    if let Some(reason @ PermissionDecisionReason::Rule { .. }) = check.decision_reason.as_ref() {
        return Some(PermissionResult::Deny {
            message,
            decision_reason: reason.clone(),
            tool_use_id: None,
        });
    }

    first_ask.get_or_insert_with(|| {
        ask(
            message,
            check.decision_reason.clone(),
            Some(resolved_path.clone()),
            vec![PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: vec![crate::utils::path::get_directory_for_path(resolved_path)],
            }],
        )
    });
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
    use crate::utils::powershell::parser::CommandNameType;
    use crate::utils::test_env::PinnedProjectDir;

    fn command(
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
            text: format!("{name} {}", args.join(" ")),
            element_types: Some(types),
            children: None,
            redirections: None,
        }
    }

    fn pipeline(commands: Vec<ParsedCommandElement>) -> ParsedPowerShellCommand {
        ParsedPowerShellCommand {
            valid: true,
            errors: Vec::new(),
            statements: vec![ParsedStatement {
                statement_type: crate::utils::powershell::parser::StatementType::PipelineAst,
                commands,
                redirections: Vec::new(),
                text: String::new(),
                nested_commands: None,
                security_patterns: None,
            }],
            variables: Vec::new(),
            has_stop_parsing: false,
            original_command: String::new(),
            type_literals: Vec::new(),
            has_using_statements: false,
            has_script_requirements: false,
        }
    }

    fn literals(count: usize) -> Vec<CommandElementType> {
        vec![CommandElementType::StringConstant; count]
    }

    #[test]
    fn unparsed_commands_cannot_be_path_validated() {
        let parsed = ParsedPowerShellCommand {
            valid: false,
            ..pipeline(Vec::new())
        };
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Cannot validate paths for unparsed command"
        ));
    }

    #[test]
    fn writes_inside_the_working_directory_pass_in_accept_edits() {
        // "Inside the working directory" is judged against the project dir.
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let context = ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        };
        let parsed = pipeline(vec![command(
            "Set-Content",
            &["./target/parity.txt", "-Value", "hi"],
            &[
                CommandElementType::StringConstant,
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
            ],
        )]);
        assert!(matches!(
            check_path_constraints(&parsed, &context, false),
            PermissionResult::Passthrough { .. }
        ));
    }

    #[test]
    fn writes_outside_the_working_directory_are_asked_with_directory_suggestions() {
        let parsed = pipeline(vec![command(
            "Set-Content",
            &["/etc/cometix-parity.txt"],
            &literals(1),
        )]);
        match check_path_constraints(&parsed, &ToolPermissionContext::default(), false) {
            PermissionResult::Ask {
                message,
                blocked_path,
                suggestions,
                ..
            } => {
                assert!(message.starts_with("set-content targeting '"), "{message}");
                assert!(message.contains("allowed working directories for this session"));
                assert!(blocked_path.is_some());
                assert!(
                    suggestions
                        .iter()
                        .any(|update| matches!(update, PermissionUpdate::SetMode { .. }))
                );
            }
            other => panic!("expected ask, got {other:?}"),
        }
    }

    #[test]
    fn deny_rules_win_over_asks_from_earlier_statements() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Edit",
                Some("Cargo.toml".to_string()),
            )],
        );
        let mut parsed = pipeline(vec![command(
            "Set-Content",
            &["/etc/cometix-parity.txt"],
            &literals(1),
        )]);
        parsed.statements.push(ParsedStatement {
            statement_type: crate::utils::powershell::parser::StatementType::PipelineAst,
            commands: vec![command("Set-Content", &["Cargo.toml"], &literals(1))],
            redirections: Vec::new(),
            text: String::new(),
            nested_commands: None,
            security_patterns: None,
        });
        assert!(matches!(
            check_path_constraints(&parsed, &context, false),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn dangerous_removals_hard_deny_before_any_rule_lookup() {
        for path in ["/", "~", "/etc", "C:\\Windows"] {
            let parsed = pipeline(vec![command("Remove-Item", &[path], &literals(1))]);
            assert!(
                matches!(
                    check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
                    PermissionResult::Deny {
                        decision_reason: PermissionDecisionReason::Other { ref reason },
                        ..
                    } if reason == "Removal targets a protected system path"
                ),
                "path={path:?}"
            );
        }
    }

    #[test]
    fn nested_control_flow_removals_are_denied_too() {
        let mut parsed = pipeline(vec![ParsedCommandElement {
            element_type: Some(PipelineElementType::CommandExpressionAst),
            text: "if ($true) { Remove-Item / }".to_string(),
            ..ParsedCommandElement::default()
        }]);
        parsed.statements[0].nested_commands =
            Some(vec![command("Remove-Item", &["/"], &literals(1))]);
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn compound_cwd_change_forces_manual_approval() {
        let parsed = pipeline(vec![command(
            "Get-Content",
            &["./Cargo.toml"],
            &literals(1),
        )]);
        match check_path_constraints(&parsed, &ToolPermissionContext::default(), true) {
            PermissionResult::Ask {
                message,
                decision_reason: Some(PermissionDecisionReason::Other { reason }),
                ..
            } => {
                assert!(message.starts_with("Compound command changes working directory"));
                assert_eq!(
                    reason,
                    "Compound command contains cd with path operation — manual approval required to prevent path resolution bypass"
                );
            }
            other => panic!("expected ask, got {other:?}"),
        }
    }

    #[test]
    fn unknown_parameters_make_the_whole_invocation_unvalidatable() {
        let parsed = pipeline(vec![command(
            "Get-Content",
            &["-NotAThing", "./Cargo.toml"],
            &[
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
            ],
        )]);
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Ask { ref message, .. }
                if message.starts_with("get-content uses a parameter or complex path expression")
        ));
    }

    #[test]
    fn write_cmdlets_with_no_target_path_require_approval() {
        let parsed = pipeline(vec![command("Set-Content", &[], &[])]);
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Ask { ref message, .. }
                if message == "set-content is a write operation but no target path could be determined; requires manual approval"
        ));
    }

    #[test]
    fn optional_write_cmdlets_without_an_output_file_are_exempt() {
        let parsed = pipeline(vec![command(
            "Invoke-WebRequest",
            &["https://example.com"],
            &literals(1),
        )]);
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Passthrough { .. }
        ));
    }

    #[test]
    fn pipeline_expression_sources_cannot_bind_a_validated_path() {
        let parsed = pipeline(vec![
            ParsedCommandElement {
                name: "'/etc/passwd'".to_string(),
                element_type: Some(PipelineElementType::CommandExpressionAst),
                text: "'/etc/passwd'".to_string(),
                ..ParsedCommandElement::default()
            },
            command("Get-Content", &[], &[]),
        ]);
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Ask { ref message, .. }
                if message == "get-content receives its path from a pipeline expression source that cannot be statically validated and requires manual approval"
        ));
    }

    #[test]
    fn variable_arguments_are_never_treated_as_literal_paths() {
        let parsed = pipeline(vec![command(
            "Remove-Item",
            &["$env:PATH"],
            &[CommandElementType::Variable],
        )]);
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Ask { ref message, .. }
                if message.starts_with("remove-item uses a parameter or complex path expression")
        ));
    }

    #[test]
    fn redirection_targets_outside_the_working_directory_are_asked() {
        let mut parsed = pipeline(vec![command("Get-Date", &[], &[])]);
        parsed.statements[0].redirections =
            vec![crate::utils::powershell::parser::ParsedRedirection {
                operator: crate::utils::powershell::parser::RedirectionOperator::Output,
                target: "/etc/cometix-parity.txt".to_string(),
                is_merging: false,
            }];
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Ask { ref message, .. }
                if message.starts_with("Output redirection to '")
        ));
    }

    #[test]
    fn null_and_merging_redirections_are_not_filesystem_writes() {
        let mut parsed = pipeline(vec![command("Get-Date", &[], &[])]);
        parsed.statements[0].redirections = vec![
            crate::utils::powershell::parser::ParsedRedirection {
                operator: crate::utils::powershell::parser::RedirectionOperator::Output,
                target: "$null".to_string(),
                is_merging: false,
            },
            crate::utils::powershell::parser::ParsedRedirection {
                operator: crate::utils::powershell::parser::RedirectionOperator::Merge,
                target: String::new(),
                is_merging: true,
            },
        ];
        assert!(matches!(
            check_path_constraints(&parsed, &ToolPermissionContext::default(), false),
            PermissionResult::Passthrough { .. }
        ));
    }

    #[test]
    fn parameter_prefix_matching_follows_powershell_abbreviation_rules() {
        assert!(matches_param("-lit", &PROVIDER_PATH_PARAMS));
        assert!(matches_param("-path", &PROVIDER_PATH_PARAMS));
        assert!(!matches_param("-x", &PROVIDER_PATH_PARAMS));
        assert!(!matches_param("-destination", &PROVIDER_PATH_PARAMS));
    }

    #[test]
    fn colon_bound_expression_values_are_flagged_as_unvalidatable() {
        for value in [
            "-Path:(1 > /tmp/x)",
            "-Path:a,b",
            "-Path:$env:X",
            "-Path:@{k=v}",
        ] {
            let extracted = extract_paths_from_command(&command(
                "Set-Content",
                &[value],
                &[CommandElementType::Parameter],
            ));
            assert!(extracted.has_unvalidatable_path_arg, "value={value:?}");
        }
        let simple = extract_paths_from_command(&command(
            "Set-Content",
            &["-Path:./out.txt"],
            &[CommandElementType::Parameter],
        ));
        assert!(!simple.has_unvalidatable_path_arg);
        assert_eq!(simple.paths, vec!["./out.txt".to_string()]);
    }

    #[test]
    fn new_item_name_extracts_leaves_but_rejects_traversal() {
        let leaf = extract_paths_from_command(&command(
            "New-Item",
            &["-Path", "./build", "-Name", "out.txt"],
            &[
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
            ],
        ));
        assert_eq!(
            leaf.paths,
            vec!["./build".to_string(), "out.txt".to_string()]
        );
        assert!(!leaf.has_unvalidatable_path_arg);

        let traversal = extract_paths_from_command(&command(
            "New-Item",
            &["-Path", "./build", "-Name", "../secret/evil"],
            &[
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
            ],
        ));
        assert!(traversal.has_unvalidatable_path_arg);
    }

    #[test]
    fn positional_skip_keeps_urls_out_of_the_path_set() {
        let extracted = extract_paths_from_command(&command(
            "Invoke-WebRequest",
            &["https://example.com", "-OutFile", "./out.bin"],
            &[
                CommandElementType::StringConstant,
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
            ],
        ));
        assert_eq!(extracted.paths, vec!["./out.bin".to_string()]);
        assert_eq!(extracted.operation_type, FileOperationType::Write);
        assert!(extracted.optional_write);
    }

    #[test]
    fn common_parameters_do_not_trip_the_unknown_parameter_guard() {
        let extracted = extract_paths_from_command(&command(
            "Get-Content",
            &[
                "./Cargo.toml",
                "-ErrorAction",
                "SilentlyContinue",
                "-Verbose",
            ],
            &[
                CommandElementType::StringConstant,
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
                CommandElementType::Parameter,
            ],
        ));
        assert!(!extracted.has_unvalidatable_path_arg);
        assert_eq!(extracted.paths, vec!["./Cargo.toml".to_string()]);
    }

    #[test]
    fn provider_and_escape_paths_are_rejected_with_the_official_reasons() {
        let context = ToolPermissionContext::default();
        let cwd = cwd();
        for (path, expected) in [
            (
                "env:HOME",
                "Path 'env:HOME' uses a non-filesystem provider and requires manual approval",
            ),
            (
                "FileSystem::/etc/passwd",
                "Module-qualified provider paths (::) cannot be statically validated and require manual approval",
            ),
            (
                "`/etc/passwd",
                "Backtick escape characters in paths cannot be statically validated and require manual approval",
            ),
            (
                "//server/share",
                "UNC paths are blocked because they can trigger network requests and credential leakage",
            ),
            (
                "$HOME/.ssh/id_rsa",
                "Variable expansion syntax in paths requires manual approval",
            ),
        ] {
            let check = validate_path(path, &cwd, &context, FileOperationType::Read);
            assert!(!check.allowed, "path={path:?}");
            assert!(
                matches!(
                    check.decision_reason,
                    Some(PermissionDecisionReason::Other { ref reason }) if reason == expected
                ),
                "path={path:?} reason={:?}",
                check.decision_reason
            );
        }
    }

    #[test]
    fn read_globs_ask_while_write_globs_are_rejected_outright() {
        let context = ToolPermissionContext::default();
        let cwd = cwd();
        let write = validate_path("./src/*.rs", &cwd, &context, FileOperationType::Write);
        assert!(matches!(
            write.decision_reason,
            Some(PermissionDecisionReason::Other { ref reason })
                if reason == "Glob patterns are not allowed in write operations. Please specify an exact file path."
        ));
        let read = validate_path("./src/*.rs", &cwd, &context, FileOperationType::Read);
        assert!(matches!(
            read.decision_reason,
            Some(PermissionDecisionReason::Other { ref reason })
                if reason == "Glob patterns in paths cannot be statically validated — symlinks inside the glob expansion are not examined. Requires manual approval."
        ));
    }

    #[test]
    fn glob_base_directory_keeps_the_trailing_separator() {
        assert_eq!(get_glob_base_directory("/tmp/*.txt"), "/tmp/");
        assert_eq!(get_glob_base_directory("*.txt"), ".");
        assert_eq!(get_glob_base_directory("/*.txt"), "/");
        assert_eq!(get_glob_base_directory("/tmp/no-glob"), "/tmp/no-glob");
        // Braces are literal in PowerShell, so they must not truncate.
        assert_eq!(get_glob_base_directory("./{x}/passwd"), "./{x}/passwd");
    }

    #[test]
    fn raw_removal_paths_match_official_expansion_rules() {
        for path in [
            "/",
            "'/'",
            "~",
            "~/",
            "C:\\",
            "C:\\Windows",
            "/etc",
            "*",
            "./x/*",
        ] {
            assert!(is_dangerous_removal_raw_path(path), "path={path:?}");
        }
        for path in ["./src", "src/main.rs", "/usr/local/share"] {
            assert!(!is_dangerous_removal_raw_path(path), "path={path:?}");
        }
    }

    #[test]
    fn removal_deny_uses_official_message_and_reason() {
        match dangerous_removal_deny("/") {
            PermissionResult::Deny {
                message,
                decision_reason: PermissionDecisionReason::Other { reason },
                ..
            } => {
                assert_eq!(
                    message,
                    "Remove-Item on system path '/' is blocked. This path is protected from removal."
                );
                assert_eq!(reason, "Removal targets a protected system path");
            }
            other => panic!("expected deny, got {other:?}"),
        }
    }
}
