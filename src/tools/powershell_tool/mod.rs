//! PowerShell tool metadata, execution, and UI helpers.
//!
//! Maps to:
//! - CC `tools/PowerShellTool/PowerShellTool.tsx` for tool metadata/schema
//!   and execution (`call`:595, `runPowerShellCommand`:863)
//! - CC `tools/PowerShellTool/UI.tsx` for recorded transcript rendering

pub mod command_semantics;
pub mod common_parameters;
pub mod destructive_command_warning;
pub mod git_safety;
pub mod mode_validation;
pub mod path_validation;
pub mod powershell_permissions;
pub mod powershell_security;
pub mod prompt;
pub mod read_only_validation;
pub mod tool_name;
pub mod ui;

pub use crate::utils::shell::shell_tool_utils::{
    is_powershell_tool_enabled, is_powershell_tool_enabled_for_platform,
};

/// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx:99-104`
/// `PS_SEARCH_COMMANDS`.
const PS_SEARCH_COMMANDS: [&str; 4] = ["select-string", "get-childitem", "findstr", "where.exe"];

/// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx:110-124`
/// `PS_READ_COMMANDS`.
const PS_READ_COMMANDS: [&str; 11] = [
    "get-content",
    "get-item",
    "test-path",
    "resolve-path",
    "get-process",
    "get-service",
    "get-childitem",
    "get-location",
    "get-filehash",
    "get-acl",
    "format-hex",
];

/// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx:129-132`
/// `PS_SEMANTIC_NEUTRAL_COMMANDS`.
const PS_SEMANTIC_NEUTRAL_COMMANDS: [&str; 2] = ["write-output", "write-host"];

/// Maps to: CC `PowerShellTool.tsx:198-201`
/// `DISALLOWED_AUTO_BACKGROUND_COMMANDS`. `sleep` is a PS built-in alias for
/// `Start-Sleep` but is not in `COMMON_ALIASES`, so both forms are listed.
const DISALLOWED_AUTO_BACKGROUND_COMMANDS: [&str; 2] = ["start-sleep", "sleep"];

/// Maps to: CC `PowerShellTool.tsx:208-213` `isAutobackgroundingAllowed`.
pub fn is_autobackgrounding_allowed(command: &str) -> bool {
    let Some(first_word) = command.split_whitespace().next() else {
        return true;
    };
    let canonical = read_only_validation::resolve_to_canonical(first_word);
    !DISALLOWED_AUTO_BACKGROUND_COMMANDS.contains(&canonical.as_str())
}

/// Maps to: CC `PowerShellTool.tsx:221-248` `detectBlockedSleepPattern`.
///
/// PS-flavoured port of BashTool's detector. Catches `Start-Sleep N`,
/// `Start-Sleep -Seconds N` and `sleep N` (built-in alias) as the FIRST
/// statement. Deliberately does not block `-Milliseconds` (sub-second pacing)
/// or float seconds (legitimate rate limiting).
///
/// CONSUMER SEAM: CC reads this from `validateInput` behind
/// `feature('MONITOR_TOOL')` (:501), an ant build feature that is off in the
/// external build and has no `FeatureFlag` entry in this port — the same
/// treatment `tools/bash_tool/prompt.rs:365` records. The detector itself is
/// ported so the gated-off branch is a wiring decision, not a missing symbol.
pub fn detect_blocked_sleep_pattern(command: &str) -> Option<String> {
    use regex::Regex;
    use std::sync::LazyLock;

    // JS `/^(?:start-sleep|sleep)(?:\s+-s(?:econds)?)?\s+(\d+)\s*$/i`.
    static SLEEP_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:start-sleep|sleep)(?:\s+-s(?:econds)?)?\s+(\d+)\s*$")
            .expect("valid blocked-sleep regex")
    });

    let trimmed = command.trim();
    // First statement only — PS separators `;`, `|`, `&`, and newline. JS
    // `split(...)[0]?.trim() ?? ''`.
    let first = trimmed
        .split([';', '|', '&', '\r', '\n'])
        .next()
        .unwrap_or_default()
        .trim();
    let captures = SLEEP_RE.captures(first)?;
    let secs: u64 = captures.get(1)?.as_str().parse().ok()?;
    if secs < 2 {
        // Sub-2s sleeps are fine (rate limiting, pacing).
        return None;
    }

    // JS `command.trim().slice(first.length)` slices the ORIGINAL trimmed
    // string by the length of the trimmed first statement, so a first statement
    // with inner leading whitespace shifts the cut. Reproduced by taking the
    // same char count off the front.
    let rest: String = trimmed.chars().skip(first.chars().count()).collect();
    let rest = rest.trim_start_matches([' ', '\t', '\n', '\r', '\x0c', '\x0b', ';', '|', '&']);
    Some(if rest.is_empty() {
        format!("standalone Start-Sleep {secs}")
    } else {
        format!("Start-Sleep {secs} followed by: {rest}")
    })
}

/// Maps to: CC `PowerShellTool.tsx:262-263` `WINDOWS_SANDBOX_POLICY_REFUSAL`.
const WINDOWS_SANDBOX_POLICY_REFUSAL: &str = "Enterprise policy requires sandboxing, but sandboxing is not available on native Windows. Shell command execution is blocked on this platform by policy.";

/// Maps to: CC `PowerShellTool.tsx:264-270`
/// `isWindowsSandboxPolicyViolation()`.
///
/// On Windows native, sandbox is unavailable (bwrap/sandbox-exec are
/// POSIX-only). If enterprise policy has `sandbox.enabled` AND forbids
/// unsandboxed commands, PowerShell cannot comply — refuse rather than
/// silently bypass the policy. On Linux/macOS/WSL2 pwsh runs under the sandbox
/// like bash, so the gate does not apply.
fn is_windows_sandbox_policy_violation() -> bool {
    if !cfg!(target_os = "windows") {
        return false;
    }
    let settings = crate::utils::settings::get_initial_settings();
    crate::utils::sandbox::sandbox_adapter::get_sandbox_enabled_setting(&settings)
        && !crate::utils::sandbox::sandbox_adapter::are_unsandboxed_commands_allowed(&settings)
}

/// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx:136-187`
/// `isSearchOrReadPowerShellCommand`.
pub(crate) fn is_search_or_read_powershell_command(
    command: &str,
) -> crate::tool::SearchOrReadCommand {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return crate::tool::SearchOrReadCommand::default();
    }

    let parts = trimmed
        .split([';', '|'])
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let mut has_search = false;
    let mut has_read = false;
    let mut has_non_neutral_command = false;

    for part in parts {
        let Some(base_command) = part.split_whitespace().next() else {
            continue;
        };
        let canonical = read_only_validation::resolve_to_canonical(base_command);
        if PS_SEMANTIC_NEUTRAL_COMMANDS.contains(&canonical.as_str()) {
            continue;
        }

        has_non_neutral_command = true;
        let is_search = PS_SEARCH_COMMANDS.contains(&canonical.as_str());
        let is_read = PS_READ_COMMANDS.contains(&canonical.as_str());
        if !is_search && !is_read {
            return crate::tool::SearchOrReadCommand::default();
        }
        has_search |= is_search;
        has_read |= is_read;
    }

    if !has_non_neutral_command {
        return crate::tool::SearchOrReadCommand::default();
    }
    crate::tool::SearchOrReadCommand {
        is_search: has_search,
        is_read: has_read,
    }
}

/// Maps to CC `PowerShellTool` metadata and `toolToAPISchema(...)` input schema
/// projection. This lives with the tool instead of `services/api/claude.rs` so
/// the API layer only converts tool definitions it is given.
/// Maps to: CC `PowerShellTool.tsx:277-296` `fullInputSchema`.
fn full_input_fields() -> Vec<crate::utils::zod::ObjectField> {
    use crate::utils::zod;
    vec![
        (
            "command",
            zod::string().describe("The PowerShell command to execute"),
        ),
        (
            "timeout",
            crate::utils::semantic_number::semantic_number(zod::number().optional()).describe(
                format!(
                    "Optional timeout in milliseconds (max {})",
                    prompt::get_max_timeout_ms()
                ),
            ),
        ),
        (
            "description",
            zod::string()
                .optional()
                .describe("Clear, concise description of what this command does in active voice."),
        ),
        (
            "run_in_background",
            crate::utils::semantic_boolean::semantic_boolean(zod::boolean().optional()).describe(
                "Set to true to run this command in the background. Use Read to read the output later.",
            ),
        ),
        (
            "dangerouslyDisableSandbox",
            crate::utils::semantic_boolean::semantic_boolean(zod::boolean().optional()).describe(
                "Set this to true to dangerously override sandbox mode and run commands without sandboxing.",
            ),
        ),
    ]
}

/// Maps to: CC `PowerShellTool.tsx:273-275` module-level
/// `isBackgroundTasksDisabled`. Unlike BashTool, the module is not in
/// `main.js`'s static import graph: `tools.ts:150-155` requires it at the
/// first `getTools()`, after `init()` applied the safe settings env. `main::run`
/// and `cli/print.rs` force it at that point.
pub(crate) static IS_BACKGROUND_TASKS_DISABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| {
        crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS").as_deref(),
        )
    });

/// Maps to: CC `PowerShellTool.tsx:299-303` `inputSchema`, gated on
/// [`IS_BACKGROUND_TASKS_DISABLED`].
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        let background_disabled = *IS_BACKGROUND_TASKS_DISABLED;
        let fields = full_input_fields()
            .into_iter()
            .filter(|(name, _)| !(background_disabled && *name == "run_in_background"))
            .collect();
        crate::utils::zod::strict_object(fields)
    })
}

pub fn powershell_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: tool_name::POWERSHELL_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

/// CC `tools/PowerShellTool/PowerShellTool.tsx` outputSchema (:310) —
/// ported subset plus display-only command/duration metadata.
pub(crate) struct PowerShellOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) interrupted: bool,
    pub(crate) return_code_interpretation: Option<String>,
    pub(crate) is_image: bool,
    pub(crate) persisted_output_path: Option<String>,
    pub(crate) persisted_output_size: Option<u64>,
    pub(crate) background_task_id: Option<String>,
    pub(crate) backgrounded_by_user: Option<bool>,
    pub(crate) assistant_auto_backgrounded: Option<bool>,
}

pub(crate) struct PowerShellExecutionError {
    content: String,
    status: crate::types::message::ToolResultStatus,
}

impl PowerShellExecutionError {
    /// Maps to CC `utils/promptShellExecution.ts` reading a shell-tool failure
    /// message for `formatBashError(...)`.
    pub(crate) fn content(&self) -> &str {
        &self.content
    }
}

/// Execute a permitted PowerShell tool use via `pwsh -NoProfile`.
/// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx` `call` (:595) /
/// `runPowerShellCommand` (:863) — foreground path with timeout/abort;
/// background, image, and persisted-output branches remain staged.
pub(crate) fn powershell_output(
    args: &serde_json::Value,
    abort: &crate::tool::AbortController,
    cwd: Option<&std::path::Path>,
) -> Result<PowerShellOutput, PowerShellExecutionError> {
    use crate::types::message::ToolResultStatus;
    use crate::utils::shell::{run_command_streaming, shell_model_content, shell_timeout_ms};

    // Load-bearing guard. Maps to: CC `PowerShellTool.tsx:602-608` — direct
    // callers (`promptShellExecution.ts`, `processBashCommand.tsx`) bypass
    // `validateInput`, so this is the check that covers ALL callers.
    if is_windows_sandbox_policy_violation() {
        return Err(PowerShellExecutionError {
            content: WINDOWS_SANDBOX_POLICY_REFUSAL.to_string(),
            status: ToolResultStatus::Error,
        });
    }

    let command = args
        .get("command")
        .and_then(|value| value.as_str())
        .or_else(|| args.get("value").and_then(|value| value.as_str()))
        .unwrap_or_default();
    if command.is_empty() {
        return Err(PowerShellExecutionError {
            content: "Error running PowerShell command: missing command".to_string(),
            status: ToolResultStatus::Error,
        });
    }

    // Maps to: CC `PowerShellTool.tsx:928-939` — pre-flight. `exec(..., 'powershell')`
    // resolves the interpreter through `getCachedPowerShellPath()`, which prefers
    // `pwsh` (7+) and falls back to `powershell` (5.1, the only in-box edition on
    // stock Windows). When neither is present CC returns code 0 with an explanatory
    // stderr so `call()` surfaces it gracefully instead of throwing ShellError —
    // the command never ran, so there is no meaningful non-zero exit to report.
    let Some(powershell_path) =
        crate::utils::shell::powershell_detection::get_cached_powershell_path()
    else {
        return Ok(PowerShellOutput {
            stdout: String::new(),
            stderr: "PowerShell is not available on this system.".to_string(),
            interrupted: false,
            return_code_interpretation: None,
            is_image: false,
            persisted_output_path: None,
            persisted_output_size: None,
            background_task_id: None,
            backgrounded_by_user: None,
            assistant_auto_backgrounded: None,
        });
    };

    let timeout_ms = shell_timeout_ms(args);
    let run = run_command_streaming(
        &powershell_path,
        // Maps to: CC `utils/shell/powershellProvider.ts:12`
        // `['-NoProfile', '-NonInteractive', '-Command', cmd]`. `-NonInteractive`
        // is load-bearing: the tool prompt promises it (`prompt.ts:105`), and
        // without it `Read-Host` / cmdlet confirmation prompts block on stdin
        // until the timeout fires instead of erroring immediately.
        &["-NoProfile", "-NonInteractive", "-Command", command],
        std::time::Duration::from_millis(timeout_ms),
        cwd,
        abort,
        None,
    );
    match run {
        Ok(run) => {
            let stdout = String::from_utf8_lossy(&run.output.stdout).to_string();
            let mut stderr = String::from_utf8_lossy(&run.output.stderr).to_string();
            let interrupted = run.timed_out || run.aborted;
            // CC reads `result.code` off ExecResult; a signal-terminated child
            // has no code, and CC's ShellCommand reports those as non-zero.
            let code = run.output.status.code().unwrap_or(1);
            if run.timed_out {
                let timeout_message = format!("Command timed out after {timeout_ms}ms");
                if stderr.is_empty() {
                    stderr = timeout_message;
                } else {
                    stderr.push('\n');
                    stderr.push_str(&timeout_message);
                }
            }

            // Maps to: CC `PowerShellTool.tsx:653-673`. Feeds the same git/PR
            // counters BashTool does (`BashTool.tsx:919`); PS invokes git/gh/glab
            // as external binaries with identical syntax, so the shell-agnostic
            // detection works as-is. The pre-flight sentinel guard (:666-670)
            // keeps a command that never ran from being counted: those paths
            // return code 0 + empty stdout + non-empty stderr, which
            // `track_git_operations` would otherwise read as a success.
            let is_pre_flight_sentinel = code == 0 && stdout.is_empty() && !stderr.is_empty();
            if !is_pre_flight_sentinel {
                crate::tools::shared::git_operation_tracking::track_git_operations(
                    command,
                    code,
                    Some(&stdout),
                );
            }

            // Maps to: CC `PowerShellTool.tsx:731-736` — semantic exit-code
            // interpretation. PS-native cmdlets exit 0 on no-match so they hit the
            // default; this primarily rescues external exes (grep/rg/findstr exit
            // 1 = no match, robocopy 1-7 = success) from being reported as errors.
            let interpretation = command_semantics::interpret_command_result(
                command,
                code,
                stdout.trim_end(),
                &stderr,
            );
            // CC `:764-771`: only `interpretation.isError` throws, not the raw
            // exit code, and an interrupt suppresses the throw.
            if interpretation.is_error && !interrupted {
                return Err(PowerShellExecutionError {
                    content: shell_model_content(&stdout, &stderr),
                    status: ToolResultStatus::Error,
                });
            }
            Ok(PowerShellOutput {
                stdout,
                stderr,
                interrupted,
                // CC `:845` records `interpretation.message` unconditionally.
                return_code_interpretation: interpretation.message,
                is_image: false,
                persisted_output_path: None,
                persisted_output_size: None,
                background_task_id: None,
                backgrounded_by_user: None,
                assistant_auto_backgrounded: None,
            })
        }
        Err(error) => Err(PowerShellExecutionError {
            content: format!("Error running PowerShell command: {error}"),
            status: ToolResultStatus::Error,
        }),
    }
}

fn strip_leading_blank_lines(mut text: String) -> String {
    loop {
        let Some((first, rest)) = text.split_once('\n') else {
            break;
        };
        if !first.trim().is_empty() {
            break;
        }
        text = rest.to_string();
    }
    text
}

/// Behavioral half of CC `PowerShellTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct PowerShellTool;

impl crate::tool::ToolCall for PowerShellTool {
    fn name(&self) -> &'static str {
        "PowerShell"
    }

    /// Maps to CC `PowerShellTool.tsx:517-521#checkPermissions`.
    fn check_permissions(
        &self,
        input: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::types::permissions::PermissionResult {
        let state = context.get_app_state();
        let permissions = state
            .as_ref()
            .map(|state| state.tool_permission_context.as_ref())
            .unwrap_or(&context.tool_permission_context);
        powershell_permissions::powershell_tool_has_permission(
            input
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
            permissions,
        )
    }

    /// Maps to: CC `PowerShellTool.tsx:412-414` `async prompt() { return getPrompt() }`.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `PowerShellTool.tsx:403` `maxResultSizeChars: 30_000`.
    fn max_result_size_chars(&self) -> usize {
        30_000
    }

    /// Maps to: CC `PowerShellTool.tsx:402`
    /// `searchHint: 'execute Windows PowerShell commands'` — the ToolSearch
    /// keyword string. The trait default is `None`, i.e. no keywords at all.
    fn search_hint(&self) -> Option<&'static str> {
        Some("execute Windows PowerShell commands")
    }

    /// Maps to: CC `PowerShellTool.tsx:406-410`
    /// `description({ description }) { return description || 'Run PowerShell command' }`
    /// — the permission-dialog line, read at `useCanUseTool.tsx:138`. JS `||`
    /// means an empty-string description also falls back to the constant.
    fn description(&self, args: &serde_json::Value) -> String {
        args.get("description")
            .and_then(serde_json::Value::as_str)
            .filter(|description| !description.is_empty())
            .unwrap_or("Run PowerShell command")
            .to_string()
    }

    /// Maps to: CC `PowerShellTool.tsx:476-485` `getActivityDescription`.
    /// Note the `??` (not `||`): unlike `description`/`getToolUseSummary`, an
    /// empty-string `description` is kept here rather than falling back to the
    /// truncated command.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        let Some(command) = args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|command| !command.is_empty())
        else {
            return Some("Running command".to_string());
        };
        let description = match args.get("description") {
            Some(serde_json::Value::String(description)) => description.clone(),
            _ => crate::utils::truncate::truncate_to_width(
                command,
                crate::constants::tool_limits::TOOL_SUMMARY_MAX_LENGTH,
            ),
        };
        Some(format!("Running {description}"))
    }

    /// Maps to: CC `PowerShellTool.tsx:447-449`
    /// `toAutoClassifierInput(input) { return input.command }`. The trait
    /// default is `""`, which the auto classifier reads as "skip this tool".
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Maps to: CC `PowerShellTool.tsx:855-860` `isResultTruncated(output)` —
    /// gates fullscreen click-to-expand (`Messages.tsx:759`).
    fn is_result_truncated(&self, data: &crate::tool::ToolOutput) -> bool {
        match data {
            crate::tool::ToolOutput::PowerShell(output) => {
                crate::utils::terminal::is_output_line_truncated(&output.stdout)
                    || crate::utils::terminal::is_output_line_truncated(&output.stderr)
            }
            _ => false,
        }
    }

    /// Maps to: CC `PowerShellTool.tsx:463-474` `getToolUseSummary` — same
    /// shape as BashTool's: no command → null, truthy description wins,
    /// otherwise the truncated command.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        let command = args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|command| !command.is_empty())?;
        if let Some(description) = args
            .get("description")
            .and_then(serde_json::Value::as_str)
            .filter(|description| !description.is_empty())
        {
            return Some(description.to_string());
        }
        Some(crate::utils::truncate::truncate_to_width(
            command,
            crate::constants::tool_limits::TOOL_SUMMARY_MAX_LENGTH,
        ))
    }

    /// Maps to: CC `PowerShellTool.tsx:277-295` Zod semantic preprocessors.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        crate::utils::semantic_number::preprocess_object_field(&mut parsed, "timeout");
        for field in ["run_in_background", "dangerouslyDisableSandbox"] {
            crate::utils::semantic_boolean::preprocess_object_field(&mut parsed, field);
        }
        parsed
    }

    // No `is_concurrency_safe` override. Maps to: CC
    // `PowerShellTool.isConcurrencySafe(input)` (PowerShellTool.tsx:416-418)
    // delegating to `isReadOnly` (:430), which is constantly false — see the
    // `is_read_only` override below. The trait default is that same `false`.
    // (The real PowerShell read-only auto-allow happens async with the AST
    // in `powershellToolHasPermission`, outside `isConcurrencySafe`.)

    /// Maps to: CC `PowerShellTool.tsx:430-446` `isReadOnly(input)`.
    ///
    /// Both halves are CC's, in CC's order: the sync security heuristics run
    /// first (`hasSyncSecurityConcerns`, readOnlyValidation.ts:1112), then the
    /// cmdlet allowlist with NO parsed AST. CC's own comment at :439-444
    /// records the consequence — `isReadOnlyCommand` without the AST cannot
    /// split pipelines/statements and `readOnlyValidation.ts:1174-1177` returns
    /// `false` for every input, so this method is constantly false.
    ///
    /// It is written out rather than left on the trait default because
    /// `has_sync_security_concerns` otherwise has no production consumer on
    /// this side, and because the identity is load-bearing for
    /// `is_concurrency_safe` above.
    fn is_read_only(&self, args: &serde_json::Value) -> bool {
        let command = args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if read_only_validation::has_sync_security_concerns(command) {
            return false;
        }
        read_only_validation::is_read_only_command(command, None)
    }

    /// Maps to: CC `PowerShellTool.tsx:491-515` `validateInput`.
    ///
    /// Defense-in-depth: the same policy check runs in `call()` (:606-608),
    /// which is the load-bearing one because direct callers
    /// (`promptShellExecution.ts`, `processBashCommand.tsx`) skip
    /// `validateInput`.
    ///
    /// CC's second branch — `detect_blocked_sleep_pattern` with `errorCode: 10`
    /// — sits behind `feature('MONITOR_TOOL')` (:501), an ant build feature
    /// that is off in the external build and has no `FeatureFlag` entry here;
    /// see [`detect_blocked_sleep_pattern`].
    fn validate_input(
        &self,
        _args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        if is_windows_sandbox_policy_violation() {
            return crate::tool::ValidationResult::error(WINDOWS_SANDBOX_POLICY_REFUSAL, 11);
        }
        crate::tool::ValidationResult::Ok
    }

    /// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx:420-429`
    /// `PowerShellTool.isSearchOrReadCommand`.
    fn is_search_or_read_command(
        &self,
        args: &serde_json::Value,
    ) -> Option<crate::tool::SearchOrReadCommand> {
        Some(
            args.get("command")
                .and_then(serde_json::Value::as_str)
                .map(is_search_or_read_powershell_command)
                .unwrap_or_default(),
        )
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            match powershell_output(
                args,
                &context.abort_controller,
                context.cwd_override.as_deref(),
            ) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::PowerShell(output),
                    new_messages: Vec::new(),
                },
                Err(error) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Composed {
                        content: error.content,
                        status: error.status,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    /// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx`
    /// `mapToolResultToToolResultBlockParam` (:530-588).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::PowerShell(output) => {
                let mut processed_stdout = output.stdout.clone();
                if !processed_stdout.is_empty() {
                    processed_stdout = strip_leading_blank_lines(processed_stdout);
                    processed_stdout = processed_stdout.trim_end().to_string();
                }

                let mut error_message = output.stderr.trim().to_string();
                if output.interrupted {
                    if !output.stderr.is_empty() {
                        error_message.push('\n');
                    }
                    error_message.push_str("<error>Command was aborted before completion</error>");
                }

                let background_info = output
                    .background_task_id
                    .as_ref()
                    .map(|task_id| format!("Command running in background with ID: {task_id}"))
                    .unwrap_or_default();

                let content = [processed_stdout, error_message, background_info]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                let status = if output.interrupted {
                    ToolResultStatus::Error
                } else {
                    ToolResultStatus::Success
                };
                (content, status)
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// Maps to: CC recording PowerShellTool's `Out` as the message's
    /// `toolUseResult` (the render layer parses it back with
    /// `ui::parse_output`). Error results record the plain error string,
    /// as CC's error path does.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::PowerShell(output) => Some(ui::output_to_value(output)),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn sample_output(stdout: &str, stderr: &str, interrupted: bool) -> PowerShellOutput {
        PowerShellOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            interrupted,
            return_code_interpretation: None,
            is_image: false,
            persisted_output_path: None,
            persisted_output_size: None,
            background_task_id: None,
            backgrounded_by_user: None,
            assistant_auto_backgrounded: None,
        }
    }

    #[test]
    fn powershell_result_budget_matches_official_max_result_size() {
        assert_eq!(
            crate::tool::ToolCall::max_result_size_chars(&PowerShellTool),
            30_000
        );
    }

    /// The metadata axis every prior tool audit found gaps on. Each assertion
    /// names the CC line that supplies the value; the trait default it replaces
    /// is in the doc comment on the override.
    #[test]
    fn powershell_metadata_matches_official_explicit_values() {
        use crate::tool::ToolCall;

        // CC :402 — the trait default is None (no ToolSearch keywords).
        assert_eq!(
            PowerShellTool.search_hint(),
            Some("execute Windows PowerShell commands")
        );
        // CC :459-461. Equal to the trait default here, pinned so a rename of
        // `name()` cannot silently move the dialog header.
        assert_eq!(PowerShellTool.user_facing_name(None), "PowerShell");

        // CC :406-410 `description || 'Run PowerShell command'` — the trait
        // default was the empty string, so the permission dialog showed nothing.
        assert_eq!(
            PowerShellTool.description(&serde_json::json!({"command": "Get-Date"})),
            "Run PowerShell command"
        );
        assert_eq!(
            PowerShellTool.description(&serde_json::json!({
                "command": "Get-Date", "description": "Print the date"
            })),
            "Print the date"
        );
        // JS `||`: the empty string is falsy and falls back.
        assert_eq!(
            PowerShellTool.description(&serde_json::json!({
                "command": "Get-Date", "description": ""
            })),
            "Run PowerShell command"
        );

        // CC :476-485 — trait default was None (no activity line at all).
        assert_eq!(
            PowerShellTool
                .get_activity_description(&serde_json::json!({"command": "Get-Date"}))
                .as_deref(),
            Some("Running Get-Date")
        );
        assert_eq!(
            PowerShellTool
                .get_activity_description(&serde_json::json!({}))
                .as_deref(),
            Some("Running command")
        );
        // `??` (not `||`) at :483 keeps an empty-string description.
        assert_eq!(
            PowerShellTool
                .get_activity_description(&serde_json::json!({
                    "command": "Get-Date", "description": ""
                }))
                .as_deref(),
            Some("Running ")
        );

        // CC :447-449 — trait default was "", which the auto classifier reads
        // as "skip this tool", so PowerShell was invisible to it.
        assert_eq!(
            PowerShellTool.to_auto_classifier_input(&serde_json::json!({
                "command": "Remove-Item -Recurse ."
            })),
            "Remove-Item -Recurse ."
        );

        // CC declares none of these for PowerShell (ast-grep over
        // tools/PowerShellTool/ finds no shouldDefer / getPath /
        // extractSearchText / isDestructive member), so the trait defaults are
        // the alignment.
        assert!(!PowerShellTool.should_defer());
        assert_eq!(
            PowerShellTool.get_path(&serde_json::json!({"command": "Get-Date"})),
            None
        );
        assert!(!PowerShellTool.is_destructive(&serde_json::json!({
            "command": "Remove-Item -Recurse -Force ."
        })));
        // CC :487-489 `isEnabled() { return true }`; the Windows/env gate is
        // the SEPARATE tools-list rail (`tools.ts:150-154`).
        assert!(PowerShellTool.is_enabled());
        // Re-confirmed, not re-derived: CC :416-418 `isConcurrencySafe` →
        // `isReadOnly` (:430-446) → `isReadOnlyCommand(command)` with no AST,
        // which `readOnlyValidation.ts:1174-1177` answers `false` for
        // unconditionally. `is_concurrency_safe` therefore stays on the trait
        // default; `is_read_only` is written out and must agree with it.
        for command in [
            "Get-Date",
            "Get-Content README.md",
            "Get-ChildItem -Recurse",
            // Sync-security path (readOnlyValidation.ts:1112-1158).
            "$(Get-Date)",
            "Get-Content \\\\server\\share\\f",
            "[Math]::Abs(-1)",
            "",
        ] {
            let args = serde_json::json!({ "command": command });
            assert!(
                !PowerShellTool.is_read_only(&args),
                "isReadOnly is constantly false without the AST; command={command:?}"
            );
            assert!(
                !PowerShellTool.is_concurrency_safe(&args),
                "command={command:?}"
            );
        }
    }

    #[test]
    fn powershell_result_truncation_matches_official_line_budget() {
        use crate::tool::ToolCall;
        // CC :855-860 — either stream over the line budget marks the result
        // truncated. The trait default was a flat `false`, so fullscreen
        // click-to-expand never lit up for PowerShell rows.
        let long = "line\n".repeat(200);
        assert!(
            PowerShellTool.is_result_truncated(&crate::tool::ToolOutput::PowerShell(
                sample_output(&long, "", false)
            ))
        );
        assert!(
            PowerShellTool.is_result_truncated(&crate::tool::ToolOutput::PowerShell(
                sample_output("", &long, false)
            ))
        );
        assert!(
            !PowerShellTool.is_result_truncated(&crate::tool::ToolOutput::PowerShell(
                sample_output("one\ntwo", "three", false)
            ))
        );
    }

    #[test]
    fn blocked_sleep_detection_matches_official_first_statement_rules() {
        // CC :221-248. Only the FIRST statement counts, `-Milliseconds` and
        // sub-2s sleeps are fine, and the trailing text rides the message.
        assert_eq!(
            detect_blocked_sleep_pattern("Start-Sleep 5").as_deref(),
            Some("standalone Start-Sleep 5")
        );
        assert_eq!(
            detect_blocked_sleep_pattern("sleep 30").as_deref(),
            Some("standalone Start-Sleep 30")
        );
        assert_eq!(
            detect_blocked_sleep_pattern("Start-Sleep -Seconds 10; Get-Date").as_deref(),
            Some("Start-Sleep 10 followed by: Get-Date")
        );
        assert_eq!(
            detect_blocked_sleep_pattern("start-sleep -s 4 | Get-Date").as_deref(),
            Some("Start-Sleep 4 followed by: Get-Date")
        );
        assert_eq!(detect_blocked_sleep_pattern("Start-Sleep 1"), None);
        assert_eq!(
            detect_blocked_sleep_pattern("Start-Sleep -Milliseconds 500"),
            None
        );
        assert_eq!(detect_blocked_sleep_pattern("Start-Sleep 1.5"), None);
        // Not the first statement.
        assert_eq!(
            detect_blocked_sleep_pattern("Get-Date; Start-Sleep 30"),
            None
        );
        assert_eq!(detect_blocked_sleep_pattern("Get-ChildItem ."), None);
        // Multi-byte input must not panic in the slice-the-rest step.
        assert_eq!(
            detect_blocked_sleep_pattern("Start-Sleep 5; Write-Output '日本語'").as_deref(),
            Some("Start-Sleep 5 followed by: Write-Output '日本語'")
        );
    }

    #[test]
    fn autobackgrounding_denylist_matches_official_canonical_names() {
        // CC :198-213 — both `start-sleep` and the `sleep` built-in alias.
        assert!(!is_autobackgrounding_allowed("Start-Sleep 60"));
        assert!(!is_autobackgrounding_allowed("sleep 60"));
        assert!(is_autobackgrounding_allowed("npm run build"));
        assert!(is_autobackgrounding_allowed(""));
    }

    #[test]
    fn powershell_input_semantic_preprocessing_matches_official_schema() {
        let parsed = crate::tool::ToolCall::normalize_input(
            &PowerShellTool,
            &serde_json::json!({
                "command": "Write-Output ok",
                "timeout": "2500",
                "run_in_background": "true",
                "dangerouslyDisableSandbox": "false"
            }),
        );
        assert_eq!(parsed["timeout"], serde_json::json!(2500));
        assert_eq!(parsed["run_in_background"], serde_json::json!(true));
        assert_eq!(
            parsed["dangerouslyDisableSandbox"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn powershell_search_read_classification_matches_official_alias_and_pipeline_rules() {
        use crate::tool::{SearchOrReadCommand, ToolCall};

        assert_eq!(
            is_search_or_read_powershell_command("Get-ChildItem ."),
            SearchOrReadCommand {
                is_search: true,
                is_read: true,
            }
        );
        assert_eq!(
            is_search_or_read_powershell_command("gc README.md | Write-Output"),
            SearchOrReadCommand {
                is_search: false,
                is_read: true,
            }
        );
        assert_eq!(
            is_search_or_read_powershell_command("sls needle src\\*.rs"),
            SearchOrReadCommand {
                is_search: true,
                is_read: false,
            }
        );
        assert_eq!(
            is_search_or_read_powershell_command("echo hello"),
            SearchOrReadCommand::default()
        );
        assert_eq!(
            is_search_or_read_powershell_command("Get-Content x; Remove-Item y"),
            SearchOrReadCommand::default()
        );
        // The source strips PATHEXT before its single alias lookup.
        assert_eq!(
            is_search_or_read_powershell_command("where.exe git"),
            SearchOrReadCommand::default()
        );
        assert_eq!(
            PowerShellTool.is_search_or_read_command(&serde_json::json!({
                "command": "Select-String -Path src\\*.rs -Pattern needle"
            })),
            Some(SearchOrReadCommand {
                is_search: true,
                is_read: false,
            })
        );
        assert_eq!(
            PowerShellTool.is_search_or_read_command(&serde_json::json!({})),
            Some(SearchOrReadCommand::default())
        );
    }

    #[test]
    fn powershell_tool_maps_official_output_to_model_content_and_display() {
        let data =
            crate::tool::ToolOutput::PowerShell(sample_output("\n\nhello\n", " warn \n", false));
        let (content, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &PowerShellTool,
            &data,
            "toolu_ps",
        );
        assert_eq!(content, "hello\nwarn");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        // The raw `toolUseResult` carries the unprocessed streams; the
        // render layer parses it back with `ui::parse_output`.
        let raw = crate::tool::ToolCall::tool_use_result(&PowerShellTool, &data)
            .expect("powershell raw rides");
        assert_eq!(raw["stdout"], serde_json::json!("\n\nhello\n"));
        assert_eq!(raw["stderr"], serde_json::json!(" warn \n"));

        let data = crate::tool::ToolOutput::PowerShell(sample_output("", "timeout", true));
        let (content, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &PowerShellTool,
            &data,
            "toolu_ps",
        );
        assert_eq!(
            content,
            "timeout\n<error>Command was aborted before completion</error>"
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);
    }

    #[test]
    fn powershell_tool_visibility_matches_official_windows_build_gate() {
        use crate::utils::build_profile::BuildAudience;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        assert!(!is_powershell_tool_enabled_for_platform(
            "macos",
            BuildAudience::AnthropicInternal,
            Some("1")
        ));
        assert!(is_powershell_tool_enabled_for_platform(
            "windows",
            BuildAudience::AnthropicInternal,
            None
        ));
        assert!(!is_powershell_tool_enabled_for_platform(
            "windows",
            BuildAudience::AnthropicInternal,
            Some("0")
        ));
        assert!(!is_powershell_tool_enabled_for_platform(
            "windows",
            BuildAudience::External,
            None
        ));
        assert!(is_powershell_tool_enabled_for_platform(
            "windows",
            BuildAudience::External,
            Some("true")
        ));

        let _disabled = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        let schema = powershell_tool_schema();
        assert_eq!(schema.name, "PowerShell");
        assert!(schema.description.contains("PowerShell Syntax Notes"));
        assert!(
            schema
                .input_schema
                .get("properties")
                .and_then(|properties| properties.get("command"))
                .is_some()
        );
        assert!(
            schema
                .input_schema
                .pointer("/properties/run_in_background")
                .is_some()
        );
        assert!(
            schema
                .input_schema
                .pointer("/properties/dangerouslyDisableSandbox")
                .is_some()
        );
    }

    /// CC reads `isBackgroundTasksDisabled` once, at module load
    /// (`BashTool.tsx:332-334`, shared by PowerShellTool), so the off state
    /// needs its own test — nextest gives it its own process.
    #[test]
    fn powershell_tool_schema_omits_background_when_gate_is_off() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _enabled = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "true");
        let schema = powershell_tool_schema();
        assert!(
            schema
                .input_schema
                .pointer("/properties/run_in_background")
                .is_none()
        );
        assert!(
            schema
                .input_schema
                .pointer("/properties/dangerouslyDisableSandbox")
                .is_some()
        );
    }
}
